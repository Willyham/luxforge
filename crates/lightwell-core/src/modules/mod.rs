//! Tool modules: each one owns its descriptor, input parsing, state validation, no-op detection
//! and the compilation of its persisted payloads into host processing primitives. Modules never
//! write the catalog, never keep an undo stack and never render.
mod basic;
mod controls;
mod crop;
mod descriptor;
mod mixer;
mod pixel;
mod presence;
mod presets;
mod processing;
mod raw;
mod registry;
mod spatial;
mod transform;
mod vignette;

pub use basic::BasicModule;
pub use controls::{
    CONTROLS_EFFECT, ControlsModule, RESET_CONTROLS, SAMPLE_CONTROLS_CURVE, SET_CONTROLS,
};
pub use crop::CropModule;
pub use crop::geometry::{
    BoxRect, COVERAGE_TOLERANCE, CropPayload, CropStage, Edge, MAX_ANGLE, MIN_ANGLE, OutputRect,
    guide_angle, largest_with_ratio_inside,
};
pub use descriptor::{
    ActionDescriptor, ActionStyle, Availability, CanvasInteraction, ChoiceStyle, ColorStyle,
    Control, CurveBackground, CurveChannel, EffectDescriptor, EffectStage, MAX_SETTINGS_ACTIONS,
    MAX_SETTINGS_FIELDS, ModuleDescriptor, ModuleLayout, NumberStyle, ParameterDescriptor,
    ParameterKind, RailDecoration, ResetAction, action_label, check_parameters, check_value,
    render_summary, valid_identity, valid_name,
};
pub use mixer::MixerModule;
pub use pixel::PixelModule;
pub use presence::PresenceModule;
pub use presets::{APPLY_PRESET, MAX_PRESET_NAME, PresetsModule};
pub use processing::{
    ColorOperation, ExactGeometry, MAX_COLOR_UNITS, PointwiseColor, Processing, Resample, Stage,
};
pub use raw::neutral::{SensorMosaic, sensor_neutral_gains, sensor_neutral_gains_mapped};
pub use raw::white_balance::gains_from_temperature_tint;
pub use raw::{RawModule, RawPayload, WhiteBalanceMode};
pub use registry::ModuleRegistry;
#[cfg(test)]
pub(crate) use registry::tests::{
    PATCH_ACTION, PATCH_MODULE, PatchModule, STAGE_ACTION, STAGE_EFFECT, StageModule, TestModule,
};
pub use spatial::{
    ESTIMATE_REDUCTION, ESTIMATE_STORE_ENTRIES, Global, MAX_GLOBAL_BYTES, MAX_GLOBAL_VALUES,
    MAX_REDUCTION_PIXELS, MAX_SPATIAL_HALO, MAX_SPATIAL_UNITS, Planes, PlanesMut, Reduction,
    Region, SPATIAL_BUDGET_BYTES, SPATIAL_TILE, SpatialOperation, SpatialUnit,
};
pub use transform::TransformModule;
pub use vignette::VignetteModule;

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
    /// effect's declared stage and order: a pixel-stage or colour-stage layer joins the stack
    /// before the geometry tail, a spatial layer after the pointwise work, a geometry layer before
    /// any finish layer and a finish layer at the end.
    /// [`StageContext::insertion_index_for`] answers where.
    Commit(Layer),
    /// Replace the layer with the same identity in place, keeping its position and every other
    /// layer. The host rejects an identity that is not in the stack.
    Update(Layer),
    /// Apply these field-patch actions, in order, as this one action: a preset is one. The host runs
    /// each step through the registry against the stack the steps before it produced, exactly as it
    /// would run that action alone, and commits the final stack once as one entry that stores this
    /// action's identity, label and parameters. At most [`MAX_COMPOSE_STEPS`] steps; a step's own
    /// plan may not be a composite.
    Compose(Vec<ActionInput>),
}

/// The most steps one [`ActionPlan::Compose`] may hold, which is the most actions a settings set
/// names.
pub const MAX_COMPOSE_STEPS: usize = MAX_SETTINGS_ACTIONS;

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
    /// Where the host would put a [`ActionPlan::Commit`] of a layer of this effect stage that
    /// declares the default order, by the placement rule in
    /// [`crate::ModuleRegistry::insertion_index`]. A module plans against that position instead of
    /// choosing one, so `stage_before` of this index is the stage its coordinates address.
    #[allow(clippy::type_complexity)]
    pub insertion_index: &'a dyn Fn(EffectStage) -> usize,
    /// Where the host would put a [`ActionPlan::Commit`] of a layer of this effect: the same rule
    /// read from the effect's own descriptor, so a module that declares an order among the layers
    /// of its stage plans against the position its layer will actually take.
    #[allow(clippy::type_complexity)]
    pub insertion_index_for: &'a dyn Fn(&str) -> usize,
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
    /// Whether a stack may hold at most one layer of this effect. The host refuses to compile a
    /// stack that holds two of them, because a module that owns exactly one layer cannot say which
    /// one an action or a payload belongs to; nothing is rewritten and the stack stays readable.
    /// The default is `false`, so a module says so only when one layer is its contract.
    fn single_layer(&self, effect_id: &str) -> bool {
        let _ = effect_id;
        false
    }
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
    /// The history label this request deserves, when the rendered `summary` template cannot say it:
    /// a patch naming the one field it changed, or a reset naming the group it cleared. The host
    /// consults this before the template and the title. Reading the request only.
    fn label(&self, input: &ActionInput) -> Option<String> {
        let _ = input;
        None
    }
    /// The parameter values a stored layer represents, reported on the layer's row of
    /// `recipe.describe` so a client can seed its controls from the displayed entry. Reading a
    /// payload only, like [`ToolModule::describe_layer`]: no render, no sample, no source.
    fn values(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let _ = (effect_id, format, payload);
        Ok(Map::new())
    }
    /// Answer one declared read-only query about the current stack.
    ///
    /// The host has already checked `parameters` against the query's declared parameters, and hands
    /// the same [`StageContext`] an action is planned against: point samplers only, so a query
    /// allocates no frame and runs no work on the catalog owner beyond `O(layers)` per sampled
    /// point. A query mutates nothing, writes no history entry and emits no event; a module that
    /// declares none never sees this call.
    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        let _ = (parameters, context);
        Err(Error::new(
            crate::ErrorKind::Validation,
            format!(
                "module {} declares no queries, so it cannot answer {query_id}",
                self.descriptor().id
            ),
        ))
    }
    /// Turn a persisted payload into a host processing primitive at its input stage.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        stage: Stage,
    ) -> Result<Processing, Error>;
}
