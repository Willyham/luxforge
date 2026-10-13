//! Merges in the catalog (`docs/design/hdr-merge.md`, "The stack and the originals", "Remove, undo
//! and retention", "Durable derived source"): publishing a result with its provenance and stack as
//! one library change, the `merge-stack` item the journal reads and writes, the rules that keep one
//! active stack per input, exact-match reuse, and what Remove and Put back change for a result.
//!
//! - **Publication** ([`EditorService::publish_merge`]) is one owner transaction after a worker
//!   made the result's source durable: it rechecks every input by stat and catalog row, files the
//!   result by its folder choice for the reference's capture day, the folders that needs made and
//!   recorded first as a change of their own, which the merge's undo leaves
//!   ([`develop::file_one`]), writes the
//!   result photograph (its Original holding the derived source), links the source, records the
//!   merge's provenance and inputs, activates its stack and records one library change. That change
//!   records the result's `asset-removal` and the merge's `merge-stack` as written
//!   ([`Desired::Written`]), from Removed and `undone`: so the journal's generic undo hides the
//!   result in Removed and marks its merge undone, deactivating the stack, and redo restores both,
//!   with the result's source, history and edits untouched and nothing merged again. A caller's
//!   own items join the same change through `extra`. A rerun's publication first replaces the
//!   merge it reruns, as Remove would its result.
//! - **The `merge-stack` item** is the merge's state (`merge_stacks`). Writing `active` claims each
//!   input path for the merge (`stack_claims`), refused with `conflict` naming the stack that
//!   already claims one; writing `removed` or `undone` releases them. A merge Empty Removed deleted
//!   reads absent, and writing it is `conflict`.
//! - **Remove and Put back** of a merge's result ([`lifecycle`]) change its merge's state with its
//!   `asset-removal`, in the same change: general Remove from catalog and Remove merge are one
//!   operation, `crate::library::remove`'s.
//! - **Reuse and overlap** are lookups: [`matching`] finds a recorded merge with the same inputs,
//!   provider and parameters; [`claims`] the active stacks that claim any of a request's inputs.
//!
//! Everything here is SQL on the owner and, at publication, one stat an input; no payload is read
//! or hashed.
// Stack browsing (`docs/design/hdr-merge.md`) calls what the `merge.*` commands and
// Remove and Put back do not yet; until it does, only the tests do.
#![cfg_attr(not(test), allow(dead_code))]
use super::{
    develop::{self, Destination},
    journal::{self, Desired, Outcome, Request},
};
use crate::{
    AssetId, EditorService, Error, SourceKind,
    artifacts::derived_source::PublishedSource,
    catalog_types::{
        AssetRemovalValue, AssetRowId, CaptureTime, FileAvailability, FileSignature,
        HeaderMetadata, LibraryItem, MergeId, MergeKind, MergeMatch, MergeProvenance, MergeRecord,
        MergeStackValue, MergeState, StackClaim, StackMember, Volume,
    },
    editor::{
        AssetRecord, NewAsset, decode, derived_source::link_asset_source, encode,
        insert_photograph, now_ms, upsert_volume, write,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// What a merge's publication commits beside the result's durable source: the provenance, the
/// catalog folder the result is filed by and the name it takes, the reference's header metadata
/// and named place its capture row is written from, and the volumes the inputs are on.
pub(crate) struct Publication<'a> {
    pub provenance: &'a MergeProvenance,
    pub source: &'a PublishedSource,
    /// The folder choice the result is filed by, for the reference's capture day (its header's).
    pub folder: &'a Destination,
    /// The result's name, the reference frame's with "HDR".
    pub file_name: &'a str,
    pub header: &'a HeaderMetadata,
    pub place: Option<&'a str>,
    /// Every volume an input is on, recorded or refreshed with the merge.
    pub volumes: &'a [Volume],
    /// The merge a rerun replaces: while its stack is active, its result goes to Removed and its
    /// stack is deactivated in the same change, so the new result leads the inputs instead.
    pub replaces: Option<&'a MergeId>,
}

/// A published merge: its identity, its result photograph and the library change that made both
/// visible, or, for a retry, the change its first attempt recorded; and the change that made the
/// folders it was filed in, recorded just before it, when it made any.
#[derive(Debug)]
pub(crate) struct PublishedMerge {
    pub merge_id: MergeId,
    pub result: AssetId,
    pub outcome: Outcome,
    pub folders: Option<Outcome>,
}

impl EditorService {
    /// Publish a merge in one catalog transaction, after a worker made `publication.source`
    /// durable: see the module documentation. A retry of `request` answers the merge its first
    /// attempt published and writes nothing. Refused, with nothing written, when the provenance is
    /// not one this build writes, an input is missing or changed since it was read, an input's
    /// catalog photograph is gone, another's or a merge's own, an active stack claims an input,
    /// or anything the result's rows or `extra` write is refused.
    ///
    /// `extra` is the caller's part of the same transaction and library change, run once the merge
    /// and its result exist: the items it answers, each with its own item-level inverse, are
    /// recorded after the result's and the stack's, so the merge's undo reverts exactly them too.
    pub(crate) fn publish_merge(
        &mut self,
        request: Request<'_>,
        publication: &Publication<'_>,
        extra: impl FnOnce(
            &Transaction<'_>,
            &MergeId,
            &AssetId,
        ) -> Result<Vec<(LibraryItem, Desired)>, Error>,
    ) -> Result<PublishedMerge, Error> {
        if let Some(published) = published_by(&self.connection, request)? {
            return Ok(published);
        }
        let provenance = publication.provenance;
        provenance.validate()?;
        let now = now_ms();
        let asset_id = AssetId::new();
        let photograph = self.new_photograph(
            result_record(&asset_id, publication.source),
            publication.header,
            now,
        )?;
        let root = self.artifact_root().to_path_buf();
        write(&mut self.connection, |tx| {
            if let Some(published) = published_by(tx, request)? {
                return Ok(published);
            }
            recheck_inputs(tx, provenance)?;
            // The folders it needs, their own change before the publication's
            // ([`develop::file_one`]): undoing the merge keeps its result in them.
            let day = publication
                .header
                .capture
                .as_ref()
                .map(CaptureTime::local_day);
            let (catalog_folder_id, folders) =
                develop::file_one(tx, request, publication.folder, day, now)?;
            let mut changes = match publication.replaces {
                Some(old) => replace(tx, old, now)?,
                None => Vec::new(),
            };
            for volume in publication.volumes {
                upsert_volume(tx, volume)?;
            }
            let reference = provenance.reference_input();
            let identity = derived_locator(&asset_id);
            insert_photograph(
                tx,
                &root,
                &photograph.original,
                &NewAsset {
                    record: &photograph.asset,
                    canonical_locator: &identity,
                    catalog_folder_id: &catalog_folder_id,
                    source_folder: reference.path.parent().unwrap_or(Path::new("")),
                    volume_id: &reference.volume_id,
                    file_name: publication.file_name,
                    developed_ms: now,
                    removed_ms: None,
                    availability: FileAvailability::Available,
                    checked_ms: now,
                    develop_moment: None,
                },
                publication.header,
                publication.place,
            )?;
            link_asset_source(tx, &root, &asset_id, publication.source)?;
            let merge_id = MergeId::new();
            insert_merge(
                tx,
                &merge_id,
                &asset_id,
                publication.source,
                provenance,
                now,
            )?;
            let item = LibraryItem::MergeStack {
                merge_id: merge_id.clone(),
            };
            claim(tx, &item, &merge_id)?;
            tx.prepare_cached("INSERT INTO merge_stacks (merge_id, state) VALUES (?1, ?2)")?
                .execute([merge_id.as_str(), MergeState::Active.as_str()])?;
            changes.extend([
                (
                    LibraryItem::AssetRemoval {
                        asset_id: asset_id.clone(),
                    },
                    Desired::Written {
                        before: Some(value(AssetRemovalValue { removed_ms: now })?),
                    },
                ),
                (
                    item,
                    Desired::Written {
                        before: Some(value(MergeStackValue {
                            state: MergeState::Undone,
                        })?),
                    },
                ),
            ]);
            changes.extend(extra(tx, &merge_id, &asset_id)?);
            let label = format!("Merged {}", publication.file_name);
            let outcome = journal::apply(tx, request, changes, |_| label)?;
            Ok(PublishedMerge {
                merge_id,
                result: asset_id,
                outcome,
                folders,
            })
        })
    }
}

/// A rerun's replacement of `old`, written before the new merge claims its inputs: while `old`'s
/// stack is active, its result goes to Removed as of `now_ms` and its stack to `removed`, releasing
/// its claims, each recorded as written so the change's undo restores both after undoing the new
/// merge, as Remove does ([`lifecycle`]). Answers the changes. An inactive `old` changes nothing:
/// its result is in Removed already.
fn replace(
    tx: &Transaction<'_>,
    old: &MergeId,
    now_ms: i64,
) -> Result<Vec<(LibraryItem, Desired)>, Error> {
    if state(tx, old)? != Some(MergeState::Active) {
        return Ok(Vec::new());
    }
    let result: String = tx
        .prepare_cached("SELECT result_asset_id FROM merges WHERE id = ?1")?
        .query_row([old.as_str()], |row| row.get(0))?;
    let result = AssetId::parse(result)?;
    let writes = [
        (
            LibraryItem::AssetRemoval { asset_id: result },
            value(AssetRemovalValue { removed_ms: now_ms })?,
        ),
        (
            LibraryItem::MergeStack {
                merge_id: old.clone(),
            },
            value(MergeStackValue {
                state: MergeState::Removed,
            })?,
        ),
    ];
    let mut changes = Vec::with_capacity(writes.len());
    for (item, to) in writes {
        let before = super::items::read(tx, &item)?;
        super::items::write(tx, &item, Some(&to))?;
        changes.push((item, Desired::Written { before }));
    }
    Ok(changes)
}

/// The record of a merge's result photograph: a derived source, no file of its own. Its locator
/// and file identity name the photograph, unique as the catalog requires, and its fingerprint is
/// the source's identity.
fn result_record(asset_id: &AssetId, source: &PublishedSource) -> AssetRecord {
    let identity = derived_locator(asset_id);
    AssetRecord {
        id: asset_id.clone(),
        source_root: PathBuf::new(),
        locator: PathBuf::from(&identity),
        fingerprint: source.id.to_string(),
        file_identity: identity,
        byte_len: source.payload_bytes(),
        width: source.width,
        height: source.height,
        source: SourceKind::Derived {
            interpretation: source.interpretation(),
        },
    }
}

fn derived_locator(asset_id: &AssetId) -> String {
    format!("derived:{asset_id}")
}

/// Check each input is still what the merge read, before anything is published: a file at its
/// path with the signature recorded (`source-unavailable` when it is gone, `conflict` when it
/// changed), and, when it names a catalog photograph, that photograph still in the catalog,
/// developed from that file and not a merge's own result. One stat and one indexed read an input.
fn recheck_inputs(connection: &Connection, provenance: &MergeProvenance) -> Result<(), Error> {
    let mut photograph =
        connection.prepare_cached("SELECT locator, source_kind FROM assets WHERE id = ?1")?;
    for input in &provenance.inputs {
        let name = file_name(&input.path);
        let metadata = input
            .path
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .ok_or_else(|| {
                Error::source_unavailable(format!(
                    "{name} is no longer at {}; the merge was not published",
                    input.path.display()
                ))
            })?;
        if !FileSignature::of(&metadata).unchanged(&input.signature) {
            return Err(Error::conflict(format!(
                "{name} changed since the merge read it; the merge was not published"
            )));
        }
        if let Some(asset) = &input.asset_id {
            let row: Option<(String, String)> = photograph
                .query_row([asset.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            match row {
                None => {
                    return Err(Error::conflict(format!(
                        "{name}'s photograph {asset} is no longer in the catalog"
                    )));
                }
                Some((_, kind)) if kind == "derived" => return Err(generated(asset)),
                Some((locator, _)) if Path::new(&locator) != input.path => {
                    return Err(Error::conflict(format!(
                        "photograph {asset} is no longer developed from {name}"
                    )));
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

/// Refuse a merge's own result, or any derived photograph, as a merge input: inputs are original
/// files or the photographs developed from them, never another generated result. One indexed read.
pub(crate) fn refuse_generated(connection: &Connection, asset: &AssetId) -> Result<(), Error> {
    let kind: Option<String> = connection
        .prepare_cached("SELECT source_kind FROM assets WHERE id = ?1")?
        .query_row([asset.as_str()], |row| row.get(0))
        .optional()?;
    match kind.as_deref() {
        None => Err(Error::validation(format!("unknown asset {asset}"))),
        Some("derived") => Err(generated(asset)),
        Some(_) => Ok(()),
    }
}

fn generated(asset: &AssetId) -> Error {
    Error::validation(format!(
        "photograph {asset} is a merged photograph; a merge takes original files only"
    ))
}

/// The name of `asset` when it is a merged photograph, with a derived source and no original file.
/// One indexed read.
pub(crate) fn merged(connection: &Connection, asset: &AssetId) -> Result<Option<String>, Error> {
    Ok(connection
        .prepare_cached("SELECT file_name FROM assets WHERE id = ?1 AND source_kind = 'derived'")?
        .query_row([asset.as_str()], |row| row.get(0))
        .optional()?)
}

/// Refuse to look for, locate or relink a merged photograph's original: its pixels are the merge's
/// durable derived source, and there is no file anywhere to find. Names it in `data.asset_id`.
pub(crate) fn no_original(asset: &AssetId, file_name: &str, action: &str) -> Error {
    Error::validation(format!(
        "{file_name} is a merged photograph and has no original file to {action}"
    ))
    .with_data(json!({ "asset_id": asset }))
}

/// Write the merge's immutable rows: its provenance and its inputs in the stack's order, each with
/// its resolved EV offset when its kind has one (HDR) and none otherwise.
fn insert_merge(
    tx: &Transaction<'_>,
    merge_id: &MergeId,
    result: &AssetId,
    source: &PublishedSource,
    provenance: &MergeProvenance,
    now: i64,
) -> Result<(), Error> {
    tx.prepare_cached(
        "INSERT INTO merges (id, kind, result_asset_id, source_id, match_key, provenance_json,
             created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        merge_id.as_str(),
        provenance.kind.as_str(),
        result.as_str(),
        source.id.as_str(),
        provenance.match_key()?,
        encode(provenance)?,
        now,
    ])?;
    let mut input = tx.prepare_cached(
        "INSERT INTO merge_inputs (merge_id, ordinal, path, asset_id, offset_ev)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (ordinal, merge_input) in provenance.inputs.iter().enumerate() {
        input.execute(params![
            merge_id.as_str(),
            ordinal as i64,
            merge_input.path.to_string_lossy(),
            merge_input.asset_id.as_ref().map(AssetId::as_str),
            provenance.resolved.offset_ev(ordinal),
        ])?;
    }
    Ok(())
}

/// The merge a request published, when it published one: a retry is answered from it. An exact
/// active match's reuse publishes nothing and records no change, so its retry is answered as a new
/// request, reusing the result again.
pub(crate) fn published_by(
    connection: &Connection,
    request: Request<'_>,
) -> Result<Option<PublishedMerge>, Error> {
    let Some(change) = journal::find(connection, request)? else {
        return Ok(None);
    };
    let detail = journal::inspect(connection, change.sequence.0)?;
    let merge_id = detail
        .rows
        .iter()
        .find_map(|row| match &row.item {
            // A rerun records the old stack's removal before the new stack's publication.
            // Identify the publication by its transition, rather than choosing the first stack:
            // only the newly published merge starts undone and becomes active.
            LibraryItem::MergeStack { merge_id }
                if row
                    .before
                    .as_ref()
                    .and_then(|value| value.get("state"))
                    .and_then(Value::as_str)
                    == Some(MergeState::Undone.as_str())
                    && row
                        .after
                        .as_ref()
                        .and_then(|value| value.get("state"))
                        .and_then(Value::as_str)
                        == Some(MergeState::Active.as_str()) =>
            {
                Some(merge_id.clone())
            }
            _ => None,
        })
        .ok_or_else(|| {
            Error::conflict(format!(
                "request {} recorded a library change that published no merge",
                request.request_id
            ))
        })?;
    let result: Option<String> = connection
        .prepare_cached("SELECT result_asset_id FROM merges WHERE id = ?1")?
        .query_row([merge_id.as_str()], |row| row.get(0))
        .optional()?;
    let result = result.ok_or_else(|| {
        Error::conflict(format!(
            "merge {merge_id}, which this request published, was deleted by emptying Removed"
        ))
    })?;
    Ok(Some(PublishedMerge {
        merge_id,
        result: AssetId::parse(result)?,
        outcome: Outcome::deduplicated(detail.change),
        folders: None,
    }))
}

/// A merge's state, or `None` when the catalog no longer has it.
pub(crate) fn state(connection: &Connection, merge: &MergeId) -> Result<Option<MergeState>, Error> {
    let state: Option<String> = connection
        .prepare_cached("SELECT state FROM merge_stacks WHERE merge_id = ?1")?
        .query_row([merge.as_str()], |row| row.get(0))
        .optional()?;
    state.as_deref().map(MergeState::parse).transpose()
}

/// Set the `merge-stack` item `item` (of `merge`) to `to`: claim every input for an active stack,
/// refused with `conflict` naming the stack that claims one already, or release them. A merge no
/// longer in the catalog is `conflict`, as an undo of a change to it must be.
pub(crate) fn set_state(
    tx: &Transaction<'_>,
    item: &LibraryItem,
    merge: &MergeId,
    to: MergeState,
) -> Result<(), Error> {
    if state(tx, merge)?.is_none() {
        return Err(Error::conflict(format!(
            "merge {merge} is no longer in the catalog: Empty Removed deleted it"
        ))
        .with_data(json!({"items": [item], "count": 1})));
    }
    if to == MergeState::Active {
        claim(tx, item, merge)?;
    } else {
        tx.prepare_cached("DELETE FROM stack_claims WHERE merge_id = ?1")?
            .execute([merge.as_str()])?;
    }
    tx.prepare_cached("UPDATE merge_stacks SET state = ?2 WHERE merge_id = ?1")?
        .execute([merge.as_str(), to.as_str()])?;
    Ok(())
}

/// Claim each of `merge`'s input paths for its stack, refused when another stack claims one: an
/// original belongs to one active stack at a time.
fn claim(tx: &Transaction<'_>, item: &LibraryItem, merge: &MergeId) -> Result<(), Error> {
    let paths: Vec<String> = tx
        .prepare_cached("SELECT path FROM merge_inputs WHERE merge_id = ?1 ORDER BY ordinal")?
        .query_map([merge.as_str()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if let Some(claim) = claims(tx, paths.iter().map(PathBuf::from))?
        .into_iter()
        .find(|claim| claim.merge_id != *merge)
    {
        return Err(overlap(tx, &claim).with_data(json!({
            "items": [item],
            "count": 1,
            "stack": claim.merge_id,
            "result_asset_id": claim.result_asset_id,
        })));
    }
    let mut insert = tx.prepare_cached(
        "INSERT INTO stack_claims (path, merge_id) VALUES (?1, ?2) ON CONFLICT(path) DO NOTHING",
    )?;
    for path in &paths {
        insert.execute([path.as_str(), merge.as_str()])?;
    }
    Ok(())
}

/// The refusal of a merge over an input an active stack claims, naming the input, the stack's kind
/// and its result.
pub(crate) fn overlap(connection: &Connection, claim: &StackClaim) -> Error {
    let result = connection
        .prepare_cached("SELECT file_name FROM assets WHERE id = ?1")
        .and_then(|mut statement| {
            statement
                .query_row([claim.result_asset_id.as_str()], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| claim.result_asset_id.to_string());
    Error::conflict(format!(
        "{} is already in the {} stack of {result} ({}); remove that merge first",
        file_name(&claim.path),
        claim.kind.noun(),
        claim.merge_id
    ))
}

/// The active stacks that claim any of `paths`, one row a claimed path, in the order named. One
/// primary-key read a path.
pub(crate) fn claims(
    connection: &Connection,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<StackClaim>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT c.merge_id, m.result_asset_id, m.kind FROM stack_claims c
         JOIN merges m ON m.id = c.merge_id WHERE c.path = ?1",
    )?;
    let mut claimed = Vec::new();
    for path in paths {
        let row: Option<(String, String, String)> = statement
            .query_row([path.to_string_lossy()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        if let Some((merge, result, kind)) = row {
            let merge_id = MergeId::parse(merge)?;
            claimed.push(StackClaim {
                kind: stored_kind(&merge_id, &kind)?,
                path,
                merge_id,
                result_asset_id: AssetId::parse(result)?,
            });
        }
    }
    Ok(claimed)
}

/// A recorded merge with exactly this key ([`MergeProvenance::match_key`]): an active one when
/// there is one, otherwise the latest. An active match is reused as it is; one in Removed is
/// offered as Put back merge, never restored silently.
pub(crate) fn matching(connection: &Connection, key: &str) -> Result<Option<MergeMatch>, Error> {
    let row: Option<(String, String, String)> = connection
        .prepare_cached(
            "SELECT m.id, m.result_asset_id, s.state FROM merges m
             JOIN merge_stacks s ON s.merge_id = m.id WHERE m.match_key = ?1
             ORDER BY s.state = 'active' DESC, m.created_ms DESC, m.id DESC LIMIT 1",
        )?
        .query_row([key], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .optional()?;
    row.map(|(merge, result, state)| {
        Ok(MergeMatch {
            merge_id: MergeId::parse(merge)?,
            result_asset_id: AssetId::parse(result)?,
            state: MergeState::parse(&state)?,
        })
    })
    .transpose()
}

/// The merges `assets` are the results of, with their states. One read for them all.
pub(crate) fn merges_of(
    connection: &Connection,
    assets: &[AssetId],
) -> Result<HashMap<AssetId, (MergeId, MergeState)>, Error> {
    let ids = serde_json::to_string(assets)
        .map_err(|error| Error::internal(format!("cannot encode photographs: {error}")))?;
    let mut statement = connection.prepare_cached(
        "SELECT m.result_asset_id, m.id, s.state FROM merges m
         JOIN merge_stacks s ON s.merge_id = m.id
         WHERE m.result_asset_id IN (SELECT value FROM json_each(?1))",
    )?;
    let rows = statement.query_map([ids], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut merges = HashMap::new();
    for row in rows {
        let (asset, merge, state) = row?;
        merges.insert(
            AssetId::parse(asset)?,
            (MergeId::parse(merge)?, MergeState::parse(&state)?),
        );
    }
    Ok(merges)
}

/// What removing a merge's result (`to` is `removed`) or putting it back (`active`) changes beside
/// its `asset-removal`: its merge's `merge-stack`, deactivated or, for Put back, active again
/// (refused when another active stack now claims an input). Removing a result whose stack is not
/// active leaves the stack's state as it is, so an undone merge stays labelled undone. `merge` is
/// the result's merge and state ([`merges_of`]); a photograph that is no merge's result has none,
/// and changes nothing here.
///
/// This is where everything else that follows a merge result's visibility joins its Remove, in
/// the same change, through the one function general Remove and Remove merge share. Ordinary picks
/// and the originals are never touched.
pub(crate) fn lifecycle(
    merge: Option<&(MergeId, MergeState)>,
    to: MergeState,
) -> Result<Option<(LibraryItem, Desired)>, Error> {
    let Some((merge_id, now)) = merge else {
        return Ok(None);
    };
    if to != MergeState::Active && *now != MergeState::Active {
        return Ok(None);
    }
    Ok(Some((
        LibraryItem::MergeStack {
            merge_id: merge_id.clone(),
        },
        Desired::Value(Some(value(MergeStackValue { state: to })?)),
    )))
}

/// The results of `merges`, as `asset.remove` and `asset.restore` take their photographs: what
/// Remove merge and Put back merge name. An unknown merge is `validation`.
pub(crate) fn results(
    connection: &Connection,
    merges: &[MergeId],
) -> Result<Vec<(AssetId, AssetRowId)>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT a.id, a.row_id FROM merges m JOIN assets a ON a.id = m.result_asset_id
         WHERE m.id = ?1",
    )?;
    merges
        .iter()
        .map(|merge| {
            let row: Option<(String, i64)> = statement
                .query_row([merge.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            let (asset, row) =
                row.ok_or_else(|| Error::validation(format!("unknown merge {merge}")))?;
            Ok((AssetId::parse(asset)?, AssetRowId(row)))
        })
        .collect()
}

/// A merge as the catalog records it, with its stack's members and its provenance; `validation`
/// for an unknown merge, and `incompatible`, the row kept as it is, for provenance this build does
/// not read.
pub(crate) fn record(connection: &Connection, merge: &MergeId) -> Result<MergeRecord, Error> {
    let row: Option<(String, String, String, String, i64, String)> = connection
        .prepare_cached(
            "SELECT m.kind, m.result_asset_id, m.source_id, s.state, m.created_ms,
                 m.provenance_json
             FROM merges m JOIN merge_stacks s ON s.merge_id = m.id WHERE m.id = ?1",
        )?
        .query_row([merge.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .optional()?;
    let (kind, result, source, state, created_ms, provenance) =
        row.ok_or_else(|| Error::validation(format!("unknown merge {merge}")))?;
    let kind = stored_kind(merge, &kind)?;
    let unread = || {
        Error::incompatible(format!(
            "merge {merge} records provenance this build does not read; it is kept unchanged"
        ))
        .with_data(json!({"kind": kind}))
    };
    let provenance: MergeProvenance = decode("stored merge provenance", provenance)?;
    if provenance.format != crate::catalog_types::MERGE_PROVENANCE_FORMAT || provenance.kind != kind
    {
        return Err(unread());
    }
    Ok(MergeRecord {
        merge_id: merge.clone(),
        kind,
        result_asset_id: AssetId::parse(result)?,
        source_id: crate::DerivedSourceId::parse(source)?,
        state: MergeState::parse(&state)?,
        created_ms,
        members: members(connection, merge)?,
        provenance,
    })
}

/// The kind a `merges.kind` value names, `incompatible` for one this build does not know, the row
/// kept as it is.
pub(crate) fn stored_kind(merge: &MergeId, kind: &str) -> Result<MergeKind, Error> {
    MergeKind::parse(kind).ok_or_else(|| {
        Error::incompatible(format!(
            "merge {merge}: unknown kind {kind}; it is kept unchanged"
        ))
    })
}

/// The position in its stack's order of the reference input of `merge`, from its provenance; none
/// for provenance this build does not read. One indexed read and one decode.
pub(crate) fn reference(connection: &Connection, merge: &MergeId) -> Result<Option<u32>, Error> {
    let provenance: Option<String> = connection
        .prepare_cached("SELECT provenance_json FROM merges WHERE id = ?1")?
        .query_row([merge.as_str()], |row| row.get(0))
        .optional()?;
    Ok(provenance
        .and_then(|text| decode::<MergeProvenance>("stored merge provenance", text).ok())
        .filter(|provenance| provenance.format == crate::catalog_types::MERGE_PROVENANCE_FORMAT)
        .map(|provenance| provenance.reference() as u32))
}

/// A stack's members, its merge's exact inputs in the stack's order. One indexed range read.
pub(crate) fn members(connection: &Connection, merge: &MergeId) -> Result<Vec<StackMember>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT ordinal, path, asset_id, offset_ev FROM merge_inputs
         WHERE merge_id = ?1 ORDER BY ordinal",
    )?;
    let rows = statement.query_map([merge.as_str()], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<f64>>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (ordinal, path, asset, offset_ev) = row?;
        Ok(StackMember {
            ordinal: ordinal as u32,
            path: path.into(),
            asset_id: asset.map(AssetId::parse).transpose()?,
            offset_ev,
        })
    })
    .collect()
}

/// The merge whose result is `asset`, if it is one.
pub(crate) fn merge_of(connection: &Connection, asset: &AssetId) -> Result<Option<MergeId>, Error> {
    let merge: Option<String> = connection
        .prepare_cached("SELECT id FROM merges WHERE result_asset_id = ?1")?
        .query_row([asset.as_str()], |row| row.get(0))
        .optional()?;
    merge.map(MergeId::parse).transpose()
}

/// The file stem an export of `asset`, a merge's result, is named from: its stack's first input's
/// stem with the kind's suffix, `-hdr` or `-pano`, as `LXF_0005-pano`; `None` when `asset` is no
/// merge's result or its first input has no UTF-8 stem. A result has no original of its own.
pub(crate) fn export_stem(
    connection: &Connection,
    asset: &AssetId,
) -> Result<Option<String>, Error> {
    let kind: Option<String> = connection
        .prepare_cached("SELECT kind FROM merges WHERE result_asset_id = ?1")?
        .query_row([asset.as_str()], |row| row.get(0))
        .optional()?;
    let Some(kind) = kind.as_deref().and_then(MergeKind::parse) else {
        return Ok(None);
    };
    let first: Option<String> = connection
        .prepare_cached(
            "SELECT i.path FROM merge_inputs i JOIN merges m ON m.id = i.merge_id
             WHERE m.result_asset_id = ?1 ORDER BY i.ordinal LIMIT 1",
        )?
        .query_row([asset.as_str()], |row| row.get(0))
        .optional()?;
    let suffix = match kind {
        MergeKind::Hdr => "-hdr",
        MergeKind::Panorama => "-pano",
    };
    Ok(first.and_then(|path| {
        Path::new(&path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(|stem| format!("{stem}{suffix}"))
    }))
}

/// A value as the journal stores it.
fn value(item: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(item)
        .map_err(|error| Error::internal(format!("cannot encode a library value: {error}")))
}

/// A path's file name as a person reads it.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
#[path = "hdr_merge_browse.rs"]
mod hdr_merge_browse;
#[cfg(test)]
#[path = "hdr_merge_lifecycle.rs"]
mod hdr_merge_lifecycle;
