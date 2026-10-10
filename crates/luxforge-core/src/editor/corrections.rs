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
