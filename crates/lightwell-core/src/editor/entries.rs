//! The owner's cache of hydrated history entries and of each asset's head.
//!
//! A history entry never changes once written: the catalog's `entries_are_immutable` trigger
//! refuses an update, and the stroke store it resolves against refuses one too. This service is the
//! catalog's only writer, over an exclusive connection, and every write takes `&mut self`. So an
//! entry read once, with its strokes resolved, is that entry for as long as the service is open, and
//! an asset's head — its row, its revision, its current entry and its redo list — changes only where
//! this service commits a write that moves it. Those writes update the head in the same place they
//! commit, after the commit succeeds:
//!
//! - an action, a composite and a `mask.*` command commit through `commit_snapshot`, and a restore
//!   through `restore`: the new entry becomes current, one revision on, with nothing to redo;
//! - undo and redo through `navigate`: the target becomes current, one revision on, with the redo
//!   list the navigation wrote;
//! - an import inserts a new asset under a new identity, which no head or entry here can name, so
//!   there is nothing to update; a repeated import writes nothing;
//! - a no-op records only its request, and naming or removing a version touches neither an entry
//!   nor a head, so neither has anything to update;
//! - reopening builds a new service, whose cache starts empty.
//!
//! A write that fails updates nothing, because the catalog kept its prior state too.
//!
//! What is cached is only ever what the catalog returned — the entry decoded from its stored JSON,
//! the head read from its rows — and never a value a write built in memory, which could differ
//! from its stored spelling in the last bit of a float. The first read after a commit therefore
//! decodes the new entry once, and every read after it decodes nothing.
use super::{AssetRecord, EditorService, catalog};
use crate::{AssetId, EntryId, Error, HistoryEntry};
use std::sync::Arc;

/// Hydrated entries kept, least recently read first out.
///
/// The reads that repeat are per open asset: its current entry, which every slider tick, preview,
/// sample and draft reads; the undo parent a mask command or an undo reads beside it; and a history
/// entry a client has selected to preview or sample. That is three, and the desktop and an agent
/// each working on a different asset make six; eight leaves room without scanning far. An entry's
/// size is bounded by its recipe's: at most 256 KiB of serialized masks and, shared rather than
/// copied, the strokes they resolve to, at most 16 masks of 8192 positions of 8 bytes, so the cache
/// holds at most about 10 MiB beside its entries' layer payloads.
pub(super) const CACHED_ENTRIES: usize = 8;

/// Asset heads kept, least recently read first out: two for each of the 8 live clients the API
/// admits. A head is one asset row, with its RAW interpretation shared, and its redo list.
pub(super) const CACHED_HEADS: usize = 16;

/// One asset's head: its row, and where its history stands.
#[derive(Clone, Debug)]
pub(super) struct Head {
    pub(super) asset: AssetRecord,
    pub(super) revision: u64,
    pub(super) current: EntryId,
    pub(super) redo: Vec<EntryId>,
}

#[derive(Debug, Default)]
pub(super) struct EntryCache {
    /// Each entry under the asset it was read for, least recently read first.
    entries: Vec<(AssetId, Arc<HistoryEntry>)>,
    /// Least recently read first.
    heads: Vec<Head>,
}

impl EntryCache {
    fn entry(&mut self, asset_id: &AssetId, entry_id: &EntryId) -> Option<Arc<HistoryEntry>> {
        let at = self
            .entries
            .iter()
            .position(|(asset, entry)| entry.id == *entry_id && asset == asset_id)?;
        let found = self.entries.remove(at);
        let entry = found.1.clone();
        self.entries.push(found);
        Some(entry)
    }

    fn keep_entry(&mut self, asset_id: &AssetId, entry: Arc<HistoryEntry>) {
        self.entries
            .retain(|(asset, kept)| !(kept.id == entry.id && asset == asset_id));
        if self.entries.len() == CACHED_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push((asset_id.clone(), entry));
    }

    fn head(&mut self, asset_id: &AssetId) -> Option<&Head> {
        let at = self
            .heads
            .iter()
            .position(|head| head.asset.id == *asset_id)?;
        let head = self.heads.remove(at);
        self.heads.push(head);
        self.heads.last()
    }

    fn keep_head(&mut self, head: Head) {
        self.heads.retain(|kept| kept.asset.id != head.asset.id);
        if self.heads.len() == CACHED_HEADS {
            self.heads.remove(0);
        }
        self.heads.push(head);
    }

    pub(super) fn revision(&self, asset_id: &AssetId) -> Option<u64> {
        self.heads
            .iter()
            .find(|head| head.asset.id == *asset_id)
            .map(|head| head.revision)
    }

    /// A committed write moved this asset's history to `current` at `revision`, with `redo` left to
    /// redo. A head that is not cached is read from the rows the write committed when next asked.
    pub(super) fn moved(
        &mut self,
        asset_id: &AssetId,
        revision: u64,
        current: &EntryId,
        redo: Vec<EntryId>,
    ) {
        if let Some(head) = self
            .heads
            .iter_mut()
            .find(|head| head.asset.id == *asset_id)
        {
            head.revision = revision;
            head.current = current.clone();
            head.redo = redo;
        }
    }

    /// How many entries and heads are held.
    #[cfg(test)]
    pub(super) fn held(&self) -> (usize, usize) {
        (self.entries.len(), self.heads.len())
    }
}

impl EditorService {
    /// This asset's head: from the cache, or read from its rows once and kept.
    pub(super) fn head(&self, asset_id: &AssetId) -> Result<Head, Error> {
        if let Some(head) = self.entries.borrow_mut().head(asset_id) {
            return Ok(head.clone());
        }
        let head = catalog::head_from(&self.connection, asset_id)?;
        self.entries.borrow_mut().keep_head(head.clone());
        Ok(head)
    }

    /// One hydrated entry, shared: from the cache, or decoded and resolved once and kept.
    ///
    /// An entry with a stroke the store does not hold is returned but not kept, because storing
    /// that stroke later would change what a fresh read resolves; it is broken data and rare, and it
    /// is read again each time rather than answered from a resolution that may have gone stale.
    pub(super) fn shared_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
    ) -> Result<Arc<HistoryEntry>, Error> {
        if let Some(entry) = self.entries.borrow_mut().entry(asset_id, entry_id) {
            return Ok(entry);
        }
        let entry = Arc::new(catalog::entry_from(&self.connection, asset_id, entry_id)?);
        if !entry.snapshot.recipe.strokes.has_missing() {
            self.entries
                .borrow_mut()
                .keep_entry(asset_id, entry.clone());
        }
        Ok(entry)
    }

    /// The cache's size, for tests that prove it is bounded.
    #[cfg(test)]
    pub(super) fn cached(&self) -> (usize, usize) {
        self.entries.borrow().held()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{
        EditorState, MutationOutcome,
        history::CommittedAction,
        read_counts,
        test_support::{brushed, commit, fixture, mutation, next_entry, stroke, temp},
    };
    use crate::{
        Draft, Snapshot, SnapshotId,
        mask::commands::{self, MaskTarget},
        modules::ActionInput,
        path::{Stroke, StrokeId},
    };
    use rusqlite::{Connection, params};
    use serde_json::{Map, Value, json};

    /// What a read with nothing cached returns: the head and the current entry from their rows.
    fn fresh(service: &EditorService, asset: &AssetId) -> EditorState {
        let head = catalog::head_from(&service.connection, asset).unwrap();
        let current_entry = catalog::entry_from(&service.connection, asset, &head.current).unwrap();
        EditorState {
            asset: head.asset,
            revision: head.revision,
            current_entry,
            redo: head.redo,
        }
    }

    /// The strokes an entry resolved, which the recipe's own equality leaves out.
    fn strokes(entry: &HistoryEntry) -> Vec<(StrokeId, Stroke)> {
        entry
            .snapshot
            .recipe
            .strokes
            .strokes()
            .map(|(id, stroke)| (id.clone(), stroke.clone()))
            .collect()
    }

    /// Every read the cache answers agrees with the rows: the state, the revision, and each entry of
    /// the asset's history with the strokes it resolves to. The state is read first, so the head is
    /// cached when the next write comes and that write's update is what the next check tests.
    fn assert_coherent(service: &EditorService, asset: &AssetId, step: &str) {
        let expected = fresh(service, asset);
        let state = service.state(asset).unwrap();
        assert_eq!(state, expected, "{step}: state");
        assert_eq!(
            strokes(&state.current_entry),
            strokes(&expected.current_entry),
            "{step}: current strokes"
        );
        assert_eq!(
            service.revision(asset).unwrap(),
            expected.revision,
            "{step}: revision"
        );
        for listed in service.history(asset, None, 100).unwrap().entries {
            let entry = service.entry(asset, &listed.id).unwrap();
            let stored = catalog::entry_from(&service.connection, asset, &listed.id).unwrap();
            assert_eq!(entry, stored, "{step}: entry {}", listed.sequence);
            assert_eq!(
                strokes(&entry),
                strokes(&stored),
                "{step}: entry {} strokes",
                listed.sequence
            );
        }
        // The state read again, after the history walk cycled the entry cache.
        assert_eq!(
            service.state(asset).unwrap(),
            expected,
            "{step}: state again"
        );
    }

    fn paint(
        service: &mut EditorService,
        asset: &AssetId,
        request: &str,
        target: MaskTarget,
        points: Value,
    ) {
        let revision = service.revision(asset).unwrap();
        service
            .apply_mask_command(
                asset,
                mutation(revision, request),
                commands::find(commands::ADD_STROKE).unwrap(),
                json!({"points": points, "size": 0.1, "feather": 50.0, "flow": 100.0, "erase": false}),
                target,
            )
            .unwrap();
    }

    /// Commit, undo, redo, restore, a no-op, a deduplicated retry, versions, a second import, a
    /// write that fails and a reopen, each followed by reads compared against the rows.
    #[test]
    fn every_write_keeps_the_cached_reads_equal_to_the_rows() {
        let dir = temp("entry-cache-coherence");
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = dir.join("catalog.sqlite");
        let second_source = dir.join("second.jpg");
        std::fs::copy(fixture(), &second_source).unwrap();
        let mut service = EditorService::open(&catalog).unwrap();

        let asset = service.import(&fixture()).unwrap().asset.id;
        assert_coherent(&service, &asset, "import");

        // Commits: an action, then two strokes a mask command stores and the entries resolve.
        let pixel = service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        assert_coherent(&service, &asset, "an action");
        paint(
            &mut service,
            &asset,
            "paint-1",
            MaskTarget::default(),
            json!([[0.2, 0.2], [0.4, 0.4]]),
        );
        assert_coherent(&service, &asset, "a first stroke");
        let recipe = service.state(&asset).unwrap().current_entry.snapshot.recipe;
        let brush = MaskTarget {
            mask: Some(recipe.masks[0].id.clone()),
            component: Some(recipe.masks[0].components[0].id.clone()),
            ..MaskTarget::default()
        };
        paint(
            &mut service,
            &asset,
            "paint-2",
            brush,
            json!([[0.6, 0.6], [0.7, 0.5]]),
        );
        assert_coherent(&service, &asset, "a further stroke");
        assert_eq!(
            service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .strokes
                .strokes()
                .count(),
            2
        );

        // Undo twice, then redo once, so the redo list moves both ways.
        for (step, request) in [("an undo", "undo-1"), ("a second undo", "undo-2")] {
            let revision = service.revision(&asset).unwrap();
            service.undo(&asset, mutation(revision, request)).unwrap();
            assert_coherent(&service, &asset, step);
        }
        let revision = service.revision(&asset).unwrap();
        service.redo(&asset, mutation(revision, "redo")).unwrap();
        assert_coherent(&service, &asset, "a redo");

        // Restore, then its deduplicated retry, then a no-op: nothing is left to redo.
        let before_restore = service.revision(&asset).unwrap();
        let restored = service
            .restore(
                &asset,
                mutation(before_restore, "restore"),
                &pixel.current_entry_id,
            )
            .unwrap();
        assert_coherent(&service, &asset, "a restore");
        let retried = service
            .restore(
                &asset,
                mutation(before_restore, "restore"),
                &pixel.current_entry_id,
            )
            .unwrap();
        assert!(retried.deduplicated);
        assert_eq!(retried.current_entry_id, restored.current_entry_id);
        assert_coherent(&service, &asset, "a deduplicated retry");
        let revision = service.revision(&asset).unwrap();
        let nothing = service
            .redo(&asset, mutation(revision, "nothing-to-redo"))
            .unwrap();
        assert_eq!(nothing.outcome, MutationOutcome::NoOp);
        assert_coherent(&service, &asset, "a no-op");

        // Versions name entries and move nothing.
        service
            .create_version(&asset, "Keeper", Some(&pixel.current_entry_id), "test")
            .unwrap();
        assert_coherent(&service, &asset, "a version");
        service.delete_version(&asset, "Keeper").unwrap();
        assert_coherent(&service, &asset, "a deleted version");

        // A second asset beside the first.
        let other = service.import(&second_source).unwrap().asset.id;
        assert_ne!(other, asset);
        assert_coherent(&service, &other, "a second import");
        assert_coherent(&service, &asset, "the first asset after a second import");
        service
            .apply_pixel(&other, mutation(0, "other-pixel"), 1, 1, [9, 9, 9])
            .unwrap();
        assert_coherent(&service, &other, "a commit to the second asset");
        assert_coherent(&service, &asset, "the first asset after the second moved");

        // A write that fails moves neither the rows nor the cache.
        service
            .connection
            .execute_batch(
                "CREATE TRIGGER injected_state_failure BEFORE UPDATE ON asset_state
                 BEGIN SELECT RAISE(ABORT, 'injected'); END;",
            )
            .unwrap();
        let before = service.state(&asset).unwrap();
        assert!(
            service
                .undo(&asset, mutation(before.revision, "fails"))
                .is_err()
        );
        service
            .connection
            .execute_batch("DROP TRIGGER injected_state_failure")
            .unwrap();
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_coherent(&service, &asset, "a failed write");

        // Reopen: a new service reads exactly what the cached one answered.
        let other_before = service.state(&other).unwrap();
        drop(service);
        let service = EditorService::open(&catalog).unwrap();
        assert_eq!(service.cached(), (0, 0), "a reopened service starts empty");
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_eq!(
            strokes(&service.state(&asset).unwrap().current_entry),
            strokes(&before.current_entry)
        );
        assert_eq!(service.state(&other).unwrap(), other_before);
        assert_coherent(&service, &asset, "a reopen");
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The acceptance check: after an asset's state and an entry have been read once, reading them
    /// again — and everything a slider tick reads, which is the state three times over — decodes no
    /// JSON and hashes no stroke. The revision a draft's conflict check compares decodes nothing
    /// even before anything is cached.
    #[test]
    fn a_read_after_the_first_decodes_and_hashes_nothing() {
        let catalog = temp("entry-cache-counts.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let painted = next_entry(
            &state,
            brushed(
                &state.current_entry.snapshot.recipe,
                &[stroke(1), stroke(2)],
            ),
        );
        drop(service);
        commit(&catalog, &painted);

        let service = EditorService::open(&catalog).unwrap();
        read_counts::take();
        assert_eq!(service.revision(&asset).unwrap(), painted.result_revision);
        assert_eq!(
            read_counts::take(),
            (0, 0),
            "a revision read with nothing cached"
        );

        let first = service.state(&asset).unwrap();
        // The asset's interpretation, its redo list and the entry, then each of the two strokes
        // parsed once and hashed twice: once against its address, once as the table keys it.
        assert_eq!(read_counts::take(), (5, 4), "the first read");
        assert_eq!(strokes(&first.current_entry).len(), 2);
        // Warm the source cache, which is not the catalog's, so what follows counts only reads.
        service.preview_job(&asset, None, None, None, None).unwrap();
        read_counts::take();

        let mut draft = Draft::new("set-basic", asset.clone(), first.revision);
        draft.merge(Map::from_iter([("exposure".to_owned(), json!(0.5))]));
        for _ in 0..3 {
            assert_eq!(service.state(&asset).unwrap(), first);
            assert_eq!(
                strokes(&service.state(&asset).unwrap().current_entry),
                strokes(&first.current_entry)
            );
            assert_eq!(
                service.entry(&asset, &painted.id).unwrap(),
                first.current_entry
            );
            assert_eq!(service.revision(&asset).unwrap(), first.revision);
            // A slider tick: the conflict check, then the draft's preview, which reads the state
            // and plans the draft against it.
            assert_eq!(service.revision(&asset).unwrap(), draft.base_revision);
            service
                .preview_job(&asset, None, None, Some(&draft), None)
                .unwrap();
            // The readout's samples, of the entry and of the draft.
            service.sample_entry(&asset, &painted.id, 3, 4).unwrap();
            service.sample_draft(&asset, &draft, 3, 4).unwrap();
            service.describe_entry(&asset, None).unwrap();
        }
        assert_eq!(read_counts::take(), (0, 0), "reads after the first");

        // Another entry is decoded once, then answered from the cache as well.
        let original = painted.undo_parent.clone().unwrap();
        service.entry(&asset, &original).unwrap();
        assert_eq!(read_counts::take(), (1, 0), "a first read of the original");
        service.entry(&asset, &original).unwrap();
        assert_eq!(read_counts::take(), (0, 0), "the original again");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Cloning a recipe shares its strokes, so a read that copies a cached entry out copies
    /// pointers and no stroke's positions.
    #[test]
    fn a_state_read_shares_the_cached_entrys_strokes() {
        let catalog = temp("entry-cache-shared-strokes.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let painted = next_entry(
            &state,
            brushed(&state.current_entry.snapshot.recipe, &[stroke(4)]),
        );
        drop(service);
        commit(&catalog, &painted);
        let service = EditorService::open(&catalog).unwrap();
        let one = service.state(&asset).unwrap();
        let two = service.state(&asset).unwrap();
        let cached = service.shared_entry(&asset, &painted.id).unwrap();
        for read in [&one.current_entry, &two.current_entry] {
            assert!(
                read.snapshot
                    .recipe
                    .strokes
                    .shares(&cached.snapshot.recipe.strokes)
            );
        }
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The cache is bounded: it keeps at most its declared entries and heads, and a read of one it
    /// let go reads the rows again and answers the same.
    #[test]
    fn the_cache_keeps_at_most_its_bounds() {
        let dir = temp("entry-cache-bounds");
        std::fs::create_dir_all(&dir).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let mut assets = Vec::new();
        for index in 0..CACHED_HEADS + 2 {
            let source = dir.join(format!("source-{index}.jpg"));
            std::fs::copy(fixture(), &source).unwrap();
            let asset = service.import(&source).unwrap().asset.id;
            service.state(&asset).unwrap();
            assets.push(asset);
        }
        assert_eq!(service.cached().1, CACHED_HEADS);
        let asset = assets[0].clone();
        let mut entries = vec![service.state(&asset).unwrap().current_entry];
        for index in 0..CACHED_ENTRIES + 2 {
            let revision = service.revision(&asset).unwrap();
            service
                .apply_pixel(
                    &asset,
                    mutation(revision, &format!("pixel-{index}")),
                    index as u32,
                    0,
                    [index as u8, 1, 2],
                )
                .unwrap();
            entries.push(service.state(&asset).unwrap().current_entry);
        }
        for entry in &entries {
            assert_eq!(service.entry(&asset, &entry.id).unwrap(), *entry);
        }
        assert_eq!(service.cached().0, CACHED_ENTRIES);
        read_counts::take();
        // The oldest entry was let go and is read from its row again, identically.
        assert_eq!(service.entry(&asset, &entries[0].id).unwrap(), entries[0]);
        assert_eq!(read_counts::take(), (1, 0));
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A stroke the store does not hold is the one resolution a later write can change — by storing
    /// the same content under the same address — so an entry holding one is read again each time
    /// rather than kept, and resolves the stroke once a commit has stored it.
    #[test]
    fn an_entry_missing_a_stroke_is_not_kept_and_resolves_once_the_stroke_is_stored() {
        let catalog = temp("entry-cache-missing-stroke.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let base = state.current_entry.snapshot.recipe.clone();
        let drawn = stroke(3);
        let lost = next_entry(&state, brushed(&base, std::slice::from_ref(&drawn)));
        drop(service);
        commit(&catalog, &lost);
        Connection::open(&catalog)
            .unwrap()
            .execute(
                "DELETE FROM strokes WHERE id=?1",
                params![drawn.id().as_str()],
            )
            .unwrap();

        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        assert!(
            state
                .current_entry
                .snapshot
                .recipe
                .strokes
                .get(&drawn.id())
                .is_none()
        );
        read_counts::take();
        service.entry(&asset, &lost.id).unwrap();
        assert_eq!(
            read_counts::take(),
            (1, 0),
            "read from its row again, not kept"
        );

        // Draw the same stroke again: the commit stores it under the address the lost entry names.
        service
            .commit_snapshot(
                &asset,
                mutation(state.revision, "again"),
                json!({"action": "again"}),
                Snapshot {
                    id: SnapshotId::new(),
                    asset_id: asset.clone(),
                    recipe: brushed(&base, std::slice::from_ref(&drawn)),
                },
                &state.asset,
                CommittedAction {
                    input: ActionInput {
                        action_id: "again".into(),
                        parameters: Map::new(),
                    },
                    label: "Again".into(),
                },
            )
            .unwrap();
        let resolved = service.entry(&asset, &lost.id).unwrap();
        assert_eq!(
            resolved.snapshot.recipe.strokes.get(&drawn.id()),
            Some(&drawn)
        );
        assert_eq!(
            strokes(&resolved),
            strokes(&catalog::entry_from(&service.connection, &asset, &lost.id).unwrap())
        );
        read_counts::take();
        service.entry(&asset, &lost.id).unwrap();
        assert_eq!(read_counts::take(), (0, 0), "complete now, and kept");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
