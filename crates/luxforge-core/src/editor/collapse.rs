//! Auto-collapse: an edit that sets the same control as the entry before it replaces that entry in
//! history rather than following it.
//!
//! A control's action stores the value it set, not a change to the value before, so a chain of
//! them — Contrast +15, −30, +30, +10 — leaves exactly the stack its last edit would have left on
//! the entry before the chain. The new entry continues from that base and the entry it superseded
//! is recorded as collapsed, which history pages leave out; an edit that returns the control to
//! the base's value writes no entry at all and moves the head back to the base. Nothing is
//! deleted: every collapsed entry stays in the catalog, readable by `history.inspect` and listed
//! by `history.list` when asked for collapsed entries.
//!
//! "Same control" is decided from the stacks, not assumed from the action: the edit must be the
//! same action over the same fields by the same actor, and planning it on the base must produce
//! the stack planning it on the current entry produces, neutral layers aside. That proves the
//! superseded entry changed nothing the new edit does not overwrite, so a relative action (a
//! quarter-turn), the same field on another mask, or a layer a patch merged into never collapses.
use super::{
    EditorService, EditorState,
    history::Collapse,
    plan::{PlannedRequest, Prepared},
};
use crate::{Error, Layer, Recipe, modules::ActionRef};
use rusqlite::params;

/// What collapsing an edit into the current entry decided: the entry it hides and the base the
/// chain began from, and whether the edit left the base's stack as it was.
pub(super) struct Collapsing {
    pub(super) collapse: Collapse,
    /// The edit planned on the base changes nothing: the control is back where the chain began.
    pub(super) returned: bool,
}

impl EditorService {
    /// Whether committing `prepared`, which planned `next` on the current entry, collapses that
    /// entry, and how. `None` unless auto-collapse is on and the current entry is an edit of the
    /// same control the head has not moved from since it was written, which no version names.
    ///
    /// Costs one more plan of the action, on the base's stack, and a comparison of two stacks,
    /// `O(layers)`; the base is read from the entry cache. Nothing is rasterized.
    pub(super) fn collapse(
        &self,
        state: &EditorState,
        prepared: &Prepared<'_>,
        actor: &str,
        next: &Recipe,
    ) -> Result<Option<Collapsing>, Error> {
        let current = &state.current_entry;
        if !self.auto_collapse
            || !matches!(prepared.action, ActionRef::Module(..))
            || current.result_revision != state.revision
            || current.restore_target.is_some()
            || current.actor != actor
            || current.action_id != prepared.input.action_id
            || prepared.input.parameters.is_empty()
            || !same_fields(&current.parameters, &prepared.input.parameters)
        {
            return Ok(None);
        }
        let Some(base_id) = &current.undo_parent else {
            return Ok(None);
        };
        if self.names_version(&current.id)? {
            return Ok(None);
        }
        let base = self.shared_entry(&state.asset.id, base_id)?;
        let PlannedRequest { planned, skipped } =
            self.plan_request(&state.asset, &base.snapshot.recipe, prepared)?;
        if !skipped.is_empty() {
            return Ok(None);
        }
        let on_base = planned
            .as_ref()
            .map_or(&base.snapshot.recipe, |p| &p.recipe);
        if !self.same_effect(on_base, next) {
            return Ok(None);
        }
        Ok(Some(Collapsing {
            collapse: Collapse {
                entry: current.id.clone(),
                base: base_id.clone(),
            },
            returned: planned.is_none(),
        }))
    }

    /// Whether a version names this entry: a named entry is a state the person kept, so it is
    /// never collapsed.
    fn names_version(&self, entry_id: &crate::EntryId) -> Result<bool, Error> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM versions WHERE entry_id=?1)",
            params![entry_id.as_str()],
            |row| row.get(0),
        )?)
    }

    /// Whether two stacks evaluate alike: the same format and mask table, and the same layers in
    /// the same order once neutral layers are left out, whatever each layer's identity. A layer
    /// whose provider cannot describe it is not neutral.
    fn same_effect(&self, a: &Recipe, b: &Recipe) -> bool {
        if a.format != b.format || a.masks != b.masks {
            return false;
        }
        if a.layers.len() == b.layers.len() && a.layers.iter().zip(&b.layers).all(same_layer) {
            return true;
        }
        let neutral = |layer: &Layer| {
            self.registry
                .layer_report(layer)
                .is_ok_and(|report| report.neutral)
        };
        let mut a = a.layers.iter().filter(|layer| !neutral(layer));
        let mut b = b.layers.iter().filter(|layer| !neutral(layer));
        loop {
            match (a.next(), b.next()) {
                (None, None) => return true,
                (Some(a), Some(b)) if same_layer((a, b)) => {}
                _ => return false,
            }
        }
    }
}

/// Two layers that apply the same effect: everything but their identity.
fn same_layer((a, b): (&Layer, &Layer)) -> bool {
    a.effect_id == b.effect_id
        && a.effect_format == b.effect_format
        && a.payload == b.payload
        && a.mask == b.mask
        && a.artifacts == b.artifacts
}

/// Whether an entry's stored parameters name exactly the fields of this request's.
fn same_fields(
    stored: &serde_json::Value,
    sent: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    stored.as_object().is_some_and(|stored| {
        stored.len() == sent.len() && stored.keys().all(|k| sent.contains_key(k))
    })
}

#[cfg(test)]
mod tests {
    use crate::editor::test_support::{fixture, mutation, temp};
    use crate::{
        AssetId, EditorService, EntryId, Mutation, MutationOutcome, MutationResult, Transform,
    };
    use serde_json::{Value, json};
    use std::path::PathBuf;

    /// A collapsing service over a fresh catalog with the JPEG fixture imported.
    fn collapsing(name: &str) -> (EditorService, AssetId, PathBuf) {
        let catalog = temp(name);
        let mut service = EditorService::open(&catalog).unwrap();
        service.set_auto_collapse(true);
        let asset = service.import(&fixture()).unwrap().asset.id;
        (service, asset, catalog)
    }

    fn by(actor: &str, service: &EditorService, asset: &AssetId, request: &str) -> Mutation {
        Mutation {
            actor: actor.into(),
            ..mutation(service.state(asset).unwrap().revision, request)
        }
    }

    fn set(
        service: &mut EditorService,
        asset: &AssetId,
        request: &str,
        fields: Value,
    ) -> MutationResult {
        let mutation = by("test", service, asset, request);
        service
            .apply_action(asset, mutation, "set-basic", fields)
            .unwrap()
    }

    /// The visible rows' labels, newest first.
    fn labels(service: &EditorService, asset: &AssetId) -> Vec<String> {
        let page = service.history(asset, None, 100).unwrap();
        page.entries.into_iter().map(|row| row.label).collect()
    }

    fn current(service: &EditorService, asset: &AssetId) -> EntryId {
        service.state(asset).unwrap().current_entry.id
    }

    #[test]
    fn a_chain_of_one_control_keeps_one_entry_that_continues_from_the_chains_base() {
        let (mut service, asset, catalog) = collapsing("collapse-chain.sqlite");
        let exposure = set(&mut service, &asset, "e", json!({"exposure": 0.5})).current_entry_id;
        let mut hidden = Vec::new();
        let mut previous = None;
        for (index, value) in [15.0, -30.0, 30.0, 10.0].into_iter().enumerate() {
            let result = set(
                &mut service,
                &asset,
                &format!("c{index}"),
                json!({"contrast": value}),
            );
            assert_eq!(result.outcome, MutationOutcome::Applied);
            assert_eq!(result.collapsed_entry_id, previous, "edit {index}");
            hidden.extend(previous.take());
            previous = result.created_entry_id;
        }
        assert_eq!(
            labels(&service, &asset),
            ["Contrast +10", "Exposure +0.50 EV", "Original"]
        );
        let state = service.state(&asset).unwrap();
        assert_eq!(state.current_entry.undo_parent.as_ref(), Some(&exposure));
        let payload = &state.current_entry.snapshot.recipe.layers[0].payload;
        assert_eq!(
            (payload["exposure"].as_f64(), payload["contrast"].as_f64()),
            (Some(0.5), Some(10.0))
        );

        // Every collapsed entry is kept, marked and readable; a page counts only what it shows.
        let all = service.history_rows(&asset, None, 100, true).unwrap();
        assert_eq!(all.entries.len(), 6);
        let marked: Vec<EntryId> = all
            .entries
            .iter()
            .filter(|row| row.collapsed)
            .map(|row| row.id.clone())
            .collect();
        assert_eq!(marked, hidden.iter().rev().cloned().collect::<Vec<_>>());
        assert_eq!(
            service.entry(&asset, &hidden[0]).unwrap().label,
            "Contrast +15"
        );
        let page = service.history(&asset, None, 2).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(
            service
                .history(&asset, page.next_before_sequence, 2)
                .unwrap()
                .entries
                .len(),
            1
        );

        // Undo steps over the chain to its base; the lineage never names a collapsed entry.
        service
            .undo(&asset, by("test", &service, &asset, "undo"))
            .unwrap();
        assert_eq!(current(&service, &asset), exposure);
        service
            .redo(&asset, by("test", &service, &asset, "redo"))
            .unwrap();
        let lineage = service.lineage(&asset, None, 100).unwrap();
        assert!(
            lineage
                .steps
                .iter()
                .all(|step| !hidden.contains(&step.entry_id))
        );
        assert_eq!(lineage.steps.len(), 3);

        // Reopened, the catalog still hides them.
        drop(service);
        let service = EditorService::open(&catalog).unwrap();
        assert_eq!(labels(&service, &asset).len(), 3);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_chain_that_returns_to_its_start_writes_nothing_and_leaves_no_row() {
        let (mut service, asset, catalog) = collapsing("collapse-return.sqlite");
        let original = current(&service, &asset);
        // From the Original, which holds no Basic layer: the chain's neutral layer is no change.
        set(&mut service, &asset, "a", json!({"contrast": 15.0}));
        let second = set(&mut service, &asset, "b", json!({"contrast": -30.0}));
        let revision = service.state(&asset).unwrap().revision;
        let back = set(&mut service, &asset, "c", json!({"contrast": 0.0}));
        assert_eq!(back.outcome, MutationOutcome::Applied);
        assert_eq!(back.created_entry_id, None);
        assert_eq!(back.current_entry_id, original);
        assert_eq!(back.collapsed_entry_id, second.created_entry_id);
        assert_eq!(back.revision, revision + 1);
        let state = service.state(&asset).unwrap();
        assert_eq!(state.current_entry.id, original);
        assert!(state.redo.is_empty(), "an edit leaves nothing to redo");
        assert_eq!(labels(&service, &asset), ["Original"]);
        assert_eq!(
            service
                .history_rows(&asset, None, 100, true)
                .unwrap()
                .entries
                .len(),
            3
        );

        // From an entry with a Basic layer: back to its own value of the control.
        let base = set(&mut service, &asset, "e", json!({"contrast": 20.0})).current_entry_id;
        set(&mut service, &asset, "f", json!({"exposure": 1.0}));
        set(&mut service, &asset, "g", json!({"exposure": 0.5}));
        let back = set(&mut service, &asset, "h", json!({"exposure": 0.0}));
        assert_eq!(back.current_entry_id, base);
        assert_eq!(labels(&service, &asset), ["Contrast +20", "Original"]);
        // The base is not an edit the head stopped on, so the next edit follows it.
        let next = set(&mut service, &asset, "i", json!({"contrast": 25.0}));
        assert_eq!(next.collapsed_entry_id, None);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn only_the_same_control_on_the_same_target_by_the_same_actor_collapses() {
        let (mut service, asset, catalog) = collapsing("collapse-same.sqlite");
        // Another control between two edits of one.
        set(&mut service, &asset, "a", json!({"contrast": 15.0}));
        set(&mut service, &asset, "b", json!({"exposure": 0.5}));
        assert_eq!(
            set(&mut service, &asset, "c", json!({"contrast": 10.0})).collapsed_entry_id,
            None
        );
        // Another set of fields on the same action.
        let both = set(
            &mut service,
            &asset,
            "d",
            json!({"contrast": 5.0, "exposure": 0.25}),
        );
        assert_eq!(both.collapsed_entry_id, None);
        // Another actor.
        let mutation = by("agent", &service, &asset, "e");
        let theirs = service
            .apply_action(
                &asset,
                mutation,
                "set-basic",
                json!({"contrast": 5.0, "exposure": 0.0}),
            )
            .unwrap();
        assert_eq!(theirs.collapsed_entry_id, None);
        let rows = labels(&service, &asset).len();

        // The same field on a mask and then everywhere: the stacks differ, so nothing collapses.
        let mutation = by("test", &service, &asset, "mask");
        let mask = service
            .run_action(
                &asset,
                mutation,
                "mask.create-linear",
                json!({"x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 1.0}),
            )
            .unwrap()
            .mask
            .unwrap();
        set(
            &mut service,
            &asset,
            "m",
            json!({"highlights": 20.0, "mask": mask}),
        );
        assert_eq!(
            set(&mut service, &asset, "g", json!({"highlights": 20.0})).collapsed_entry_id,
            None
        );
        // The same field on the same mask does.
        set(
            &mut service,
            &asset,
            "m2",
            json!({"shadows": 20.0, "mask": mask}),
        );
        let masked = set(
            &mut service,
            &asset,
            "m3",
            json!({"shadows": -20.0, "mask": mask}),
        );
        assert!(masked.collapsed_entry_id.is_some());
        assert_eq!(labels(&service, &asset).len(), rows + 4);

        // A relative action never collapses: two quarter-turns are two turns.
        for turn in ["r1", "r2"] {
            let mutation = by("test", &service, &asset, turn);
            let turned = service
                .apply_transform(&asset, mutation, Transform::RotateRight)
                .unwrap();
            assert_eq!(turned.collapsed_entry_id, None);
        }
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_moved_head_a_named_version_or_the_preference_off_keeps_every_entry() {
        let (mut service, asset, catalog) = collapsing("collapse-kept.sqlite");
        // Undo and redo back to an entry: the head moved since it was written.
        set(&mut service, &asset, "a", json!({"contrast": 15.0}));
        service
            .undo(&asset, by("test", &service, &asset, "undo"))
            .unwrap();
        service
            .redo(&asset, by("test", &service, &asset, "redo"))
            .unwrap();
        assert_eq!(
            set(&mut service, &asset, "b", json!({"contrast": 20.0})).collapsed_entry_id,
            None
        );
        // A version names the current entry.
        service
            .create_version(&asset, "Kept", None, "test")
            .unwrap();
        assert_eq!(
            set(&mut service, &asset, "c", json!({"contrast": 25.0})).collapsed_entry_id,
            None
        );
        // Off.
        service.set_auto_collapse(false);
        assert_eq!(
            set(&mut service, &asset, "d", json!({"contrast": 30.0})).collapsed_entry_id,
            None
        );
        assert_eq!(labels(&service, &asset).len(), 5);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_retried_collapse_answers_as_it_did_and_hides_nothing_more() {
        let (mut service, asset, catalog) = collapsing("collapse-retry.sqlite");
        set(&mut service, &asset, "a", json!({"contrast": 15.0}));
        let mutation = by("test", &service, &asset, "b");
        let first = service
            .apply_action(
                &asset,
                mutation.clone(),
                "set-basic",
                json!({"contrast": 0.0}),
            )
            .unwrap();
        let again = service
            .apply_action(&asset, mutation, "set-basic", json!({"contrast": 0.0}))
            .unwrap();
        assert!(again.deduplicated);
        assert_eq!(
            MutationResult {
                deduplicated: false,
                ..again
            },
            first
        );
        assert_eq!(
            service
                .history_rows(&asset, None, 100, true)
                .unwrap()
                .entries
                .len(),
            2
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
