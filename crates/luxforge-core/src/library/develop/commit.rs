//! Committing a batch of developed picks, on the owner: what each file becomes, decided against
//! the catalog, and the one library change that records the batch.
//!
//! - **Linked**: its bytes are already a photograph's. The file is that photograph's original
//!   (the same path or file identity), or a photograph with its fingerprint has its original
//!   available; or no photograph with its fingerprint can take it (below). Nothing is added.
//! - **Relinked**: a photograph with its fingerprint, file name and length whose original is not
//!   where the catalog looks for it (missing, offline or changed) now points at it, as Locate
//!   points a photograph at a file (`asset-source`). Its catalog folder, collections and history
//!   stay as they are.
//! - **Created**: a new photograph in the batch's catalog folder, with its fingerprint, its
//!   interpretation, its Original entry, its capture row and the folder on disk it came from.
//!
//! A file that is another photograph's original with other bytes is refused (`conflict`, naming
//! that photograph): its original changed since it was developed, and two photographs are never
//! made of one file. Identical bytes are never added twice, within a batch too.
use super::{Developed, plan::Destination};
use crate::{
    AssetId, AssetRecord, EditorService, Error,
    catalog_types::{
        AssetSourceValue, AvailabilityRow, DevelopOutcome, DevelopedPick, FileAvailability,
        ItemFailure, LibraryItem,
    },
    editor::{NewAsset, NewPhotograph, insert_photograph, source_signature, upsert_volume},
    library::{
        availability,
        items,
        journal::{self, Desired, Outcome, Request},
        locate,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;
use std::{collections::HashMap, path::Path};

/// What one developed file becomes.
#[derive(Debug)]
pub(crate) enum Becomes {
    Created(Box<NewPhotograph>),
    Linked(AssetId),
    Relinked {
        asset: AssetId,
        source: AssetSourceValue,
    },
}

/// One developed file with what it becomes.
#[derive(Debug)]
pub(crate) struct Decided {
    pub developed: Developed,
    pub becomes: Becomes,
}

impl Decided {
    /// The file as a Develop's report lists it.
    pub(crate) fn reported(&self) -> DevelopedPick {
        let (asset_id, outcome) = match &self.becomes {
            Becomes::Created(photograph) => (photograph.asset.id.clone(), DevelopOutcome::Created),
            Becomes::Linked(asset) => (asset.clone(), DevelopOutcome::Linked),
            Becomes::Relinked { asset, .. } => (asset.clone(), DevelopOutcome::Relinked),
        };
        DevelopedPick {
            path: self.developed.pick.clone(),
            used: self.developed.used.clone(),
            asset_id,
            outcome,
        }
    }
}

/// Decide what each file of a batch becomes, on the owner, reading the catalog and one stat per
/// candidate original: each file must still be the file that was read, then it is linked,
/// relinked or created (the module's rules). A new photograph's Original is made and admitted here,
/// outside the transaction that writes it. Answers the decided files, in order, and the ones
/// refused.
pub(crate) fn decide(
    service: &EditorService,
    files: Vec<Developed>,
    now_ms: i64,
) -> Result<(Vec<Decided>, Vec<ItemFailure>), Error> {
    let catalog = &service.connection;
    let mut decided = Vec::with_capacity(files.len());
    let mut failed = Vec::new();
    // What this batch has already made of each fingerprint, and the photographs it relinked.
    let mut made: HashMap<String, AssetId> = HashMap::new();
    let mut relinked: Vec<AssetId> = Vec::new();
    for developed in files {
        let file = &developed.file;
        if !locate::unchanged(&file.path, &file.signature) {
            failed.push(failure(
                &developed.pick,
                None,
                &Error::conflict(format!("{} changed after it was read", file.path.display())),
            ));
            continue;
        }
        let becomes = match made.get(&file.fingerprint) {
            Some(asset) => Becomes::Linked(asset.clone()),
            None => match matching(catalog, file, &relinked)? {
                Match::Named { asset, same_bytes } if same_bytes => Becomes::Linked(asset),
                Match::Named { asset, .. } => {
                    failed.push(failure(
                        &developed.pick,
                        Some(&asset),
                        &Error::conflict(format!(
                            "{} is already the original of photograph {asset}, which was \
                             developed from other bytes",
                            file.path.display()
                        ))
                        .with_data(json!({"asset_id": asset})),
                    ));
                    continue;
                }
                Match::Available(asset) | Match::Unavailable(asset) => Becomes::Linked(asset),
                Match::Relink(asset) => {
                    relinked.push(asset.clone());
                    Becomes::Relinked {
                        asset,
                        source: locate::source_value(
                            &file.path,
                            file.volume.id.clone(),
                            file.signature.file_identity(),
                        ),
                    }
                }
                Match::None => {
                    let record = AssetRecord {
                        id: AssetId::new(),
                        source_root: super::plan::parent(&file.path).to_path_buf(),
                        locator: file.path.clone(),
                        fingerprint: file.fingerprint.clone(),
                        file_identity: file.signature.file_identity().to_owned(),
                        byte_len: file.signature.byte_len(),
                        width: file.width,
                        height: file.height,
                        source: file.source.clone(),
                    };
                    match service.new_photograph(record, now_ms) {
                        Ok(photograph) => {
                            made.insert(file.fingerprint.clone(), photograph.asset.id.clone());
                            Becomes::Created(Box::new(photograph))
                        }
                        Err(error) => {
                            failed.push(failure(&developed.pick, None, &error));
                            continue;
                        }
                    }
                }
            },
        };
        decided.push(Decided { developed, becomes });
    }
    Ok((decided, failed))
}

/// What the catalog holds of a file's bytes.
enum Match {
    /// A photograph whose original is this very file (its path or its file identity), with the
    /// same bytes or not.
    Named { asset: AssetId, same_bytes: bool },
    /// A photograph with its fingerprint whose original is where the catalog looks for it.
    Available(AssetId),
    /// A photograph with its fingerprint, name and length whose original is not.
    Relink(AssetId),
    /// A photograph with its fingerprint whose original is not there and whose name or length
    /// differs: a relink needs both to match, so the file is linked and the photograph stays as
    /// it is.
    Unavailable(AssetId),
    None,
}

/// What the catalog holds of `file`'s bytes, in the order the module states; `relinked` are
/// photographs this batch already relinked, whose originals count as available.
fn matching(
    catalog: &Connection,
    file: &super::ReadFile,
    relinked: &[AssetId],
) -> Result<Match, Error> {
    let named: Option<(String, String)> = catalog
        .prepare_cached(
            "SELECT id, fingerprint FROM assets WHERE canonical_locator = ?1 OR file_identity = ?2
             ORDER BY row_id LIMIT 1",
        )?
        .query_row(
            params![file.path.to_string_lossy(), file.signature.file_identity()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((asset, fingerprint)) = named {
        return Ok(Match::Named {
            asset: AssetId::parse(asset)?,
            same_bytes: fingerprint == file.fingerprint,
        });
    }
    let same: Vec<(String, String, String, i64, String)> = catalog
        .prepare_cached(
            "SELECT id, locator, file_identity, byte_len, file_name FROM assets
             WHERE fingerprint = ?1 ORDER BY row_id",
        )?
        .query_map([&file.fingerprint], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut candidates = Vec::with_capacity(same.len());
    for (asset, locator, identity, byte_len, name) in same {
        let asset = AssetId::parse(asset)?;
        let available = relinked.contains(&asset)
            || original_available(Path::new(&locator), &identity, byte_len as u64);
        if available {
            return Ok(Match::Available(asset));
        }
        candidates.push((asset, byte_len as u64, name));
    }
    let file_name = file
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some((asset, ..)) = candidates.iter().find(|(_, byte_len, name)| {
        *byte_len == file.signature.byte_len() && name.eq_ignore_ascii_case(&file_name)
    }) {
        return Ok(Match::Relink(asset.clone()));
    }
    Ok(candidates
        .into_iter()
        .next()
        .map_or(Match::None, |(asset, ..)| Match::Unavailable(asset)))
}

/// Whether a photograph's recorded original is at `locator`: the file there has its identity and
/// length. One stat.
fn original_available(locator: &Path, identity: &str, byte_len: u64) -> bool {
    locator.metadata().is_ok_and(|metadata| {
        let signature = source_signature(locator, &metadata);
        metadata.is_file()
            && signature.file_identity() == identity
            && signature.byte_len() == byte_len
    })
}

/// Record a batch in the owner's transaction, as one part of the Develop's request: the batch's
/// new catalog folder when it is the first to go into it, each new photograph's rows, each
/// relinked photograph's source, and each file's pick cleared, in that order per file, so the
/// change's undo — which writes its rows back in reverse — re-picks each file before sending its
/// photograph back and deletes the folder last. A new photograph and the folder are written here
/// and recorded as written (`developed-asset`, `catalog-folder`); a relinked photograph's original
/// is recorded available.
pub(crate) fn write(
    tx: &Transaction<'_>,
    request: Request<'_>,
    artifact_root: &Path,
    destination: &Destination,
    decided: &[Decided],
    now_ms: i64,
) -> Result<Outcome, Error> {
    let mut changes = Vec::with_capacity(1 + 2 * decided.len());
    if let Destination::New { folder, .. } = destination {
        let item = LibraryItem::CatalogFolder {
            folder_id: folder.id.clone(),
        };
        if items::read(tx, &item)?.is_none() {
            let value = serde_json::to_value(folder)
                .map_err(|error| Error::internal(format!("cannot encode a folder: {error}")))?;
            items::write(tx, &item, Some(&value))?;
            changes.push((item, Desired::Written { before: None }));
        }
    }
    let folder_id = destination.id();
    let mut relinked = Vec::new();
    for decided in decided {
        let file = &decided.developed.file;
        match &decided.becomes {
            Becomes::Created(photograph) => {
                upsert_volume(tx, &file.volume)?;
                let canonical = file.path.to_string_lossy();
                let file_name = file
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                insert_photograph(
                    tx,
                    artifact_root,
                    &photograph.original,
                    &NewAsset {
                        record: &photograph.asset,
                        canonical_locator: &canonical,
                        catalog_folder_id: folder_id,
                        source_folder: super::plan::parent(&file.path),
                        volume_id: &file.volume.id,
                        file_name: &file_name,
                        developed_ms: now_ms,
                        removed_ms: None,
                        availability: FileAvailability::Available,
                        checked_ms: now_ms,
                        develop_moment: decided.developed.moment.as_ref(),
                    },
                    &file.header,
                    file.place.as_deref(),
                )?;
                changes.push((
                    LibraryItem::DevelopedAsset {
                        asset_id: photograph.asset.id.clone(),
                    },
                    Desired::Written { before: None },
                ));
            }
            Becomes::Relinked { asset, source } => {
                upsert_volume(tx, &file.volume)?;
                changes.push((
                    LibraryItem::AssetSource {
                        asset_id: asset.clone(),
                    },
                    Desired::Value(Some(locate::encode_source(source)?)),
                ));
                relinked.push(AvailabilityRow {
                    asset_id: asset.clone(),
                    availability: FileAvailability::Available,
                    checked_ms: now_ms,
                });
            }
            Becomes::Linked(_) => {}
        }
        changes.push((
            LibraryItem::Pick {
                path: decided.developed.pick.clone(),
            },
            Desired::Value(None),
        ));
    }
    let label = label(decided);
    let outcome = journal::apply_part(tx, request, changes, |_| label)?;
    availability::record(tx, &relinked)?;
    Ok(outcome)
}

/// A batch's label: "Developed DSC_0412.NEF", "Developed 17".
fn label(decided: &[Decided]) -> String {
    match decided {
        [one] => format!(
            "Developed {}",
            one.developed.pick.file_name().map_or_else(
                || one.developed.pick.display().to_string(),
                |name| name.to_string_lossy().into_owned()
            )
        ),
        many => format!("Developed {}", many.len()),
    }
}

/// A pick a Develop did not bring in, as its report lists it.
pub(crate) fn failure(path: &Path, asset: Option<&AssetId>, error: &Error) -> ItemFailure {
    ItemFailure {
        path: path.to_path_buf(),
        asset_id: asset.cloned(),
        code: error.kind.code().to_owned(),
        message: error.detail.clone(),
    }
}
