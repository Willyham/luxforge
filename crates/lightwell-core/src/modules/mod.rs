//! Tool modules: each one owns its descriptor, input parsing, state validation, no-op detection
//! and the compilation of its persisted payloads into host processing primitives. Modules never
//! write the catalog, never keep an undo stack and never render.
mod crop;
mod descriptor;
mod pixel;
mod processing;
mod registry;
mod transform;

pub use crop::CropModule;
pub use crop::geometry::{
    BoxRect, COVERAGE_TOLERANCE, CropPayload, CropStage, Edge, MAX_ANGLE, MIN_ANGLE, OutputRect,
    guide_angle, largest_with_ratio_inside,
};
pub use descriptor::{
    ActionDescriptor, Availability, CanvasInteraction, Control, EffectDescriptor, EffectStage,
    ModuleDescriptor, ParameterDescriptor, ParameterKind, ResetAction, action_label,
    check_parameters, render_summary, valid_identity, valid_name,
};
pub use pixel::PixelModule;
pub use processing::{ExactGeometry, Processing, Resample, Stage};
pub use registry::ModuleRegistry;
pub use transform::TransformModule;

use crate::{Error, Layer};
use serde_json::{Map, Value};

/// A normalized action request: the durable history action identity and the parameter object
/// stored on the history entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionInput {
    pub action_id: String,
    pub parameters: Map<String, Value>,
}

/// What an action does to the current stack. A no-op records the request without a history row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionPlan {
    NoOp,
    /// Append a new layer to the end of the stack.
    Commit(Layer),
    /// Replace the layer with the same identity in place, keeping its position and every other
    /// layer. The host rejects an identity that is not in the stack.
    Update(Layer),
}

/// The current output stage, the current ordered layers and a point sampler over the current
/// stack. The sampler evaluates one pixel without rasterizing, so planning an action never
/// allocates a frame.
pub struct StageContext<'a> {
    pub stage: Stage,
    /// The current recipe's layers in evaluation order, so a module can find its own layer to
    /// update. Planning never mutates them.
    pub layers: &'a [Layer],
    #[allow(clippy::type_complexity)]
    pub sampler: &'a dyn Fn(u32, u32) -> Result<Option<[u8; 4]>, Error>,
    /// The stage the layer at index `i` receives, which is the output stage of the layers before
    /// it; `layers.len()` is [`StageContext::stage`]. A module updating a layer in place plans
    /// against that layer's own input stage, not the final one. The host answers by compiling the
    /// recipe prefix, so this costs `O(layers)` and rasterizes nothing.
    #[allow(clippy::type_complexity)]
    pub stage_before: &'a dyn Fn(usize) -> Result<Stage, Error>,
}

pub trait ToolModule: Send + Sync {
    fn descriptor(&self) -> &ModuleDescriptor;
    /// Normalize an already schema-checked request into its durable action identity and stored
    /// parameters.
    fn parse(&self, action_id: &str, parameters: &Map<String, Value>)
    -> Result<ActionInput, Error>;
    /// Reject out-of-stage input and report no-ops against the current stack.
    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error>;
    /// Accept or reject a persisted payload structurally.
    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error>;
    /// One short line describing what this stored layer does, for the recipe row. Reading a
    /// payload only: it never renders, samples or touches the source.
    fn describe_layer(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<String, Error>;
    /// Turn a persisted payload into a host processing primitive at its input stage.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        stage: Stage,
    ) -> Result<Processing, Error>;
}
