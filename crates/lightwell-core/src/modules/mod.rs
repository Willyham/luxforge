//! Tool modules: each one owns its descriptor, input parsing, state validation, no-op detection
//! and the compilation of its persisted payloads into host processing primitives. Modules never
//! write the catalog, never keep an undo stack and never render.
pub(crate) mod basic;
mod capabilities_proof;
mod controls;
mod crop;
mod descriptor;
mod field_patch;
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

pub use basic::{BASIC_EFFECT, BasicModule};
pub use capabilities_proof::{
    APPLY_PROOF_TINT, CapabilitiesProofModule, PROOF_ADAPTER, PROOF_EFFECT, PROOF_GENERATE_PATH,
    PROOF_MODULE, PROOF_PALETTE, PROOF_PALETTE_GAINS, PROOF_PALETTE_PATH, PROOF_PALETTE_SHA256,
    PROOF_RESOURCE, PROOF_RESOURCE_VERSION, PROOF_TASK, PROOF_TINT_KIND, RESET_PROOF_TINT,
    palette_bytes,
};
pub use controls::{
    CONTROLS_EFFECT, ControlsModule, RESET_CONTROLS, SAMPLE_CONTROLS_CURVE, SET_CONTROLS,
};
pub use crop::geometry::{
    BoxRect, COVERAGE_TOLERANCE, CropPayload, CropStage, Edge, MAX_ANGLE, MIN_ANGLE, OutputRect,
    guide_angle, largest_with_ratio_inside,
};
pub use crop::{CROP_EFFECT, CropModule};
pub(crate) use descriptor::title_case;
pub use descriptor::{
    ActionDescriptor, ActionStyle, Availability, CanvasInteraction, ChoiceStyle, ColorStyle,
    Control, CurveBackground, CurveChannel, EffectDescriptor, EffectStage, MAX_COORDINATE,
    MAX_ENDPOINT_BYTES, MAX_SECRET_LENGTH, MAX_SETTINGS_ACTIONS, MAX_SETTINGS_FIELDS,
    ModuleDescriptor, ModuleLayout, NumberStyle, ParameterDescriptor, ParameterKind,
    RailDecoration, ResetAction, action_label, check_parameters, check_value, render_summary,
    valid_identity, valid_name,
};
pub(crate) use descriptor::{
    check_declaration, check_declared_values, check_parameter_declarations,
};
pub use mixer::{MIXER_EFFECT, MixerModule};
pub use pixel::{PIXEL_EFFECT, PixelModule};
pub use presence::{PRESENCE_EFFECT, PresenceModule};
pub use presets::{APPLY_PRESET, MAX_PRESET_NAME, PresetsModule};
pub use processing::{
    ColorOperation, ExactGeometry, MAX_COLOR_UNITS, PointwiseColor, Processing, Resample, Stage,
};
pub use raw::neutral::{SensorMosaic, sensor_neutral_gains, sensor_neutral_gains_mapped};
pub use raw::white_balance::{gains_from_temperature_tint, temperature_tint_from_gains};
pub use raw::{RAW_EFFECT, RawModule, RawPayload, WhiteBalanceMode};
#[cfg(test)]
pub(crate) use registry::tests::{
    HELD_ACTION, HELD_EFFECT, HeldModule, PATCH_ACTION, PATCH_MODULE, PatchModule, RenderGate,
    STAGE_ACTION, STAGE_EFFECT, StageModule, TestModule,
};
pub use registry::{ModuleRegistry, builtin_modules, insertion_index_among};
pub use spatial::{
    ESTIMATE_REDUCTION, ESTIMATE_STORE_ENTRIES, Global, MAX_GLOBAL_BYTES, MAX_GLOBAL_VALUES,
    MAX_MASKED_SPATIAL_LAYERS, MAX_REDUCTION_PIXELS, MAX_SPATIAL_HALO, MAX_SPATIAL_UNITS,
    Parallelism, Planes, PlanesMut, Reduction, Region, SPATIAL_BUDGET_BYTES, SPATIAL_TILE,
    SpatialOperation, SpatialUnit,
};
pub use transform::{ORIENTATION_EFFECT, TransformModule};
pub use vignette::{VIGNETTE_EFFECT, VignetteModule};

use crate::{
    ArtifactId, Error, Layer, LayerId, MaskId, artifacts::PreparedArtifact,
    capabilities::context::ModuleContext,
};
use serde_json::{Map, Value};
use std::{path::Path, sync::Arc};

/// A normalized action request: the durable history action identity and the parameter object
/// stored on the history entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionInput {
    pub action_id: String,
    pub parameters: Map<String, Value>,
}

/// A layer a plan adds: which effect, and what its payload holds. Everything else about the layer is
/// the host's: it gives the layer a new identity, the format its effect declares and the mask target
/// the request named, and places it by the effect's declared stage and order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewLayer {
    pub effect_id: String,
    pub payload: Value,
    /// The derived artifacts the payload is evaluated with, only for an effect that declares
    /// `artifacts`; empty otherwise.
    pub artifacts: Vec<ArtifactId>,
}

impl NewLayer {
    pub fn new(effect_id: impl Into<String>, payload: Value) -> Self {
        Self {
            effect_id: effect_id.into(),
            payload,
            artifacts: Vec::new(),
        }
    }

    pub fn with_artifacts(self, artifacts: Vec<ArtifactId>) -> Self {
        Self { artifacts, ..self }
    }
}

/// A change a plan makes to a layer already in the stack: which layer, and its new payload. The
/// layer keeps its identity, effect, position and mask; the host writes its effect's declared
/// format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerUpdate {
    pub id: LayerId,
    pub payload: Value,
    /// The layer's derived artifacts after the change, only for an effect that declares
    /// `artifacts`; empty otherwise, which also clears any the layer listed.
    pub artifacts: Vec<ArtifactId>,
}

impl LayerUpdate {
    pub fn new(id: LayerId, payload: Value) -> Self {
        Self {
            id,
            payload,
            artifacts: Vec::new(),
        }
    }

    pub fn with_artifacts(self, artifacts: Vec<ArtifactId>) -> Self {
        Self { artifacts, ..self }
    }
}

/// What an action does to the current stack. A no-op records the request without a history row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionPlan {
    NoOp,
    /// Add a new layer to the stack. The host, not the module, chooses its position from the
    /// effect's declared stage and order: a pixel-stage or colour-stage layer joins the stack
    /// before the geometry tail, a spatial layer after the pointwise work, a geometry layer before
    /// any finish layer and a finish layer at the end.
    /// [`StageContext::insertion_index_for`] answers where. The host also gives it its identity,
    /// its effect's format and the request's mask target.
    Commit(NewLayer),
    /// Replace the payload of the layer with this identity in place, keeping its position, effect
    /// and mask and every other layer. The host rejects an identity that is not in the stack.
    Update(LayerUpdate),
    /// Change several layers as this one action, in order, each by the rule of the single-layer
    /// plan it names, and commit the final stack once. A transform over a crop is one: its
    /// orientation goes ahead of the crop, and the crop is re-expressed through it in the same
    /// entry, so the output is the transform applied to what the stack showed. Never empty.
    Edits(Vec<LayerEdit>),
    /// Apply these field-patch actions, in order, as this one action: a preset is one. The host runs
    /// each step through the registry against the stack the steps before it produced, exactly as it
    /// would run that action alone, and commits the final stack once as one entry that stores this
    /// action's identity, label and parameters. At most [`MAX_COMPOSE_STEPS`] steps; a step's own
    /// plan may not be a composite.
    Compose(Vec<ActionInput>),
}

/// One layer change of an [`ActionPlan::Edits`]: a [`ActionPlan::Commit`] or an
/// [`ActionPlan::Update`], with the same placement and identity rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerEdit {
    Commit(NewLayer),
    Update(LayerUpdate),
}

/// The most steps one [`ActionPlan::Compose`] may hold, which is the most actions a settings set
/// names.
pub const MAX_COMPOSE_STEPS: usize = MAX_SETTINGS_ACTIONS;

/// The questions about a stack that compile a prefix of it or read its pixels, which the host
/// answers for a [`StageContext`]. The host answers each one only when a module asks it, so a plan
/// that reads no pixel never prepares the original, never develops a RAW and never compiles a
/// prefix evaluation: planning a transform or a RAW white balance asks nothing here but stages.
pub trait StageQuestions {
    /// The stage the layer at index `index` receives, which is the output stage of the layers
    /// before it. The host compiles that prefix, so this costs `O(layers)` and rasterizes nothing.
    fn stage_before(&self, index: usize) -> Result<Stage, Error>;
    /// One pixel of the stage the first `index` layers produce, or `None` outside that stage.
    /// The host evaluates that one point segment by segment, so it allocates no frame.
    fn sample_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error>;
    /// The mean pre-white-balance sensor values of a bounded patch at upright content coordinates,
    /// green-normalized, which a RAW neutral pick sets its gains from. Only a RAW original has a
    /// sensor, so the default, for a stack without one, refuses.
    fn sensor_neutral(&self, x: u32, y: u32) -> Result<[f32; 3], Error> {
        let _ = (x, y);
        Err(Error::new(
            crate::ErrorKind::Validation,
            "RAW neutral picker requires a RAW original",
        ))
    }
}

/// What a module may ask about the current stack while planning an action or answering a query:
/// the output stage, the ordered layers, the stage any position receives, where a commit of an
/// effect would land, its own layer, one pixel of any prefix and, for a RAW original, a sensor
/// patch. Every sample evaluates one pixel without rasterizing, so planning never allocates a
/// frame, and the host answers the questions that read pixels only when they are asked.
pub struct StageContext<'a> {
    /// The output stage of the whole stack, which the host compiles from the source's dimensions
    /// before the module is asked anything.
    pub stage: Stage,
    /// The current recipe's layers in evaluation order, so a module can find its own layer to
    /// update. Planning never mutates them.
    pub layers: &'a [Layer],
    /// The providers, which answer [`StageContext::own_layer`] and
    /// [`StageContext::insertion_index_for`].
    pub registry: &'a ModuleRegistry,
    /// The target this plan or query addresses: `None` for the global layer, or the mask the
    /// request named.
    pub target: Option<&'a MaskId>,
    /// The answers that compile a prefix or read pixels.
    pub questions: &'a dyn StageQuestions,
}

impl<'a> StageContext<'a> {
    /// The one layer of `effect_id` that belongs to the target this plan or query addresses, with
    /// its index: how a module that owns one layer finds it. It is
    /// [`ModuleRegistry::own_layer`] over these layers and this target, so a masked layer of a
    /// maskable effect belongs only to its own mask's target and a stack that holds two layers of
    /// the effect for the target is refused, as the whole-stack compile refuses it for a declared
    /// `single` effect. `O(layers)`; reads no pixels.
    pub fn own_layer(&self, effect_id: &str) -> Result<Option<(usize, &'a Layer)>, Error> {
        self.registry.own_layer(self.layers, effect_id, self.target)
    }

    /// Where the host would put an [`ActionPlan::Commit`] of a layer of this effect, by the stage
    /// and order its descriptor declares ([`ModuleRegistry::insertion_index_for`]). A module plans
    /// against that position instead of choosing one, so [`StageContext::stage_before`] of this
    /// index is the stage its coordinates address. `O(layers)`; reads no pixels.
    pub fn insertion_index_for(&self, effect_id: &str) -> usize {
        self.registry.insertion_index_for(self.layers, effect_id)
    }

    /// The stage the layer at index `index` receives ([`StageQuestions::stage_before`]);
    /// `layers.len()` is [`StageContext::stage`]. A module updating a layer in place plans against
    /// that layer's own input stage, not the final one.
    pub fn stage_before(&self, index: usize) -> Result<Stage, Error> {
        self.questions.stage_before(index)
    }

    /// One pixel of the stage the first `index` layers produce
    /// ([`StageQuestions::sample_before`]).
    pub fn sample_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error> {
        self.questions.sample_before(index, x, y)
    }

    /// A RAW original's sensor patch at upright content coordinates
    /// ([`StageQuestions::sensor_neutral`]).
    pub fn sensor_neutral(&self, x: u32, y: u32) -> Result<[f32; 3], Error> {
        self.questions.sensor_neutral(x, y)
    }
}

/// A test's answers to every stage question: each prefix receives `stage`, every point reads
/// `pixel`, and a RAW sensor patch reads `neutral` when there is one.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FixedStage {
    pub stage: Stage,
    pub pixel: Option<[u8; 4]>,
    pub neutral: Option<[f32; 3]>,
}

#[cfg(test)]
impl FixedStage {
    /// Every prefix receives `stage` and no point has a pixel.
    pub(crate) fn new(stage: Stage) -> Self {
        Self {
            stage,
            pixel: None,
            neutral: None,
        }
    }

    /// Every point reads `pixel`.
    pub(crate) fn reading(self, pixel: [u8; 4]) -> Self {
        Self {
            pixel: Some(pixel),
            ..self
        }
    }

    /// A context over `layers` for the global target, whose output stage is also `stage`.
    pub(crate) fn context<'a>(
        &'a self,
        layers: &'a [Layer],
        registry: &'a ModuleRegistry,
    ) -> StageContext<'a> {
        StageContext {
            stage: self.stage,
            layers,
            registry,
            target: None,
            questions: self,
        }
    }
}

#[cfg(test)]
impl StageQuestions for FixedStage {
    fn stage_before(&self, _: usize) -> Result<Stage, Error> {
        Ok(self.stage)
    }
    fn sample_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[u8; 4]>, Error> {
        Ok(self.pixel)
    }
    fn sensor_neutral(&self, x: u32, y: u32) -> Result<[f32; 3], Error> {
        match self.neutral {
            Some(neutral) => Ok(neutral),
            None => Err(Error::new(
                crate::ErrorKind::Validation,
                format!("no RAW sensor at ({x}, {y})"),
            )),
        }
    }
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
    /// Whether this stored layer changes nothing, so a client need not show it as an edit: a field
    /// patch at its neutral values, a whole-image crop, the identity orientation, a RAW development
    /// at As shot and 0 EV. `recipe.describe` reports it on the layer's row. Reading a payload only,
    /// like [`ToolModule::describe_layer`]: no render, no sample, no source. The default is `false`,
    /// because a payload with no neutral form is always an edit.
    fn is_neutral(&self, effect_id: &str, format: u32, payload: &Value) -> Result<bool, Error> {
        let _ = (effect_id, format, payload);
        Ok(false)
    }
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
    /// Compile a layer that references derived artifacts: the same as [`ToolModule::compile`], with
    /// the verified bytes of every artifact the layer lists, in the layer's order. The host calls
    /// this instead of `compile` only for a layer whose `artifacts` list is not empty, which only an
    /// effect declaring `artifacts: true` may have. The bytes are immutable and already checked
    /// against their hash; the module decides what they mean and refuses what it cannot use. The
    /// default ignores them, so a module that declares no such effect never implements it.
    fn compile_bound(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        stage: Stage,
        artifacts: &[Arc<PreparedArtifact>],
    ) -> Result<Processing, Error> {
        let _ = artifacts;
        self.compile(effect_id, format, payload, stage)
    }
    /// Load what the module's declared activation needs, on the capability worker's module lane,
    /// after the host checked every required setting and resource. `context` gives the installed
    /// resources' paths, the settings and the declared secrets; the module keeps what it loads
    /// until [`ToolModule::deactivate`]. Call `context.checkpoint()` between units of work and
    /// return its error when cancelled. After an activation that does not succeed, or one cancelled
    /// as it finished, the host calls `deactivate` itself, so partial state is released in one
    /// place. Never called on the owner or UI thread, and never by discovery or catalog reopen.
    fn activate(&self, context: &ModuleContext) -> Result<(), Error> {
        let _ = context;
        Ok(())
    }
    /// Release everything `activate` loaded. Called on the module lane, after any work queued
    /// before it; it must tolerate being called when nothing is loaded.
    fn deactivate(&self) {}
    /// Check that a staged resource's bytes are the format the module declares, before the host
    /// installs it. The bytes already match the pinned length and SHA-256. Called on the transfer
    /// lane; a refusal leaves nothing installed. Read the file; never execute or load it with a
    /// general object loader.
    fn validate_resource(&self, resource_id: &str, path: &Path) -> Result<(), Error> {
        let _ = (resource_id, path);
        Ok(())
    }
    /// Run one declared worker task on the capability worker's module lane and return its result
    /// value, which the host reports as the job's `result`.
    ///
    /// Before the job was queued the host checked the task's parameters (`parameters` holds them
    /// with their declared defaults), its asset and profile, its activation requirement and a live
    /// grant for every capability it `uses`, and prepared the data it may send. `context` is the
    /// only way to reach any of it: `send` for a granted `remote-image-request` whose body the host
    /// built, `publish_artifact` for a result the catalog records when the task succeeds, and the
    /// settings, secrets, progress and cancellation every job has. Call `context.checkpoint()`
    /// between units of work and return its error when cancelled; artifacts a task publishes before
    /// it fails or is cancelled are never recorded. Never called on the owner or UI thread. The
    /// default refuses, so a module that declares no tasks never implements it.
    fn run_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        let _ = (task_id, parameters, context);
        Err(Error::new(
            crate::ErrorKind::Validation,
            format!("module {} declares no tasks", self.descriptor().id),
        ))
    }
}
