//! Tool modules: each one owns its descriptor, input parsing, state validation, no-op detection
//! and the compilation of its persisted payloads into host processing primitives. Modules never
//! write the catalog, never keep an undo stack and never render.
mod crop;
mod descriptor;
mod pixel;
mod processing;
mod raw;
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
pub use raw::neutral::{SensorMosaic, sensor_neutral_gains};
pub use raw::white_balance::gains_from_temperature_tint;
pub use raw::{RawModule, RawPayload, WhiteBalanceMode};
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
    /// Add a new layer to the stack. The host, not the module, chooses its position from the
    /// effect's declared stage: a pixel-stage layer joins the stack before the geometry tail, a
    /// geometry-stage layer extends that tail. [`StageContext::insertion_index`] answers where.
    Commit(Layer),
    /// Replace the layer with the same identity in place, keeping its position and every other
    /// layer. The host rejects an identity that is not in the stack.
    Update(Layer),
}

/// What a module may ask about the current stack while planning: the output stage, the ordered
/// layers, the stage any position receives, where a commit of a given stage would land, and point
/// samplers over the whole stack or over any prefix of it. Every sampler evaluates one pixel
/// without rasterizing, so planning an action never allocates a frame.
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
    /// Where the host would put a [`ActionPlan::Commit`] of a layer with this effect stage: the
    /// index of the first geometry-stage layer for a pixel-stage effect, `layers.len()` for a
    /// geometry-stage one. A module plans against that position instead of choosing one, so
    /// `stage_before` of this index is the stage its coordinates address.
    #[allow(clippy::type_complexity)]
    pub insertion_index: &'a dyn Fn(EffectStage) -> usize,
    /// One pixel of the stage the first `index` layers produce, or `None` outside that stage.
    /// Evaluated segment by segment like [`StageContext::sampler`], so a module that plans against
    /// an insertion stage still allocates no frame.
    #[allow(clippy::type_complexity)]
    pub sample_before: &'a dyn Fn(usize, u32, u32) -> Result<Option<[u8; 4]>, Error>,
    /// A bounded pre-WB sensor patch at upright content coordinates, only for RAW sources.
    #[allow(clippy::type_complexity)]
    pub sensor_neutral: Option<&'a dyn Fn(u32, u32) -> Result<[f32; 3], Error>>,
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
