//! The host's side of frozen patches (`docs/design/corrections.md`, "Frozen patches"): the input
//! identity a new top correction has now, which stack's patches are stale and why, and the commit
//! of a candidate the catalog owner has pinned and published. Everything here reads the recipe and
//! the asset's row and compiles the prefix before the repair layer; no pixel, file or prepared
//! source.
use super::EditorService;
use crate::{
    AcceptedCandidate, ActionResult, AssetId, AssetRecord, Error, Mutation, Recipe, RepairInput,
    StaleCorrection,
};
use serde_json::Value;
use std::sync::Arc;

impl EditorService {
    /// The input identity a new top correction of `asset_id`'s current stack has now: what a task's
    /// candidates are generated against.
    pub(crate) fn correction_input(&self, asset_id: &AssetId) -> Result<RepairInput, Error> {
        let state = self.state(asset_id)?;
        let recipe = &state.current_entry.snapshot.recipe;
        self.with_stage_context(&state.asset, recipe, None, |context| {
            crate::modules::corrections_top_input(context)
        })
    }

    /// The stale frozen patches of `recipe`, a stack of `asset`, in operation order; empty when it
    /// holds none. What `recipe.describe`, `export.plan`, `export.jpeg` and `batch.export` disclose.
    pub(crate) fn stale_corrections(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
    ) -> Result<Vec<StaleCorrection>, Error> {
        if !recipe
            .layers
            .iter()
            .any(|layer| layer.effect_id == crate::modules::CORRECTIONS_EFFECT && !layer.artifacts.is_empty())
        {
            return Ok(Vec::new());
        }
        self.with_stage_context(asset, recipe, None, crate::modules::stale_corrections)
    }

    /// Commit a candidate-accepting action ([`crate::ActionDescriptor::accepts_candidate`]) with
    /// `accepted`, the candidate the owner pinned and published and whose artifact it recorded: the
    /// ordinary action path, which checks the revision and plans the action against the current
    /// stack, while the plan reads `accepted` ([`crate::StageQuestions::accepting`]).
    pub(crate) fn accept_candidate(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        action_id: &str,
        parameters: Value,
        accepted: Arc<AcceptedCandidate>,
    ) -> Result<ActionResult, Error> {
        *self.accepting.borrow_mut() = Some(accepted);
        let result = self.run_action(asset_id, mutation, action_id, parameters);
        *self.accepting.borrow_mut() = None;
        result
    }
}

/// What an export acknowledges it writes stale: the entry it freezes and exactly that entry's stale
/// frozen patches (`export.jpeg`'s and `batch.export`'s `acknowledge_stale`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaleAcknowledgement {
    pub entry_id: crate::EntryId,
    /// `correction-<n>` identities.
    pub operations: Vec<String>,
}

/// One photograph's acknowledgement in a batch export.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchStaleAcknowledgement {
    pub asset_id: AssetId,
    pub entry_id: crate::EntryId,
    pub operations: Vec<String>,
}

/// The code a stale export without its acknowledgement is refused, or skipped, with.
pub(crate) const STALE_UNACKNOWLEDGED: &str = "stale-unacknowledged";

/// Hold an export of `entry`, whose stale patches are `stale`, to `acknowledged`: with nothing stale
/// any acknowledgement is accepted and ignored; otherwise it must name exactly that entry and
/// exactly its stale operations, or the export is a `conflict` with `data: {code:
/// stale-unacknowledged, entry_id, stale}`. Answers what was acknowledged, which an accepted export
/// echoes.
pub(crate) fn acknowledged(
    entry: &crate::EntryId,
    stale: Vec<StaleCorrection>,
    acknowledgement: Option<(&crate::EntryId, &[String])>,
) -> Result<Vec<StaleCorrection>, Error> {
    if stale.is_empty() {
        return Ok(stale);
    }
    let mut named: Vec<&str> = stale.iter().map(|row| row.operation.as_str()).collect();
    named.sort_unstable();
    let matches = acknowledgement.is_some_and(|(acknowledged, operations)| {
        let mut sent: Vec<&str> = operations.iter().map(String::as_str).collect();
        sent.sort_unstable();
        sent.dedup();
        acknowledged == entry && sent == named
    });
    if matches {
        return Ok(stale);
    }
    let names: Vec<String> = stale.iter().map(|row| row.name.clone()).collect();
    Err(Error::conflict(format!(
        "{} {} stale; acknowledge exactly the entry and its stale corrections to export it",
        names.join(", "),
        if names.len() == 1 { "is" } else { "are" }
    ))
    .with_data(serde_json::json!({
        "code": STALE_UNACKNOWLEDGED,
        "entry_id": entry,
        "stale": stale,
    })))
}

impl EditorService {
    /// The entry an export of `asset_id` freezes — `entry_id`, or the current entry now — and its
    /// stale frozen patches. Reads the head and that entry; no pixel or source.
    pub(crate) fn export_stale(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&crate::EntryId>,
    ) -> Result<(crate::EntryId, Vec<StaleCorrection>), Error> {
        let head = self.head(asset_id)?;
        let entry_id = entry_id.cloned().unwrap_or(head.current);
        let entry = self.shared_entry(asset_id, &entry_id)?;
        let stale = self.stale_corrections(&head.asset, &entry.snapshot.recipe)?;
        Ok((entry_id, stale))
    }
}
