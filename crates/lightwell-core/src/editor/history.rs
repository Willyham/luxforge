use super::{
    AssetRecord, EditorService, EditorState, HistoryPage, Lineage, LineageStep, MutationOutcome,
    MutationResult, Version, VersionResult, artifact_store,
    catalog::{
        catalog_error, decode, encode, ensure_request_absent, input_hash, insert_entry,
        insert_request, json_error, next_sequence, now_ms,
    },
    masks::MASK_FIELD,
    source::validate_source_recipe,
};
use crate::{
    AssetId, EntryId, Error, ErrorKind, HistoryEntry, MaskId, Mutation, Recipe, Snapshot,
    SnapshotId, artifacts::PreparedArtifact, modules::ActionInput,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::sync::Arc;

const MAX_HISTORY_PAGE: usize = 100;
const MAX_VERSION_NAME: usize = 64;

impl EditorService {
    pub fn history(
        &self,
        asset_id: &AssetId,
        before_sequence: Option<u64>,
        limit: usize,
    ) -> Result<HistoryPage, Error> {
        if limit == 0 || limit > MAX_HISTORY_PAGE {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("history page limit must be 1..={MAX_HISTORY_PAGE}"),
            ));
        }
        let before = before_sequence
            .unwrap_or(i64::MAX as u64)
            .min(i64::MAX as u64) as i64;
        let mut statement = self
            .connection
            .prepare("SELECT entry_json FROM entries WHERE asset_id=?1 AND sequence<?2 ORDER BY sequence DESC LIMIT ?3")
            .map_err(catalog_error)?;
        let rows = statement
            .query_map(params![asset_id.as_str(), before, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(catalog_error)?;
        let mut entries: Vec<HistoryEntry> = Vec::new();
        for row in rows {
            entries.push(decode(
                "invalid history entry",
                row.map_err(catalog_error)?,
            )?);
        }
        let next_before_sequence =
            (entries.len() == limit).then(|| entries.last().unwrap().sequence);
        Ok(HistoryPage {
            entries,
            next_before_sequence,
        })
    }

    /// Admit one stack a write is about to persist as a new snapshot. Every path that writes one —
    /// an action, a composite, a `mask.*` command and a restore — admits through here, so none
    /// persists a stack with fewer checks than another.
    ///
    /// The stack is validated against the registry (an unavailable provider, a refused payload or a
    /// mask on a stage that cannot carry one is refused by name) and against the asset's source
    /// kind. Every artifact it lists must be recorded with a present file, and is then bound; the
    /// bound bytes are returned for the caller to hold until the write, whose transaction checks
    /// the references again and records them. Last, the stack is compiled against the asset's
    /// dimensions, which resolves every stroke it references, so a later layer addressing a stage
    /// that no longer exists or a stroke the store has lost is refused with the compile error and
    /// nothing is written. `O(layers)`; it rasterizes nothing.
    fn admit(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
    ) -> Result<Vec<Arc<PreparedArtifact>>, Error> {
        self.registry.validate_recipe(recipe)?;
        validate_source_recipe(asset, recipe)?;
        artifact_store::recorded_artifacts(&self.connection, &self.artifact_root, recipe)?;
        let artifacts = self.require_artifacts(recipe)?;
        self.registry.compile(asset.width, asset.height, recipe)?;
        Ok(artifacts)
    }

    /// Persist one resulting stack: the same path for an appended and an updated layer. The stack
    /// is [admitted](Self::admit) first, so a stack any check refuses writes nothing.
    pub(super) fn commit_snapshot(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        request: Value,
        snapshot: Snapshot,
        asset: &AssetRecord,
        action: CommittedAction,
    ) -> Result<MutationResult, Error> {
        let mut state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let _artifacts = self.admit(asset, &snapshot.recipe)?;
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset_id.clone(),
            sequence: next_sequence(&self.connection, asset_id)?,
            action_id: action.input.action_id,
            label: action.label,
            parameters: Value::Object(action.input.parameters),
            actor: mutation.actor.clone(),
            timestamp_ms: now_ms(),
            request_id: Some(mutation.request_id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot,
            undo_parent: Some(state.current_entry.id.clone()),
            restore_target: None,
        };
        let result = MutationResult {
            outcome: MutationOutcome::Applied,
            revision: entry.result_revision,
            current_entry_id: entry.id.clone(),
            created_entry_id: Some(entry.id.clone()),
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        ensure_request_absent(&tx, asset_id, &mutation.request_id)?;
        insert_entry(&self.registry, &tx, &self.artifact_root, &entry)?;
        tx.execute("UPDATE asset_state SET current_entry_id=?2,revision=?3,redo_json='[]' WHERE asset_id=?1", params![asset_id.as_str(), entry.id.as_str(), entry.result_revision as i64]).map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, &request, &result)?;
        tx.commit().map_err(catalog_error)?;
        self.entries
            .get_mut()
            .moved(asset_id, entry.result_revision, &entry.id, Vec::new());
        state.current_entry = entry;
        Ok(result)
    }

    pub fn undo(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
    ) -> Result<MutationResult, Error> {
        self.navigate(asset_id, mutation, Navigation::Undo)
    }

    pub fn redo(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
    ) -> Result<MutationResult, Error> {
        self.navigate(asset_id, mutation, Navigation::Redo)
    }

    fn navigate(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        navigation: Navigation,
    ) -> Result<MutationResult, Error> {
        mutation.validate()?;
        let action = match navigation {
            Navigation::Undo => "undo",
            Navigation::Redo => "redo",
        };
        let input = json!({"action":action,"mutation":mutation});
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &input)? {
            return Ok(result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let mut redo = state.redo.clone();
        let target = match navigation {
            Navigation::Undo => {
                let Some(parent) = state.current_entry.undo_parent.clone() else {
                    return self.persist_noop(asset_id, &mutation, &input, &state);
                };
                redo.push(state.current_entry.id.clone());
                parent
            }
            Navigation::Redo => {
                let Some(entry) = redo.pop() else {
                    return self.persist_noop(asset_id, &mutation, &input, &state);
                };
                entry
            }
        };
        let _ = self.entry(asset_id, &target)?;
        let result = MutationResult {
            outcome: MutationOutcome::Navigated,
            revision: state.revision + 1,
            current_entry_id: target.clone(),
            created_entry_id: None,
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        tx.execute(
            "UPDATE asset_state SET current_entry_id=?2,revision=?3,redo_json=?4 WHERE asset_id=?1",
            params![
                asset_id.as_str(),
                target.as_str(),
                result.revision as i64,
                encode(&redo)?
            ],
        )
        .map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, &input, &result)?;
        tx.commit().map_err(catalog_error)?;
        self.entries
            .get_mut()
            .moved(asset_id, result.revision, &target, redo);
        Ok(result)
    }

    pub fn restore(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        target_id: &EntryId,
    ) -> Result<MutationResult, Error> {
        mutation.validate()?;
        let input = json!({"action":"restore","mutation":mutation,"target_entry_id":target_id});
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &input)? {
            return Ok(result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let target = self.entry(asset_id, target_id)?;
        // A restore admits the stack it copies exactly as a commit admits the stack it writes, so
        // restoring one this build cannot evaluate — an unavailable provider, a lost stroke or
        // artifact — fails explicitly; browsing it with undo, redo and history stays available.
        let _artifacts = self.admit(&state.asset, &target.snapshot.recipe)?;
        if target.snapshot.recipe == state.current_entry.snapshot.recipe {
            return self.persist_noop(asset_id, &mutation, &input, &state);
        }
        let snapshot = Snapshot {
            id: SnapshotId::new(),
            asset_id: asset_id.clone(),
            recipe: target.snapshot.recipe.clone(),
        };
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset_id.clone(),
            sequence: next_sequence(&self.connection, asset_id)?,
            action_id: "restore".into(),
            label: format!("Restore entry {}", target.sequence),
            parameters: json!({"target_entry_id":target_id}),
            actor: mutation.actor.clone(),
            timestamp_ms: now_ms(),
            request_id: Some(mutation.request_id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot,
            undo_parent: Some(state.current_entry.id.clone()),
            restore_target: Some(target_id.clone()),
        };
        let result = MutationResult {
            outcome: MutationOutcome::Applied,
            revision: entry.result_revision,
            current_entry_id: entry.id.clone(),
            created_entry_id: Some(entry.id.clone()),
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        insert_entry(&self.registry, &tx, &self.artifact_root, &entry)?;
        tx.execute("UPDATE asset_state SET current_entry_id=?2,revision=?3,redo_json='[]' WHERE asset_id=?1", params![asset_id.as_str(), entry.id.as_str(), entry.result_revision as i64]).map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, &input, &result)?;
        tx.commit().map_err(catalog_error)?;
        self.entries
            .get_mut()
            .moved(asset_id, entry.result_revision, &entry.id, Vec::new());
        Ok(result)
    }

    /// Walk undo parents from `from` (default: current) towards Original, newest first.
    pub fn lineage(
        &self,
        asset_id: &AssetId,
        from: Option<&EntryId>,
        limit: usize,
    ) -> Result<Lineage, Error> {
        if limit == 0 || limit > MAX_HISTORY_PAGE {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("lineage limit must be 1..={MAX_HISTORY_PAGE}"),
            ));
        }
        let mut next = Some(match from {
            Some(entry_id) => entry_id.clone(),
            None => self.state(asset_id)?.current_entry.id,
        });
        let mut steps = Vec::new();
        while let Some(entry_id) = next.take() {
            if steps.len() == limit {
                next = Some(entry_id);
                break;
            }
            let (sequence, action_id, parent): (i64, String, Option<String>) = self
                .connection
                .query_row(
                    "SELECT sequence,action_id,undo_parent_id FROM entries WHERE id=?1 AND asset_id=?2",
                    params![entry_id.as_str(), asset_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(catalog_error)?
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Validation,
                        "history entry does not belong to this asset",
                    )
                })?;
            let undo_parent = parent.map(EntryId::parse).transpose()?;
            next = undo_parent.clone();
            steps.push(LineageStep {
                entry_id,
                sequence: u64::try_from(sequence)
                    .map_err(|_| Error::new(ErrorKind::Catalog, "invalid history sequence"))?,
                action_id,
                undo_parent,
            });
        }
        Ok(Lineage {
            steps,
            next_entry_id: next,
        })
    }

    /// Saved versions of one asset in creation order.
    pub fn versions(&self, asset_id: &AssetId) -> Result<Vec<Version>, Error> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT v.name,v.entry_id,e.sequence,v.actor,v.created_ms FROM versions v
                 JOIN entries e ON e.id = v.entry_id
                 WHERE v.asset_id=?1 ORDER BY v.created_ms, v.name",
            )
            .map_err(catalog_error)?;
        let rows = statement
            .query_map([asset_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(catalog_error)?;
        let mut versions = Vec::new();
        for row in rows {
            let (name, entry_id, sequence, actor, created_ms) = row.map_err(catalog_error)?;
            versions.push(Version {
                asset_id: asset_id.clone(),
                name,
                entry_id: EntryId::parse(entry_id)?,
                entry_sequence: u64::try_from(sequence)
                    .map_err(|_| Error::new(ErrorKind::Catalog, "invalid history sequence"))?,
                actor,
                created_ms,
            });
        }
        Ok(versions)
    }

    /// Name a retained entry (default: current). Re-creating the same name on the same entry is a no-op;
    /// on a different entry it is a conflict. Names are unique per asset ignoring case.
    pub fn create_version(
        &mut self,
        asset_id: &AssetId,
        name: &str,
        entry_id: Option<&EntryId>,
        actor: &str,
    ) -> Result<VersionResult, Error> {
        let name = valid_version_name(name)?;
        if actor.is_empty() || actor.len() > 128 {
            return Err(Error::new(
                ErrorKind::Validation,
                "actor must contain 1..128 characters",
            ));
        }
        let entry = match entry_id {
            Some(entry_id) => self.entry(asset_id, entry_id)?,
            None => self.state(asset_id)?.current_entry,
        };
        if let Some(existing) = self
            .versions(asset_id)?
            .into_iter()
            .find(|version| version.name.eq_ignore_ascii_case(&name))
        {
            if existing.entry_id == entry.id {
                return Ok(VersionResult {
                    outcome: MutationOutcome::NoOp,
                    version: Some(existing),
                });
            }
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("version {:?} already names another entry", existing.name),
            ));
        }
        let version = Version {
            asset_id: asset_id.clone(),
            name,
            entry_id: entry.id,
            entry_sequence: entry.sequence,
            actor: actor.into(),
            created_ms: now_ms(),
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        tx.execute(
            "INSERT INTO versions VALUES (?1,?2,?3,?4,?5)",
            params![
                asset_id.as_str(),
                version.name,
                version.entry_id.as_str(),
                version.actor,
                version.created_ms
            ],
        )
        .map_err(catalog_error)?;
        tx.commit().map_err(catalog_error)?;
        Ok(VersionResult {
            outcome: MutationOutcome::Applied,
            version: Some(version),
        })
    }

    /// Remove a version name. The entry it named is retained history and stays reachable.
    pub fn delete_version(
        &mut self,
        asset_id: &AssetId,
        name: &str,
    ) -> Result<VersionResult, Error> {
        let name = valid_version_name(name)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        let removed = tx
            .execute(
                "DELETE FROM versions WHERE asset_id=?1 AND name=?2",
                params![asset_id.as_str(), name],
            )
            .map_err(catalog_error)?;
        tx.commit().map_err(catalog_error)?;
        Ok(VersionResult {
            outcome: if removed == 0 {
                MutationOutcome::NoOp
            } else {
                MutationOutcome::Applied
            },
            version: None,
        })
    }

    pub(super) fn persist_noop(
        &mut self,
        asset_id: &AssetId,
        mutation: &Mutation,
        input: &Value,
        state: &EditorState,
    ) -> Result<MutationResult, Error> {
        let result = MutationResult {
            outcome: MutationOutcome::NoOp,
            revision: state.revision,
            current_entry_id: state.current_entry.id.clone(),
            created_entry_id: None,
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, input, &result)?;
        tx.commit().map_err(catalog_error)?;
        Ok(result)
    }

    pub(super) fn request_result(
        &self,
        asset_id: &AssetId,
        request_id: &str,
        input: &Value,
    ) -> Result<Option<MutationResult>, Error> {
        let found = self
            .connection
            .query_row(
                "SELECT input_hash,result_json FROM requests WHERE asset_id=?1 AND request_id=?2",
                params![asset_id.as_str(), request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(catalog_error)?;
        let Some((stored_hash, result_json)) = found else {
            return Ok(None);
        };
        if stored_hash != input_hash(input)? {
            return Err(Error::new(
                ErrorKind::Conflict,
                "request_id was already used with different input",
            ));
        }
        let mut result: MutationResult = decode("invalid saved request result", result_json)?;
        result.deduplicated = true;
        Ok(Some(result))
    }
}

enum Navigation {
    Undo,
    Redo,
}

/// What one action records on the entry it commits: the durable identity and stored parameters the
/// module parsed, and the label the host rendered from the action that was requested.
pub(super) struct CommittedAction {
    pub(super) input: ActionInput,
    pub(super) label: String,
}

pub(super) fn ensure_revision(state: &EditorState, expected: u64) -> Result<(), Error> {
    if state.revision == expected {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "stale revision {expected}; current revision is {}",
                state.revision
            ),
        ))
    }
}

/// The deduplicated request identity: the durable action, the mutation envelope, the mask target and
/// the parsed parameters as top-level fields.
///
/// The target belongs here because it is part of what the request *is*: the same fields sent to the
/// global layer and to a mask are two different edits, and a client that reused one request id for
/// both must not receive the first one's result for the second.
pub(super) fn request_input(
    input: &ActionInput,
    mutation: &Mutation,
    mask: Option<&MaskId>,
) -> Result<Value, Error> {
    let mut request = serde_json::Map::new();
    request.insert("action".into(), Value::from(input.action_id.as_str()));
    request.insert(
        "mutation".into(),
        serde_json::to_value(mutation).map_err(|e| json_error("cannot encode request", e))?,
    );
    if let Some(mask) = mask {
        request.insert(MASK_FIELD.into(), Value::from(mask.as_str()));
    }
    for (name, value) in &input.parameters {
        request.insert(name.clone(), value.clone());
    }
    Ok(Value::Object(request))
}

fn valid_version_name(name: &str) -> Result<String, Error> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_VERSION_NAME
        || name.chars().any(char::is_control)
    {
        return Err(Error::new(
            ErrorKind::Validation,
            format!("version name must contain 1..={MAX_VERSION_NAME} printable characters"),
        ));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transform;
    use crate::editor::test_support::{
        SHRINK_ACTION, SHRINK_EFFECT, ShrinkModule, brushed, commit, fixture, mutation, next_entry,
        shrink, stroke, temp,
    };
    use rusqlite::Connection;
    use serde_json::Map;

    #[test]
    fn persistent_history_journey_retains_snapshots_navigation_and_source() {
        let catalog = temp("journey.sqlite");
        let source_path = fixture();
        let source_bytes = std::fs::read(&source_path).unwrap();
        let (asset, original, a, b);
        {
            let mut service = EditorService::open(&catalog).unwrap();
            let state = service.import(&source_path).unwrap();
            asset = state.asset.id.clone();
            original = state.current_entry.id.clone();
            let result = service
                .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
                .unwrap();
            a = result.current_entry_id;
            let result = service
                .apply_pixel(&asset, mutation(1, "b"), 0, 0, [4, 5, 6])
                .unwrap();
            b = result.current_entry_id;
            assert_eq!(
                service.render_current(&asset).unwrap().pixel(0, 0),
                Some([4, 5, 6, 255])
            );
            service.undo(&asset, mutation(2, "undo")).unwrap();
            assert_eq!(
                service.render_current(&asset).unwrap().pixel(0, 0),
                Some([1, 2, 3, 255])
            );
            service.redo(&asset, mutation(3, "redo")).unwrap();
            service.restore(&asset, mutation(4, "restore"), &a).unwrap();
            service
                .apply_pixel(&asset, mutation(5, "c"), 1, 0, [7, 8, 9])
                .unwrap();
            assert_eq!(service.history(&asset, None, 20).unwrap().entries.len(), 5);
        }
        let service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        assert_eq!(state.revision, 6);
        assert!(
            service
                .entry(&asset, &original)
                .unwrap()
                .snapshot
                .recipe
                .layers
                .is_empty()
        );
        assert_eq!(
            service.render_entry(&asset, &b).unwrap().pixel(0, 0),
            Some([4, 5, 6, 255])
        );
        assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn retries_noops_conflicts_pagination_and_wrong_assets_are_safe() {
        let catalog = temp("requests.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        let original_pixel = service.render_current(&asset).unwrap().pixel(0, 0).unwrap();
        let no_op = service
            .apply_pixel(
                &asset,
                mutation(0, "noop"),
                0,
                0,
                [original_pixel[0], original_pixel[1], original_pixel[2]],
            )
            .unwrap();
        assert_eq!(no_op.outcome, MutationOutcome::NoOp);
        let first = service
            .apply_pixel(&asset, mutation(0, "same"), 0, 0, [1, 2, 3])
            .unwrap();
        let retry = service
            .apply_pixel(&asset, mutation(0, "same"), 0, 0, [1, 2, 3])
            .unwrap();
        assert_eq!(first.current_entry_id, retry.current_entry_id);
        assert!(retry.deduplicated);
        assert!(
            service
                .apply_pixel(&asset, mutation(0, "same"), 0, 0, [9, 9, 9])
                .is_err()
        );
        assert_eq!(
            service
                .apply_pixel(&asset, mutation(0, "stale"), 1, 0, [9, 9, 9])
                .unwrap_err()
                .kind,
            ErrorKind::Conflict
        );
        assert_eq!(service.history(&asset, None, 1).unwrap().entries.len(), 1);
        assert!(service.history(&asset, None, 101).is_err());
        assert!(
            service
                .entry(&AssetId::new(), &first.current_entry_id)
                .is_err()
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn versions_name_retained_entries_and_survive_reopen() {
        let catalog = temp("versions.sqlite");
        let (asset, a, original);
        {
            let mut service = EditorService::open(&catalog).unwrap();
            let state = service.import(&fixture()).unwrap();
            asset = state.asset.id.clone();
            original = state.current_entry.id.clone();
            a = service
                .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
                .unwrap()
                .current_entry_id;
            let created = service
                .create_version(&asset, " Keeper ", None, "test")
                .unwrap();
            assert_eq!(created.outcome, MutationOutcome::Applied);
            assert_eq!(created.version.as_ref().unwrap().name, "Keeper");
            assert_eq!(created.version.as_ref().unwrap().entry_id, a);
            let again = service
                .create_version(&asset, "keeper", Some(&a), "test")
                .unwrap();
            assert_eq!(again.outcome, MutationOutcome::NoOp);
            assert_eq!(
                service
                    .create_version(&asset, "KEEPER", Some(&original), "test")
                    .unwrap_err()
                    .kind,
                ErrorKind::Conflict
            );
            assert!(service.create_version(&asset, "", None, "test").is_err());
            assert!(
                service
                    .create_version(&asset, "bad\u{7}", None, "test")
                    .is_err()
            );
            service.undo(&asset, mutation(1, "undo")).unwrap();
            service
                .create_version(&asset, "Start", None, "test")
                .unwrap();
            let names: Vec<(String, u64)> = service
                .versions(&asset)
                .unwrap()
                .into_iter()
                .map(|version| (version.name, version.entry_sequence))
                .collect();
            assert_eq!(names, [("Keeper".to_string(), 1), ("Start".to_string(), 0)]);
        }
        let mut service = EditorService::open(&catalog).unwrap();
        let versions = service.versions(&asset).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].entry_id, original);
        service
            .restore(&asset, mutation(2, "restore-keeper"), &versions[0].entry_id)
            .unwrap();
        assert_eq!(
            service.render_current(&asset).unwrap().pixel(0, 0),
            Some([1, 2, 3, 255])
        );
        assert_eq!(
            service.delete_version(&asset, "keeper").unwrap().outcome,
            MutationOutcome::Applied
        );
        assert_eq!(
            service.delete_version(&asset, "keeper").unwrap().outcome,
            MutationOutcome::NoOp
        );
        assert_eq!(service.versions(&asset).unwrap().len(), 1);
        assert!(
            service.entry(&asset, &a).is_ok(),
            "deleting a version keeps its entry"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn lineage_follows_undo_parents_and_skips_abandoned_branches() {
        let catalog = temp("lineage.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        let original = state.current_entry.id;
        let a = service
            .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
            .unwrap()
            .current_entry_id;
        let b = service
            .apply_pixel(&asset, mutation(1, "b"), 1, 0, [4, 5, 6])
            .unwrap()
            .current_entry_id;
        service.undo(&asset, mutation(2, "undo")).unwrap();
        let c = service
            .apply_pixel(&asset, mutation(3, "c"), 2, 0, [7, 8, 9])
            .unwrap()
            .current_entry_id;
        let ids = |lineage: Lineage| -> Vec<EntryId> {
            lineage
                .steps
                .into_iter()
                .map(|step| step.entry_id)
                .collect()
        };
        assert_eq!(
            ids(service.lineage(&asset, None, 10).unwrap()),
            [c.clone(), a.clone(), original.clone()]
        );
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 4);
        assert_eq!(
            ids(service.lineage(&asset, Some(&b), 10).unwrap()),
            [b.clone(), a.clone(), original.clone()]
        );
        let page = service.lineage(&asset, None, 2).unwrap();
        assert_eq!(page.steps.len(), 2);
        assert_eq!(page.next_entry_id, Some(original.clone()));
        assert_eq!(
            ids(service
                .lineage(&asset, page.next_entry_id.as_ref(), 2)
                .unwrap()),
            [original]
        );
        assert!(service.lineage(&asset, None, 0).is_err());
        assert!(service.lineage(&AssetId::new(), Some(&c), 5).is_err());
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn every_entry_carries_the_label_its_history_row_shows() {
        let catalog = temp("labels.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        assert_eq!(state.current_entry.label, "Original");
        let label =
            |service: &EditorService, entry: &EntryId| service.entry(&asset, entry).unwrap().label;
        let pixel = service
            .apply_pixel(&asset, mutation(0, "pixel"), 3, 4, [1, 2, 3])
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &pixel), "Pixel 3, 4");
        // The requested action renders the label; the entry stores the durable transform identity.
        let rotated = service
            .apply_transform(&asset, mutation(1, "rotate"), Transform::RotateLeft)
            .unwrap()
            .current_entry_id;
        let entry = service.entry(&asset, &rotated).unwrap();
        assert_eq!(
            (entry.action_id.as_str(), entry.label.as_str()),
            ("rotate-left", "Rotate left")
        );
        let cropped = service
            .apply_action(
                &asset,
                mutation(2, "crop"),
                "crop",
                json!({"angle":3.5,"x":0.1,"y":0.1,"width":0.5,"height":0.5}),
            )
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &cropped), "Crop 3.5°");
        let fitted = service
            .apply_action(
                &asset,
                mutation(3, "fit"),
                "crop-fit",
                json!({"aspect":"16:9"}),
            )
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &fitted), "Crop 16:9");
        // An action without a template is labelled by its title.
        let reset = service
            .apply_action(&asset, mutation(4, "reset"), "crop-reset", json!({}))
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &reset), "Reset crop");
        let restored = service
            .restore(&asset, mutation(5, "restore"), &pixel)
            .unwrap()
            .current_entry_id;
        assert_eq!(
            label(&service, &restored),
            "Restore entry 1",
            "a restore names the sequence it copied"
        );
        // Labels are stored with the entries, so reopening reads the same rows.
        drop(service);
        let service = EditorService::open(&catalog).unwrap();
        let labels: Vec<String> = service
            .history(&asset, None, 50)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.label)
            .collect();
        assert_eq!(
            labels,
            [
                "Restore entry 1",
                "Reset crop",
                "Crop 16:9",
                "Crop 3.5°",
                "Rotate left",
                "Pixel 3, 4",
                "Original",
            ]
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn failed_entry_write_rolls_back_snapshot_state_and_request() {
        let catalog = temp("rollback.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let before = service.state(&asset).unwrap();
        service
            .connection
            .execute_batch(
                "CREATE TRIGGER injected_entry_failure BEFORE INSERT ON entries
                 WHEN NEW.sequence > 0 BEGIN SELECT RAISE(ABORT, 'injected'); END;",
            )
            .unwrap();
        assert!(
            service
                .apply_pixel(&asset, mutation(0, "will-fail"), 0, 0, [1, 2, 3])
                .is_err()
        );
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 1);
        service
            .connection
            .execute_batch("DROP TRIGGER injected_entry_failure")
            .unwrap();
        assert!(
            service
                .apply_pixel(&asset, mutation(0, "will-fail"), 0, 0, [1, 2, 3])
                .is_ok(),
            "a failed transaction must not retain a false request result"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn undoing_restore_returns_to_the_preceding_current_entry() {
        let catalog = temp("restore-undo.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        let a = service
            .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
            .unwrap()
            .current_entry_id;
        let b = service
            .apply_pixel(&asset, mutation(1, "b"), 0, 0, [4, 5, 6])
            .unwrap()
            .current_entry_id;
        service
            .restore(&asset, mutation(2, "restore-a"), &a)
            .unwrap();
        service.undo(&asset, mutation(3, "undo-restore")).unwrap();
        assert_eq!(service.state(&asset).unwrap().current_entry.id, b);
        assert_eq!(
            service.render_current(&asset).unwrap().pixel(0, 0),
            Some([4, 5, 6, 255])
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Restore admits the stack it copies through the one admission every commit uses, so it
    /// refuses exactly what a commit of that stack refuses: here a stack naming a stroke the store
    /// has lost, which a registry check alone admits, and a stack whose provider this build does
    /// not have. Nothing is written, and undo still reaches the refused stack.
    #[test]
    fn restore_refuses_a_stack_exactly_as_a_commit_of_it_is_refused() {
        /// Restore `target` over the current entry, then commit its stack through the commit path,
        /// and return both refusals after checking that neither wrote anything.
        fn refusals(service: &mut EditorService, asset: &AssetId, target: &EntryId) -> [Error; 2] {
            let before = service.state(asset).unwrap();
            let rows = service.history(asset, None, 50).unwrap().entries.len();
            let restored = service
                .restore(asset, mutation(before.revision, "restore"), target)
                .expect_err("restore refuses the stack");
            let recipe = service.entry(asset, target).unwrap().snapshot.recipe;
            let committed = service
                .commit_snapshot(
                    asset,
                    mutation(before.revision, "commit"),
                    json!({"action": "commit"}),
                    Snapshot {
                        id: SnapshotId::new(),
                        asset_id: asset.clone(),
                        recipe,
                    },
                    &before.asset,
                    CommittedAction {
                        input: ActionInput {
                            action_id: "commit".into(),
                            parameters: Map::new(),
                        },
                        label: "Commit".into(),
                    },
                )
                .expect_err("a commit refuses the stack");
            assert_eq!(service.state(asset).unwrap(), before, "nothing moved");
            assert_eq!(
                service.history(asset, None, 50).unwrap().entries.len(),
                rows,
                "no entry was written"
            );
            [restored, committed]
        }

        // A stroke the store has lost.
        let catalog = temp("restore-admission-stroke.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.state(&asset).unwrap();
        let drawn = stroke(3);
        let brushed_entry = next_entry(
            &original,
            brushed(
                &original.current_entry.snapshot.recipe,
                std::slice::from_ref(&drawn),
            ),
        );
        drop(service);
        commit(&catalog, &brushed_entry);
        let mut service = EditorService::open(&catalog).unwrap();
        service
            .restore(&asset, mutation(1, "back"), &original.current_entry.id)
            .unwrap();
        drop(service);
        Connection::open(&catalog)
            .unwrap()
            .execute(
                "DELETE FROM strokes WHERE id=?1",
                params![drawn.id().as_str()],
            )
            .unwrap();
        let mut service = EditorService::open(&catalog).unwrap();
        let [restored, committed] = refusals(&mut service, &asset, &brushed_entry.id);
        assert_eq!(restored.kind, ErrorKind::Incompatible);
        assert_eq!(
            restored.detail,
            format!(
                "stroke {} of entry {} is not in the stroke store referenced by component \
                 Brush 1 of mask Mask 1",
                drawn.id(),
                brushed_entry.id
            )
        );
        assert_eq!(
            (restored.kind, &restored.detail),
            (committed.kind, &committed.detail)
        );
        service.undo(&asset, mutation(2, "undo")).unwrap();
        assert_eq!(
            service.state(&asset).unwrap().current_entry.id,
            brushed_entry.id,
            "undo still reaches the refused stack"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();

        // A provider this build does not have.
        let catalog = temp("restore-admission-provider.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.state(&asset).unwrap().current_entry.id;
        let shrunk = service
            .apply_action(&asset, mutation(0, "shrink"), SHRINK_ACTION, shrink(10, 10))
            .unwrap()
            .current_entry_id;
        service
            .restore(&asset, mutation(1, "back"), &original)
            .unwrap();
        drop(service);
        let mut service = EditorService::open(&catalog).unwrap();
        let [restored, committed] = refusals(&mut service, &asset, &shrunk);
        assert_eq!(restored.kind, ErrorKind::Incompatible);
        assert!(
            restored.detail.contains(SHRINK_EFFECT),
            "{}",
            restored.detail
        );
        assert_eq!(
            (restored.kind, &restored.detail),
            (committed.kind, &committed.detail)
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
