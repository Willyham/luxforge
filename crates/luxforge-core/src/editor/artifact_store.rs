//! The catalog side of derived artifacts: their rows, the references each entry holds, the root
//! they live in, the verified bytes kept ready and the binding step every evaluation or admission
//! of a stack passes before anything compiles it, which fills the recipe's own artifact table.
//! Reading and hashing artifact bytes belongs to [`crate::artifacts`] on a worker; the owner only
//! stats files and reads the small manifest, so binding a stack costs `O(references)` lookups and
//! stats (performance rule 5).
use super::{EditorService, SourceSignature, source_signature, write};
use crate::{
    Error, HistoryEntry, Recipe,
    artifacts::{
        self, ArtifactId, ArtifactMeta, ArtifactRead, ArtifactRecord, ArtifactTable,
        ArtifactWriter, Collection, LiveArtifacts, MANIFEST, PREPARED_ARTIFACT_BYTES,
        PREPARED_ARTIFACT_ENTRIES, PreparedArtifact, RootState, VerifiedArtifact,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::HashSet,
    io,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

/// Every artifact a stack references, each once, in the order its layers first list them.
pub(super) fn referenced(recipe: &Recipe) -> Vec<ArtifactId> {
    let mut seen = HashSet::new();
    recipe
        .layers
        .iter()
        .flat_map(|layer| &layer.artifacts)
        .filter(|id| seen.insert(*id))
        .cloned()
        .collect()
}

/// The object file of one recorded artifact, by stat alone: a missing file is
/// `source-unavailable: artifact <id> is missing`, another length is
/// `source-unavailable: artifact <id> is corrupt`, and the signature of a present one keys the
/// verified bytes kept ready for it.
fn object_signature(root: &Path, id: &ArtifactId, bytes: u64) -> Result<SourceSignature, Error> {
    let path = artifacts::object_path(root, id);
    let metadata = match path.metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(Error::file_access(format!(
                "cannot read artifact {id}: {}",
                error.kind()
            )));
        }
        Ok(metadata) => metadata.is_file().then_some(metadata),
    };
    let Some(metadata) = metadata else {
        return Err(Error::source_unavailable(format!(
            "artifact {id} is missing"
        )));
    };
    if metadata.len() != bytes {
        return Err(Error::source_unavailable(format!(
            "artifact {id} is corrupt"
        )));
    }
    Ok(source_signature(&path, &metadata))
}

/// The recorded length and metadata of one artifact row, if the catalog holds it.
fn artifact_row(
    connection: &Connection,
    id: &ArtifactId,
) -> Result<Option<(u64, ArtifactMeta)>, Error> {
    Ok(connection
        .query_row(
            "SELECT bytes,kind,width,height,colour FROM artifacts WHERE id=?1",
            [id.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    ArtifactMeta {
                        kind: row.get(1)?,
                        width: row.get::<_, Option<i64>>(2)?.map(|width| width as u32),
                        height: row.get::<_, Option<i64>>(3)?.map(|height| height as u32),
                        colour: row.get(4)?,
                    },
                ))
            },
        )
        .optional()?)
}

/// Check that every artifact a stack references is recorded in the catalog and present in the root
/// with its recorded length, and return them. Stat only. An unrecorded artifact is
/// `validation: unknown artifact <id>`, because nothing may reference an artifact the catalog never
/// recorded; a recorded one whose file is gone is `source-unavailable`.
pub(super) fn recorded_artifacts(
    connection: &Connection,
    root: &Path,
    recipe: &Recipe,
) -> Result<Vec<ArtifactId>, Error> {
    let ids = referenced(recipe);
    for id in &ids {
        let (bytes, _) = artifact_row(connection, id)?
            .ok_or_else(|| Error::validation(format!("unknown artifact {id}")))?;
        object_signature(root, id, bytes)?;
    }
    Ok(ids)
}

/// Record one entry's artifact references in the transaction that writes the entry, after checking
/// them again inside it: a snapshot never points at an artifact the catalog does not hold, and a
/// failed write rolls the references back with everything else.
pub(super) fn link_artifacts(
    tx: &Transaction<'_>,
    root: &Path,
    entry: &HistoryEntry,
) -> Result<(), Error> {
    for id in recorded_artifacts(tx, root, &entry.snapshot.recipe)? {
        tx.execute(
            "INSERT INTO artifact_refs (entry_id,artifact_id) VALUES (?1,?2)",
            params![entry.id.as_str(), id.as_str()],
        )?;
    }
    Ok(())
}

/// Rows no entry references and that are not live: what a collection removes.
fn collection_candidates(
    connection: &Connection,
    live: &LiveArtifacts,
) -> Result<Vec<ArtifactId>, Error> {
    let mut statement = connection.prepare(
        "SELECT id FROM artifacts a WHERE NOT EXISTS
             (SELECT 1 FROM artifact_refs r WHERE r.artifact_id=a.id) ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut candidates = Vec::new();
    for row in rows {
        let id = ArtifactId::parse(row?)?;
        if !live.contains(&id) {
            candidates.push(id);
        }
    }
    Ok(candidates)
}

/// What binding one referenced artifact found.
enum Binding {
    /// Its verified bytes are kept ready under the file's current signature.
    Ready(Arc<PreparedArtifact>),
    /// Its file is present with the recorded length, and a worker must read and verify it.
    Unprepared(ArtifactRead),
}

impl EditorService {
    /// A writer that publishes into this catalog's artifact root from any thread. No I/O: the root
    /// is created by its first publish.
    pub fn artifact_writer(&self) -> Result<ArtifactWriter, Error> {
        Ok(ArtifactWriter::new(
            self.artifact_root.clone(),
            self.catalog_id.clone(),
            self.live_artifacts.clone(),
        ))
    }

    /// Record an artifact a writer published and keep its verified bytes ready. Idempotent: the
    /// row is inserted once, whoever publishes the same bytes again. `live` says it was published
    /// while this service is open, so no collection removes it before the catalog is reopened; this
    /// service's writers already mark their own publishes live. The object file must be present
    /// with the recorded length; nothing is read or hashed here.
    pub fn register_artifact(
        &mut self,
        record: ArtifactRecord,
        prepared: Arc<PreparedArtifact>,
        live: bool,
    ) -> Result<(), Error> {
        record.validate()?;
        if prepared.id != record.id || prepared.bytes.len() as u64 != record.bytes {
            return Err(Error::validation(format!(
                "the prepared bytes are not artifact {}",
                record.id
            )));
        }
        let signature = object_signature(&self.artifact_root, &record.id, record.bytes)?;
        write(&mut self.connection, |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO artifacts
                 (id,sha256,bytes,kind,width,height,colour,module_id,created_ms)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    record.id.as_str(),
                    record.sha256,
                    record.bytes as i64,
                    record.meta.kind,
                    record.meta.width.map(i64::from),
                    record.meta.height.map(i64::from),
                    record.meta.colour,
                    record.module_id,
                    record.created_ms,
                ],
            )?;
            Ok(())
        })?;
        if live {
            self.live_artifacts.mark(&record.id);
        }
        self.adopt(signature, prepared);
        Ok(())
    }

    /// Bind every artifact a stack references into the recipe's own [`Recipe::artifacts`] table,
    /// which is what compilation reads them from. Every path that evaluates or admits a stack
    /// binds it first, where the recipe enters that path: a preview, analysis or sample job's
    /// recipe carries the bytes to its worker, so an eviction from the owner's cache never breaks
    /// it, and nothing else has to hold them.
    ///
    /// Each artifact needs a catalog row (else `source-unavailable: artifact <id> is not in this
    /// catalog`), a usable root (a missing directory is `source-unavailable` naming it; a manifest
    /// naming another catalog is `incompatible`) and a present object file of the recorded length.
    /// Everything one stack binds must fit the prepared cache at once, or it is a `resource-limit`.
    /// Bytes kept ready under the file's current signature are a hit. On a miss a direct service
    /// reads and hashes synchronously; the catalog owner instead answers `preparation-required`,
    /// which the evaluation that bound the stack names with everything that stack needs
    /// ([`EditorService::needing`]), so a source job reads the missing identities on the worker.
    /// The table is replaced by exactly what the stack lists, and only when all of it is bound; a
    /// stack without artifacts costs one walk of its layers and allocates nothing.
    pub fn bind_artifacts(&self, recipe: &mut Recipe) -> Result<(), Error> {
        let ids = referenced(recipe);
        if ids.is_empty() {
            recipe.artifacts = ArtifactTable::default();
            return Ok(());
        }
        let mut bound = Vec::with_capacity(ids.len());
        let mut unprepared = Vec::new();
        for binding in self.bind(&ids)? {
            match binding {
                Binding::Ready(artifact) => bound.push(artifact),
                Binding::Unprepared(read) => unprepared.push(read),
            }
        }
        if !unprepared.is_empty() && !self.allow_sync_source {
            return Err(Error::preparation_required("artifact preparation required"));
        }
        let never = AtomicBool::new(false);
        for read in &unprepared {
            let verified = artifacts::read_verified(read, &never)?;
            bound.extend(self.adopt_artifacts(vec![verified]));
        }
        recipe.artifacts = bound.into_iter().collect();
        Ok(())
    }

    /// [`Self::bind_artifacts`] for a recipe the caller only borrows: the recipe itself when its
    /// table already holds everything it lists — which every recipe without a module-published
    /// layer does, and so does one this call has bound — and otherwise a bound copy. Fails exactly
    /// as binding does. `O(references)` lookups before any copy.
    pub(super) fn bound<'r>(&self, recipe: &'r Recipe) -> Result<Cow<'r, Recipe>, Error> {
        let held = recipe
            .layers
            .iter()
            .flat_map(|layer| &layer.artifacts)
            .all(|id| recipe.artifacts.get(id).is_some());
        if held {
            return Ok(Cow::Borrowed(recipe));
        }
        let mut bound = recipe.clone();
        self.bind_artifacts(&mut bound)?;
        Ok(Cow::Owned(bound))
    }

    /// The artifacts a stack references that are not kept ready: what a source job must read and
    /// verify before the stack can be bound. Fails like [`Self::bind_artifacts`] when one cannot
    /// be prepared at all.
    pub(super) fn unprepared_artifacts(&self, recipe: &Recipe) -> Result<Vec<ArtifactId>, Error> {
        Ok(self
            .artifact_reads(&referenced(recipe))?
            .into_iter()
            .map(|read| read.id)
            .collect())
    }

    /// How a source job reads and verifies each of these artifacts that is not kept ready; one
    /// that became ready since it was named is left out. Fails like [`Self::bind_artifacts`] when
    /// one cannot be prepared at all.
    pub(crate) fn artifact_reads(&self, ids: &[ArtifactId]) -> Result<Vec<ArtifactRead>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .bind(ids)?
            .into_iter()
            .filter_map(|binding| match binding {
                Binding::Ready(_) => None,
                Binding::Unprepared(read) => Some(read),
            })
            .collect())
    }

    /// Keep bytes a worker verified ready under the signature they were read with, and return what
    /// was kept.
    pub(crate) fn adopt_artifacts(
        &self,
        verified: Vec<VerifiedArtifact>,
    ) -> Vec<Arc<PreparedArtifact>> {
        verified
            .into_iter()
            .map(
                |VerifiedArtifact {
                     artifact,
                     signature,
                 }| self.adopt(signature, artifact),
            )
            .collect()
    }

    /// Keep one artifact's verified bytes ready. When the cache already holds bytes of the same
    /// identity verified against the same file, those are kept and returned instead, so recipes
    /// bound before and after share one allocation rather than two equal ones.
    fn adopt(
        &self,
        signature: SourceSignature,
        artifact: Arc<PreparedArtifact>,
    ) -> Arc<PreparedArtifact> {
        let mut cache = self.prepared_artifacts.borrow_mut();
        if let Some(kept) = cache.get(&artifact.id, &signature) {
            return kept;
        }
        cache.insert(signature, artifact.clone());
        artifact
    }

    /// Forget every verified artifact kept ready. Bound recipes keep what they hold. Production
    /// code has no caller; kept for tests that simulate an eviction.
    #[cfg(test)]
    pub(crate) fn clear_prepared_artifacts(&self) {
        self.prepared_artifacts.borrow_mut().clear();
        self.checked_manifest.replace(None);
    }

    /// Rows, then the root, then each file and the cache. Stat and lookup only. Everything one
    /// evaluation binds must fit the prepared cache at once, or it could never all be ready; more
    /// is a `resource-limit` rather than a preparation that never ends.
    fn bind(&self, ids: &[ArtifactId]) -> Result<Vec<Binding>, Error> {
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            let row = artifact_row(&self.connection, id)?.ok_or_else(|| {
                Error::source_unavailable(format!("artifact {id} is not in this catalog"))
            })?;
            rows.push((id, row));
        }
        let total: u64 = rows.iter().map(|(_, (bytes, _))| bytes).sum();
        if total > PREPARED_ARTIFACT_BYTES || rows.len() > PREPARED_ARTIFACT_ENTRIES {
            return Err(Error::resource_limit(format!(
                "the stack binds {} artifacts of {total} bytes, more than the {PREPARED_ARTIFACT_ENTRIES} artifacts or {PREPARED_ARTIFACT_BYTES} bytes that can be held ready",
                rows.len()
            )));
        }
        self.check_root()?;
        let mut cache = self.prepared_artifacts.borrow_mut();
        rows.into_iter()
            .map(|(id, (bytes, meta))| {
                let signature = object_signature(&self.artifact_root, id, bytes)?;
                Ok(match cache.get(id, &signature) {
                    Some(artifact) => Binding::Ready(artifact),
                    None => Binding::Unprepared(ArtifactRead {
                        root: self.artifact_root.clone(),
                        id: id.clone(),
                        bytes,
                        meta,
                    }),
                })
            })
            .collect()
    }

    /// The root must exist and its manifest must name this catalog. An unchanged manifest costs one
    /// stat; a changed one is read again (at most 4 KiB).
    fn check_root(&self) -> Result<(), Error> {
        let manifest = self.artifact_root.join(MANIFEST);
        let signature = manifest
            .metadata()
            .ok()
            .map(|metadata| source_signature(&manifest, &metadata));
        if signature.is_some() && *self.checked_manifest.borrow() == signature {
            return Ok(());
        }
        let root = self.artifact_root.display();
        match artifacts::root_state(&self.artifact_root, &self.catalog_id)? {
            RootState::Ready => {
                self.checked_manifest.replace(signature);
                Ok(())
            }
            RootState::Absent => Err(Error::source_unavailable(format!(
                "artifact directory {root} is missing; move it with the catalog"
            ))),
            RootState::Unmarked => Err(Error::source_unavailable(format!(
                "artifact directory {root} has no manifest"
            ))),
            RootState::Foreign(detail) => Err(Error::incompatible(detail)),
        }
    }

    /// `artifact.status`: the root, what its manifest says, and how many artifacts are referenced,
    /// collectable and recorded in bytes. Reads the manifest and counts rows; touches no object.
    pub(crate) fn artifact_status(&self) -> Result<Value, Error> {
        let (rows, bytes): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM artifacts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let referenced: i64 = self.connection.query_row(
            "SELECT COUNT(DISTINCT artifact_id) FROM artifact_refs",
            [],
            |row| row.get(0),
        )?;
        let state = match artifacts::root_state(&self.artifact_root, &self.catalog_id)? {
            RootState::Absent if rows == 0 => "absent",
            RootState::Absent | RootState::Unmarked => "missing",
            RootState::Foreign(_) => "foreign",
            RootState::Ready => "ready",
        };
        Ok(json!({
            "root": self.artifact_root,
            "state": state,
            "catalog_id": self.catalog_id,
            "referenced": referenced,
            "candidates": collection_candidates(&self.connection, &self.live_artifacts)?.len(),
            "bytes": bytes,
        }))
    }

    /// `artifact.inspect`: one artifact's record, whether its file is present with the recorded
    /// length, how many entries reference it and whether a task of this process published it.
    pub(crate) fn inspect_artifact(&self, id: &ArtifactId) -> Result<Value, Error> {
        let record = self
            .connection
            .query_row(
                "SELECT sha256,bytes,kind,width,height,colour,module_id,created_ms
                 FROM artifacts WHERE id=?1",
                [id.as_str()],
                |row| {
                    Ok(ArtifactRecord {
                        id: id.clone(),
                        sha256: row.get(0)?,
                        bytes: row.get::<_, i64>(1)? as u64,
                        meta: ArtifactMeta {
                            kind: row.get(2)?,
                            width: row.get::<_, Option<i64>>(3)?.map(|width| width as u32),
                            height: row.get::<_, Option<i64>>(4)?.map(|height| height as u32),
                            colour: row.get(5)?,
                        },
                        module_id: row.get(6)?,
                        created_ms: row.get(7)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::validation(format!("unknown artifact {id}")))?;
        let references: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM artifact_refs WHERE artifact_id=?1",
            [id.as_str()],
            |row| row.get(0),
        )?;
        let file = match artifacts::object_path(&self.artifact_root, id).metadata() {
            Ok(metadata) if metadata.is_file() && metadata.len() == record.bytes => "present",
            Ok(metadata) if metadata.is_file() => "wrong-length",
            _ => "missing",
        };
        let mut value =
            serde_json::to_value(&record).map_err(|error| Error::internal(error.to_string()))?;
        value["file"] = json!(file);
        value["references"] = json!(references);
        value["live"] = json!(self.live_artifacts.contains(id));
        Ok(value)
    }

    /// Plan `artifact.collect`: remove, in one transaction, the rows no entry references and no
    /// task of this process published, forget their verified bytes, and hand the source job the
    /// artifacts the catalog still records, so it removes every other object file. A referenced
    /// row is never removed; the foreign keys refuse it even if this query were wrong.
    pub(crate) fn plan_collection(&mut self) -> Result<Collection, Error> {
        let live = &self.live_artifacts;
        let (removed, keep) = write(&mut self.connection, |tx| {
            let removed = collection_candidates(tx, live)?;
            for id in &removed {
                tx.execute("DELETE FROM artifacts WHERE id=?1", [id.as_str()])?;
            }
            let mut statement = tx.prepare("SELECT id FROM artifacts")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut keep = HashSet::new();
            for row in rows {
                keep.insert(ArtifactId::parse(row?)?);
            }
            Ok((removed, keep))
        })?;
        let mut cache = self.prepared_artifacts.borrow_mut();
        for id in &removed {
            cache.remove(id);
        }
        Ok(Collection {
            root: self.artifact_root.clone(),
            catalog_id: self.catalog_id.clone(),
            keep,
            live: self.live_artifacts.clone(),
            rows: removed.len(),
        })
    }

    /// This catalog's own identity, which its artifact root's manifest must name. Production code
    /// has no caller; kept for a test that checks the manifest.
    #[cfg(test)]
    pub(crate) fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
}
