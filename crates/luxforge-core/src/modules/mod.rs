//! Tool modules: each one owns its descriptor, input parsing, state validation, no-op detection
//! and the compilation of its persisted payloads into host processing primitives. Modules never
//! write the catalog, never keep an undo stack and never render.
pub(crate) mod basic;
mod capabilities_proof;
mod capability;
mod controls;
mod crop;
mod curve;
mod descriptor;
mod detail;
mod field_patch;
pub(crate) mod lens;
mod look;
mod mixer;
mod perspective;
mod pixel;
mod presence;
mod presets;
mod processing;
mod raw;
mod registry;
mod spatial;
mod vignette;

pub use crate::render::map::{Mapping, RadialModel, WarpStep};

/// Every GPU program a built-in module ships (`docs/design/gpu-preview.md`): each a `.wgsl` file
/// beside the CPU unit it mirrors, in the order the units run in a stack. The core's tests validate
/// each under the photo surface's calling convention and fail for a `.wgsl` file this list omits;
/// the desktop's tests check each against the surface's own prelude and qualify each on a device.
pub static GPU_PROGRAMS: &[&crate::GpuProgram] = &[
    &detail::DETAIL_PROGRAM,
    &basic::WHITE_BALANCE_PROGRAM,
    &basic::EXPOSURE_PROGRAM,
    &basic::TONE_PROGRAM,
    &basic::COLOUR_ADJUST_PROGRAM,
    &look::LOOK_PROGRAM,
    &curve::TONE_CURVE_PROGRAM,
    &mixer::MIXER_PROGRAM,
    &vignette::VIGNETTE_PROGRAM,
    &presence::PRESENCE_PROGRAM,
];
pub use basic::BASIC_EFFECT;
pub(crate) use basic::BasicModule;
pub(crate) use capabilities_proof::CapabilitiesProofModule;
#[cfg(test)]
pub(crate) use capabilities_proof::{
    APPLY_PROOF_TINT, PROOF_EFFECT, PROOF_MODULE, PROOF_PALETTE_GAINS, PROOF_TINT_KIND,
};
pub use capabilities_proof::{
    PROOF_GENERATE_PATH, PROOF_PALETTE, PROOF_PALETTE_PATH, palette_bytes,
};
pub use capability::CapabilityModule;
pub use controls::{CONTROLS_EFFECT, controls_module};
pub(crate) use crop::CropModule;
pub use crop::ORIENTATION_EFFECT;
pub use crop::geometry::{
    BoxRect, CropPayload, CropStage, Edge, MAX_ANGLE, MIN_ANGLE, OutputRect, guide_angle,
    largest_with_ratio_inside,
};
pub(crate) use crop::stored_orientation;
pub use crop::{CROP_EFFECT, CropAspect};
pub use curve::CURVE_EFFECT;
pub(crate) use curve::CurveModule;
pub use descriptor::{
    ActionControl, ActionDescriptor, ActionStyle, Availability, CanvasInteraction, ChoiceStyle,
    ColorStyle, Control, CurveBackground, CurveChannel, CurveControl, EffectDescriptor,
    EffectStage, GroupControl, ModuleDescriptor, ModuleLayout, NumberControl, NumberStyle,
    ParameterDescriptor, ParameterKind, PickerControl, PresetsControl, QueryChoiceControl,
    RailDecoration, ResetAction, check_parameters, check_value, resolve_control,
    resolve_group_reset,
};
pub use descriptor::{
    ChoiceControl, ColorControl, ControlVariant, IdentityKind, RangeControl, ResolvedControl,
    ResolvedReset, TaskControl, ToggleControl,
};
pub(crate) use descriptor::{
    MAX_SECRET_LENGTH, MAX_SETTINGS_ACTIONS, MAX_SETTINGS_FIELDS, valid_identity, valid_name,
};
pub(crate) use descriptor::{
    PRESET_SETTINGS, check_declaration, check_declared_values, check_parameter_declarations,
    check_settings, check_target, decode_parameters, label_value, not_applicable, take_parameters,
    title_case,
};
pub use detail::DETAIL_EFFECT;
use detail::DetailModule;
#[cfg(test)]
pub(crate) use detail::gpu_functions as detail_gpu_functions;
#[cfg(feature = "qualification")]
pub use detail::qualification as detail_qualification;
pub use lens::LENS_EFFECT;
pub(crate) use lens::{LENS_MODULE, LensModule};
pub use look::LOOK_EFFECT;
pub(crate) use look::LookModule;
pub use mixer::MIXER_EFFECT;
pub(crate) use mixer::MixerModule;
pub use perspective::PERSPECTIVE_EFFECT;
pub(crate) use perspective::PerspectiveModule;
pub use pixel::PIXEL_EFFECT;
pub(crate) use pixel::PixelModule;
pub use presence::PRESENCE_EFFECT;
pub(crate) use presence::PresenceModule;
#[cfg(test)]
pub(crate) use presence::gpu_functions as presence_gpu_functions;
#[cfg(feature = "qualification")]
pub use presence::qualification as presence_qualification;
pub(crate) use presets::{APPLY_PRESET, MAX_PRESET_NAME, PASTE_SETTINGS, PresetsModule};
pub(crate) use processing::MAX_COLOR_UNITS;
pub use processing::{
    ColorOperation, CompileStage, OperationIdentity, PointwiseColor, Processing, SamplingScale,
    Stage,
};
pub use processing::{ExactGeometry, Resample};
pub(crate) use raw::lightroom_white_balance::lightroom_to_luxforge;
pub use raw::white_balance::{gains_from_temperature_tint, temperature_tint_from_gains};
pub use raw::{RawModule, RawPayload, WhiteBalanceMode};
pub(crate) use raw::{is_raw_development, white_balance_variants};
pub(crate) use registry::Superseded;
#[cfg(test)]
pub(crate) use registry::linked_modules;
#[cfg(test)]
pub(crate) use registry::stack_compiles;
#[cfg(test)]
pub(crate) use registry::tests::{
    HELD_ACTION, HELD_EFFECT, HeldModule, PATCH_ACTION, PATCH_MODULE, PatchModule, STAGE_ACTION,
    STAGE_EFFECT, StageModule, TestModule,
};
pub use registry::{ActionRef, QueryRef};
pub use registry::{ModuleRegistry, Provider, RegistryOptions, insertion_index_among};
pub(crate) use spatial::{
    ESTIMATE_REDUCTION, Global, MAX_REDUCTION_PIXELS, MAX_SPATIAL_HALO, Parallelism, Planes,
    PlanesMut, Reduction, SPATIAL_BUDGET_BYTES, SpatialUnit,
};
pub use spatial::{MAX_MASKED_SPATIAL_LAYERS, Region, SpatialOperation};
pub use vignette::VIGNETTE_EFFECT;
pub(crate) use vignette::VignetteModule;

use crate::{ArtifactId, Error, Layer, LayerId, MaskId, Orientation, SourceTag};
use serde_json::{Map, Value};

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
    /// `StageContext::insertion_index_for` answers where. The host also gives it its identity,
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
    /// action's identity, label and parameters. At most `MAX_COMPOSE_STEPS` steps; a step's own
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
pub(crate) const MAX_COMPOSE_STEPS: usize = MAX_SETTINGS_ACTIONS;

/// The questions about a stack that compile a prefix of it or read its pixels, which the host
/// answers for a [`StageContext`]. The host answers each one only when a module asks it, so a plan
/// that reads no pixel never prepares the original, never develops a RAW and never compiles a
/// prefix evaluation: planning a transform or a RAW white balance asks nothing here but stages.
pub trait StageQuestions {
    /// Capture identity and correction status from the cached verified source. No source is
    /// opened here; an unprepared source explicitly refuses the question.
    fn optics(&self) -> Result<crate::SourceOptics, Error> {
        Err(Error::preparation_required(
            "source optics require a prepared source",
        ))
    }

    /// The stage the layer at index `index` receives, which is the output stage of the layers
    /// before it. The host compiles that prefix, so this costs `O(layers)` and rasterizes nothing.
    fn stage_before(&self, index: usize) -> Result<Stage, Error>;
    /// One pixel of the stage the first `index` layers produce, or `None` outside that stage.
    /// The host evaluates that one point segment by segment, so it allocates no frame.
    fn sample_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[u8; 4]>, Error>;
    /// A layer's input in linear light, including the spatial hand-off's precision.
    fn input_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        Ok(self.sample_before(index, x, y)?.map(|rgba| {
            crate::colour::srgb::decode_pixel([rgba[0], rgba[1], rgba[2]]).map(f64::from)
        }))
    }
    /// The mean pre-white-balance sensor values of a bounded patch at upright content coordinates,
    /// green-normalized, which a RAW neutral pick sets its gains from. Only a RAW original has a
    /// sensor, so the default, for a stack without one, refuses.
    fn sensor_neutral(&self, x: u32, y: u32) -> Result<[f32; 3], Error> {
        let _ = (x, y);
        Err(Error::validation(
            "RAW neutral picker requires a RAW original",
        ))
    }
}

/// What a module may ask about the current stack while planning an action or answering a query:
/// the ordered layers, the output stage, the stage any position receives, where a commit of an
/// effect would land, its own layer, one pixel of any prefix and, for a RAW original, a sensor
/// patch. Every sample evaluates one pixel without rasterizing, so planning never allocates a
/// frame, and the host answers each question that compiles or reads pixels only when it is asked:
/// a plan that asks for no stage compiles nothing.
pub struct StageContext<'a> {
    /// The current recipe's layers in evaluation order, so a module can find its own layer to
    /// update. Planning never mutates them.
    pub layers: &'a [Layer],
    /// The providers, which answer `StageContext::own_layer` and
    /// `StageContext::insertion_index_for`.
    pub registry: &'a ModuleRegistry,
    /// The target this plan or query addresses: `None` for the global layer, or the mask the
    /// request named.
    pub target: Option<&'a MaskId>,
    /// The photo's source kind, the `kind` tag `asset.state` reports: what a plan reads instead of
    /// inspecting the stack to learn which kind of photo it edits.
    pub kind: SourceTag,
    /// The recipe's masks in list order, which place a masked layer among the layers of its own
    /// effect (`StageContext::insertion_index_for`).
    pub masks: &'a [crate::Mask],
    /// The answers that compile a prefix or read pixels.
    pub questions: &'a dyn StageQuestions,
}

impl<'a> StageContext<'a> {
    /// The verified original's optical identity and derived correction ledger.
    pub fn optics(&self) -> Result<crate::SourceOptics, Error> {
        self.questions.optics()
    }

    /// The one layer of `effect_id` that belongs to the target this plan or query addresses, with
    /// its index: how a module that owns one layer finds it. It is
    /// [`ModuleRegistry::own_layer`] over these layers and this target, so a masked layer of a
    /// maskable effect belongs only to its own mask's target and a stack that holds two layers of
    /// the effect for the target is refused, as the whole-stack compile refuses it for a declared
    /// `single` effect. `O(layers)`; reads no pixels.
    pub fn own_layer(&self, effect_id: &str) -> Result<Option<(usize, &'a Layer)>, Error> {
        self.registry.own_layer(self.layers, effect_id, self.target)
    }

    /// Where the host would put an [`ActionPlan::Commit`] of a layer of this effect for this
    /// context's target, by the stage and order its descriptor declares and, for a mask, after the
    /// layers of the same effect its target follows ([`ModuleRegistry::insertion_index_for_target`]).
    /// A module plans against that position instead of choosing one, so
    /// [`StageContext::stage_before`] of this index is the stage its coordinates address.
    /// `O(layers · masks)`; reads no pixels.
    pub fn insertion_index_for(&self, effect_id: &str) -> usize {
        self.registry
            .insertion_index_for_target(self.layers, effect_id, self.target, self.masks)
    }

    /// The output stage of the whole stack: [`StageContext::stage_before`] of `layers.len()`. The
    /// host compiles the stack to answer it, `O(layers)`, only when a module asks, so a plan that
    /// never needs the stage, such as a Basic patch, costs no compile.
    pub fn stage(&self) -> Result<Stage, Error> {
        self.stage_before(self.layers.len())
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

    pub fn input_before(&self, index: usize, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        self.questions.input_before(index, x, y)
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
    pub kind: SourceTag,
}

#[cfg(test)]
impl FixedStage {
    /// Every prefix receives `stage` and no point has a pixel.
    pub(crate) fn new(stage: Stage) -> Self {
        Self {
            stage,
            pixel: None,
            neutral: None,
            kind: SourceTag::Jpeg,
        }
    }

    /// The stack of a photo of `kind`.
    pub(crate) fn of_kind(self, kind: SourceTag) -> Self {
        Self { kind, ..self }
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
            layers,
            registry,
            target: None,
            kind: self.kind,
            masks: &[],
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
            None => Err(Error::validation(format!("no RAW sensor at ({x}, {y})"))),
        }
    }
}

/// What a stored layer is, as its module reads it from the payload alone: what `recipe.describe`
/// reports on the layer's row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerReport {
    /// One short line describing what the layer does.
    pub summary: String,
    /// The parameter values the layer represents, named as the module's action parameters are, so
    /// a client can seed its controls from the displayed entry. Empty for a module that reports
    /// none.
    pub values: Map<String, Value>,
    /// Whether the layer changes nothing, so a client need not show it as an edit: a field patch
    /// at its neutral values, a whole-image crop, the identity orientation, a RAW development at As
    /// shot. `false` for a payload with no neutral form, which is always an edit.
    pub neutral: bool,
}

impl LayerReport {
    /// A layer described by `summary` alone: no values, and an edit.
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            ..Self::default()
        }
    }
}

/// What a module may decide a new photograph's Original from ([`ToolModule::original`]): its
/// source kind, what the Develop read of its file without decoding pixels, and the person's
/// preferences for new photographs. Metadata only: it names no file and answers no pixel.
#[derive(Clone, Copy, Debug)]
pub struct OriginalContext<'a> {
    pub source: SourceTag,
    /// A RAW's interpretation (camera, mode, as-shot white balance and calibration), read from its
    /// header and mosaic layout; `None` for a JPEG.
    pub raw: Option<&'a crate::RawInterpretation>,
    /// The file's header metadata: capture time, camera, lens and exposure, as the catalog's
    /// capture row records them.
    pub header: &'a crate::catalog_types::HeaderMetadata,
    pub preferences: OriginalPreferences,
}

/// The person's preferences a module may consult when it starts a new photograph's Original: a
/// copy of the values, never the preference store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OriginalPreferences {
    /// The look a new RAW photograph starts from.
    pub raw_look: crate::preferences::RawLook,
}

/// A layer a module contributes to a new photograph's Original: one of its own effects and the
/// payload it holds. Everything else is the host's, as for a [`NewLayer`]: the identity, the
/// effect's declared format, no mask, and the position the effect's stage and order give it.
#[derive(Clone, Debug, PartialEq)]
pub struct OriginalLayer {
    pub effect_id: String,
    pub payload: Value,
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
    /// What this stored layer is, for its row of `recipe.describe`: a one-line summary, the
    /// parameter values it represents and whether it changes nothing ([`LayerReport`]). Reading a
    /// payload only: it never renders, samples or touches the source.
    fn describe(&self, effect_id: &str, format: u32, payload: &Value)
    -> Result<LayerReport, Error>;
    /// The history label this request stores: the action's title unless the module says more, as
    /// a patch names the one field it changed, a reset the group it cleared, a transform the
    /// orientation it applies and a crop its angle or ratio. `action` is the action that was
    /// requested, which is not always the durable identity `input` carries: `transform` is
    /// requested, `rotate-left` stored. Reading the request only.
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        let _ = input;
        action.title.clone()
    }
    /// A history label that needs the resolved result of planning, such as a frozen profile
    /// and its EXIF focal length. Reads bounded payloads only; never queries or renders.
    fn planned_label(&self, input: &ActionInput, layers: &[Layer], fallback: &str) -> String {
        let _ = (input, layers);
        fallback.to_owned()
    }

    /// What a preset captures of a stored layer: the fields, named as the module's patch action's
    /// parameters, that reproduce this layer's state when applied to another photo. The default is
    /// the values [`ToolModule::describe`] reports; a module whose values report more than a
    /// preset should carry narrows it, as the RAW development captures `{white-balance: as-shot}`
    /// under As shot rather than this camera's equivalent temperature. Reading a payload only.
    fn settings(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<Map<String, Value>, Error> {
        self.describe(effect_id, format, payload)
            .map(|report| report.values)
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
        Err(Error::validation(format!(
            "module {} declares no queries, so it cannot answer {query_id}",
            self.descriptor().id
        )))
    }
    /// The payload of the neutral layer a GPU preview plans in place of a layer of `effect_id`
    /// the stack does not hold yet: a drafted layer before its first commit, and the first drag
    /// the warm list plans for a module the stack does not hold. In the GPU shape
    /// (`CompileStage::gpu_shape`) it compiles to every unit the effect can hold, each the
    /// identity. The default, `{}`, is every field patch's neutral payload; a module whose stored
    /// form spells its neutral state otherwise, as the look's `{"look": "neutral"}`, names it.
    fn neutral_payload(&self, effect_id: &str) -> Value {
        let _ = effect_id;
        Value::Object(Map::new())
    }
    /// Turn a persisted payload into a host processing primitive at its input stage.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: crate::CompileStage,
    ) -> Result<Processing, Error>;
    /// This stored geometry layer re-expressed for its input stage turned or reflected by
    /// `orientation`, so it selects the same content in the turned stage. `input` is the stage the
    /// layer received before the orientation. The crop module asks it of every geometry layer
    /// after the orientation when an action turns or reflects the photograph, and stores what
    /// comes back through the layer's own update, so the orientation goes ahead of a module it
    /// never names. `Ok(None)` says the payload is unchanged, the default: an effect whose payload
    /// does not address its input stage's coordinates is orientation-invariant. It only reads the payload and never renders; an
    /// effect that cannot be carried exactly refuses, and the whole action with it.
    ///
    /// Only an exact quarter-turn or reflection is carried. A geometry that is not affine over the
    /// whole stage is not something this hook can fake; see the architecture design's "Geometry
    /// beyond an exact orientation".
    fn carry(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        input: Stage,
        orientation: Orientation,
    ) -> Result<Option<Value>, Error> {
        let _ = (effect_id, format, payload, input, orientation);
        Ok(None)
    }
    /// The module's capability hooks, when it declares worker tasks, managed resources or an
    /// effect evaluated with derived artifacts: such a module implements
    /// `CapabilityModule` and returns itself here, and [`ModuleRegistry::register`] refuses one
    /// whose descriptor needs the hooks when this is `None`. Every other module keeps the default.
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        None
    }
    /// One of this module's own actions to commit when a photo is first opened, or `None`, the
    /// default. The host asks when a preparation of the photo's original completes while its head
    /// has never moved (revision 0), against its Original's stack, and commits the action as an
    /// ordinary history entry by the `system` actor, so Undo removes it and the Original stays as
    /// developed. Once the head has moved it is never asked again for that photo: not when the file
    /// is developed again, not on reopen. Planning reads metadata through the context, never
    /// pixels; an error is reported with the preparation and commits nothing.
    fn first_open(&self, context: &StageContext<'_>) -> Result<Option<ActionInput>, Error> {
        let _ = context;
        Ok(None)
    }
    /// Block until whatever [`ToolModule::first_open`] reads is loaded. The host calls it on the
    /// source worker before such a preparation completes, never on the catalog owner or a UI
    /// thread, so `first_open` itself never waits. The default has nothing to wait for.
    fn await_first_open(&self) {}
    /// A layer of one of this module's own effects for a new photograph's Original, or `None`, the
    /// default. The host asks every available module that applies to the photograph's source kind,
    /// in registry order, when it builds the Original of a photograph it is bringing into the
    /// catalog ([`crate::EditorService`]'s `new_photograph`, and a seeded catalog by the same
    /// rule). It checks the layer with [`ToolModule::validate_payload`] and inserts it where its
    /// effect's declared stage and order place it, after the layers modules before it gave (a
    /// source layer stays at index 0), so the layer is part of the Original: no history entry
    /// records it, and Before shows it. It decides from metadata and preferences alone
    /// ([`OriginalContext`]): it never reads a file or a pixel and never renders, so a batch
    /// Develop costs nothing more. An error, or a layer that is not one of this module's effects or
    /// that its own check refuses, refuses the new photograph by name; the layer is never silently
    /// dropped. It is never asked about a photograph already in the catalog.
    fn original(&self, context: &OriginalContext<'_>) -> Result<Option<OriginalLayer>, Error> {
        let _ = context;
        Ok(None)
    }
}
