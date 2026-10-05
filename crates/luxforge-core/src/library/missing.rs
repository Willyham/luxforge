//! Resolving missing originals, per photograph, grouped by the folder on disk each was developed
//! from (`docs/design/catalog.md`, "Missing originals"; `docs/specs/source-recovery.md`, "Later:
//! folder moves and assistance").
//!
//! - **What is missing** ([`missing`], `source.missing`): every photograph not removed whose
//!   original was last recorded offline, missing or changed ([`super::availability`]), grouped by
//!   its source folder with one grouped query, each group's reason from one look at its volume's
//!   mount point and, on a mounted volume, one at its folder. Nothing is looked at per file.
//! - **Find** ([`search`], `source.find`): on the lane's worker, one bounded, cancellable walk of
//!   one chosen folder, candidates by file name and length, then each candidate's streamed
//!   fingerprint, one file at a time; each photograph's result is settled against the photographs
//!   that already name each file ([`settle`]), which are never taken. A find changes nothing: the
//!   files it verified are remembered on the owner ([`Verifications`]) for a relink.
//! - **Relink** ([`plan_relink`], [`commit_relink`], `source.relink`): the pairs a find verified,
//!   each still the file it verified and named by no other photograph, in one transaction as one
//!   library change of `asset-source` items, as a Locate writes one ([`super::locate`]), undone
//!   with `library.undo`. Any other pair refuses the whole request, changing nothing.
//!
//! No file is ever written, moved or renamed: originals are only read.
#[cfg(test)]
mod resolve_missing_tests;
mod search;

pub(crate) use search::{SearchJob, SearchLimits, Searched, search};

use super::{
    availability,
    journal::{self, Desired, Outcome, Request},
    locate,
};
use crate::{
    AssetId, Error,
    atomic_file::file_error,
    catalog_types::{
        AssetRowId, AvailabilityRow, CatalogFolderId, FileAvailability, FindReport, FindResult,
        FindRow, FolderRef, LibraryChangeRow, LibraryItem, MAX_LIBRARY_BATCH, MissingGroup,
        MissingOriginals, MissingReason, RelinkPair, Volume, VolumeId,
    },
    editor::{SourceSignature, library_rows, source_signature, upsert_volume},
};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Files finds verified that the owner remembers for relinks, across every photograph; past it the
/// photographs remembered longest ago are forgotten first.
pub(crate) const MAX_REMEMBERED_FILES: usize = 2 * MAX_LIBRARY_BATCH;

/// How many pairs a refused relink names in its error's data; its message names the first.
const NAMED_PAIRS: usize = 100;

// ── What is missing ───────────────────────────────────────────────────────────────────────────

/// One group as the grouped query reads it, before its reason is known.
struct Grouped {
    source_folder: PathBuf,
    volume_id: VolumeId,
    count: u32,
    changed: u32,
    catalog_folders: Vec<FolderRef>,
}

/// Every photograph not removed whose original was last recorded offline, missing or changed,
/// grouped by the folder on disk it was developed from, in folder order: one grouped query over
/// the recorded availability (`assets_by_availability`), then one look at each group's volume's
/// mount point ([`availability::mounted`], once per volume) and, on a mounted volume, one at its
/// folder. A group is `volume-offline` when its volume is not mounted, `folder-gone` when the
/// volume is and the folder is not, `changed` when every photograph in it was recorded changed and
/// `files-gone` otherwise, a group mixing missing and changed originals included. A photograph
/// whose availability no check or refusal has recorded yet is not listed.
pub(crate) fn missing(connection: &Connection) -> Result<MissingOriginals, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT a.source_folder, a.volume_id, f.id, f.name, COUNT(*),
             SUM(a.availability = 'changed')
         FROM assets a JOIN catalog_folders f ON f.id = a.catalog_folder_id
         WHERE a.availability IN ('offline', 'missing', 'changed') AND a.removed_ms IS NULL
         GROUP BY a.source_folder, a.volume_id, f.id
         ORDER BY a.source_folder, a.volume_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut grouped: Vec<Grouped> = Vec::new();
    for row in rows {
        let (folder, volume, folder_id, name, count, changed) = row?;
        let (folder, volume_id) = (PathBuf::from(folder), VolumeId::parse(volume)?);
        let reference = FolderRef {
            id: CatalogFolderId::parse(folder_id)?,
            name,
        };
        let (count, changed) = (count as u32, changed as u32);
        match grouped.last_mut() {
            Some(group) if group.source_folder == folder && group.volume_id == volume_id => {
                group.count += count;
                group.changed += changed;
                group.catalog_folders.push(reference);
            }
            _ => grouped.push(Grouped {
                source_folder: folder,
                volume_id,
                count,
                changed,
                catalog_folders: vec![reference],
            }),
        }
    }
    let mut volumes: HashMap<VolumeId, Option<(Volume, bool)>> = HashMap::new();
    let mut answer = MissingOriginals::default();
    for mut group in grouped {
        if !volumes.contains_key(&group.volume_id) {
            let seen = volume(connection, &group.volume_id)?.map(|volume| {
                let mounted = availability::mounted(&volume);
                (volume, mounted)
            });
            volumes.insert(group.volume_id.clone(), seen);
        }
        let reason = match &volumes[&group.volume_id] {
            Some((volume, false)) => MissingReason::VolumeOffline {
                label: volume.label.clone(),
            },
            _ if !group.source_folder.is_dir() => MissingReason::FolderGone,
            _ if group.changed == group.count => MissingReason::Changed,
            _ => MissingReason::FilesGone,
        };
        group.catalog_folders.sort_by(|a, b| {
            (a.name.to_lowercase(), a.id.as_str()).cmp(&(b.name.to_lowercase(), b.id.as_str()))
        });
        answer.count += group.count;
        answer.groups.push(MissingGroup {
            source_folder: group.source_folder,
            volume_id: group.volume_id,
            count: group.count,
            catalog_folders: group.catalog_folders,
            reason,
        });
    }
    Ok(answer)
}

/// The volume `id` as the catalog records it, if it does.
fn volume(connection: &Connection, id: &VolumeId) -> Result<Option<Volume>, Error> {
    Ok(connection
        .prepare_cached(
            "SELECT mount_point, label, removable, platform_id, last_seen_ms FROM volumes
             WHERE id = ?1",
        )?
        .query_row([id.as_str()], |row| {
            Ok(Volume {
                id: id.clone(),
                mount_point: row.get::<_, String>(0)?.into(),
                label: row.get(1)?,
                removable: row.get(2)?,
                platform_id: row.get(3)?,
                last_seen_ms: row.get(4)?,
            })
        })
        .optional()?)
}

// ── Find ──────────────────────────────────────────────────────────────────────────────────────

/// One photograph a find looks for: its original's file name, length and fingerprint.
#[derive(Clone, Debug)]
pub(crate) struct Sought {
    pub asset_id: AssetId,
    pub file_name: String,
    pub byte_len: u64,
    pub fingerprint: String,
}

/// The folder a find searches, checked on the owner before anything is walked: an absolute path to
/// a folder that can be read (`validation` or `read-error` otherwise), answered canonical.
pub(crate) fn search_root(path: &Path) -> Result<PathBuf, Error> {
    if !path.is_absolute() {
        return Err(Error::validation(format!(
            "search_root must be an absolute path, not {}",
            path.display()
        )));
    }
    let unreadable =
        |error: std::io::Error| file_error(format!("cannot read {}", path.display()), error.kind());
    if !path.metadata().map_err(unreadable)?.is_dir() {
        return Err(Error::validation(format!(
            "{} is not a folder; choose the folder to search",
            path.display()
        )));
    }
    path.canonicalize().map_err(unreadable)
}

/// The photographs `assets` names, as a find looks for them: one indexed read each.
pub(crate) fn sought(
    connection: &Connection,
    assets: &[(AssetId, AssetRowId)],
) -> Result<Vec<Sought>, Error> {
    let mut statement = connection
        .prepare_cached("SELECT file_name, byte_len, fingerprint FROM assets WHERE row_id = ?1")?;
    let mut sought = Vec::with_capacity(assets.len());
    for (asset_id, row) in assets {
        let (file_name, byte_len, fingerprint): (String, i64, String) = statement
            .query_row([row.0], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .optional()?
            .ok_or_else(|| Error::validation(format!("unknown asset {asset_id}")))?;
        sought.push(Sought {
            asset_id: asset_id.clone(),
            file_name,
            byte_len: byte_len as u64,
            fingerprint,
        });
    }
    nonempty(sought)
}

/// Every photograph not removed whose original was last recorded offline, missing or changed and
/// that was developed from `folder`, as `source.missing` groups them, by file name: one indexed
/// read. More than [`MAX_LIBRARY_BATCH`] is `resource-limit`.
pub(crate) fn sought_from(connection: &Connection, folder: &Path) -> Result<Vec<Sought>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT id, file_name, byte_len, fingerprint FROM assets
         WHERE availability IN ('offline', 'missing', 'changed') AND source_folder = ?1
             AND removed_ms IS NULL
         ORDER BY file_name, id LIMIT ?2",
    )?;
    let rows = statement.query_map(
        rusqlite::params![folder.to_string_lossy(), MAX_LIBRARY_BATCH as i64 + 1],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    let mut sought = Vec::new();
    for row in rows {
        let (asset_id, file_name, byte_len, fingerprint) = row?;
        sought.push(Sought {
            asset_id: AssetId::parse(asset_id)?,
            file_name,
            byte_len: byte_len as u64,
            fingerprint,
        });
    }
    if sought.len() > MAX_LIBRARY_BATCH {
        return Err(Error::resource_limit(format!(
            "more than {MAX_LIBRARY_BATCH} missing photographs were developed from {}",
            folder.display()
        )));
    }
    if sought.is_empty() {
        return Err(Error::validation(format!(
            "no photograph whose original is missing was developed from {}",
            folder.display()
        )));
    }
    Ok(sought)
}

fn nonempty(sought: Vec<Sought>) -> Result<Vec<Sought>, Error> {
    if sought.is_empty() {
        return Err(Error::validation("targets name no photograph to look for"));
    }
    Ok(sought)
}

/// A same-name file a search proved to hold a photograph's original bytes, with its signature
/// while it was read.
#[derive(Clone, Debug)]
pub(crate) struct SameBytes {
    pub path: PathBuf,
    pub signature: SourceSignature,
}

/// What a search found for one photograph, before it is settled against the photographs that name
/// each file.
#[derive(Clone, Debug, Default)]
pub(crate) struct Looked {
    /// The same-name files whose bytes are the original's, in path order.
    pub same: Vec<SameBytes>,
    /// The first same-name file, in path order, whose bytes differ, or that changed while it was
    /// read and so could not be verified.
    pub different: Option<PathBuf>,
}

/// The photographs that name each of `files` — by canonical path or by file identity, both unique
/// in the catalog — in order, each list at most two: one indexed read a file.
pub(crate) fn claimants(
    connection: &Connection,
    files: &[(PathBuf, String)],
) -> Result<Vec<Vec<AssetId>>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT id FROM assets WHERE canonical_locator = ?1
         UNION SELECT id FROM assets WHERE file_identity = ?2",
    )?;
    files
        .iter()
        .map(|(path, identity)| {
            statement
                .query_map(rusqlite::params![path.to_string_lossy(), identity], |row| {
                    row.get::<_, String>(0)
                })?
                .map(|id| AssetId::parse(id?))
                .collect()
        })
        .collect()
}

/// One photograph's result, from what the search found for it and the photographs that name each
/// of its same-bytes files (`claims`, aligned with `looked.same`), and the files it may be relinked
/// to: one same-bytes file no other photograph names is `found`; several are `several-identical`,
/// to choose between; one another photograph names, and none free, is `claimed`, never taken;
/// otherwise a same-name file whose bytes differ is `different-bytes`, and nothing is `not-found`.
pub(crate) fn settle<'a>(
    sought: &Sought,
    looked: &'a Looked,
    claims: &[Vec<AssetId>],
) -> (FindResult, Vec<&'a SameBytes>) {
    let mut free = Vec::new();
    let mut taken = None;
    for (file, claimants) in looked.same.iter().zip(claims) {
        match claimants.iter().find(|other| **other != sought.asset_id) {
            Some(other) => {
                taken.get_or_insert((file, other));
            }
            None => free.push(file),
        }
    }
    let result = match (free.as_slice(), taken) {
        ([one], _) => FindResult::Found {
            path: one.path.clone(),
        },
        ([], Some((file, by))) => FindResult::Claimed {
            path: file.path.clone(),
            by: by.clone(),
        },
        ([], None) => match &looked.different {
            Some(path) => FindResult::DifferentBytes { path: path.clone() },
            None => FindResult::NotFound,
        },
        (several, _) => FindResult::SeveralIdentical {
            paths: several.iter().map(|file| file.path.clone()).collect(),
        },
    };
    (result, free)
}

/// A finished search's answer, and what it leaves the owner to remember.
#[derive(Debug)]
pub(crate) struct Reported {
    pub report: FindReport,
    /// Each photograph with the files it may be relinked to, none for most results.
    pub verified: Vec<(AssetId, Vec<VerifiedFile>)>,
}

/// A finished search's report, each photograph settled against the photographs that name each
/// file now (so a file taken while the search ran is `claimed`), with the files each may be
/// relinked to. On the owner: one indexed read per same-bytes file.
pub(crate) fn report(connection: &Connection, searched: &Searched) -> Result<Reported, Error> {
    let files: Vec<(PathBuf, String)> = searched
        .looked
        .iter()
        .flat_map(|looked| &looked.same)
        .map(|file| (file.path.clone(), file.signature.file_identity().to_owned()))
        .collect();
    let mut claims = claimants(connection, &files)?.into_iter();
    let volume = Arc::new(searched.volume.clone());
    let mut rows = Vec::with_capacity(searched.sought.len());
    let mut verified = Vec::with_capacity(searched.sought.len());
    for (sought, looked) in searched.sought.iter().zip(&searched.looked) {
        let mine: Vec<_> = claims.by_ref().take(looked.same.len()).collect();
        let (result, free) = settle(sought, looked, &mine);
        rows.push(FindRow {
            asset_id: sought.asset_id.clone(),
            file_name: sought.file_name.clone(),
            result,
        });
        verified.push((
            sought.asset_id.clone(),
            free.into_iter()
                .map(|file| VerifiedFile {
                    path: file.path.clone(),
                    signature: file.signature.clone(),
                    volume: volume.clone(),
                })
                .collect(),
        ));
    }
    Ok(Reported {
        report: FindReport { rows },
        verified,
    })
}

// ── What finds verified ───────────────────────────────────────────────────────────────────────

/// A file a find verified as a photograph's original: its signature while it was read, and the
/// volume it is on.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedFile {
    pub path: PathBuf,
    pub signature: SourceSignature,
    pub volume: Arc<Volume>,
}

/// The files finds verified, remembered on the owner so that a relink commits exactly what was
/// verified: per photograph, the files its latest finished find verified as its original and no
/// other photograph named. A newer find's result for a photograph replaces its files, whatever it
/// found; a file that changed since is forgotten when a relink finds it so. At most
/// [`MAX_REMEMBERED_FILES`], the photographs remembered longest ago forgotten first. Nothing here
/// is catalog data: it is lost, harmlessly, when the owner stops.
#[derive(Debug, Default)]
pub(crate) struct Verifications {
    photographs: HashMap<AssetId, (u64, Vec<VerifiedFile>)>,
    /// Each photograph by when it was remembered, oldest first.
    order: BTreeMap<u64, AssetId>,
    next: u64,
    files: usize,
}

impl Verifications {
    /// Remember what a find verified for `asset_id`, replacing what an earlier one did.
    pub(crate) fn remember(&mut self, asset_id: AssetId, files: Vec<VerifiedFile>) {
        self.drop_photograph(&asset_id);
        if files.is_empty() {
            return;
        }
        self.next += 1;
        self.files += files.len();
        self.order.insert(self.next, asset_id.clone());
        self.photographs.insert(asset_id, (self.next, files));
        while self.files > MAX_REMEMBERED_FILES {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, files)) = self.photographs.remove(&oldest) {
                self.files -= files.len();
            }
        }
    }

    /// The file at `path` a find verified for `asset_id`, if one did.
    pub(crate) fn get(&self, asset_id: &AssetId, path: &Path) -> Option<&VerifiedFile> {
        self.photographs
            .get(asset_id)?
            .1
            .iter()
            .find(|file| file.path == path)
    }

    /// Forget the file at `path` for `asset_id`: it is no longer the file that was verified.
    pub(crate) fn forget(&mut self, asset_id: &AssetId, path: &Path) {
        let Some((_, files)) = self.photographs.get_mut(asset_id) else {
            return;
        };
        let before = files.len();
        files.retain(|file| file.path != path);
        self.files -= before - files.len();
        if files.is_empty() {
            self.drop_photograph(asset_id);
        }
    }

    fn drop_photograph(&mut self, asset_id: &AssetId) {
        if let Some((sequence, files)) = self.photographs.remove(asset_id) {
            self.order.remove(&sequence);
            self.files -= files.len();
        }
    }
}

// ── Relink ────────────────────────────────────────────────────────────────────────────────────

/// A pair a relink commits: a photograph and the file a find verified for it.
#[derive(Clone, Debug)]
pub(crate) struct Relink {
    pub asset_id: AssetId,
    pub file: VerifiedFile,
}

/// Why a pair cannot be relinked.
enum Refused {
    NotVerified,
    Changed,
    Gone,
    Claimed(AssetId),
    SameFile,
}

impl Refused {
    fn code(&self) -> &'static str {
        match self {
            Self::NotVerified => "not-verified",
            Self::Changed => "changed",
            Self::Gone => "gone",
            Self::Claimed(_) => "claimed",
            Self::SameFile => "same-file",
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::NotVerified => {
                "no finished search verified it as this photograph's original".to_owned()
            }
            Self::Changed => "it changed after it was verified".to_owned(),
            Self::Gone => "it is no longer there".to_owned(),
            Self::Claimed(other) => format!("it is already the original of photograph {other}"),
            Self::SameFile => "it is the same file as another pair's".to_owned(),
        }
    }
}

/// Check every pair of a relink on the owner, before anything is written: each photograph must
/// exist and each path be absolute, and no photograph or file may be named twice (`validation`);
/// more than [`MAX_LIBRARY_BATCH`] pairs is `resource-limit`. A photograph already pointing at its
/// pair's file needs nothing. Every other pair must be a file named by no other photograph, which
/// a finished find verified for that photograph ([`Verifications`]) and which still has the
/// signature it was verified with; otherwise the whole request is refused, naming every such pair
/// in `data.pairs` (the first 100, with `data.count`): `source-unavailable` when any file is gone,
/// and `conflict` otherwise. A file found changed is forgotten. A stat and a few indexed reads a
/// pair.
pub(crate) fn plan_relink(
    connection: &Connection,
    verified: &mut Verifications,
    pairs: &[RelinkPair],
) -> Result<Vec<Relink>, Error> {
    if pairs.is_empty() {
        return Err(Error::validation("pairs names no photograph to relink"));
    }
    if pairs.len() > MAX_LIBRARY_BATCH {
        return Err(Error::resource_limit(format!(
            "{} pairs is more than the {MAX_LIBRARY_BATCH} one relink covers",
            pairs.len()
        )));
    }
    let mut photographs = HashSet::with_capacity(pairs.len());
    let mut paths = HashSet::with_capacity(pairs.len());
    let mut identities = HashSet::with_capacity(pairs.len());
    let mut refused = Vec::new();
    let mut relinks = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let RelinkPair { asset_id, path } = pair;
        if !path.is_absolute() {
            return Err(Error::validation(format!(
                "a pair's path must be absolute, not {}",
                path.display()
            )));
        }
        if !photographs.insert(asset_id) {
            return Err(Error::validation(format!(
                "photograph {asset_id} is named by two pairs"
            )));
        }
        let source = library_rows::asset_source(connection, asset_id)?
            .ok_or_else(|| Error::validation(format!("unknown asset {asset_id}")))?;
        let Ok(canonical) = path.canonicalize() else {
            refused.push((pair, Refused::Gone));
            continue;
        };
        if !paths.insert(canonical.clone()) {
            return Err(Error::validation(format!(
                "{} is named by two pairs: one file is the original of one photograph",
                canonical.display()
            )));
        }
        if source.locator == canonical {
            continue;
        }
        let Ok(metadata) = canonical.metadata() else {
            refused.push((pair, Refused::Gone));
            continue;
        };
        let now = source_signature(&canonical, &metadata);
        if let Some(other) =
            locate::claimed_by(connection, asset_id, &canonical, now.file_identity())?
        {
            refused.push((pair, Refused::Claimed(other)));
            continue;
        }
        let Some(file) = verified.get(asset_id, &canonical).cloned() else {
            refused.push((pair, Refused::NotVerified));
            continue;
        };
        if now != file.signature {
            verified.forget(asset_id, &canonical);
            refused.push((pair, Refused::Changed));
            continue;
        }
        if !identities.insert(now.file_identity().to_owned()) {
            refused.push((pair, Refused::SameFile));
            continue;
        }
        relinks.push(Relink {
            asset_id: asset_id.clone(),
            file,
        });
    }
    if refused.is_empty() {
        Ok(relinks)
    } else {
        Err(refusal(&refused))
    }
}

/// The refusal of a relink with pairs that cannot be committed: nothing changes.
fn refusal(refused: &[(&RelinkPair, Refused)]) -> Error {
    let (first, why) = &refused[0];
    let more = match refused.len() {
        1 => String::new(),
        count => format!(", and {} more pairs cannot be relinked", count - 1),
    };
    let detail = format!(
        "nothing was relinked: {} for photograph {}: {}{more}",
        first.path.display(),
        first.asset_id,
        why.describe()
    );
    let pairs: Vec<_> = refused
        .iter()
        .take(NAMED_PAIRS)
        .map(|(pair, why)| {
            let mut named =
                json!({"asset_id": pair.asset_id, "path": pair.path, "reason": why.code()});
            if let Refused::Claimed(other) = why {
                named["by"] = json!(other);
            }
            named
        })
        .collect();
    let error = if refused.iter().any(|(_, why)| matches!(why, Refused::Gone)) {
        Error::source_unavailable(detail)
    } else {
        Error::conflict(detail)
    };
    error.with_data(json!({"pairs": pairs, "count": refused.len()}))
}

/// Commit a planned relink in the owner's transaction: each file's volume recorded, each
/// photograph's `asset-source` item pointed at its file (its source folder becoming the file's
/// folder) as one library change labelled "Relinked <file>" or "Relinked N originals", and each
/// original recorded available at `checked_ms`. History, fingerprints and interpretations are not
/// touched.
pub(crate) fn commit_relink(
    tx: &Transaction<'_>,
    request: Request<'_>,
    relinks: &[Relink],
    checked_ms: i64,
) -> Result<Outcome, Error> {
    let mut volumes = HashSet::new();
    for relink in relinks {
        if volumes.insert(&relink.file.volume.id) {
            upsert_volume(tx, &relink.file.volume)?;
        }
    }
    let changes = relinks
        .iter()
        .map(|relink| {
            let source = locate::source_value(
                &relink.file.path,
                relink.file.volume.id.clone(),
                relink.file.signature.file_identity(),
            );
            Ok((
                LibraryItem::AssetSource {
                    asset_id: relink.asset_id.clone(),
                },
                Desired::Value(Some(locate::encode_source(&source)?)),
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let outcome = journal::apply(tx, request, changes, relink_label)?;
    if !matches!(
        outcome,
        Outcome::Recorded {
            deduplicated: true,
            ..
        }
    ) {
        let rows: Vec<_> = relinks
            .iter()
            .map(|relink| AvailabilityRow {
                asset_id: relink.asset_id.clone(),
                availability: FileAvailability::Available,
                checked_ms,
            })
            .collect();
        availability::record(tx, &rows)?;
    }
    Ok(outcome)
}

/// The label of a relink: "Relinked DSC_0412.NEF", "Relinked 18 originals".
fn relink_label(rows: &[LibraryChangeRow]) -> String {
    match rows {
        [row] => format!("Relinked {}", locate::file_name_of(row)),
        rows => format!("Relinked {} originals", rows.len()),
    }
}
