//! The catalog side of derived sources (`docs/design/hdr-merge.md#durable-derived-source`): the
//! rows that record each published source and its chunks, the asset-source references that retain
//! a source for the photograph holding it, and the retention closure every collection keeps.
//!
//! A source becomes visible in one owner transaction ([`link_asset_source`]) after a worker made
//! its objects durable (`crate::artifacts::derived_source`). The owner stats the objects and
//! writes rows; it never reads or hashes a payload. What retains a source is a photograph's
//! asset-source reference — whatever the photograph's visibility, Removed or hidden, and with its
//! whole immutable history — or its publication by a writer of this process; recipe and layer
//! artifact references are separate and never involved. Only emptying Removed deletes an
//! asset-source reference, and only then can a collection remove the source's objects.
// The HDR merge's publication (`tasks/library/hdr-merge.json` TASK-004 and TASK-006) composes the
// writers; until it does, only the tests and the qualification harness publish a source.
#![cfg_attr(not(test), allow(dead_code))]
use super::{AssetRecord, EditorService, SourceSignature, decode, now_ms, write};
use crate::{
    AssetId, DerivedInterpretation, Error, SourceKind,
    artifacts::derived_source::{
        DerivedSourceId, DerivedSourceRead, DerivedSourceWriter, LiveSources, PublishedSource,
        object_length,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{collections::HashSet, path::Path};

/// Check, by stat alone, that every object of `published` is a file of its own under `root` with
/// the length the writer recorded: a missing one is `source-unavailable: derived source <id> is
/// missing`, another length `... is corrupt`.
fn stat_objects(root: &Path, published: &PublishedSource) -> Result<(), Error> {
    let id = &published.id;
    let objects = std::iter::once((id.sha256(), published.manifest_bytes)).chain(
        published
            .chunks
            .iter()
            .map(|chunk| (chunk.sha256.as_str(), chunk.bytes)),
    );
    for (sha256, bytes) in objects {
        match object_length(root, sha256)? {
            None => {
                return Err(Error::source_unavailable(format!(
                    "derived source {id} is missing"
                )));
            }
            Some(length) if length != bytes => {
                return Err(Error::source_unavailable(format!(
                    "derived source {id} is corrupt"
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Record `published` and its manifest-chunk references in the caller's transaction, after checking
/// the record's bounds and, by stat, that its objects are durable under `root`. Recording the same
/// source again changes nothing; a row of the same identity with another shape is `incompatible`
/// and left as it is. Rolled back with the caller's transaction.
pub(crate) fn record_source(
    tx: &Transaction<'_>,
    root: &Path,
    published: &PublishedSource,
) -> Result<(), Error> {
    published.validate()?;
    stat_objects(root, published)?;
    let id = published.id.as_str();
    let colour = serde_json::to_value(published.colour)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| Error::internal("cannot encode a derived source colour"))?;
    let shape = (
        published.manifest_bytes as i64,
        i64::from(published.width),
        i64::from(published.height),
        colour,
        published.chunks.len() as i64,
    );
    let recorded: Option<(i64, i64, i64, String, i64)> = tx
        .prepare_cached(
            "SELECT manifest_bytes,width,height,colour,chunk_count FROM derived_sources WHERE id=?1",
        )?
        .query_row([id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })
        .optional()?;
    if let Some(recorded) = recorded {
        if recorded != shape {
            return Err(Error::incompatible(format!(
                "derived source {id} is recorded with another shape; it is kept unchanged"
            )));
        }
        return Ok(());
    }
    tx.prepare_cached(
        "INSERT INTO derived_sources (id,manifest_bytes,width,height,colour,chunk_count,created_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
    )?
    .execute(params![
        id,
        shape.0,
        shape.1,
        shape.2,
        shape.3,
        shape.4,
        now_ms()
    ])?;
    let mut chunk = tx.prepare_cached(
        "INSERT INTO derived_source_chunks (source_id,ordinal,sha256,bytes) VALUES (?1,?2,?3,?4)",
    )?;
    for (ordinal, published) in published.chunks.iter().enumerate() {
        chunk.execute(params![
            id,
            ordinal as i64,
            published.sha256,
            published.bytes as i64
        ])?;
    }
    Ok(())
}

/// Make `source`, already recorded ([`record_source`]), the source of the photograph `asset_id` in
/// the caller's transaction. The photograph must be a derived one whose interpretation names
/// exactly this source; attaching the source it already holds changes nothing, and a photograph's
/// source is never replaced (`conflict`). The reference is permanent: only emptying Removed
/// deletes it, with the photograph.
pub(crate) fn attach_source(
    tx: &Transaction<'_>,
    asset_id: &AssetId,
    source: &DerivedSourceId,
) -> Result<(), Error> {
    let stored: Option<String> = tx
        .prepare_cached("SELECT source_json FROM assets WHERE id=?1")?
        .query_row([asset_id.as_str()], |row| row.get(0))
        .optional()?;
    let stored = stored.ok_or_else(|| Error::validation("unknown asset"))?;
    let kind: SourceKind = decode("stored source interpretation", stored)?;
    match kind.derived() {
        Some(interpretation) if &interpretation.source_id == source => {}
        Some(interpretation) => {
            return Err(Error::validation(format!(
                "asset {asset_id} is derived from {}, not {source}",
                interpretation.source_id
            )));
        }
        None => {
            return Err(Error::validation(format!(
                "asset {asset_id} is not a derived photograph"
            )));
        }
    }
    let held: Option<String> = tx
        .prepare_cached("SELECT source_id FROM asset_sources WHERE asset_id=?1")?
        .query_row([asset_id.as_str()], |row| row.get(0))
        .optional()?;
    match held {
        Some(held) if held == source.as_str() => Ok(()),
        Some(held) => Err(Error::conflict(format!(
            "asset {asset_id} already holds derived source {held}; a source is never replaced"
        ))),
        None => {
            let recorded: bool = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM derived_sources WHERE id=?1)")?
                .query_row([source.as_str()], |row| row.get(0))?;
            if !recorded {
                return Err(Error::validation(format!(
                    "derived source {source} is not recorded in this catalog"
                )));
            }
            tx.prepare_cached("INSERT INTO asset_sources (asset_id,source_id) VALUES (?1,?2)")?
                .execute(params![asset_id.as_str(), source.as_str()])?;
            Ok(())
        }
    }
}

/// [`record_source`] then [`attach_source`]: the source references of one publication, written in
/// the caller's transaction beside the photograph's own rows — its asset row, Original and
/// whatever else the publication makes visible — so a failure anywhere leaves no visible source.
/// The asset row must already be written in this transaction. Stat and SQL only.
pub(crate) fn link_asset_source(
    tx: &Transaction<'_>,
    root: &Path,
    asset_id: &AssetId,
    published: &PublishedSource,
) -> Result<(), Error> {
    record_source(tx, root, published)?;
    attach_source(tx, asset_id, &published.id)
}

/// The record of the source `id` as the catalog holds it, to give another photograph the same
/// immutable source (a rerun that matches its merge exactly); none when it is not recorded. SQL
/// only.
pub(crate) fn recorded_source(
    connection: &Connection,
    id: &DerivedSourceId,
) -> Result<Option<PublishedSource>, Error> {
    let row: Option<(i64, i64, i64, String)> = connection
        .prepare_cached(
            "SELECT manifest_bytes,width,height,colour FROM derived_sources WHERE id=?1",
        )?
        .query_row([id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    let Some((manifest_bytes, width, height, colour)) = row else {
        return Ok(None);
    };
    let colour =
        serde_json::from_value(serde_json::Value::String(colour.clone())).map_err(|_| {
            Error::incompatible(format!("derived source colour {colour} is not supported"))
        })?;
    let mut statement = connection.prepare_cached(
        "SELECT sha256, bytes FROM derived_source_chunks WHERE source_id=?1 ORDER BY ordinal",
    )?;
    let chunks = statement
        .query_map([id.as_str()], |row| {
            Ok(crate::artifacts::derived_source::PublishedChunk {
                sha256: row.get(0)?,
                bytes: row.get::<_, i64>(1)? as u64,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(PublishedSource {
        id: id.clone(),
        manifest_bytes: manifest_bytes as u64,
        width: width as u32,
        height: height as u32,
        colour,
        chunks,
    }))
}

/// The SHA-256 of every source object the catalog retains: each recorded source's manifest and
/// every chunk it names. What a collection keeps.
pub(super) fn retained_source_objects(connection: &Connection) -> Result<HashSet<String>, Error> {
    let mut keep = HashSet::new();
    let mut sources = connection.prepare_cached("SELECT id FROM derived_sources")?;
    for id in sources.query_map([], |row| row.get::<_, String>(0))? {
        keep.insert(DerivedSourceId::parse(id?)?.sha256().to_owned());
    }
    let mut chunks =
        connection.prepare_cached("SELECT DISTINCT sha256 FROM derived_source_chunks")?;
    for sha256 in chunks.query_map([], |row| row.get::<_, String>(0))? {
        keep.insert(sha256?);
    }
    Ok(keep)
}

/// Delete, in the caller's transaction, each of `candidates` that no photograph holds and no writer
/// of this process published, with its chunk rows, and answer which went.
pub(crate) fn release_sources(
    tx: &Transaction<'_>,
    live: &LiveSources,
    candidates: impl IntoIterator<Item = DerivedSourceId>,
) -> Result<Vec<DerivedSourceId>, Error> {
    let mut held =
        tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM asset_sources WHERE source_id=?1)")?;
    let mut chunks = tx.prepare_cached("DELETE FROM derived_source_chunks WHERE source_id=?1")?;
    let mut source = tx.prepare_cached("DELETE FROM derived_sources WHERE id=?1")?;
    let mut released = Vec::new();
    for candidate in candidates {
        if live.holds(&candidate) || held.query_row([candidate.as_str()], |row| row.get(0))? {
            continue;
        }
        chunks.execute([candidate.as_str()])?;
        if source.execute([candidate.as_str()])? > 0 {
            released.push(candidate);
        }
    }
    Ok(released)
}

/// Every recorded source, in identity order.
fn recorded_sources(connection: &Connection) -> Result<Vec<DerivedSourceId>, Error> {
    let mut statement = connection.prepare_cached("SELECT id FROM derived_sources ORDER BY id")?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|id| DerivedSourceId::parse(id?))
        .collect()
}

/// The sources the photographs `ids` (a JSON array) hold, each once: what emptying Removed may
/// release once their references are gone.
pub(crate) fn held_by(connection: &Connection, ids: &str) -> Result<Vec<DerivedSourceId>, Error> {
    connection
        .prepare_cached(
            "SELECT DISTINCT source_id FROM asset_sources
             WHERE asset_id IN (SELECT value FROM json_each(?1)) ORDER BY source_id",
        )?
        .query_map([ids], |row| row.get::<_, String>(0))?
        .map(|id| DerivedSourceId::parse(id?))
        .collect()
}

/// A collection's part for derived sources, in the caller's transaction: release every recorded
/// source no photograph holds and no writer of this process published, and answer how many went
/// and every object the catalog still retains.
pub(super) fn plan_source_collection(
    tx: &Transaction<'_>,
    live: &LiveSources,
) -> Result<(usize, HashSet<String>), Error> {
    let released = release_sources(tx, live, recorded_sources(tx)?)?;
    Ok((released.len(), retained_source_objects(tx)?))
}

impl SourceSignature {
    /// A derived source's signature: its identity, which names its content, and its payload's
    /// length. Its objects are immutable and content-addressed, so the identity says of them what a
    /// file's identity, length and times say of a file: a prepared source with this signature is
    /// the stored one.
    fn derived(interpretation: &DerivedInterpretation, byte_len: u64) -> Self {
        Self {
            byte_len,
            modified: None,
            file_identity: format!("derived:{}", interpretation.source_id),
            change_marker: None,
        }
    }
}

impl EditorService {
    /// The signature of the derived photograph `asset`'s source while its manifest is still
    /// stored, as an original's is while its file is there: the artifact root checked as this
    /// catalog's (one stat of its manifest once checked) and one stat of the source's manifest
    /// object. A missing manifest is `source-unavailable: derived source <id> is missing`, another
    /// length `... is corrupt`; a chunk is verified when the source is read. Stat and SQL only.
    pub(super) fn derived_signature(
        &self,
        asset: &AssetRecord,
        interpretation: &DerivedInterpretation,
    ) -> Result<SourceSignature, Error> {
        let id = &interpretation.source_id;
        self.check_root()?;
        let recorded: Option<i64> = self
            .connection
            .prepare_cached(
                "SELECT s.manifest_bytes FROM asset_sources a
                 JOIN derived_sources s ON s.id = a.source_id
                 WHERE a.asset_id=?1 AND s.id=?2",
            )?
            .query_row(params![asset.id.as_str(), id.as_str()], |row| row.get(0))
            .optional()?;
        let recorded = recorded.ok_or_else(|| {
            Error::source_unavailable(format!(
                "derived source {id} is not recorded for photograph {}",
                asset.id
            ))
        })?;
        match object_length(&self.artifact_root, id.sha256())? {
            None => Err(Error::source_unavailable(format!(
                "derived source {id} is missing"
            ))),
            Some(length) if length != recorded as u64 => Err(Error::source_unavailable(format!(
                "derived source {id} is corrupt"
            ))),
            Some(_) => Ok(SourceSignature::derived(interpretation, asset.byte_len)),
        }
    }

    /// A writer that publishes derived sources into this catalog's artifact root from any thread.
    /// No I/O: the root is created by its first publish.
    pub(crate) fn derived_source_writer(&self) -> DerivedSourceWriter {
        DerivedSourceWriter::new(
            self.artifact_root.clone(),
            self.catalog_id.clone(),
            self.live_sources.clone(),
        )
    }

    /// [`link_asset_source`] in a transaction of its own, for a caller that has nothing else to
    /// write with it.
    pub(crate) fn link_derived_source(
        &mut self,
        asset_id: &AssetId,
        published: &PublishedSource,
    ) -> Result<(), Error> {
        let root = self.artifact_root.clone();
        write(&mut self.connection, |tx| {
            link_asset_source(tx, &root, asset_id, published)
        })
    }

    /// A derived photograph of `published`, brought into the catalog as a merge's publication
    /// brings one: its asset row of a derived source, capture row and Original — the stack
    /// [`super::new_photograph`] builds for its kind — and state, then its source references, all in
    /// one transaction, with `extra` last in it. It joins the catalog folder and volume of the
    /// catalog's first photograph, which must exist. Named `name`. For the tests and the
    /// qualification harness, which make derived photographs before the merge publishes any.
    #[cfg(any(test, feature = "qualification"))]
    pub(crate) fn publish_derived_with(
        &mut self,
        published: &PublishedSource,
        name: &str,
        extra: impl FnOnce(&Transaction<'_>) -> Result<(), Error>,
    ) -> Result<AssetId, Error> {
        use crate::catalog_types::{CatalogFolderId, FileAvailability, HeaderMetadata, VolumeId};
        let (folder, volume): (String, String) = self
            .connection
            .query_row(
                "SELECT catalog_folder_id, volume_id FROM assets ORDER BY row_id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                Error::validation("a derived photograph joins an existing photograph's folder")
            })?;
        let (folder, volume) = (CatalogFolderId::parse(folder)?, VolumeId::parse(volume)?);
        let asset_id = AssetId::new();
        // A derived photograph's locator, identity and fingerprint name its source, never a file.
        let identity = format!("derived:{asset_id}");
        let record = AssetRecord {
            id: asset_id.clone(),
            source_root: std::path::PathBuf::new(),
            locator: std::path::PathBuf::from(&identity),
            fingerprint: published.id.to_string(),
            file_identity: identity.clone(),
            byte_len: published.payload_bytes(),
            width: published.width,
            height: published.height,
            source: SourceKind::Derived {
                interpretation: published.interpretation(),
            },
        };
        let header = HeaderMetadata::default();
        let now = now_ms();
        let photograph = self.new_photograph(record, &header, now)?;
        let root = self.artifact_root.clone();
        write(&mut self.connection, |tx| {
            super::insert_photograph(
                tx,
                &root,
                &photograph.original,
                &super::catalog_rows::NewAsset {
                    record: &photograph.asset,
                    canonical_locator: &identity,
                    catalog_folder_id: &folder,
                    source_folder: std::path::Path::new("derived"),
                    volume_id: &volume,
                    file_name: name,
                    developed_ms: now,
                    removed_ms: None,
                    availability: FileAvailability::Available,
                    checked_ms: now,
                    develop_moment: None,
                },
                &header,
                None,
            )?;
            link_asset_source(tx, &root, &asset_id, published)?;
            extra(tx)
        })?;
        Ok(asset_id)
    }

    /// A derived photograph of `planes` (`width × height` red values row by row, then green, then
    /// blue), written as a merge's worker writes its result and published by
    /// [`Self::publish_derived_with`]. For the tests and the qualification harness.
    #[cfg(any(test, feature = "qualification"))]
    pub(crate) fn publish_planes(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        planes: &[f32],
    ) -> Result<AssetId, Error> {
        let published = self.derived_source_writer().write_planes(
            width,
            height,
            planes,
            None,
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        self.publish_derived_with(&published, name, |_| Ok(()))
    }

    /// How a worker reads and verifies the source of the derived photograph `asset_id`: its
    /// recorded identity and manifest length, after checking the root is this catalog's and, by
    /// stat, that every recorded object is present with its recorded length. Stat and SQL only;
    /// a missing object is `source-unavailable` naming the source, and a photograph that is not
    /// derived is `validation`.
    pub(crate) fn derived_source_read(
        &self,
        asset_id: &AssetId,
    ) -> Result<DerivedSourceRead, Error> {
        let row: Option<(String, i64)> = self
            .connection
            .prepare_cached(
                "SELECT s.id, s.manifest_bytes FROM asset_sources a
                 JOIN derived_sources s ON s.id = a.source_id WHERE a.asset_id=?1",
            )?
            .query_row([asset_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let (id, manifest_bytes) = row.ok_or_else(|| {
            Error::validation(format!("asset {asset_id} holds no derived source"))
        })?;
        let id = DerivedSourceId::parse(id)?;
        self.check_root()?;
        let mut objects = vec![(id.sha256().to_owned(), manifest_bytes)];
        let mut statement = self.connection.prepare_cached(
            "SELECT sha256, bytes FROM derived_source_chunks WHERE source_id=?1 ORDER BY ordinal",
        )?;
        for chunk in statement.query_map([id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))? {
            objects.push(chunk?);
        }
        for (sha256, bytes) in objects {
            match object_length(&self.artifact_root, &sha256)? {
                None => {
                    return Err(Error::source_unavailable(format!(
                        "derived source {id} is missing"
                    )));
                }
                Some(length) if length != bytes as u64 => {
                    return Err(Error::source_unavailable(format!(
                        "derived source {id} is corrupt"
                    )));
                }
                Some(_) => {}
            }
        }
        Ok(DerivedSourceRead {
            root: self.artifact_root.clone(),
            catalog_id: self.catalog_id.clone(),
            id,
            manifest_bytes: manifest_bytes as u64,
        })
    }
}
