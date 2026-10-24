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
        self, ArtifactId, ArtifactMeta, ArtifactRead, ArtifactRecord, ArtifactWriter, Collection,
        LiveArtifacts, MANIFEST, PREPARED_ARTIFACT_BYTES, PREPARED_ARTIFACT_ENTRIES,
        PreparedArtifact, RootState, VerifiedArtifact,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{borrow::Cow, collections::HashSet, io, path::Path, sync::Arc};

/// Every artifact a stack references, each once: the ones its layers list, in the order they first
/// list them, then the ones its masks' components bind (a model selection's raster), in mask and
/// component order. A mask's artifact is referenced whether or not a layer draws the mask yet, so
/// admission checks it, the entry's references record it and retention keeps it exactly as a
/// layer's.
pub(super) fn referenced(recipe: &Recipe) -> Vec<ArtifactId> {
    let mut seen = HashSet::new();
    recipe
        .layers
        .iter()
        .flat_map(|layer| layer.artifacts.iter().cloned())
        .chain(crate::mask::mask_artifacts(&recipe.masks))
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// Every mask component that binds an artifact, checked against the artifact's recorded row by
/// its kind: a model selection references a `mask-alpha` raster of its declared size, refused
/// naming the artifact and the component. Lookups only; reads no artifact.
fn check_mask_artifacts(connection: &Connection, recipe: &Recipe) -> Result<(), Error> {
    for component in recipe.masks.iter().flat_map(|mask| &mask.components) {
        let Some(id) = crate::mask::component_artifact(component) else {
            continue;
        };
        let (bytes, meta) = artifact_row(connection, &id)?
            .ok_or_else(|| Error::validation(format!("unknown artifact {id}")))?;
        crate::mask::check_component_artifact(
            component,
            &id,
            &meta.kind,
            meta.width,
            meta.height,
            bytes,
        )?;
    }
    Ok(())
}

/// Whether a stack binding `entries` artifacts of `total` bytes fits the prepared cache at once:
/// otherwise a `resource-limit` with `data: {limit: "artifacts", artifact_bytes, budget}`.
fn within_budget(total: u64, entries: usize) -> Result<(), Error> {
    if total > PREPARED_ARTIFACT_BYTES || entries > PREPARED_ARTIFACT_ENTRIES {
        return Err(Error::resource_limit(format!(
            "the stack binds {entries} artifacts of {total} bytes, more than the {PREPARED_ARTIFACT_ENTRIES} artifacts or {PREPARED_ARTIFACT_BYTES} bytes that can be held ready"
        ))
        .with_data(json!({
            "limit": "artifacts",
            "artifact_bytes": total,
            "budget": PREPARED_ARTIFACT_BYTES,
        })));
    }
    Ok(())
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
    check_mask_artifacts(connection, recipe)?;
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
#[derive(Debug)]
pub(crate) enum Binding {
    /// Its verified bytes are kept ready under the file's current signature.
    Ready(Arc<PreparedArtifact>),
    /// Its file is present with the recorded length, and a worker must read and verify it.
    Unprepared(ArtifactRead),
}

impl EditorService {
    /// A writer that publishes into this catalog's artifact root from any thread. No I/O: the root
    /// is created by its first publish.
    pub(crate) fn artifact_writer(&self) -> Result<ArtifactWriter, Error> {
        Ok(ArtifactWriter::new(
            self.artifact_root.clone(),
            self.catalog_id.clone(),
            self.live_artifacts.clone(),
        ))
    }

    /// The catalog's candidate store, shared with the tasks that publish into it and the workers
    /// that read and accept from it.
    pub(crate) fn candidates(&self) -> &Arc<crate::artifacts::candidates::Candidates> {
        &self.candidates
    }

    /// What a task's candidates of `asset_id` are generated against: the asset, its revision now,
    /// its content dimensions and the input identity a new top correction has now
    /// ([`Self::correction_input`]). A stack that cannot say its input still queues the task, whose
    /// candidates then cannot be accepted. Reads the current entry and compiles the prefix before
    /// the repair layer; no pixel.
    pub(crate) fn candidate_basis(
        &self,
        asset_id: &crate::AssetId,
    ) -> Result<crate::artifacts::candidates::CandidateBasis, Error> {
        let head = self.head(asset_id)?;
        let input = self.correction_input(asset_id).ok();
        let [width, height] = input
            .as_ref()
            .map_or([head.asset.width, head.asset.height], |input| {
                input.dimensions
            });
        Ok(crate::artifacts::candidates::CandidateBasis {
            asset_id: head.asset.id,
            revision: head.revision,
            width,
            height,
            input,
        })
    }

    /// Record an artifact a writer published and keep its verified bytes ready. Idempotent: the
    /// row is inserted once, whoever publishes the same bytes again. `live` says it was published
    /// while this service is open, so no collection removes it before the catalog is reopened; this
    /// service's writers already mark their own publishes live. The object file must be present
    /// with the recorded length; nothing is read or hashed here.
    pub(crate) fn register_artifact(
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
    /// Bytes kept ready under the file's current signature are a hit. Nothing is read here: a miss
    /// is `preparation-required`, which the evaluation that bound the stack names with everything
    /// that stack needs ([`EditorService::needing`]), so a source job reads the missing identities
    /// ([`super::SourceWork`]).
    /// The table is replaced by exactly what the stack lists, and only when all of it is bound; a
    /// stack without artifacts costs one walk of its layers and allocates nothing.
    pub(crate) fn bind_artifacts(&self, recipe: &mut Recipe) -> Result<(), Error> {
        self.bind_artifacts_holding(recipe, None)
    }

    /// [`Self::bind_artifacts`] for a drafted stack that may reference `held`, the patch a
    /// candidate draft holds in memory ([`crate::DraftCandidate`]): that artifact is bound from
    /// the draft, without a catalog row or an object file, and every other one as before. The held
    /// patch counts against the prepared-artifact budget as one more artifact of its length, as
    /// the committed entry's would, so a draft is refused `resource-limit` exactly when its commit
    /// would bind past the budget.
    pub(crate) fn bind_artifacts_holding(
        &self,
        recipe: &mut Recipe,
        held: Option<&Arc<PreparedArtifact>>,
    ) -> Result<(), Error> {
        let mut ids = referenced(recipe);
        let held = held.filter(|artifact| ids.contains(&artifact.id));
        if let Some(artifact) = held {
            ids.retain(|id| id != &artifact.id);
        }
        let held_bytes = held.map_or(0, |artifact| artifact.bytes.len() as u64);
        if ids.is_empty() {
            if held.is_some() {
                within_budget(held_bytes, 1)?;
            }
            recipe.artifacts = held.cloned().into_iter().collect();
            return Ok(());
        }
        let mut bound = Vec::with_capacity(ids.len() + 1);
        bound.extend(held.cloned());
        let mut unprepared = false;
        for binding in self.bind_with(&ids, held_bytes, usize::from(held.is_some()))? {
            match binding {
                Binding::Ready(artifact) => bound.push(artifact),
                Binding::Unprepared(_) => unprepared = true,
            }
        }
        if unprepared {
            // The held patch stays in the table, so the preparation this refusal asks for reads
            // only what the catalog holds ([`Self::unprepared_artifacts`]).
            if let Some(artifact) = held {
                recipe.artifacts = std::iter::once(artifact.clone()).collect();
            }
            return Err(Error::preparation_required("artifact preparation required"));
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
            .flat_map(|layer| layer.artifacts.iter().cloned())
            .chain(crate::mask::mask_artifacts(&recipe.masks))
            .all(|id| recipe.artifacts.get(&id).is_some());
        if held {
            return Ok(Cow::Borrowed(recipe));
        }
        let mut bound = recipe.clone();
        self.bind_artifacts(&mut bound)?;
        Ok(Cow::Owned(bound))
    }

    /// The artifacts a stack references that are not kept ready: what a source job must read and
    /// verify before the stack can be bound. A candidate draft's patch, which the recipe's own
    /// table holds and the catalog does not yet, needs nothing ([`Self::bind_artifacts_holding`]).
    /// Fails like [`Self::bind_artifacts`] when one cannot be prepared at all.
    pub(super) fn unprepared_artifacts(&self, recipe: &Recipe) -> Result<Vec<ArtifactId>, Error> {
        let mut ids = referenced(recipe);
        ids.retain(|id| {
            recipe.artifacts.get(id).is_none()
                || !matches!(artifact_row(&self.connection, id), Ok(None))
        });
        Ok(self
            .artifact_reads(&ids)?
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

    /// The artifacts a task's parameters name, bound before it is queued: each must be recorded in
    /// this catalog (`validation: unknown artifact <id>` otherwise, as a commit refuses one) and
    /// present in the root with its recorded length; its verified bytes when they are kept ready,
    /// and otherwise how the source work reads and verifies them. Lookup and stat only.
    pub(crate) fn task_artifacts(
        &self,
        ids: &[ArtifactId],
    ) -> Result<Vec<(ArtifactId, Binding)>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        for id in ids {
            if artifact_row(&self.connection, id)?.is_none() {
                return Err(Error::validation(format!("unknown artifact {id}")));
            }
        }
        Ok(ids.iter().cloned().zip(self.bind(ids)?).collect())
    }

    /// Every artifact `recipe` references, split for a worker that reads its own artifacts off the
    /// catalog owner and never hands them back: the verified bytes kept ready, which it binds as
    /// they are, and how it reads and verifies the rest. What a rendered catalog preview plans on
    /// the owner (`crate::previews::rendered`). Fails like [`Self::bind_artifacts`] when one cannot
    /// be prepared at all. Stat and lookup only; nothing is read.
    pub(crate) fn artifact_bindings(
        &self,
        recipe: &Recipe,
    ) -> Result<(Vec<Arc<PreparedArtifact>>, Vec<ArtifactRead>), Error> {
        let ids = referenced(recipe);
        let (mut ready, mut reads) = (Vec::new(), Vec::new());
        if ids.is_empty() {
            return Ok((ready, reads));
        }
        for binding in self.bind(&ids)? {
            match binding {
                Binding::Ready(artifact) => ready.push(artifact),
                Binding::Unprepared(read) => reads.push(read),
            }
        }
        Ok((ready, reads))
    }

    /// Keep bytes a worker verified ready under the signature they were read with, and return what
    /// was kept.
    pub(super) fn adopt_artifacts(
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
        self.bind_with(ids, 0, 0)
    }

    /// [`Self::bind`] beside `held_entries` artifacts of `held_bytes` the stack binds from
    /// elsewhere (a candidate draft's patch), which count against the same budget.
    fn bind_with(
        &self,
        ids: &[ArtifactId],
        held_bytes: u64,
        held_entries: usize,
    ) -> Result<Vec<Binding>, Error> {
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            let row = artifact_row(&self.connection, id)?.ok_or_else(|| {
                Error::source_unavailable(format!("artifact {id} is not in this catalog"))
            })?;
            rows.push((id, row));
        }
        let total: u64 = rows.iter().map(|(_, (bytes, _))| bytes).sum();
        within_budget(total + held_bytes, rows.len() + held_entries)?;
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
    pub(super) fn check_root(&self) -> Result<(), Error> {
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

    /// When the catalog recorded artifact `id` as published, if it records the artifact. One
    /// lookup.
    pub(super) fn artifact_created_ms(&self, id: &ArtifactId) -> Result<Option<i64>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT created_ms FROM artifacts WHERE id=?1",
                [id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?)
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
    /// task of this process published, and the derived sources no photograph holds and no writer
    /// of this process published, forget the removed artifacts' verified bytes, and hand the
    /// source job the artifacts and source objects the catalog still records, so it removes every
    /// other object file. A referenced row is never removed; the foreign keys refuse it even if
    /// this query were wrong.
    pub(crate) fn plan_collection(&mut self) -> Result<Collection, Error> {
        self.plan_collection_for(None)
    }

    pub(crate) fn plan_collection_for(
        &mut self,
        job: Option<&crate::JobId>,
    ) -> Result<Collection, Error> {
        let key = self.catalog_request.clone();
        let live = &self.live_artifacts;
        let live_sources = &self.live_sources;
        let (removed, keep, (source_rows, keep_sources)) = write(&mut self.connection, |tx| {
            let removed = collection_candidates(tx, live)?;
            for id in &removed {
                tx.execute("DELETE FROM artifacts WHERE id=?1", [id.as_str()])?;
            }
            let keep = recorded_artifact_ids(tx)?;
            let sources = super::derived_source::plan_source_collection(tx, live_sources)?;
            if let (Some(key), Some(job)) = (key.as_ref(), job) {
                crate::editor::catalog_requests::admit(
                    tx,
                    key,
                    job,
                    "collect",
                    None,
                    &json!({"rows":removed.len(),"source_rows":sources.0}),
                )?;
            }
            Ok((removed, keep, sources))
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
            keep_sources,
            live_sources: self.live_sources.clone(),
            source_rows,
            candidates: self.candidates.clone(),
        })
    }
}

/// Every artifact the catalog records: what a collection keeps.
pub(super) fn recorded_artifact_ids(connection: &Connection) -> Result<HashSet<ArtifactId>, Error> {
    let mut statement = connection.prepare_cached("SELECT id FROM artifacts")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut keep = HashSet::new();
    for row in rows {
        keep.insert(ArtifactId::parse(row?)?);
    }
    Ok(keep)
}
