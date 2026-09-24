//! Plain-data module descriptors: one serializable source for API discovery, generated controls
//! and every validation limit a module declares.
use crate::{
    Error, ErrorKind,
    capabilities::descriptor::{
        ActivationDescriptor, CapabilityDescriptor, ResourceDescriptor, SettingsDescriptor,
        TaskDescriptor,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Groups nest for layout only; a descriptor deeper than this is rejected rather than walked.
const MAX_CONTROL_DEPTH: usize = 8;

/// The longest text a `string` parameter may declare, in characters.
const MAX_STRING_LENGTH: usize = 256;

/// The most field-patch actions one `settings` value names.
pub const MAX_SETTINGS_ACTIONS: usize = 16;

/// The most fields one action of a `settings` value sets.
pub const MAX_SETTINGS_FIELDS: usize = 64;

/// The parameters a `presets` control submits its action with: the settings set, the preset's name
/// and, optionally, the library identity it came from.
pub(crate) const PRESET_SETTINGS: &str = "settings";
pub(crate) const PRESET_NAME: &str = "name";
pub(crate) const PRESET_ID: &str = "preset-id";

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

fn valid_segments(value: &str, separator: char, alphabetic_segments: bool) -> bool {
    !value.is_empty()
        && value.split(separator).enumerate().all(|(index, segment)| {
            let mut bytes = segment.bytes();
            let Some(first) = bytes.next() else {
                return false;
            };
            let starts = first.is_ascii_lowercase()
                || (!alphabetic_segments && index > 0 && first.is_ascii_digit());
            starts && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Module and effect identity: lowercase words separated by dots, e.g. `lightwell.pixel.replace`.
pub fn valid_identity(value: &str) -> bool {
    valid_segments(value, '.', true)
}

/// Action and parameter identity: lowercase words separated by hyphens, e.g. `set-pixel`.
pub fn valid_name(value: &str) -> bool {
    valid_segments(value, '-', false)
}

/// Where an effect acts, and so where the host puts a new layer. The stages run in this order:
/// a source effect prepares the content stage at index zero; pixel and colour effects address that
/// content stage and join the stack before everything that follows; a spatial effect reads a
/// bounded neighbourhood of the content stage, so it follows the pointwise work; a geometry effect
/// changes the stage and extends the geometry tail; and a finish effect is evaluated last, in the
/// output coordinates the tail produced. [`crate::ModuleRegistry::insertion_index`] states the
/// placement rule each stage gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectStage {
    Source,
    Geometry,
    Pixel,
    /// Pointwise colour over the whole stage, compiled into [`crate::Processing::Color`].
    Color,
    /// Depends on a bounded neighbourhood of its input stage, in content coordinates.
    Spatial,
    /// Pointwise but position-dependent, in the output coordinates after the geometry tail.
    Finish,
}

impl EffectStage {
    /// The declared name, spelled as the descriptor serializes it, for an error that has to say
    /// which stage refused something.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Geometry => "geometry",
            Self::Pixel => "pixel",
            Self::Color => "color",
            Self::Spatial => "spatial",
            Self::Finish => "finish",
        }
    }
}

/// A durable effect identity stored in every layer, with its internal payload format marker and the
/// order it takes among layers of its own stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectDescriptor {
    pub id: String,
    pub format: u32,
    pub stage: EffectStage,
    /// Where a new layer of this effect goes among the layers of its own stage: after the last one
    /// whose order is at most this and before the first whose order is greater. Every delivered
    /// effect declares `0`, so an omitted order is the earliest position of its stage. It is a
    /// placement rule only: a stored stack always renders in its stored order.
    #[serde(default)]
    pub order: u16,
    /// Whether a layer of this effect may be bound to a mask, and therefore whether the host adds
    /// its one optional `mask` request field to the actions of this effect's module
    /// (`docs/design/masking.md`, "How a mask reaches an effect"). It is the whole of what a module
    /// says about masking: the field, the target semantics, the compiled mask and the blend are the
    /// host's, and no module parses, plans or compiles any of it.
    ///
    /// A mask's geometry is stored in content-stage coordinates, so a `geometry` or `finish` effect
    /// cannot declare it: registration refuses that descriptor by name rather than accepting a flag
    /// that could never be honoured.
    ///
    /// Serialized only when it is true, as every other flag a descriptor carries is, so an effect
    /// that is not maskable describes itself exactly as it did before masking existed. A client
    /// reads maskability from this flag and reads the field it adds from `schema.list`, which lists
    /// `mask` among the optional fields of every action that accepts it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub maskable: bool,
    /// Whether a layer of this effect may reference derived artifacts through its host-owned
    /// `artifacts` list. A layer of an effect that does not declare it must list none.
    #[serde(default, skip_serializing_if = "is_default")]
    pub artifacts: bool,
    /// Whether a stack holds at most one layer of this effect per target, because the module owns
    /// exactly one layer's worth of state and could not say which of two holds it. The global layer
    /// and each mask are distinct targets of a maskable effect. The host refuses to compile a stack
    /// that holds two layers of such an effect for one target, with `ambiguous <module title>
    /// layers`, and a module finds its one layer through [`super::StageContext::own_layer`].
    /// Serialized only when it is true, like every other flag here.
    #[serde(default, skip_serializing_if = "is_default")]
    pub single: bool,
}

/// The closed set of parameter types v0 modules may declare. `f64` bounds rule out `Eq` here and
/// on every descriptor that contains a parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ParameterKind {
    Integer {
        min: i64,
        max: i64,
    },
    /// A finite `f64` within the closed range. A JSON integer is accepted as a number.
    Number {
        min: f64,
        max: f64,
    },
    Enum {
        options: Vec<String>,
    },
    /// Three 8-bit sRGB channels as a JSON array.
    Color,
    Boolean,
    /// An ordered path: `[x, y]` positions in the content stage's normalized coordinates, in drawn
    /// order, as a drawn gesture produces them. The one parameter kind a painting action needs, and
    /// a parameter kind rather than a control kind because a path is drawn on the canvas and no
    /// panel widget edits one.
    ///
    /// Unlike a [`ParameterKind::Curve`] it is a path and not a function: positions may repeat an
    /// `x`, may run in any direction and are not sorted. The coordinate range, the stored
    /// precision, the decimation contract and the per-stroke bound are
    /// [`crate::path`]'s and are published with the schema, so a client can post a path without
    /// reading any desktop code.
    Points {
        points_min: usize,
        points_max: usize,
    },
    /// One derived artifact published in this catalog, as its opaque `artifact-…` identity. The
    /// generic check validates the identity's syntax; the commit checks that the artifact exists.
    Artifact,
    /// Ordered [x, y] fractions. Interpolation belongs to the module.
    Curve {
        points_min: usize,
        points_max: usize,
        #[serde(default)]
        monotone: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fixed_x: Option<Vec<f64>>,
    },
    /// UTF-8 text of at most `max_length` characters (at most 256) with no control characters.
    /// Whether an empty string means anything is the module's to decide.
    String {
        max_length: usize,
    },
    /// A settings set: an object whose keys are field-patch action identities and whose values are
    /// non-empty objects of that action's fields. The generic check validates only this shape; the
    /// host checks every action and field against its own descriptor when the set is applied.
    Settings,
}

/// Serialized flat: `{"name": "x", "kind": "integer", "min": 0, "max": 16383, ...}`. Flattening
/// the kind rules out `deny_unknown_fields` here; unknown fields are ignored on read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParameterDescriptor {
    pub name: String,
    #[serde(flatten)]
    pub kind: ParameterKind,
    pub required: bool,
    pub default: Option<Value>,
    pub unit: Option<String>,
    /// The keyboard and slider increment of a `number` parameter. A client hint: the host validates
    /// that it is finite and positive and stores it, and never rounds a request to it.
    #[serde(default)]
    pub step: Option<f64>,
    /// How many decimals a client shows for a `number` parameter, at most six. A display hint: the
    /// stored value keeps every digit it was sent with.
    #[serde(default)]
    pub precision: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fine_step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zero: Option<f64>,
    pub notes: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NumberStyle {
    #[default]
    Slider,
    Field,
    Stepper,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChoiceStyle {
    #[default]
    Automatic,
    Segmented,
    Chips,
    Menu,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorStyle {
    #[default]
    Fields,
    Picker,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionStyle {
    #[default]
    Default,
    Primary,
    Icon,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RailDecoration {
    #[default]
    Plain,
    Hue,
    Temperature,
    Tint,
    Gradient {
        stops: Vec<[u8; 3]>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveChannel {
    pub parameter: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CurveBackground {
    #[default]
    None,
    Histogram,
}

/// A declared action with fixed parameter values: the header and group reset buttons. It is the
/// same API action a client can call itself, so a reset is never a GUI-only gesture.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetAction {
    pub action: String,
    #[serde(default)]
    pub preset: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDescriptor {
    pub id: String,
    pub title: String,
    pub notes: String,
    /// A one-line history label template over this action's own parameters, e.g. `Crop {angle}°`.
    /// Every `{name}` names a declared parameter; without a template the label is the title.
    #[serde(default)]
    pub summary: Option<String>,
    /// A field patch: the generic check validates the fields the caller sent and fills no declared
    /// defaults, so the module receives exactly those fields and merges them over its own stored
    /// state. Every parameter of a patch action is optional, whatever it declares.
    #[serde(default)]
    pub patch: bool,
    pub parameters: Vec<ParameterDescriptor>,
}

impl ActionDescriptor {
    pub fn parameter(&self, name: &str) -> Option<&ParameterDescriptor> {
        self.parameters
            .iter()
            .find(|parameter| parameter.name == name)
    }
}

/// Ordered semantic controls. A client renders them; it never invents an operation of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Control {
    Group {
        label: String,
        controls: Vec<Control>,
        /// The action that returns this group to its neutral values, shown on the group header.
        #[serde(default)]
        reset: Option<ResetAction>,
        #[serde(default, skip_serializing_if = "is_default")]
        collapsed: bool,
    },
    Number {
        action: String,
        parameter: String,
        label: String,
        #[serde(default, skip_serializing_if = "is_default")]
        style: NumberStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rail: Option<RailDecoration>,
        /// What resetting this one field runs — a double-click on its label or rail, or its
        /// field's own reset — when that is not the field's declared default: an action of this
        /// module with fixed parameters, validated like a group's reset. Without it the field
        /// resets to its parameter's declared default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset: Option<ResetAction>,
    },
    Toggle {
        action: String,
        parameter: String,
        label: String,
    },
    Choice {
        action: String,
        parameter: String,
        label: String,
        #[serde(default, skip_serializing_if = "is_default")]
        style: ChoiceStyle,
    },
    Color {
        action: String,
        parameter: String,
        label: String,
        #[serde(default, skip_serializing_if = "is_default")]
        style: ColorStyle,
    },
    Curve {
        action: String,
        channels: Vec<CurveChannel>,
        label: String,
        /// Query receiving the active channel's point list and returning sampled fractions.
        sample_query: String,
        #[serde(default, skip_serializing_if = "is_default")]
        background: CurveBackground,
    },
    Action {
        action: String,
        label: String,
        #[serde(default)]
        preset: Map<String, Value>,
        #[serde(default, skip_serializing_if = "is_default")]
        style: ActionStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    /// The module's own canvas pick, offered beside the controls that pick fills rather than in a
    /// mode strip. It binds to the [`CanvasInteraction`] this module declares — a `point-pick` or a
    /// `sample-apply`, never a `crop-frame` — and carries no action of its own: entering and
    /// leaving the mode is `workspace.set`, and the pick itself is what the canvas declares. A
    /// module declares at most one, and a module that declares a pick canvas declares exactly one,
    /// so every pick mode is reachable from the panel.
    Picker { label: String },
    /// A button that runs one of this module's own worker tasks through the client's consent and
    /// progress flow; when the task declares `apply`, the client offers Apply with its result.
    Task { task: String, label: String },
    /// The host's preset library. Choosing a preset submits `action` once with that preset's
    /// `settings`, `name` and `preset-id`, so applying one is the same API call every client makes.
    /// The action is this module's own, with a required `settings` parameter of kind `settings`, a
    /// required `name` of kind `string`, optionally a `preset-id` of kind `string` and nothing
    /// else. A module declares at most one.
    Presets { action: String },
}

/// How a module lets the canvas drive its action. Neither kind commits by itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CanvasInteraction {
    /// A pointer pick on the image fills the named integer parameters of `action`.
    PointPick {
        action: String,
        x: String,
        y: String,
        /// The mode's name in the canvas mode strip.
        title: String,
        /// One uppercase ASCII letter that selects the mode, unique across the registry.
        shortcut: Option<String>,
    },
    /// A pointer pick on the image runs a query at the picked content pixel and, when the query
    /// answers, submits its numeric result fields to `action` once. `x`/`y` name that query's integer
    /// coordinate parameters; the fields submitted are every top-level number field of the result
    /// whose name is a parameter of `action`. A refused query commits nothing and its reason is shown
    /// instead.
    ///
    /// **The pair may be a module's or the host's, and never one of each.** A module declaring this
    /// names a query and an action it declares itself, which is what
    /// [`ModuleDescriptor::validate`] checks. The **host** declares its own through
    /// [`crate::mask::commands::canvas`], where both names are `mask.*` methods of the one command
    /// family — a mask is a host object and no module declares one, so a pick that fills part of a
    /// mask could not be expressed at all until this variant admitted a host target (proposal P17 of
    /// `docs/design/range-study.md`). Everything else about the interaction is unchanged, which is the
    /// point: one pick mechanism, one field-matching rule, one refusal path, and a client that knows
    /// neither a module nor a mask by name.
    ///
    /// **Why a query at all, rather than a colour the client read.** The query is the only way the
    /// picked *value* reaches the action. A mask's value-based parts are evaluated on the input the
    /// masked operation receives, while the frame a client can see holds that operation's output, so a
    /// colour decoded from the picture would be a different colour and the selection would not be the
    /// one the person picked. The host answers with the pixel it already computes and the client
    /// carries numbers it never interprets.
    SampleApply {
        query: String,
        x: String,
        y: String,
        action: String,
        /// The mode's name in the canvas mode strip.
        title: String,
        /// One uppercase ASCII letter that selects the mode, unique across the registry.
        shortcut: Option<String>,
    },
    /// The host's crop-frame editor edits a transient draft of the named number parameters of
    /// `action` and derives its ratio presets from the `aspect` enum of `fit_action`. Only Apply
    /// calls an action.
    CropFrame {
        action: String,
        angle: String,
        x: String,
        y: String,
        width: String,
        height: String,
        fit_action: String,
        aspect: String,
        title: String,
        shortcut: Option<String>,
    },
}

impl CanvasInteraction {
    /// The mode strip entry's name.
    pub fn title(&self) -> &str {
        match self {
            Self::PointPick { title, .. }
            | Self::SampleApply { title, .. }
            | Self::CropFrame { title, .. } => title,
        }
    }

    /// The letter that selects this mode, when the module declares one.
    pub fn shortcut(&self) -> Option<&str> {
        match self {
            Self::PointPick { shortcut, .. }
            | Self::SampleApply { shortcut, .. }
            | Self::CropFrame { shortcut, .. } => shortcut.as_deref(),
        }
    }
}

/// How a client lays out a module's top-level controls. A hint for clients: it changes only what
/// a client draws, never what the host accepts, the vocabulary's rule for every hint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModuleLayout {
    /// Each top-level group renders as its own stacked section. Right for a module whose groups
    /// are different controls, such as Basic's white balance, tone and colour.
    #[default]
    Stacked,
    /// The top-level groups render as one segmented row, one group visible at a time, for a
    /// module whose groups are parallel views of the same controls, such as the colour mixer's
    /// Hue, Saturation and Luminance over the same eight ranges.
    Tabs,
}

/// An unavailable provider keeps its descriptor and effect identities so stored data stays readable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Availability {
    #[default]
    Available,
    Unavailable {
        reason: String,
    },
}

/// `Default` is an empty, unregistrable descriptor: a base for struct update, so a module states
/// only the fields it declares and every capability field stays empty unless it names one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleDescriptor {
    pub id: String,
    pub title: String,
    /// A short line for the collapsed section header, e.g. `Rotate, mirror and flip`.
    #[serde(default)]
    pub hint: Option<String>,
    pub effects: Vec<EffectDescriptor>,
    pub actions: Vec<ActionDescriptor>,
    /// Read-only questions this module answers about a stored stack, declared and validated exactly
    /// like an action and reached through the generated `query.<id>` method. A query mutates
    /// nothing, adds no history entry and emits no event; it answers from point samples of the
    /// stage its own layer addresses, so it allocates no frame. The neutral picker is one.
    #[serde(default)]
    pub queries: Vec<ActionDescriptor>,
    pub controls: Vec<Control>,
    /// The action that returns the whole module to its neutral state, shown on the section header.
    #[serde(default)]
    pub reset: Option<ResetAction>,
    pub canvas: Option<CanvasInteraction>,
    /// A proof or diagnostic tool rather than a photo-editing one; hidden unless the client asks
    /// for developer tools. The API is unaffected.
    #[serde(default)]
    pub developer: bool,
    /// The section starts collapsed in a client's tools panel; a person's own expand or collapse
    /// still wins. A hint for clients, never a rule for the API.
    #[serde(default)]
    pub collapsed: bool,
    /// How a client arranges this module's top-level controls: stacked sections (the default) or
    /// one tab row. Validated at registration; see [`ModuleLayout`].
    #[serde(default)]
    pub layout: ModuleLayout,
    pub availability: Availability,
    /// User-level settings and provider profiles the host stores for this module, outside every
    /// catalog. Discovery lists the declarations only, never a value or a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SettingsDescriptor>,
    /// What the module may be granted: reading a file setting, sending an asset's data to a
    /// profile's endpoint, or installing a declared resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Pinned files the host may install for this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceDescriptor>,
    /// What explicit activation requires; `None` is a module with nothing to activate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<ActivationDescriptor>,
    /// Worker tasks, each reached through the generated `task.<id>` method. A task identity is
    /// unique across the registry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskDescriptor>,
}

impl ModuleDescriptor {
    /// Read a descriptor from JSON, reporting a missing or malformed field as a validation error.
    pub fn parse(value: &Value) -> Result<Self, Error> {
        if let Some(controls) = value.get("controls").and_then(Value::as_array) {
            for control in controls {
                check_raw_control_hints(control)?;
            }
        }
        crate::capabilities::descriptor::check_raw(value)?;
        let descriptor: Self = serde_json::from_value(value.clone())
            .map_err(|error| validation(format!("invalid module descriptor: {error}")))?;
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn action(&self, id: &str) -> Option<&ActionDescriptor> {
        self.actions.iter().find(|action| action.id == id)
    }

    /// The read-only query this module declares under that identity. Queries have their own
    /// namespace: `query.<id>` and `edit.<id>` are different methods, so a module may name a query
    /// after the action its result feeds without either shadowing the other.
    pub fn query(&self, id: &str) -> Option<&ActionDescriptor> {
        self.queries.iter().find(|query| query.id == id)
    }

    pub fn effect(&self, id: &str) -> Option<&EffectDescriptor> {
        self.effects.iter().find(|effect| effect.id == id)
    }

    pub fn capability(&self, id: &str) -> Option<&CapabilityDescriptor> {
        self.capabilities
            .iter()
            .find(|capability| capability.id == id)
    }

    pub fn resource(&self, id: &str) -> Option<&ResourceDescriptor> {
        self.resources.iter().find(|resource| resource.id == id)
    }

    pub fn task(&self, id: &str) -> Option<&TaskDescriptor> {
        self.tasks.iter().find(|task| task.id == id)
    }

    pub fn is_available(&self) -> bool {
        matches!(self.availability, Availability::Available)
    }

    /// Reject every descriptor a client could not render or validate against.
    pub fn validate(&self) -> Result<(), Error> {
        if !valid_identity(&self.id) {
            return Err(validation(format!("invalid module identity {}", self.id)));
        }
        if self.title.trim().is_empty() {
            return Err(validation(format!("module {} has no title", self.id)));
        }
        let mut effects = HashSet::with_capacity(self.effects.len());
        for effect in &self.effects {
            if !valid_identity(&effect.id) {
                return Err(validation(format!("invalid effect identity {}", effect.id)));
            }
            if !effects.insert(effect.id.as_str()) {
                return Err(validation(format!("duplicate effect {}", effect.id)));
            }
            // A mask is stored in content-stage coordinates, so an effect whose input is not that
            // content stage has nothing to read one in: a geometry effect changes the stage and a
            // finish effect is defined in the output coordinates the geometry tail produced.
            if effect.maskable
                && matches!(effect.stage, EffectStage::Geometry | EffectStage::Finish)
            {
                return Err(validation(format!(
                    "effect {} declares maskable at the {} stage, which a mask stored in \
                     content-stage coordinates cannot reach",
                    effect.id,
                    effect.stage.as_str()
                )));
            }
        }
        let mut actions = HashSet::with_capacity(self.actions.len());
        for action in &self.actions {
            check_declared(action, "action", &mut actions)?;
        }
        // A query declares and validates exactly like an action, in its own identity namespace.
        let mut queries = HashSet::with_capacity(self.queries.len());
        for query in &self.queries {
            check_declared(query, "query", &mut queries)?;
        }
        // Settings, capabilities, resources, activation and tasks refer to each other and to the
        // actions above, so they are checked together once those are known to be sound.
        crate::capabilities::descriptor::validate(self)?;
        for control in &self.controls {
            self.check_control(control, 1)?;
        }
        // One picker stands for one pick mode, so two would be two ways into the same mode and a
        // panel could not say which is selected.
        let pickers = Self::count(&self.controls, &|control| {
            matches!(control, Control::Picker { .. })
        });
        if pickers > 1 {
            return Err(validation(format!(
                "module {} declares {pickers} picker controls; a module declares at most one",
                self.id
            )));
        }
        // One library per module: two would show the same presets twice with no way to say which
        // one a client should render.
        let presets = Self::count(&self.controls, &|control| {
            matches!(control, Control::Presets { .. })
        });
        if presets > 1 {
            return Err(validation(format!(
                "module {} declares {presets} presets controls; a module declares at most one",
                self.id
            )));
        }
        self.check_reset(self.reset.as_ref())?;
        if self.layout == ModuleLayout::Tabs {
            let all_groups = self
                .controls
                .iter()
                .all(|control| matches!(control, Control::Group { .. }));
            if self.controls.len() < 2 || !all_groups {
                return Err(validation(format!(
                    "module {} declares layout: tabs but needs at least two top-level groups",
                    self.id
                )));
            }
        }
        match &self.canvas {
            Some(CanvasInteraction::PointPick {
                action,
                x,
                y,
                title,
                shortcut,
            }) => {
                self.check_canvas_mode(title, shortcut.as_deref())?;
                self.check_picker(pickers)?;
                let declared = self.declared_action(action)?;
                for name in [x, y] {
                    let parameter = self.declared_parameter(declared, name)?;
                    if !matches!(parameter.kind, ParameterKind::Integer { .. }) {
                        return Err(validation(format!(
                            "canvas parameter {name} of action {action} is not an integer"
                        )));
                    }
                }
            }
            Some(CanvasInteraction::SampleApply {
                query,
                x,
                y,
                action,
                title,
                shortcut,
            }) => {
                self.check_canvas_mode(title, shortcut.as_deref())?;
                self.check_picker(pickers)?;
                // The query answers the pick and the action receives its result, so both identities
                // and both coordinate parameters must be declared here before a client sees them.
                let declared = self.declared_query(query)?;
                for name in [x, y] {
                    let parameter = self.declared_parameter(declared, name)?;
                    if !matches!(parameter.kind, ParameterKind::Integer { .. }) {
                        return Err(validation(format!(
                            "canvas parameter {name} of query {query} is not an integer"
                        )));
                    }
                }
                self.declared_action(action)?;
            }
            Some(CanvasInteraction::CropFrame {
                action,
                angle,
                x,
                y,
                width,
                height,
                fit_action,
                aspect,
                title,
                shortcut,
            }) => {
                self.check_canvas_mode(title, shortcut.as_deref())?;
                let declared = self.declared_action(action)?;
                for name in [angle, x, y, width, height] {
                    self.canvas_number(declared, name)?;
                }
                let fit = self.declared_action(fit_action)?;
                let chosen = self.declared_parameter(fit, aspect)?;
                if !matches!(chosen.kind, ParameterKind::Enum { .. }) {
                    return Err(validation(format!(
                        "canvas parameter {aspect} of action {fit_action} is not an enum"
                    )));
                }
                // Fitting a ratio needs the draft angle, so the fit action declares one too.
                self.canvas_number(fit, angle)?;
            }
            None => {}
        }
        Ok(())
    }

    /// A canvas mode needs a name for the mode strip, and its optional shortcut is exactly one
    /// uppercase ASCII letter so a keymap can hold it without parsing.
    fn check_canvas_mode(&self, title: &str, shortcut: Option<&str>) -> Result<(), Error> {
        if title.trim().is_empty() {
            return Err(validation(format!(
                "module {} declares a canvas interaction without a title",
                self.id
            )));
        }
        match shortcut {
            Some(letter)
                if letter.len() != 1 || !letter.starts_with(|c: char| c.is_ascii_uppercase()) =>
            {
                Err(validation(format!(
                    "canvas shortcut {letter} of module {} must be one uppercase ASCII letter",
                    self.id
                )))
            }
            _ => Ok(()),
        }
    }

    /// A pick mode is entered from the panel, so the module that declares one declares the picker
    /// control that reaches it. Without this a pick would be reachable only by its letter.
    fn check_picker(&self, pickers: usize) -> Result<(), Error> {
        if pickers == 1 {
            return Ok(());
        }
        Err(validation(format!(
            "module {} declares a pick canvas but no picker control",
            self.id
        )))
    }

    /// A reset is validated exactly like an action control: the action must be declared here and
    /// every preset value must be in its parameter's range.
    fn check_reset(&self, reset: Option<&ResetAction>) -> Result<(), Error> {
        let Some(reset) = reset else {
            return Ok(());
        };
        let declared = self.declared_action(&reset.action)?;
        for (name, value) in &reset.preset {
            check_value(self.declared_parameter(declared, name)?, value)?;
        }
        Ok(())
    }

    fn canvas_number(&self, action: &ActionDescriptor, name: &str) -> Result<(), Error> {
        let parameter = self.declared_parameter(action, name)?;
        if matches!(parameter.kind, ParameterKind::Number { .. }) {
            Ok(())
        } else {
            Err(validation(format!(
                "canvas parameter {name} of action {} is not a number",
                action.id
            )))
        }
    }

    fn declared_action(&self, id: &str) -> Result<&ActionDescriptor, Error> {
        self.action(id).ok_or_else(|| {
            validation(format!(
                "module {} references undeclared action {id}",
                self.id
            ))
        })
    }

    fn declared_query(&self, id: &str) -> Result<&ActionDescriptor, Error> {
        self.query(id).ok_or_else(|| {
            validation(format!(
                "module {} references undeclared query {id}",
                self.id
            ))
        })
    }

    fn declared_parameter<'a>(
        &self,
        action: &'a ActionDescriptor,
        name: &str,
    ) -> Result<&'a ParameterDescriptor, Error> {
        action
            .parameter(name)
            .ok_or_else(|| validation(format!("action {} has no parameter {name}", action.id)))
    }

    fn check_control(&self, control: &Control, depth: usize) -> Result<(), Error> {
        if depth > MAX_CONTROL_DEPTH {
            return Err(validation(format!(
                "module {} nests controls deeper than {MAX_CONTROL_DEPTH} levels",
                self.id
            )));
        }
        match control {
            Control::Group {
                label,
                controls,
                reset,
                ..
            } => {
                if label.trim().is_empty() {
                    return Err(validation(format!(
                        "module {} has an unlabelled group",
                        self.id
                    )));
                }
                self.check_reset(reset.as_ref())?;
                for child in controls {
                    self.check_control(child, depth + 1)?;
                }
            }
            Control::Number {
                action,
                parameter,
                rail,
                reset,
                ..
            } => {
                self.check_reset(reset.as_ref())?;
                let declared = self.declared_action(action)?;
                let declared = self.declared_parameter(declared, parameter)?;
                if !matches!(
                    declared.kind,
                    ParameterKind::Integer { .. } | ParameterKind::Number { .. }
                ) {
                    return Err(validation(format!(
                        "number control for {parameter} of action {action} is not an integer or a number"
                    )));
                }
                if rail.is_some() && !matches!(declared.kind, ParameterKind::Number { .. }) {
                    return Err(validation(format!(
                        "number control for {parameter} of action {action} declares a rail hint on a non-number parameter"
                    )));
                }
                if let Some(RailDecoration::Gradient { stops }) = rail
                    && !(2..=8).contains(&stops.len())
                {
                    return Err(validation(format!(
                        "number control for {parameter} of action {action} needs 2..=8 gradient stops"
                    )));
                }
            }
            Control::Toggle {
                action, parameter, ..
            } => {
                let declared = self.declared_parameter(self.declared_action(action)?, parameter)?;
                if !matches!(declared.kind, ParameterKind::Boolean) {
                    return Err(validation(format!(
                        "toggle control for {parameter} of action {action} is not a boolean"
                    )));
                }
            }
            Control::Choice {
                action, parameter, ..
            } => {
                let declared = self.declared_parameter(self.declared_action(action)?, parameter)?;
                if !matches!(declared.kind, ParameterKind::Enum { .. }) {
                    return Err(validation(format!(
                        "choice control for {parameter} of action {action} is not an enum"
                    )));
                }
            }
            Control::Color {
                action, parameter, ..
            } => {
                let declared = self.declared_action(action)?;
                let declared = self.declared_parameter(declared, parameter)?;
                if !matches!(declared.kind, ParameterKind::Color) {
                    return Err(validation(format!(
                        "color control for {parameter} of action {action} is not a color"
                    )));
                }
            }
            Control::Curve {
                action,
                channels,
                sample_query,
                ..
            } => {
                self.declared_action(action)?;
                let query = self.declared_query(sample_query)?;
                if channels.is_empty() || channels.len() > 8 {
                    return Err(validation(format!(
                        "curve control of action {action} needs 1..=8 channels"
                    )));
                }
                let mut seen = HashSet::with_capacity(channels.len());
                for channel in channels {
                    let parameter = &channel.parameter;
                    let declared =
                        self.declared_parameter(self.declared_action(action)?, parameter)?;
                    if !matches!(declared.kind, ParameterKind::Curve { .. }) {
                        return Err(validation(format!(
                            "curve control for {parameter} of action {action} is not a curve"
                        )));
                    }
                    let query_parameter = self.declared_parameter(query, parameter)?;
                    if query_parameter.kind != declared.kind {
                        return Err(validation(format!(
                            "curve control for {parameter} of action {action} has a mismatched sample query {sample_query}"
                        )));
                    }
                    if query_parameter.required || query_parameter.default.is_some() {
                        return Err(validation(format!(
                            "curve control for {parameter} of action {action} needs an optional sample query field without a default"
                        )));
                    }
                    if channel.label.trim().is_empty() || !seen.insert(parameter) {
                        return Err(validation(format!(
                            "curve control for {parameter} of action {action} needs distinct labelled channels"
                        )));
                    }
                }
                if query
                    .parameters
                    .iter()
                    .any(|parameter| parameter.required && !seen.contains(&parameter.name))
                {
                    return Err(validation(format!(
                        "curve control of action {action} has sample query {sample_query} with unrelated required parameters"
                    )));
                }
            }
            Control::Action {
                action,
                preset,
                icon,
                ..
            } => {
                let declared = self.declared_action(action)?;
                if declared.patch && preset.len() != 1 {
                    return Err(validation(format!(
                        "action control for patch action {action} needs exactly one preset field"
                    )));
                }
                for (name, value) in preset {
                    check_value(self.declared_parameter(declared, name)?, value)?;
                }
                if let Some(icon) = icon
                    && !valid_name(icon)
                {
                    return Err(validation(format!(
                        "action control {action} has invalid icon name {icon}"
                    )));
                }
            }
            // A picker is the panel's way into this module's own pick mode, so the module must
            // declare one. A crop frame takes the whole canvas and has its own controls; it is not
            // a pick and a picker cannot stand for it.
            Control::Picker { label } => {
                if label.trim().is_empty() {
                    return Err(validation(format!(
                        "module {} has an unlabelled picker",
                        self.id
                    )));
                }
                match &self.canvas {
                    Some(CanvasInteraction::PointPick { .. })
                    | Some(CanvasInteraction::SampleApply { .. }) => {}
                    Some(CanvasInteraction::CropFrame { .. }) | None => {
                        return Err(validation(format!(
                            "module {} declares a picker control without a point-pick or sample-apply canvas",
                            self.id
                        )));
                    }
                }
            }
            Control::Task { task, label } => {
                if label.trim().is_empty() {
                    return Err(validation(format!(
                        "task control for {task} of module {} has no label",
                        self.id
                    )));
                }
                if self.task(task).is_none() {
                    return Err(validation(format!(
                        "task control of module {} names undeclared task {task}",
                        self.id
                    )));
                }
            }
            Control::Presets { action } => {
                let declared = self.declared_action(action)?;
                self.check_presets_action(declared)?;
            }
        }
        Ok(())
    }

    /// A presets control submits its action with a preset's settings set, name and library
    /// identity, and nothing else, so the action declares exactly those parameters with the kinds
    /// that carry them. A field patch makes every parameter optional, so it cannot require them.
    fn check_presets_action(&self, action: &ActionDescriptor) -> Result<(), Error> {
        let id = &action.id;
        if action.patch {
            return Err(validation(format!(
                "presets control action {id} is a field patch, so it cannot require its settings and name"
            )));
        }
        let required = |name: &str, kind: &str, matches: fn(&ParameterKind) -> bool| {
            let parameter = self.declared_parameter(action, name)?;
            if !matches(&parameter.kind) || !parameter.required || parameter.default.is_some() {
                return Err(validation(format!(
                    "presets control action {id} needs a required {kind} parameter {name}"
                )));
            }
            Ok(())
        };
        required(PRESET_SETTINGS, "settings", |kind| {
            matches!(kind, ParameterKind::Settings)
        })?;
        required(PRESET_NAME, "string", |kind| {
            matches!(kind, ParameterKind::String { .. })
        })?;
        if let Some(parameter) = action.parameter(PRESET_ID)
            && (!matches!(parameter.kind, ParameterKind::String { .. }) || parameter.required)
        {
            return Err(validation(format!(
                "presets control action {id} may declare only an optional string parameter {PRESET_ID}"
            )));
        }
        if let Some(extra) = action.parameters.iter().find(|parameter| {
            ![PRESET_SETTINGS, PRESET_NAME, PRESET_ID].contains(&&*parameter.name)
        }) {
            return Err(validation(format!(
                "presets control action {id} declares parameter {} beyond {PRESET_SETTINGS}, {PRESET_NAME} and {PRESET_ID}",
                extra.name
            )));
        }
        Ok(())
    }

    /// How many controls of one kind this module declares, at any depth.
    fn count(controls: &[Control], kind: &dyn Fn(&Control) -> bool) -> usize {
        controls
            .iter()
            .map(|control| match control {
                Control::Group { controls, .. } => Self::count(controls, kind),
                control => usize::from(kind(control)),
            })
            .sum()
    }
}

/// Preserve the action and parameter names when malformed JSON puts a rail on another kind.
/// Typed descriptors cannot express that state, so this guard runs before deserialization.
fn check_raw_control_hints(control: &Value) -> Result<(), Error> {
    if let Some(children) = control.get("controls").and_then(Value::as_array) {
        for child in children {
            check_raw_control_hints(child)?;
        }
    }
    if control.get("rail").is_some()
        && control.get("kind").and_then(Value::as_str) != Some("number")
    {
        let kind = control
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let action = control
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let parameter = control
            .get("parameter")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(validation(format!(
            "{kind} control for {parameter} of action {action} declares a rail hint on a non-number control"
        )));
    }
    Ok(())
}

/// One declared action or query: a valid, unique identity, a title, and parameters whose names,
/// ranges, hints and defaults a caller can be validated against. Actions and queries are checked by
/// the same rules because a client calls them the same way; only the method prefix differs.
fn check_declared<'a>(
    declared: &'a ActionDescriptor,
    kind: &str,
    seen: &mut HashSet<&'a str>,
) -> Result<(), Error> {
    if !valid_name(&declared.id) {
        return Err(validation(format!(
            "invalid {kind} identity {}",
            declared.id
        )));
    }
    if !seen.insert(declared.id.as_str()) {
        return Err(validation(format!("duplicate {kind} {}", declared.id)));
    }
    if declared.title.trim().is_empty() {
        return Err(validation(format!("{kind} {} has no title", declared.id)));
    }
    check_parameter_declarations(kind, &declared.id, &declared.parameters)?;
    check_summary(declared)
}

/// The parameters one action, query or task declares: valid, unique names, sound kinds, hints and
/// defaults, so every caller can be validated against them the same way.
pub(crate) fn check_parameter_declarations(
    kind: &str,
    id: &str,
    declared: &[ParameterDescriptor],
) -> Result<(), Error> {
    let mut parameters = HashSet::with_capacity(declared.len());
    for parameter in declared {
        if !valid_name(&parameter.name) {
            return Err(validation(format!(
                "invalid parameter name {} of {kind} {id}",
                parameter.name
            )));
        }
        if !parameters.insert(parameter.name.as_str()) {
            return Err(validation(format!(
                "duplicate parameter {} of {kind} {id}",
                parameter.name
            )));
        }
        match &parameter.kind {
            ParameterKind::Integer { min, max } if min > max => {
                return Err(validation(format!(
                    "parameter {} declares an empty range {min}..={max}",
                    parameter.name
                )));
            }
            ParameterKind::Number { min, max }
                if !min.is_finite() || !max.is_finite() || min > max =>
            {
                return Err(validation(format!(
                    "parameter {} declares an empty range {min}..={max}",
                    parameter.name
                )));
            }
            ParameterKind::Enum { options } if options.is_empty() => {
                return Err(validation(format!(
                    "parameter {} declares no options",
                    parameter.name
                )));
            }
            ParameterKind::String { max_length }
                if *max_length == 0 || *max_length > MAX_STRING_LENGTH =>
            {
                return Err(validation(format!(
                    "parameter {} declares a max_length {max_length} outside 1..={MAX_STRING_LENGTH}",
                    parameter.name
                )));
            }
            ParameterKind::Points {
                points_min,
                points_max,
            } if *points_min < 1
                || *points_max > crate::path::POINTS_PER_STROKE
                || points_min > points_max =>
            {
                return Err(validation(format!(
                    "parameter {} declares invalid path point bounds; 1..={} is the limit",
                    parameter.name,
                    crate::path::POINTS_PER_STROKE
                )));
            }
            ParameterKind::Curve {
                points_min,
                points_max,
                fixed_x,
                ..
            } => {
                if *points_min < 2 || *points_max > 32 || points_min > points_max {
                    return Err(validation(format!(
                        "parameter {} declares invalid curve point bounds",
                        parameter.name
                    )));
                }
                if let Some(xs) = fixed_x
                    && (xs.len() < *points_min
                        || xs.len() > *points_max
                        || xs
                            .iter()
                            .any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
                        || xs.windows(2).any(|pair| pair[0] >= pair[1]))
                {
                    return Err(validation(format!(
                        "parameter {} declares invalid fixed_x curve points",
                        parameter.name
                    )));
                }
            }
            _ => {}
        }
        check_hints(parameter)?;
        if let Some(default) = &parameter.default {
            check_value(parameter, default)?;
        }
    }
    Ok(())
}

/// The largest number of decimals a client is asked to display. Beyond this a slider's text is
/// noise rather than information, so a descriptor declaring more is rejected at registration.
const MAX_PRECISION: u8 = 6;

/// Steps and precision describe numeric and curve controls. They remain client hints: requests
/// are validated against the hard kind and are never rounded to a hint.
fn check_hints(parameter: &ParameterDescriptor) -> Result<(), Error> {
    let name = &parameter.name;
    if !matches!(
        parameter.kind,
        ParameterKind::Integer { .. } | ParameterKind::Number { .. } | ParameterKind::Curve { .. }
    ) && (parameter.step.is_some() || parameter.precision.is_some())
    {
        return Err(validation(format!(
            "parameter {name} declares a step or precision but is not numeric or a curve"
        )));
    }
    let bounds = match parameter.kind {
        ParameterKind::Integer { min, max } => Some((min as f64, max as f64)),
        ParameterKind::Number { min, max } => Some((min, max)),
        _ => None,
    };
    if bounds.is_none()
        && [parameter.soft_min, parameter.soft_max, parameter.zero]
            .iter()
            .any(Option::is_some)
    {
        return Err(validation(format!(
            "parameter {name} declares numeric hints but is not a number"
        )));
    }
    if bounds.is_none()
        && !matches!(parameter.kind, ParameterKind::Curve { .. })
        && parameter.fine_step.is_some()
    {
        return Err(validation(format!(
            "parameter {name} declares a fine step but is not numeric or a curve"
        )));
    }
    if let Some((min, max)) = bounds {
        let soft_min = parameter.soft_min.unwrap_or(min);
        let soft_max = parameter.soft_max.unwrap_or(max);
        if !soft_min.is_finite()
            || !soft_max.is_finite()
            || soft_min < min
            || soft_max > max
            || (min < max && soft_min >= soft_max)
        {
            return Err(validation(format!(
                "parameter {name} declares a soft range outside {min}..={max}"
            )));
        }
        if let Some(fine_step) = parameter.fine_step
            && (!fine_step.is_finite() || fine_step <= 0.0)
        {
            return Err(validation(format!(
                "parameter {name} declares a fine step that is not finite and positive"
            )));
        }
        if let Some(zero) = parameter.zero
            && (!zero.is_finite() || zero < min || zero > max)
        {
            return Err(validation(format!(
                "parameter {name} declares a zero outside {min}..={max}"
            )));
        }
    }
    if let Some(fine_step) = parameter.fine_step
        && (!fine_step.is_finite() || fine_step <= 0.0)
    {
        return Err(validation(format!(
            "parameter {name} declares a fine step that is not finite and positive"
        )));
    }
    if let Some(step) = parameter.step
        && (!step.is_finite() || step <= 0.0)
    {
        return Err(validation(format!(
            "parameter {name} declares a step that is not finite and positive"
        )));
    }
    if let Some(precision) = parameter.precision
        && precision > MAX_PRECISION
    {
        return Err(validation(format!(
            "parameter {name} declares a precision above {MAX_PRECISION}"
        )));
    }
    Ok(())
}

/// Every `{name}` in a summary template names a parameter of its own action, and the braces are
/// balanced, so rendering one at commit cannot silently produce a wrong label.
fn check_summary(action: &ActionDescriptor) -> Result<(), Error> {
    let Some(template) = &action.summary else {
        return Ok(());
    };
    let unbalanced = || {
        validation(format!(
            "summary of action {} has unbalanced braces",
            action.id
        ))
    };
    let mut rest = template.as_str();
    while let Some(index) = rest.find(['{', '}']) {
        let tail = &rest[index..];
        if tail.starts_with('}') {
            return Err(unbalanced());
        }
        let after = &tail[1..];
        let close = after.find('}').ok_or_else(unbalanced)?;
        let name = &after[..close];
        if name.contains('{') {
            return Err(unbalanced());
        }
        if action.parameter(name).is_none() {
            return Err(validation(format!(
                "summary of action {} names undeclared parameter {name}",
                action.id
            )));
        }
        rest = &after[close + 1..];
    }
    Ok(())
}

/// Render a validated summary template with an action's stored parameters. Pure and allocation
/// bounded by the template: integers as written, numbers without trailing zeros, three channels as
/// `r,g,b`, enum options title-cased with hyphens as spaces, anything else as its JSON text. A
/// parameter the request did not carry renders as nothing.
pub fn render_summary(template: &str, parameters: &Map<String, Value>) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        rendered.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Validation rejects this template; render what is left literally rather than panic.
            rendered.push_str(after);
            return rendered;
        };
        if let Some(value) = parameters.get(&after[..close]) {
            rendered.push_str(&summary_value(value));
        }
        rest = &after[close + 1..];
    }
    rendered.push_str(rest);
    rendered
}

/// The history label one requested action commits: its rendered summary, or its title when it
/// declares no template or the template rendered nothing.
pub fn action_label(action: &ActionDescriptor, parameters: &Map<String, Value>) -> String {
    match &action.summary {
        Some(template) => {
            let rendered = render_summary(template, parameters);
            if rendered.trim().is_empty() {
                action.title.clone()
            } else {
                rendered
            }
        }
        None => action.title.clone(),
    }
}

fn summary_value(value: &Value) -> String {
    match value {
        // `{}` on an f64 already drops trailing zeros: 3.5, 0, -12.
        Value::Number(number) if number.is_f64() => match number.as_f64() {
            Some(number) => format!("{number}"),
            None => number.to_string(),
        },
        Value::Number(number) => number.to_string(),
        Value::String(text) => title_case(text),
        Value::Array(channels) if channels.iter().all(Value::is_number) => channels
            .iter()
            .map(summary_value)
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

/// `rotate-left` reads as `Rotate left`; `16:9` and other punctuated options keep their shape. A
/// mask component's display name is built from its kind the same way, so `luminance-range` reads as
/// `Luminance range 1`.
pub(crate) fn title_case(text: &str) -> String {
    // A declared name reaches this as a kind (`colour-range`) or as a parameter (`colour_refine`),
    // and both read as a phrase, so both separators become a space rather than one of them being
    // shown to a person as it is spelled in a request.
    let spaced = text.replace(['-', '_'], " ");
    let mut characters = spaced.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => spaced,
    }
}

/// One value against one declared parameter: the check every caller of an action gets, exposed so
/// a draft can validate a single field without assembling a whole request.
pub fn check_value(parameter: &ParameterDescriptor, value: &Value) -> Result<(), Error> {
    let name = &parameter.name;
    match &parameter.kind {
        ParameterKind::Integer { min, max } => {
            let number = value
                .as_i64()
                .ok_or_else(|| validation(format!("parameter {name} must be an integer")))?;
            if number < *min || number > *max {
                return Err(validation(format!(
                    "parameter {name} must be an integer within {min}..={max}"
                )));
            }
        }
        ParameterKind::Number { min, max } => {
            // `as_f64` accepts a JSON integer; NaN and infinities are not JSON numbers, and a
            // value built in process that is not finite is rejected here too.
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| validation(format!("parameter {name} must be a number")))?;
            if number < *min || number > *max {
                return Err(validation(format!(
                    "parameter {name} must be a number within {min}..={max}"
                )));
            }
        }
        ParameterKind::Enum { options } => {
            let text = value
                .as_str()
                .ok_or_else(|| validation(format!("parameter {name} must be a string")))?;
            if !options.iter().any(|option| option == text) {
                return Err(validation(format!(
                    "parameter {name} must be one of {}",
                    options.join(", ")
                )));
            }
        }
        ParameterKind::Color => {
            let valid = value.as_array().is_some_and(|channels| {
                channels.len() == 3
                    && channels
                        .iter()
                        .all(|channel| channel.as_u64().is_some_and(|channel| channel <= 255))
            });
            if !valid {
                return Err(validation(format!(
                    "parameter {name} must be three sRGB channels 0..=255"
                )));
            }
        }
        ParameterKind::Boolean => {
            if !value.is_boolean() {
                return Err(validation(format!("parameter {name} must be a boolean")));
            }
        }
        ParameterKind::Points {
            points_min,
            points_max,
        } => {
            use crate::path::{COORDINATE_MAX, COORDINATE_MIN};
            let Some(points) = value.as_array() else {
                return Err(validation(format!("parameter {name} must be a path")));
            };
            // The count is refused as a resource limit when it is over the bound and as a
            // validation error when it is under one, because the two are different facts: a path
            // longer than a build will store names the limit it exceeded, and a path too short to
            // be a gesture is a malformed request.
            if points.len() > *points_max {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "parameter {name} has {} positions; the limit is {points_max} points per \
                         stroke",
                        points.len()
                    ),
                ));
            }
            if points.len() < *points_min {
                return Err(validation(format!(
                    "parameter {name} must hold at least {points_min} positions"
                )));
            }
            for (index, point) in points.iter().enumerate() {
                let Some(pair) = point.as_array().filter(|pair| pair.len() == 2) else {
                    return Err(validation(format!(
                        "parameter {name} has a malformed position {index}"
                    )));
                };
                let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) else {
                    return Err(validation(format!(
                        "parameter {name} has a malformed position {index}"
                    )));
                };
                if !x.is_finite()
                    || !y.is_finite()
                    || !(COORDINATE_MIN..=COORDINATE_MAX).contains(&x)
                    || !(COORDINATE_MIN..=COORDINATE_MAX).contains(&y)
                {
                    return Err(validation(format!(
                        "parameter {name} position {index} must hold two numbers within \
                         {COORDINATE_MIN:.0}..={COORDINATE_MAX:.0}"
                    )));
                }
            }
        }
        ParameterKind::Artifact => {
            let valid = value
                .as_str()
                .is_some_and(|text| crate::ArtifactId::parse(text).is_ok());
            if !valid {
                return Err(validation(format!(
                    "parameter {name} must be an artifact identity"
                )));
            }
        }
        ParameterKind::Curve {
            points_min,
            points_max,
            monotone,
            fixed_x,
        } => {
            let Some(points) = value.as_array() else {
                return Err(validation(format!(
                    "parameter {name} must be a curve point list"
                )));
            };
            if points.len() < *points_min
                || points.len() > *points_max
                || fixed_x.as_ref().is_some_and(|xs| xs.len() != points.len())
            {
                return Err(validation(format!(
                    "parameter {name} has an invalid curve point count"
                )));
            }
            let mut previous = None;
            for (index, point) in points.iter().enumerate() {
                let Some(pair) = point.as_array().filter(|pair| pair.len() == 2) else {
                    return Err(validation(format!(
                        "parameter {name} has a malformed curve point {index}"
                    )));
                };
                let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) else {
                    return Err(validation(format!(
                        "parameter {name} has a malformed curve point {index}"
                    )));
                };
                if !x.is_finite()
                    || !y.is_finite()
                    || !(0.0..=1.0).contains(&x)
                    || !(0.0..=1.0).contains(&y)
                    || previous.is_some_and(|(px, py)| x <= px || (*monotone && y < py))
                    || fixed_x.as_ref().is_some_and(|xs| x != xs[index])
                {
                    return Err(validation(format!(
                        "parameter {name} has an invalid curve point {index}"
                    )));
                }
                previous = Some((x, y));
            }
        }
        ParameterKind::String { max_length } => {
            let text = value
                .as_str()
                .ok_or_else(|| validation(format!("parameter {name} must be a string")))?;
            // Characters, not bytes: the bound is what a person reads and types.
            if text.chars().count() > *max_length {
                return Err(validation(format!(
                    "parameter {name} must be at most {max_length} characters"
                )));
            }
            if text.chars().any(char::is_control) {
                return Err(validation(format!(
                    "parameter {name} must not contain control characters"
                )));
            }
        }
        ParameterKind::Settings => check_settings(name, value)?,
    }
    Ok(())
}

/// The shape of a settings set, and nothing more: 1 to 16 action identities, each giving a
/// non-empty object of at most 64 fields with valid parameter names. Whether each action exists,
/// is a field patch and accepts each value is the host's check against the registry, made when the
/// set is applied, because a descriptor cannot see other modules.
fn check_settings(name: &str, value: &Value) -> Result<(), Error> {
    let actions = value
        .as_object()
        .ok_or_else(|| validation(format!("parameter {name} must be a settings object")))?;
    if actions.is_empty() || actions.len() > MAX_SETTINGS_ACTIONS {
        return Err(validation(format!(
            "parameter {name} must name 1..={MAX_SETTINGS_ACTIONS} actions"
        )));
    }
    for (action, fields) in actions {
        if !valid_name(action) {
            return Err(validation(format!(
                "parameter {name} names invalid action identity {action}"
            )));
        }
        let fields = fields
            .as_object()
            .filter(|fields| !fields.is_empty())
            .ok_or_else(|| {
                validation(format!(
                    "parameter {name} must give action {action} a non-empty object of fields"
                ))
            })?;
        if fields.len() > MAX_SETTINGS_FIELDS {
            return Err(validation(format!(
                "parameter {name} gives action {action} more than {MAX_SETTINGS_FIELDS} fields"
            )));
        }
        if let Some(field) = fields.keys().find(|field| !valid_name(field)) {
            return Err(validation(format!(
                "parameter {name} gives action {action} invalid field name {field}"
            )));
        }
    }
    Ok(())
}

/// Apply declared defaults and reject anything an action did not declare, so every caller of an
/// action gets the same structured validation error before the module sees the request.
///
/// A patch action is checked differently: the fields the caller sent are validated and returned as
/// sent, no declared default is applied and no required parameter is demanded, so the module
/// receives exactly the named fields and merges them over the state it already holds.
pub fn check_parameters(
    action: &ActionDescriptor,
    input: &Value,
) -> Result<Map<String, Value>, Error> {
    check_declared_values(
        "action",
        &action.id,
        &action.parameters,
        action.patch,
        input,
    )
}

/// The generic check behind [`check_parameters`], for anything that declares parameters the way an
/// action does: `what` and `id` name it in every refusal, e.g. `task generate-proof-tint`.
pub(crate) fn check_declared_values(
    what: &str,
    id: &str,
    parameters: &[ParameterDescriptor],
    patch: bool,
    input: &Value,
) -> Result<Map<String, Value>, Error> {
    let declared = |name: &str| parameters.iter().find(|parameter| parameter.name == name);
    let empty = Map::new();
    let object = match input {
        Value::Object(object) => object,
        Value::Null => &empty,
        _ => {
            return Err(validation(format!(
                "parameters of {what} {id} must be a JSON object"
            )));
        }
    };
    for name in object.keys() {
        if declared(name).is_none() {
            return Err(validation(format!(
                "unknown parameter {name} for {what} {id}"
            )));
        }
    }
    let mut checked = Map::new();
    if patch {
        for (name, value) in object {
            let parameter =
                declared(name).expect("every key was matched to a declared parameter above");
            check_value(parameter, value)?;
            checked.insert(name.clone(), value.clone());
        }
        return Ok(checked);
    }
    for parameter in parameters {
        match (object.get(&parameter.name), &parameter.default) {
            (Some(value), _) => {
                check_value(parameter, value)?;
                checked.insert(parameter.name.clone(), value.clone());
            }
            (None, Some(default)) => {
                checked.insert(parameter.name.clone(), default.clone());
            }
            (None, None) if parameter.required => {
                return Err(validation(format!(
                    "missing required parameter {} for {what} {id}",
                    parameter.name
                )));
            }
            (None, None) => {}
        }
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn integer(name: &str) -> ParameterDescriptor {
        ParameterDescriptor {
            name: name.into(),
            kind: ParameterKind::Integer { min: 0, max: 10 },
            required: true,
            default: None,
            unit: Some("px".into()),
            step: None,
            precision: None,
            notes: "test".into(),
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
        }
    }

    fn number(name: &str, min: f64, max: f64) -> ParameterDescriptor {
        ParameterDescriptor {
            name: name.into(),
            kind: ParameterKind::Number { min, max },
            required: true,
            default: None,
            unit: None,
            step: None,
            precision: None,
            notes: "test".into(),
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
        }
    }

    /// A descriptor whose one parameter carries these decimal hints and no controls, so only the
    /// hint rule under test can fail.
    fn with_hints(
        step: Option<f64>,
        precision: Option<u8>,
        parameter: ParameterDescriptor,
    ) -> ModuleDescriptor {
        ModuleDescriptor {
            actions: vec![ActionDescriptor {
                summary: None,
                parameters: vec![ParameterDescriptor {
                    step,
                    precision,
                    ..parameter
                }],
                ..action()
            }],
            controls: Vec::new(),
            reset: None,
            ..descriptor()
        }
    }

    fn enumerated(name: &str) -> ParameterDescriptor {
        ParameterDescriptor {
            name: name.into(),
            kind: ParameterKind::Enum {
                options: vec!["free".into(), "1:1".into()],
            },
            required: true,
            default: None,
            unit: None,
            step: None,
            precision: None,
            notes: "test".into(),
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
        }
    }

    fn frame_canvas(action: &str, fit_action: &str) -> CanvasInteraction {
        CanvasInteraction::CropFrame {
            action: action.into(),
            angle: "angle".into(),
            x: "x".into(),
            y: "y".into(),
            width: "width".into(),
            height: "height".into(),
            fit_action: fit_action.into(),
            aspect: "aspect".into(),
            title: "Frame".into(),
            shortcut: Some("R".into()),
        }
    }

    /// A frame action, a fit action and the canvas that binds them: the shape the crop module
    /// declares, used here to prove every crop-frame rejection.
    fn frame_descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            actions: vec![
                ActionDescriptor {
                    id: "set-frame".into(),
                    title: "Set frame".into(),
                    notes: "test".into(),
                    summary: None,
                    patch: false,
                    parameters: vec![
                        number("angle", -45.0, 45.0),
                        number("x", 0.0, 1.0),
                        number("y", 0.0, 1.0),
                        number("width", 0.0, 1.0),
                        number("height", 0.0, 1.0),
                    ],
                },
                ActionDescriptor {
                    id: "fit-frame".into(),
                    title: "Fit frame".into(),
                    notes: "test".into(),
                    summary: None,
                    patch: false,
                    parameters: vec![enumerated("aspect"), number("angle", -45.0, 45.0)],
                },
            ],
            controls: vec![Control::Number {
                action: "set-frame".into(),
                parameter: "angle".into(),
                label: "Angle".into(),
                style: crate::NumberStyle::Slider,
                rail: None,
                reset: None,
            }],
            // The shared descriptor's reset names an action this one does not declare.
            reset: None,
            canvas: Some(frame_canvas("set-frame", "fit-frame")),
            ..descriptor()
        }
    }

    /// The frame descriptor with one action's parameter list replaced and no controls, so only the
    /// canvas rule under test can fail.
    fn frame_with(action_id: &str, parameters: Vec<ParameterDescriptor>) -> ModuleDescriptor {
        let mut descriptor = frame_descriptor();
        descriptor.controls = Vec::new();
        for action in &mut descriptor.actions {
            if action.id == action_id {
                action.parameters = parameters.clone();
            }
        }
        descriptor
    }

    /// The read-only query shape a module declares: two integer coordinates, no summary.
    fn query() -> ActionDescriptor {
        ActionDescriptor {
            id: "neutral-sample".into(),
            title: "Neutral sample".into(),
            notes: "test".into(),
            summary: None,
            patch: false,
            parameters: vec![integer("x"), integer("y")],
        }
    }

    fn sample_apply(query: &str, x: &str, y: &str, action: &str) -> CanvasInteraction {
        CanvasInteraction::SampleApply {
            query: query.into(),
            x: x.into(),
            y: y.into(),
            action: action.into(),
            title: "Pick".into(),
            shortcut: Some("W".into()),
        }
    }

    /// A module declaring one query and the sample-apply canvas that binds it to an action: the
    /// shape the Basic module declares, used here to prove every sample-apply rejection.
    fn sample_descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            queries: vec![query()],
            // A pick canvas declares the picker control that reaches it.
            controls: vec![Control::Picker {
                label: "Pick".into(),
            }],
            canvas: Some(sample_apply("neutral-sample", "x", "y", "set-thing")),
            ..descriptor()
        }
    }

    fn action() -> ActionDescriptor {
        ActionDescriptor {
            id: "set-thing".into(),
            title: "Set thing".into(),
            notes: "test".into(),
            summary: Some("Thing {x} {mode}".into()),
            patch: false,
            parameters: vec![
                integer("x"),
                ParameterDescriptor {
                    name: "rgb".into(),
                    kind: ParameterKind::Color,
                    required: true,
                    default: None,
                    unit: None,
                    step: None,
                    precision: None,
                    notes: "test".into(),
                    soft_min: None,
                    soft_max: None,
                    fine_step: None,
                    zero: None,
                },
                ParameterDescriptor {
                    name: "mode".into(),
                    kind: ParameterKind::Enum {
                        options: vec!["fast".into(), "exact".into()],
                    },
                    required: false,
                    default: Some(json!("exact")),
                    unit: None,
                    step: None,
                    precision: None,
                    notes: "test".into(),
                    soft_min: None,
                    soft_max: None,
                    fine_step: None,
                    zero: None,
                },
            ],
        }
    }

    /// A `points` parameter declaring these bounds, on a module with no controls: a path is drawn
    /// on the canvas and the control vocabulary has no widget for one, so the parameter stands on
    /// its own and only the bounds rule under test can fail.
    fn points_module(points_min: usize, points_max: usize) -> ModuleDescriptor {
        ModuleDescriptor {
            actions: vec![ActionDescriptor {
                summary: None,
                parameters: vec![ParameterDescriptor {
                    name: "path".into(),
                    kind: ParameterKind::Points {
                        points_min,
                        points_max,
                    },
                    required: true,
                    default: None,
                    unit: None,
                    step: None,
                    precision: None,
                    notes: "the drawn path".into(),
                    soft_min: None,
                    soft_max: None,
                    fine_step: None,
                    zero: None,
                }],
                ..action()
            }],
            controls: Vec::new(),
            reset: None,
            ..descriptor()
        }
    }

    /// The test module with one number control whose field reset runs `action` with `preset`.
    fn number_reset(action: &str, preset: Value) -> ModuleDescriptor {
        ModuleDescriptor {
            controls: vec![Control::Number {
                action: "set-thing".into(),
                parameter: "x".into(),
                label: "X".into(),
                style: crate::NumberStyle::Slider,
                rail: None,
                reset: Some(ResetAction {
                    action: action.into(),
                    preset: preset.as_object().unwrap().clone(),
                }),
            }],
            ..descriptor()
        }
    }

    #[test]
    fn a_points_parameter_is_declared_within_the_host_path_bound() {
        let limit = crate::path::POINTS_PER_STROKE;
        points_module(1, limit)
            .validate()
            .expect("the whole bound is declarable");
        points_module(1, 1)
            .validate()
            .expect("a one-position path is a legal declaration");
        for (min, max) in [(0, 8), (1, limit + 1), (8, 4)] {
            assert_eq!(
                points_module(min, max).validate().unwrap_err().detail,
                format!(
                    "parameter path declares invalid path point bounds; 1..={limit} is the limit"
                )
            );
        }
        // A path is not numeric and not a curve, so it carries none of the numeric display hints.
        let hinted = ModuleDescriptor {
            actions: vec![ActionDescriptor {
                parameters: vec![ParameterDescriptor {
                    step: Some(0.1),
                    ..points_module(1, 8).actions[0].parameters[0].clone()
                }],
                ..points_module(1, 8).actions[0].clone()
            }],
            ..points_module(1, 8)
        };
        assert_eq!(
            hinted.validate().unwrap_err().detail,
            "parameter path declares a step or precision but is not numeric or a curve"
        );
    }

    /// A number control may declare what resetting its field runs. It is validated like a group's
    /// reset, lists in `module.list` beside the control's other fields, and a control without one
    /// lists no `reset` at all, so every other control keeps its shape.
    #[test]
    fn a_number_control_declares_its_own_field_reset() {
        let declared = number_reset("set-thing", json!({"mode": "fast"}));
        declared
            .validate()
            .expect("a reset naming this module's action");
        let listed = serde_json::to_value(&declared).unwrap();
        assert_eq!(
            listed["controls"][0],
            json!({"kind":"number","action":"set-thing","parameter":"x","label":"X",
                "reset":{"action":"set-thing","preset":{"mode":"fast"}}})
        );
        assert_eq!(ModuleDescriptor::parse(&listed).unwrap(), declared);
        let plain = serde_json::to_value(descriptor()).unwrap();
        let number = &plain["controls"][0]["controls"][0];
        assert_eq!(number["kind"], "number");
        assert!(number.get("reset").is_none(), "{number}");
        // A preset naming another module's action is refused like any undeclared action.
        let error = number_reset("reset-raw", json!({}))
            .validate()
            .expect_err("another module's action");
        assert_eq!(
            error.detail,
            "module test.module references undeclared action reset-raw"
        );
    }

    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "test.module".into(),
            title: "Test".into(),
            hint: Some("A test module".into()),
            effects: vec![EffectDescriptor {
                id: "test.module.effect".into(),
                format: 1,
                stage: EffectStage::Pixel,
                order: 0,
                maskable: false,
                artifacts: false,
                single: false,
            }],
            actions: vec![action()],
            queries: Vec::new(),
            controls: vec![Control::Group {
                label: "Test".into(),
                reset: Some(ResetAction {
                    action: "set-thing".into(),
                    preset: json!({"x": 0}).as_object().unwrap().clone(),
                }),
                controls: vec![
                    Control::Number {
                        action: "set-thing".into(),
                        parameter: "x".into(),
                        label: "X".into(),
                        style: crate::NumberStyle::Slider,
                        rail: None,
                        reset: None,
                    },
                    Control::Action {
                        action: "set-thing".into(),
                        label: "Apply".into(),
                        preset: Map::new(),
                        style: crate::ActionStyle::Default,
                        icon: None,
                    },
                ],
                collapsed: false,
            }],
            reset: Some(ResetAction {
                action: "set-thing".into(),
                preset: Map::new(),
            }),
            canvas: None,
            developer: false,
            collapsed: false,
            layout: ModuleLayout::Stacked,
            availability: Availability::Available,
            ..ModuleDescriptor::default()
        }
    }

    #[test]
    fn identity_rules_accept_declared_names_and_reject_malformed_ones() {
        for value in ["lightwell.pixel", "a", "lightwell.pixel.replace", "a1.b2"] {
            assert!(valid_identity(value), "{value}");
        }
        for value in [
            "",
            "Lightwell.pixel",
            "lightwell..pixel",
            ".pixel",
            "pixel.",
            "light_well",
            "1pixel",
            "set-pixel",
        ] {
            assert!(!valid_identity(value), "{value}");
        }
        for value in ["set-pixel", "transform", "rotate-left", "x", "crop-16-9"] {
            assert!(valid_name(value), "{value}");
        }
        for value in [
            "",
            "Set-Pixel",
            "set--pixel",
            "-set",
            "set-",
            "set.pixel",
            "1set",
        ] {
            assert!(!valid_name(value), "{value}");
        }
    }

    #[test]
    fn descriptors_reject_malformed_identities_duplicates_and_invalid_controls() {
        assert!(descriptor().validate().is_ok());
        assert!(
            frame_descriptor().validate().is_ok(),
            "a crop frame over declared number parameters is accepted"
        );
        assert!(
            sample_descriptor().validate().is_ok(),
            "a sample-apply over a declared query and action is accepted"
        );
        assert_eq!(
            sample_descriptor().query("neutral-sample").map(|q| &q.id),
            Some(&"neutral-sample".to_owned())
        );
        // A picker is a declared control bound to the module's own pick canvas, with the serialized
        // shape a client discovers it by, and it nests in a group like every other control.
        assert_eq!(
            serde_json::to_value(Control::Picker {
                label: "Neutral picker".into()
            })
            .unwrap(),
            json!({"kind": "picker", "label": "Neutral picker"})
        );
        let nested = ModuleDescriptor {
            controls: vec![Control::Group {
                label: "White balance".into(),
                reset: None,
                controls: vec![Control::Picker {
                    label: "Pick".into(),
                }],
                collapsed: false,
            }],
            ..sample_descriptor()
        };
        assert!(
            nested.validate().is_ok(),
            "a picker inside a group satisfies the module's one-picker rule"
        );
        assert_eq!(
            ModuleDescriptor::parse(&serde_json::to_value(&nested).unwrap()).unwrap(),
            nested,
            "a picker round-trips through JSON"
        );
        assert!(
            descriptor().queries.is_empty(),
            "queries are optional and default to none"
        );
        let cases: Vec<(&str, ModuleDescriptor)> = vec![
            (
                "module identity",
                ModuleDescriptor {
                    id: "Test Module".into(),
                    ..descriptor()
                },
            ),
            (
                "module title",
                ModuleDescriptor {
                    title: "  ".into(),
                    ..descriptor()
                },
            ),
            (
                "effect identity",
                ModuleDescriptor {
                    effects: vec![EffectDescriptor {
                        id: "Test-Effect".into(),
                        format: 1,
                        stage: EffectStage::Pixel,
                        order: 0,
                        maskable: false,
                        artifacts: false,
                        single: false,
                    }],
                    ..descriptor()
                },
            ),
            (
                "duplicate effect",
                ModuleDescriptor {
                    effects: vec![
                        descriptor().effects[0].clone(),
                        descriptor().effects[0].clone(),
                    ],
                    ..descriptor()
                },
            ),
            (
                "duplicate action",
                ModuleDescriptor {
                    actions: vec![action(), action()],
                    ..descriptor()
                },
            ),
            (
                "action identity",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        id: "Set.Thing".into(),
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "duplicate parameter",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        parameters: vec![integer("x"), integer("x")],
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "empty integer range",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        parameters: vec![ParameterDescriptor {
                            kind: ParameterKind::Integer { min: 5, max: 1 },
                            ..integer("x")
                        }],
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "empty enum",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        parameters: vec![ParameterDescriptor {
                            name: "mode".into(),
                            kind: ParameterKind::Enum {
                                options: Vec::new(),
                            },
                            required: true,
                            default: None,
                            unit: None,
                            step: None,
                            precision: None,
                            notes: "test".into(),
                            soft_min: None,
                            soft_max: None,
                            fine_step: None,
                            zero: None,
                        }],
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "default out of range",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        parameters: vec![ParameterDescriptor {
                            default: Some(json!(99)),
                            ..integer("x")
                        }],
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "undeclared action",
                ModuleDescriptor {
                    controls: vec![Control::Number {
                        action: "missing".into(),
                        parameter: "x".into(),
                        label: "X".into(),
                        style: crate::NumberStyle::Slider,
                        rail: None,
                        reset: None,
                    }],
                    ..descriptor()
                },
            ),
            (
                "undeclared parameter",
                ModuleDescriptor {
                    controls: vec![Control::Number {
                        action: "set-thing".into(),
                        parameter: "missing".into(),
                        label: "X".into(),
                        style: crate::NumberStyle::Slider,
                        rail: None,
                        reset: None,
                    }],
                    ..descriptor()
                },
            ),
            (
                "wrong control kind",
                ModuleDescriptor {
                    controls: vec![Control::Color {
                        action: "set-thing".into(),
                        parameter: "x".into(),
                        label: "X".into(),
                        style: crate::ColorStyle::Fields,
                    }],
                    ..descriptor()
                },
            ),
            (
                "preset out of range",
                ModuleDescriptor {
                    controls: vec![Control::Action {
                        action: "set-thing".into(),
                        label: "Apply".into(),
                        preset: json!({"x": 99}).as_object().unwrap().clone(),
                        style: crate::ActionStyle::Default,
                        icon: None,
                    }],
                    ..descriptor()
                },
            ),
            (
                "preset names an undeclared parameter",
                ModuleDescriptor {
                    controls: vec![Control::Action {
                        action: "set-thing".into(),
                        label: "Apply".into(),
                        preset: json!({"missing": 1}).as_object().unwrap().clone(),
                        style: crate::ActionStyle::Default,
                        icon: None,
                    }],
                    ..descriptor()
                },
            ),
            (
                "canvas parameter is not an integer",
                ModuleDescriptor {
                    canvas: Some(CanvasInteraction::PointPick {
                        action: "set-thing".into(),
                        x: "x".into(),
                        y: "rgb".into(),
                        title: "Pick".into(),
                        shortcut: None,
                    }),
                    ..descriptor()
                },
            ),
            (
                "empty number range",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        parameters: vec![number("angle", 5.0, 1.0)],
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "non-finite number range",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        parameters: vec![number("angle", 0.0, f64::INFINITY)],
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "number default out of range",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        parameters: vec![ParameterDescriptor {
                            default: Some(json!(2.5)),
                            ..number("angle", -1.0, 1.0)
                        }],
                        ..action()
                    }],
                    controls: Vec::new(),
                    ..descriptor()
                },
            ),
            (
                "number control on a color parameter",
                ModuleDescriptor {
                    controls: vec![Control::Number {
                        action: "set-thing".into(),
                        parameter: "rgb".into(),
                        label: "RGB".into(),
                        style: crate::NumberStyle::Slider,
                        rail: None,
                        reset: None,
                    }],
                    ..descriptor()
                },
            ),
            (
                "crop frame names an undeclared action",
                ModuleDescriptor {
                    canvas: Some(frame_canvas("missing", "fit-frame")),
                    controls: Vec::new(),
                    ..frame_descriptor()
                },
            ),
            (
                "crop frame names an undeclared fit action",
                ModuleDescriptor {
                    canvas: Some(frame_canvas("set-frame", "missing")),
                    controls: Vec::new(),
                    ..frame_descriptor()
                },
            ),
            (
                "crop frame parameter is missing",
                frame_with(
                    "set-frame",
                    vec![
                        number("angle", -45.0, 45.0),
                        number("x", 0.0, 1.0),
                        number("y", 0.0, 1.0),
                        number("width", 0.0, 1.0),
                    ],
                ),
            ),
            (
                "crop frame parameter is not a number",
                frame_with(
                    "set-frame",
                    vec![
                        integer("angle"),
                        number("x", 0.0, 1.0),
                        number("y", 0.0, 1.0),
                        number("width", 0.0, 1.0),
                        number("height", 0.0, 1.0),
                    ],
                ),
            ),
            (
                "crop frame aspect is not an enum",
                frame_with(
                    "fit-frame",
                    vec![number("aspect", 0.0, 1.0), number("angle", -45.0, 45.0)],
                ),
            ),
            (
                "crop frame fit action has no angle",
                frame_with("fit-frame", vec![enumerated("aspect")]),
            ),
            (
                "crop frame fit angle is not a number",
                frame_with("fit-frame", vec![enumerated("aspect"), integer("angle")]),
            ),
            (
                "module reset names an undeclared action",
                ModuleDescriptor {
                    reset: Some(ResetAction {
                        action: "missing".into(),
                        preset: Map::new(),
                    }),
                    ..descriptor()
                },
            ),
            (
                "module reset preset out of range",
                ModuleDescriptor {
                    reset: Some(ResetAction {
                        action: "set-thing".into(),
                        preset: json!({"x": 99}).as_object().unwrap().clone(),
                    }),
                    ..descriptor()
                },
            ),
            (
                "group reset names an undeclared parameter",
                ModuleDescriptor {
                    controls: vec![Control::Group {
                        label: "Test".into(),
                        controls: Vec::new(),
                        reset: Some(ResetAction {
                            action: "set-thing".into(),
                            preset: json!({"missing": 1}).as_object().unwrap().clone(),
                        }),
                        collapsed: false,
                    }],
                    ..descriptor()
                },
            ),
            (
                "group reset preset out of range",
                ModuleDescriptor {
                    controls: vec![Control::Group {
                        label: "Test".into(),
                        controls: Vec::new(),
                        reset: Some(ResetAction {
                            action: "set-thing".into(),
                            preset: json!({"mode": "sloppy"}).as_object().unwrap().clone(),
                        }),
                        collapsed: false,
                    }],
                    ..descriptor()
                },
            ),
            (
                "number reset names an undeclared action",
                number_reset("missing", json!({})),
            ),
            (
                "number reset names an undeclared parameter",
                number_reset("set-thing", json!({"missing": 1})),
            ),
            (
                "number reset preset out of range",
                number_reset("set-thing", json!({"x": 99})),
            ),
            (
                "number reset preset of the wrong kind",
                number_reset("set-thing", json!({"mode": 3})),
            ),
            (
                "summary names an undeclared parameter",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        summary: Some("Thing {missing}".into()),
                        ..action()
                    }],
                    ..descriptor()
                },
            ),
            (
                "summary opens a placeholder it does not close",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        summary: Some("Thing {x".into()),
                        ..action()
                    }],
                    ..descriptor()
                },
            ),
            (
                "summary closes a placeholder it did not open",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        summary: Some("Thing x}".into()),
                        ..action()
                    }],
                    ..descriptor()
                },
            ),
            (
                "summary nests braces",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        summary: Some("Thing {{x}}".into()),
                        ..action()
                    }],
                    ..descriptor()
                },
            ),
            (
                "canvas mode without a title",
                ModuleDescriptor {
                    canvas: Some(CanvasInteraction::PointPick {
                        action: "set-thing".into(),
                        x: "x".into(),
                        y: "x".into(),
                        title: "  ".into(),
                        shortcut: None,
                    }),
                    ..descriptor()
                },
            ),
            (
                "canvas shortcut is not one uppercase letter",
                ModuleDescriptor {
                    canvas: Some(CanvasInteraction::PointPick {
                        action: "set-thing".into(),
                        x: "x".into(),
                        y: "x".into(),
                        title: "Pick".into(),
                        shortcut: Some("r".into()),
                    }),
                    ..descriptor()
                },
            ),
            (
                "canvas shortcut is a word",
                ModuleDescriptor {
                    canvas: Some(CanvasInteraction::PointPick {
                        action: "set-thing".into(),
                        x: "x".into(),
                        y: "x".into(),
                        title: "Pick".into(),
                        shortcut: Some("RR".into()),
                    }),
                    ..descriptor()
                },
            ),
            (
                "a query with an invalid identity",
                ModuleDescriptor {
                    queries: vec![ActionDescriptor {
                        id: "Neutral.Sample".into(),
                        ..query()
                    }],
                    canvas: None,
                    ..descriptor()
                },
            ),
            (
                "a duplicate query",
                ModuleDescriptor {
                    queries: vec![query(), query()],
                    canvas: None,
                    ..descriptor()
                },
            ),
            (
                "a query parameter with an empty range",
                ModuleDescriptor {
                    queries: vec![ActionDescriptor {
                        parameters: vec![ParameterDescriptor {
                            kind: ParameterKind::Integer { min: 9, max: 1 },
                            ..integer("x")
                        }],
                        ..query()
                    }],
                    canvas: None,
                    ..descriptor()
                },
            ),
            (
                "a sample-apply naming an undeclared query",
                ModuleDescriptor {
                    canvas: Some(sample_apply("missing", "x", "y", "set-thing")),
                    ..sample_descriptor()
                },
            ),
            (
                "a sample-apply naming an undeclared coordinate",
                ModuleDescriptor {
                    canvas: Some(sample_apply("neutral-sample", "x", "z", "set-thing")),
                    ..sample_descriptor()
                },
            ),
            (
                "a sample-apply whose coordinate is not an integer",
                ModuleDescriptor {
                    queries: vec![ActionDescriptor {
                        parameters: vec![integer("x"), number("y", 0.0, 10.0)],
                        ..query()
                    }],
                    ..sample_descriptor()
                },
            ),
            (
                "a sample-apply naming an undeclared action",
                ModuleDescriptor {
                    canvas: Some(sample_apply("neutral-sample", "x", "y", "missing")),
                    ..sample_descriptor()
                },
            ),
            (
                "a sample-apply without a title",
                ModuleDescriptor {
                    canvas: Some(CanvasInteraction::SampleApply {
                        query: "neutral-sample".into(),
                        x: "x".into(),
                        y: "y".into(),
                        action: "set-thing".into(),
                        title: "  ".into(),
                        shortcut: Some("W".into()),
                    }),
                    ..sample_descriptor()
                },
            ),
            (
                "a sample-apply whose shortcut is not one uppercase letter",
                ModuleDescriptor {
                    canvas: Some(CanvasInteraction::SampleApply {
                        query: "neutral-sample".into(),
                        x: "x".into(),
                        y: "y".into(),
                        action: "set-thing".into(),
                        title: "Pick".into(),
                        shortcut: Some("w".into()),
                    }),
                    ..sample_descriptor()
                },
            ),
            // A picker binds to the module's own pick canvas, so it needs one and there is
            // exactly one of it; and a pick canvas needs the control that reaches it.
            (
                "a picker on a module with no canvas at all",
                ModuleDescriptor {
                    controls: vec![Control::Picker {
                        label: "Pick".into(),
                    }],
                    ..descriptor()
                },
            ),
            (
                "a picker on a crop-frame canvas, which is not a pick",
                ModuleDescriptor {
                    controls: vec![Control::Picker {
                        label: "Pick".into(),
                    }],
                    ..frame_descriptor()
                },
            ),
            (
                "two pickers in one module",
                ModuleDescriptor {
                    controls: vec![
                        Control::Picker {
                            label: "Pick".into(),
                        },
                        Control::Group {
                            label: "Nested".into(),
                            reset: None,
                            controls: vec![Control::Picker {
                                label: "Pick again".into(),
                            }],
                            collapsed: false,
                        },
                    ],
                    ..sample_descriptor()
                },
            ),
            (
                "an unlabelled picker",
                ModuleDescriptor {
                    controls: vec![Control::Picker { label: "  ".into() }],
                    ..sample_descriptor()
                },
            ),
            (
                "a sample-apply canvas with no picker control",
                ModuleDescriptor {
                    controls: Vec::new(),
                    ..sample_descriptor()
                },
            ),
            (
                "a point-pick canvas with no picker control",
                ModuleDescriptor {
                    controls: Vec::new(),
                    canvas: Some(CanvasInteraction::PointPick {
                        action: "set-thing".into(),
                        x: "x".into(),
                        y: "x".into(),
                        title: "Pick".into(),
                        shortcut: Some("W".into()),
                    }),
                    ..descriptor()
                },
            ),
            (
                "a step that is zero",
                with_hints(Some(0.0), None, number("angle", -45.0, 45.0)),
            ),
            (
                "a negative step",
                with_hints(Some(-0.5), None, number("angle", -45.0, 45.0)),
            ),
            (
                "a step that is not finite",
                with_hints(Some(f64::NAN), None, number("angle", -45.0, 45.0)),
            ),
            (
                "an infinite step",
                with_hints(Some(f64::INFINITY), None, number("angle", -45.0, 45.0)),
            ),
            (
                "a precision above six",
                with_hints(None, Some(7), number("angle", -45.0, 45.0)),
            ),
            (
                "a step on a boolean parameter",
                with_hints(
                    Some(1.0),
                    None,
                    ParameterDescriptor {
                        kind: ParameterKind::Boolean,
                        ..integer("x")
                    },
                ),
            ),
            (
                "a precision on an enum parameter",
                with_hints(None, Some(2), enumerated("mode")),
            ),
            (
                "a string that declares no characters",
                with_hints(None, None, string("name", 0, true)),
            ),
            (
                "a string longer than 256 characters",
                with_hints(None, None, string("name", 257, true)),
            ),
            (
                "a string default longer than its bound",
                with_hints(
                    None,
                    None,
                    ParameterDescriptor {
                        default: Some(json!("abcde")),
                        ..string("name", 4, false)
                    },
                ),
            ),
            (
                "a string default with a control character",
                with_hints(
                    None,
                    None,
                    ParameterDescriptor {
                        default: Some(json!("a\tb")),
                        ..string("name", 4, false)
                    },
                ),
            ),
            (
                "a step on a string parameter",
                with_hints(Some(1.0), None, string("name", 4, true)),
            ),
            (
                "a settings default that is not a settings set",
                with_hints(
                    None,
                    None,
                    ParameterDescriptor {
                        default: Some(json!({})),
                        ..settings("settings")
                    },
                ),
            ),
        ];
        let cases = cases.into_iter().chain(
            presets_cases()
                .into_iter()
                .map(|(case, descriptor, _)| (case, descriptor)),
        );
        for (case, descriptor) in cases {
            let error = descriptor
                .validate()
                .expect_err(&format!("{case} must be rejected"));
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        }
    }

    fn string(name: &str, max_length: usize, required: bool) -> ParameterDescriptor {
        ParameterDescriptor {
            kind: ParameterKind::String { max_length },
            required,
            unit: None,
            ..integer(name)
        }
    }

    fn settings(name: &str) -> ParameterDescriptor {
        ParameterDescriptor {
            kind: ParameterKind::Settings,
            unit: None,
            ..integer(name)
        }
    }

    /// A module shaped like the presets module: one action taking a settings set, a name and an
    /// optional library identity, and the one presets control that submits it.
    fn presets_descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            effects: Vec::new(),
            actions: vec![ActionDescriptor {
                id: "apply-thing".into(),
                title: "Apply thing".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: vec![
                    settings("settings"),
                    string("name", 128, true),
                    string("preset-id", 96, false),
                ],
            }],
            controls: vec![Control::Presets {
                action: "apply-thing".into(),
            }],
            reset: None,
            ..descriptor()
        }
    }

    /// The presets descriptor with its action's parameters replaced.
    fn presets_with(parameters: Vec<ParameterDescriptor>) -> ModuleDescriptor {
        let mut descriptor = presets_descriptor();
        descriptor.actions[0].parameters = parameters;
        descriptor
    }

    /// Every way a presets control can be bound wrongly, with the fragment its error names.
    fn presets_cases() -> Vec<(&'static str, ModuleDescriptor, &'static str)> {
        vec![
            (
                "a presets control naming an undeclared action",
                ModuleDescriptor {
                    controls: vec![Control::Presets {
                        action: "missing".into(),
                    }],
                    ..presets_descriptor()
                },
                "references undeclared action missing",
            ),
            (
                "a presets action without settings",
                presets_with(vec![string("name", 128, true)]),
                "action apply-thing has no parameter settings",
            ),
            (
                "a presets action whose settings is another kind",
                presets_with(vec![
                    ParameterDescriptor {
                        kind: ParameterKind::Curve {
                            points_min: 2,
                            points_max: 4,
                            monotone: false,
                            fixed_x: None,
                        },
                        unit: None,
                        ..integer("settings")
                    },
                    string("name", 128, true),
                ]),
                "needs a required settings parameter settings",
            ),
            (
                "a presets action whose settings is optional",
                presets_with(vec![
                    ParameterDescriptor {
                        required: false,
                        ..settings("settings")
                    },
                    string("name", 128, true),
                ]),
                "needs a required settings parameter settings",
            ),
            (
                "a presets action without a name",
                presets_with(vec![settings("settings")]),
                "action apply-thing has no parameter name",
            ),
            (
                "a presets action whose name is not a string",
                presets_with(vec![settings("settings"), enumerated("name")]),
                "needs a required string parameter name",
            ),
            (
                "a presets action whose name has a default",
                presets_with(vec![
                    settings("settings"),
                    ParameterDescriptor {
                        default: Some(json!("Preset")),
                        ..string("name", 128, true)
                    },
                ]),
                "needs a required string parameter name",
            ),
            (
                "a presets action whose library identity is required",
                presets_with(vec![
                    settings("settings"),
                    string("name", 128, true),
                    string("preset-id", 96, true),
                ]),
                "may declare only an optional string parameter preset-id",
            ),
            (
                "a presets action whose library identity is not a string",
                presets_with(vec![
                    settings("settings"),
                    string("name", 128, true),
                    ParameterDescriptor {
                        required: false,
                        ..integer("preset-id")
                    },
                ]),
                "may declare only an optional string parameter preset-id",
            ),
            (
                "a presets action with another parameter",
                presets_with(vec![
                    settings("settings"),
                    string("name", 128, true),
                    integer("x"),
                ]),
                "declares parameter x beyond settings, name and preset-id",
            ),
            (
                "a presets control on a field patch",
                ModuleDescriptor {
                    actions: vec![ActionDescriptor {
                        patch: true,
                        ..presets_descriptor().actions[0].clone()
                    }],
                    ..presets_descriptor()
                },
                "is a field patch",
            ),
            (
                "two presets controls in one module",
                ModuleDescriptor {
                    controls: vec![
                        Control::Presets {
                            action: "apply-thing".into(),
                        },
                        Control::Group {
                            label: "Nested".into(),
                            reset: None,
                            controls: vec![Control::Presets {
                                action: "apply-thing".into(),
                            }],
                            collapsed: false,
                        },
                    ],
                    ..presets_descriptor()
                },
                "declares 2 presets controls; a module declares at most one",
            ),
        ]
    }

    /// A presets control binds to an action shaped exactly for it, at most once per module, and
    /// keeps the serialized form a client discovers it by.
    #[test]
    fn a_presets_control_needs_its_own_action_with_exactly_the_preset_parameters() {
        let valid = presets_descriptor();
        valid.validate().expect("the presets shape is accepted");
        assert!(
            presets_with(vec![settings("settings"), string("name", 128, true)])
                .validate()
                .is_ok(),
            "the library identity is optional"
        );
        let nested = ModuleDescriptor {
            controls: vec![Control::Group {
                label: "Library".into(),
                reset: None,
                controls: vec![Control::Presets {
                    action: "apply-thing".into(),
                }],
                collapsed: false,
            }],
            ..presets_descriptor()
        };
        nested
            .validate()
            .expect("one presets control inside a group is still one");
        assert_eq!(
            serde_json::to_value(&valid.controls[0]).unwrap(),
            json!({"kind": "presets", "action": "apply-thing"})
        );
        assert_eq!(
            ModuleDescriptor::parse(&serde_json::to_value(&valid).unwrap()).unwrap(),
            valid,
            "a presets descriptor round-trips through JSON"
        );
        for (case, descriptor, fragment) in presets_cases() {
            let error = descriptor
                .validate()
                .expect_err(&format!("{case} must be rejected"));
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
    }

    /// A string is bounded in characters, not bytes, refuses control characters and leaves
    /// emptiness to the module; it serializes flat like every other kind.
    #[test]
    fn string_parameters_bound_characters_and_refuse_control_characters() {
        let parameter = string("name", 4, true);
        assert_eq!(
            serde_json::to_value(string("name", 128, true)).unwrap(),
            json!({
                "name": "name",
                "kind": "string",
                "max_length": 128,
                "required": true,
                "default": null,
                "unit": null,
                "step": null,
                "precision": null,
                "notes": "test",
            })
        );
        assert_eq!(
            serde_json::from_value::<ParameterDescriptor>(
                serde_json::to_value(&parameter).unwrap()
            )
            .unwrap(),
            parameter
        );
        assert!(
            with_hints(None, None, string("name", 256, true))
                .validate()
                .is_ok(),
            "256 characters is the longest bound"
        );
        for accepted in ["", "abcd", "ééé", "日本語だ", "a b "] {
            check_value(&parameter, &json!(accepted))
                .unwrap_or_else(|error| panic!("{accepted:?}: {error}"));
        }
        assert_eq!("日本語だ".len(), 12, "four characters are twelve bytes");
        for (case, value, fragment) in [
            ("a number", json!(4), "parameter name must be a string"),
            ("null", Value::Null, "parameter name must be a string"),
            (
                "an array",
                json!(["abcd"]),
                "parameter name must be a string",
            ),
            (
                "five characters",
                json!("abcde"),
                "parameter name must be at most 4 characters",
            ),
            (
                "five two-byte characters",
                json!("ééééé"),
                "parameter name must be at most 4 characters",
            ),
            (
                "a newline",
                json!("a\nb"),
                "parameter name must not contain control characters",
            ),
            (
                "a bell",
                json!("\u{7}"),
                "parameter name must not contain control characters",
            ),
            (
                "a C1 control",
                json!("a\u{85}"),
                "parameter name must not contain control characters",
            ),
        ] {
            let error = check_value(&parameter, &value).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert_eq!(error.detail, fragment, "{case}");
        }
    }

    /// The generic settings check validates the shape of a set and nothing else: the actions and
    /// fields it names are the host's to check against the registry.
    #[test]
    fn settings_parameters_check_only_the_shape_of_a_settings_set() {
        let parameter = settings("settings");
        assert_eq!(
            serde_json::to_value(&parameter).unwrap()["kind"],
            json!("settings")
        );
        assert!(
            serde_json::to_value(&parameter)
                .unwrap()
                .get("max_length")
                .is_none(),
            "a settings kind carries nothing but its tag"
        );
        assert_eq!(
            serde_json::from_value::<ParameterDescriptor>(
                serde_json::to_value(&parameter).unwrap()
            )
            .unwrap(),
            parameter
        );
        let fields = |count: usize| -> Value {
            Value::Object(
                (0..count)
                    .map(|index| (format!("field-{index}"), json!(index)))
                    .collect(),
            )
        };
        let actions = |count: usize| -> Value {
            Value::Object(
                (0..count)
                    .map(|index| (format!("set-thing-{index}"), fields(MAX_SETTINGS_FIELDS)))
                    .collect(),
            )
        };
        for (case, accepted) in [
            ("one action", json!({"set-basic": {"exposure": 0.35}})),
            (
                "an action no module declares, which is the host's to refuse",
                json!({"set-anything": {"any-field": "any value"}}),
            ),
            ("the most actions and fields", actions(MAX_SETTINGS_ACTIONS)),
        ] {
            check_value(&parameter, &accepted).unwrap_or_else(|error| panic!("{case}: {error}"));
        }
        for (case, value, fragment) in [
            (
                "an array",
                json!([]),
                "parameter settings must be a settings object",
            ),
            (
                "a string",
                json!("set-basic"),
                "parameter settings must be a settings object",
            ),
            (
                "null",
                Value::Null,
                "parameter settings must be a settings object",
            ),
            (
                "no actions",
                json!({}),
                "parameter settings must name 1..=16 actions",
            ),
            (
                "seventeen actions",
                actions(MAX_SETTINGS_ACTIONS + 1),
                "parameter settings must name 1..=16 actions",
            ),
            (
                "a dotted action identity",
                json!({"set.basic": {"exposure": 1}}),
                "parameter settings names invalid action identity set.basic",
            ),
            (
                "an upper-case action identity",
                json!({"Set-Basic": {"exposure": 1}}),
                "parameter settings names invalid action identity Set-Basic",
            ),
            (
                "an empty field object",
                json!({"set-basic": {}}),
                "parameter settings must give action set-basic a non-empty object of fields",
            ),
            (
                "fields that are not an object",
                json!({"set-basic": 1}),
                "parameter settings must give action set-basic a non-empty object of fields",
            ),
            (
                "sixty-five fields",
                json!({ "set-basic": fields(MAX_SETTINGS_FIELDS + 1) }),
                "parameter settings gives action set-basic more than 64 fields",
            ),
            (
                "an invalid field name",
                json!({"set-basic": {"Exposure": 1}}),
                "parameter settings gives action set-basic invalid field name Exposure",
            ),
        ] {
            let error = check_value(&parameter, &value).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert_eq!(error.detail, fragment, "{case}");
        }
    }

    #[test]
    fn a_summary_renders_every_parameter_kind_the_way_a_history_row_reads_it() {
        let parameters = |value: Value| value.as_object().expect("an object").clone();
        for (case, template, values, expected) in [
            (
                "integers as written",
                "Pixel {x}, {y}",
                json!({"x": 12, "y": 0}),
                "Pixel 12, 0",
            ),
            (
                "numbers without trailing zeros",
                "Crop {angle}°",
                json!({"angle": 3.5}),
                "Crop 3.5°",
            ),
            (
                "a whole number",
                "Crop {angle}°",
                json!({"angle": 0.0}),
                "Crop 0°",
            ),
            (
                "a negative whole number",
                "Crop {angle}°",
                json!({"angle": -12.0}),
                "Crop -12°",
            ),
            (
                "three channels",
                "Colour {rgb}",
                json!({"rgb": [1, 2, 3]}),
                "Colour 1,2,3",
            ),
            (
                "an enum option",
                "{transform}",
                json!({"transform": "rotate-left"}),
                "Rotate left",
            ),
            (
                "a punctuated option",
                "Crop {aspect}",
                json!({"aspect": "16:9"}),
                "Crop 16:9",
            ),
            (
                "a one-word option",
                "Crop {aspect}",
                json!({"aspect": "free"}),
                "Crop Free",
            ),
            (
                "a boolean",
                "Linked {locked}",
                json!({"locked": true}),
                "Linked true",
            ),
            (
                "null",
                "Centre {center-x}",
                json!({ "center-x": Value::Null }),
                "Centre null",
            ),
            (
                "a parameter the request did not carry",
                "Crop {angle}°",
                json!({}),
                "Crop °",
            ),
            (
                "no placeholder at all",
                "Reset crop",
                json!({}),
                "Reset crop",
            ),
        ] {
            assert_eq!(
                render_summary(template, &parameters(values)),
                expected,
                "{case}"
            );
        }
    }

    #[test]
    fn a_label_is_the_rendered_summary_or_the_action_title() {
        let with_template = action();
        let mut without_template = action();
        without_template.summary = None;
        let parameters = json!({"x": 4, "mode": "fast"}).as_object().unwrap().clone();
        assert_eq!(action_label(&with_template, &parameters), "Thing 4 Fast");
        assert_eq!(action_label(&without_template, &parameters), "Set thing");
        let only_placeholder = ActionDescriptor {
            summary: Some("{x}".into()),
            ..action()
        };
        assert_eq!(
            action_label(&only_placeholder, &Map::new()),
            "Set thing",
            "a template that renders nothing falls back to the title"
        );
    }

    #[test]
    fn a_parameter_without_a_kind_is_a_validation_error() {
        let mut value = serde_json::to_value(descriptor()).unwrap();
        value["actions"][0]["parameters"][0]
            .as_object_mut()
            .unwrap()
            .remove("kind");
        let error = ModuleDescriptor::parse(&value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.contains("invalid module descriptor"),
            "{error}"
        );
        assert_eq!(
            ModuleDescriptor::parse(&serde_json::to_value(descriptor()).unwrap()).unwrap(),
            descriptor()
        );
    }

    /// Every declared stage keeps its wire name and is accepted by descriptor validation, so a
    /// colour-stage module declares itself exactly as a pixel or geometry one does, and the order a
    /// module takes among the layers of its stage travels with the effect.
    #[test]
    fn effect_stages_keep_their_serialized_names_and_validate() {
        for (stage, name, order) in [
            (EffectStage::Geometry, "geometry", 0),
            (EffectStage::Pixel, "pixel", 0),
            (EffectStage::Color, "color", 10),
            (EffectStage::Spatial, "spatial", 0),
            (EffectStage::Finish, "finish", 65535),
        ] {
            let effect = EffectDescriptor {
                id: "test.module.effect".into(),
                format: 1,
                stage,
                order,
                maskable: false,
                artifacts: false,
                single: false,
            };
            assert_eq!(serde_json::to_value(stage).unwrap(), json!(name));
            // `order` is always serialized, so `module.list` reports it for every effect.
            assert_eq!(
                serde_json::to_value(&effect).unwrap(),
                json!({"id": "test.module.effect", "format": 1, "stage": name, "order": order})
            );
            assert_eq!(
                serde_json::from_value::<EffectDescriptor>(serde_json::to_value(&effect).unwrap())
                    .unwrap(),
                effect
            );
            // An effect written without an order is the default earliest position of its stage.
            assert_eq!(
                serde_json::from_value::<EffectDescriptor>(
                    json!({"id": "test.module.effect", "format": 1, "stage": name})
                )
                .unwrap()
                .order,
                0
            );
            let descriptor = ModuleDescriptor {
                effects: vec![effect],
                ..descriptor()
            };
            assert!(descriptor.validate().is_ok(), "{name}");
            assert_eq!(
                ModuleDescriptor::parse(&serde_json::to_value(&descriptor).unwrap()).unwrap(),
                descriptor,
                "{name} round-trips through JSON"
            );
        }
    }

    #[test]
    fn number_parameters_and_the_crop_frame_canvas_keep_their_serialized_form() {
        assert_eq!(
            serde_json::to_value(number("angle", -45.0, 45.0)).unwrap(),
            json!({
                "name": "angle",
                "kind": "number",
                "min": -45.0,
                "max": 45.0,
                "required": true,
                "default": null,
                "unit": null,
                "step": null,
                "precision": null,
                "notes": "test",
            })
        );
        // The decimal hints a slider needs travel with the parameter and survive a round trip.
        let exposure = ParameterDescriptor {
            step: Some(0.01),
            precision: Some(2),
            unit: Some("EV".into()),
            ..number("exposure", -5.0, 5.0)
        };
        assert_eq!(
            serde_json::to_value(&exposure).unwrap(),
            json!({
                "name": "exposure",
                "kind": "number",
                "min": -5.0,
                "max": 5.0,
                "required": true,
                "default": null,
                "unit": "EV",
                "step": 0.01,
                "precision": 2,
                "notes": "test",
            })
        );
        assert_eq!(
            serde_json::from_value::<ParameterDescriptor>(serde_json::to_value(&exposure).unwrap())
                .unwrap(),
            exposure
        );
        // A descriptor written before the hints existed still reads, with neither hint declared.
        let without = serde_json::from_value::<ParameterDescriptor>(json!({
            "name": "exposure",
            "kind": "number",
            "min": -5.0,
            "max": 5.0,
            "required": true,
            "default": null,
            "unit": null,
            "notes": "test",
        }))
        .expect("the hints are optional");
        assert_eq!(without.step, None);
        assert_eq!(without.precision, None);
        let canvas = serde_json::to_value(frame_canvas("set-frame", "fit-frame")).unwrap();
        assert_eq!(
            canvas,
            json!({
                "kind": "crop-frame",
                "action": "set-frame",
                "angle": "angle",
                "x": "x",
                "y": "y",
                "width": "width",
                "height": "height",
                "fit_action": "fit-frame",
                "aspect": "aspect",
                "title": "Frame",
                "shortcut": "R",
            })
        );
        let descriptor = frame_descriptor();
        assert_eq!(
            ModuleDescriptor::parse(&serde_json::to_value(&descriptor).unwrap()).unwrap(),
            descriptor,
            "a crop-frame descriptor round-trips through JSON"
        );
    }

    #[test]
    fn number_parameters_accept_finite_values_in_range_and_reject_everything_else() {
        let action = ActionDescriptor {
            parameters: vec![
                number("angle", -45.0, 45.0),
                ParameterDescriptor {
                    required: false,
                    default: Some(json!(1.0)),
                    ..number("width", 0.0, 1.0)
                },
            ],
            ..action()
        };
        let checked = check_parameters(&action, &json!({"angle": -3.5})).unwrap();
        assert_eq!(checked["angle"], json!(-3.5), "a number passes through");
        assert_eq!(checked["width"], json!(1.0), "declared default applied");
        assert_eq!(
            check_parameters(&action, &json!({"angle": 0})).unwrap()["angle"],
            json!(0),
            "a JSON integer is accepted as a number and kept as written"
        );
        assert_eq!(
            check_parameters(&action, &json!({"angle": 45})).unwrap()["angle"],
            json!(45),
            "the range is closed"
        );
        for (case, input, fragment) in [
            (
                "above the range",
                json!({"angle": 45.0001}),
                "parameter angle must be a number within -45..=45",
            ),
            (
                "below the range",
                json!({"angle": -90}),
                "parameter angle must be a number within -45..=45",
            ),
            (
                "not a number",
                json!({"angle": "0"}),
                "parameter angle must be a number",
            ),
            (
                "a boolean",
                json!({"angle": true}),
                "parameter angle must be a number",
            ),
            (
                // NaN and the infinities are not JSON numbers; serde_json encodes them as null.
                "not finite",
                json!({"angle": f64::NAN}),
                "parameter angle must be a number",
            ),
            (
                "infinite",
                json!({"angle": f64::INFINITY}),
                "parameter angle must be a number",
            ),
        ] {
            let error = check_parameters(&action, &input).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
    }

    /// A patch action is validated field by field: what the caller named is checked and returned,
    /// nothing declared is filled in, and an action that is not a patch keeps its old behaviour.
    #[test]
    fn a_patch_action_validates_the_sent_fields_and_fills_no_defaults() {
        let parameter = |name: &str| ParameterDescriptor {
            required: false,
            default: Some(json!(0.0)),
            step: Some(0.01),
            precision: Some(2),
            ..number(name, -5.0, 5.0)
        };
        let patch = ActionDescriptor {
            summary: None,
            patch: true,
            parameters: vec![
                parameter("exposure"),
                // A required parameter of a patch is still not demanded: only what was sent counts.
                ParameterDescriptor {
                    required: true,
                    default: None,
                    ..number("contrast", -100.0, 100.0)
                },
            ],
            ..action()
        };
        assert!(
            ModuleDescriptor {
                actions: vec![patch.clone()],
                controls: Vec::new(),
                reset: None,
                ..descriptor()
            }
            .validate()
            .is_ok(),
            "declared steps and precisions on number parameters are accepted"
        );
        let checked = check_parameters(&patch, &json!({"exposure": -0.5})).unwrap();
        assert_eq!(
            checked,
            json!({"exposure": -0.5}).as_object().unwrap().clone()
        );
        assert!(
            check_parameters(&patch, &json!({})).unwrap().is_empty(),
            "an empty patch is a legal request that changes nothing"
        );
        assert!(
            check_parameters(&patch, &Value::Null).unwrap().is_empty(),
            "no parameters at all is the same empty patch"
        );
        for (case, sent, fragment) in [
            (
                "unknown field",
                json!({"vibrance": 1}),
                "unknown parameter vibrance",
            ),
            (
                "out of range",
                json!({"exposure": 6.0}),
                "parameter exposure must be a number within -5..=5",
            ),
            (
                "not finite",
                json!({"exposure": f64::NAN}),
                "parameter exposure must be a number",
            ),
            (
                "wrong kind",
                json!({"exposure": "0.5"}),
                "parameter exposure must be a number",
            ),
        ] {
            let error = check_parameters(&patch, &sent).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
        // The same parameters without the patch marker keep the whole-request behaviour.
        let whole = ActionDescriptor {
            patch: false,
            ..patch
        };
        let error = check_parameters(&whole, &json!({"exposure": -0.5})).expect_err("required");
        assert!(error.detail.contains("missing required parameter contrast"));
        assert_eq!(
            check_parameters(&whole, &json!({"contrast": 0.0})).unwrap()["exposure"],
            json!(0.0),
            "a declared default is applied when the action is not a patch"
        );
    }

    #[test]
    fn generic_parameter_checks_apply_defaults_and_name_the_broken_parameter() {
        let action = action();
        let checked = check_parameters(&action, &json!({"x": 3, "rgb": [1, 2, 3]})).unwrap();
        assert_eq!(checked["x"], json!(3));
        assert_eq!(checked["mode"], json!("exact"), "declared default applied");
        for (case, input, fragment) in [
            (
                "missing required",
                json!({"x": 1}),
                "missing required parameter rgb",
            ),
            (
                "unknown field",
                json!({"x": 1, "rgb": [1, 2, 3], "z": 4}),
                "unknown parameter z",
            ),
            (
                "out of range",
                json!({"x": 11, "rgb": [1, 2, 3]}),
                "parameter x must be an integer within 0..=10",
            ),
            (
                "not an integer",
                json!({"x": 1.5, "rgb": [1, 2, 3]}),
                "parameter x must be an integer",
            ),
            (
                "unknown enum option",
                json!({"x": 1, "rgb": [1, 2, 3], "mode": "sloppy"}),
                "parameter mode must be one of fast, exact",
            ),
            (
                "malformed color",
                json!({"x": 1, "rgb": [1, 2, 300]}),
                "parameter rgb must be three sRGB channels 0..=255",
            ),
            (
                "short color",
                json!({"x": 1, "rgb": [1, 2]}),
                "parameter rgb must be three sRGB channels 0..=255",
            ),
            ("not an object", json!([1, 2]), "must be a JSON object"),
        ] {
            let error = check_parameters(&action, &input).expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
    }

    fn controls_descriptor() -> ModuleDescriptor {
        let mut descriptor = descriptor();
        let bool_param = ParameterDescriptor {
            name: "enabled".into(),
            kind: ParameterKind::Boolean,
            required: false,
            default: Some(json!(false)),
            ..number("enabled", 0.0, 1.0)
        };
        let curve_kind = ParameterKind::Curve {
            points_min: 2,
            points_max: 4,
            monotone: true,
            fixed_x: Some(vec![0.0, 0.5, 1.0]),
        };
        let curve_param = ParameterDescriptor {
            name: "curve".into(),
            kind: curve_kind,
            required: false,
            default: Some(json!([[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]])),
            ..number("curve", 0.0, 1.0)
        };
        let mut numeric = number("amount", -10.0, 10.0);
        numeric.soft_min = Some(-5.0);
        numeric.soft_max = Some(5.0);
        numeric.fine_step = Some(0.01);
        numeric.zero = Some(0.0);
        let action = ActionDescriptor {
            id: "set-controls".into(),
            title: "Set controls".into(),
            notes: "test".into(),
            summary: None,
            patch: true,
            parameters: vec![
                bool_param,
                enumerated("mode"),
                curve_param.clone(),
                numeric,
                ParameterDescriptor {
                    kind: ParameterKind::Color,
                    ..number("rgb", 0.0, 1.0)
                },
            ],
        };
        descriptor.actions = vec![action];
        descriptor.queries = vec![ActionDescriptor {
            id: "sample-curve".into(),
            title: "Sample curve".into(),
            notes: "test".into(),
            summary: None,
            patch: false,
            parameters: vec![ParameterDescriptor {
                required: false,
                default: None,
                ..curve_param
            }],
        }];
        descriptor.controls = vec![
            Control::Toggle {
                action: "set-controls".into(),
                parameter: "enabled".into(),
                label: "Enabled".into(),
            },
            Control::Choice {
                action: "set-controls".into(),
                parameter: "mode".into(),
                label: "Mode".into(),
                style: ChoiceStyle::Menu,
            },
            Control::Number {
                action: "set-controls".into(),
                parameter: "amount".into(),
                label: "Amount".into(),
                style: NumberStyle::Stepper,
                rail: Some(RailDecoration::Hue),
                reset: None,
            },
            Control::Curve {
                action: "set-controls".into(),
                channels: vec![CurveChannel {
                    parameter: "curve".into(),
                    label: "Master".into(),
                }],
                label: "Curve".into(),
                sample_query: "sample-curve".into(),
                background: CurveBackground::Histogram,
            },
            Control::Action {
                action: "set-controls".into(),
                label: "Run".into(),
                preset: json!({"amount": 0.0}).as_object().unwrap().clone(),
                style: ActionStyle::Icon,
                icon: Some("rotate-left".into()),
            },
            Control::Color {
                action: "set-controls".into(),
                parameter: "rgb".into(),
                label: "Colour".into(),
                style: ColorStyle::Picker,
            },
            Control::Group {
                label: "More".into(),
                controls: Vec::new(),
                reset: None,
                collapsed: true,
            },
        ];
        descriptor.reset = None;
        descriptor
    }

    #[test]
    fn new_control_kinds_and_hints_round_trip_and_validate() {
        let descriptor = controls_descriptor();
        descriptor.validate().unwrap();
        let mut stepped_integer = integer("count");
        stepped_integer.step = Some(1.0);
        stepped_integer.fine_step = Some(0.1);
        stepped_integer.precision = Some(0);
        assert!(with_hints(None, None, stepped_integer).validate().is_ok());
        let mut stepped_curve = descriptor.actions[0].parameter("curve").unwrap().clone();
        stepped_curve.step = Some(0.01);
        stepped_curve.fine_step = Some(0.001);
        assert!(with_hints(None, None, stepped_curve).validate().is_ok());
        let serialized = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(serialized["controls"][0]["kind"], "toggle");
        assert_eq!(serialized["controls"][1]["style"], "menu");
        assert_eq!(serialized["controls"][2]["rail"], "hue");
        assert_eq!(serialized["controls"][3]["sample_query"], "sample-curve");
        assert_eq!(serialized["controls"][5]["style"], "picker");
        assert_eq!(serialized["controls"][6]["collapsed"], true);
        assert_eq!(serialized["actions"][0]["parameters"][3]["soft_min"], -5.0);
        assert_eq!(ModuleDescriptor::parse(&serialized).unwrap(), descriptor);
        let minimal: Control = serde_json::from_value(
            json!({"kind":"number","action":"set-controls","parameter":"amount","label":"Amount"}),
        )
        .unwrap();
        assert!(matches!(
            minimal,
            Control::Number {
                style: NumberStyle::Slider,
                rail: None,
                ..
            }
        ));
    }

    #[test]
    fn patch_action_buttons_name_one_field_but_declared_resets_may_name_a_group() {
        let mut descriptor = controls_descriptor();
        for (preset, description) in [
            (json!({}), "empty"),
            (json!({"amount": 0.0, "enabled": true}), "multiple"),
        ] {
            if let Control::Action { preset: fields, .. } = &mut descriptor.controls[4] {
                *fields = preset.as_object().unwrap().clone();
            }
            let error = descriptor.validate().expect_err(description);
            assert_eq!(error.kind, ErrorKind::Validation, "{description}");
            assert_eq!(
                error.detail,
                "action control for patch action set-controls needs exactly one preset field",
                "{description}"
            );
        }
        // A group reset is a separate declared gesture, and may intentionally restore several
        // parameters of a patch action at once without making a button's patch ambiguous.
        descriptor.controls[4] = Control::Action {
            action: "set-controls".into(),
            label: "Run".into(),
            preset: json!({"amount": 0.0}).as_object().unwrap().clone(),
            style: ActionStyle::Icon,
            icon: Some("rotate-left".into()),
        };
        descriptor.reset = Some(ResetAction {
            action: "set-controls".into(),
            preset: json!({"amount": 0.0, "enabled": false})
                .as_object()
                .unwrap()
                .clone(),
        });
        descriptor.validate().expect("a reset may restore a group");
    }

    #[test]
    fn invalid_new_bindings_and_hints_name_the_control_or_parameter() {
        let base = controls_descriptor();
        let mut malformed = serde_json::to_value(&base).unwrap();
        malformed["controls"][0]["rail"] = json!("hue");
        let error = ModuleDescriptor::parse(&malformed).expect_err("rail on a toggle");
        assert!(
            error
                .detail
                .contains("toggle control for enabled of action set-controls")
        );
        for (case, edit, fragment) in [
            (
                "toggle",
                Box::new(|d: &mut ModuleDescriptor| {
                    if let Control::Toggle { parameter, .. } = &mut d.controls[0] {
                        *parameter = "amount".into();
                    }
                }) as Box<dyn Fn(&mut ModuleDescriptor)>,
                "toggle control for amount",
            ),
            (
                "choice",
                Box::new(|d: &mut ModuleDescriptor| {
                    if let Control::Choice { parameter, .. } = &mut d.controls[1] {
                        *parameter = "enabled".into();
                    }
                }),
                "choice control for enabled",
            ),
            (
                "rail",
                Box::new(|d: &mut ModuleDescriptor| {
                    if let Control::Number { parameter, .. } = &mut d.controls[2] {
                        *parameter = "enabled".into();
                    }
                }),
                "number control for enabled",
            ),
            (
                "curve",
                Box::new(|d: &mut ModuleDescriptor| {
                    if let Control::Curve { channels, .. } = &mut d.controls[3] {
                        channels[0].parameter = "enabled".into();
                    }
                }),
                "curve control for enabled",
            ),
            (
                "query",
                Box::new(|d: &mut ModuleDescriptor| {
                    if let Control::Curve { sample_query, .. } = &mut d.controls[3] {
                        *sample_query = "missing".into();
                    }
                }),
                "undeclared query missing",
            ),
            (
                "icon",
                Box::new(|d: &mut ModuleDescriptor| {
                    if let Control::Action { icon, .. } = &mut d.controls[4] {
                        *icon = Some("Bad_Icon".into());
                    }
                }),
                "invalid icon name Bad_Icon",
            ),
            (
                "soft",
                Box::new(|d: &mut ModuleDescriptor| {
                    d.actions[0].parameters[3].soft_min = Some(-11.0)
                }),
                "parameter amount declares a soft range",
            ),
            (
                "fine",
                Box::new(|d: &mut ModuleDescriptor| {
                    d.actions[0].parameters[3].fine_step = Some(0.0)
                }),
                "parameter amount declares a fine step",
            ),
            (
                "zero",
                Box::new(|d: &mut ModuleDescriptor| d.actions[0].parameters[3].zero = Some(11.0)),
                "parameter amount declares a zero",
            ),
        ] {
            let mut d = base.clone();
            edit(&mut d);
            let error = d.validate().expect_err(case);
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
    }

    #[test]
    fn boolean_and_curve_requests_keep_exact_values_and_reject_malformed_points() {
        let d = controls_descriptor();
        let action = &d.actions[0];
        let valid = json!({"enabled": true, "curve": [[0.0, 0.0], [0.5, 0.49], [1.0, 1.0]]});
        assert_eq!(
            check_parameters(action, &valid).unwrap(),
            valid.as_object().unwrap().clone()
        );
        for (case, value) in [
            ("boolean", json!({"enabled": 1})),
            ("curve shape", json!({"curve": [0.0, 1.0]})),
            (
                "curve order",
                json!({"curve": [[0.0, 0.0], [0.0, 0.5], [1.0, 1.0]]}),
            ),
            (
                "curve monotone",
                json!({"curve": [[0.0, 0.0], [0.5, 0.8], [1.0, 0.7]]}),
            ),
            (
                "curve fixed x",
                json!({"curve": [[0.0, 0.0], [0.4, 0.5], [1.0, 1.0]]}),
            ),
            (
                "curve range",
                json!({"curve": [[0.0, 0.0], [0.5, 1.1], [1.0, 1.0]]}),
            ),
        ] {
            let error = check_parameters(action, &value).expect_err(case);
            assert!(
                error.detail.contains(if case == "boolean" {
                    "enabled"
                } else {
                    "curve"
                }),
                "{case}: {error}"
            );
        }
    }

    /// A descriptor with two top-level groups, each one slider, over the base descriptor's own
    /// declared action: the minimal shape `layout: tabs` accepts.
    fn two_group_descriptor() -> ModuleDescriptor {
        let group = |label: &str| Control::Group {
            label: label.into(),
            reset: None,
            controls: vec![Control::Number {
                action: "set-thing".into(),
                parameter: "x".into(),
                label: "X".into(),
                style: crate::NumberStyle::Slider,
                rail: None,
                reset: None,
            }],
            collapsed: false,
        };
        ModuleDescriptor {
            controls: vec![group("First"), group("Second")],
            ..descriptor()
        }
    }

    #[test]
    fn layout_defaults_to_stacked_and_tabs_needs_at_least_two_top_level_groups() {
        let stacked = descriptor();
        assert_eq!(
            stacked.layout,
            ModuleLayout::Stacked,
            "the default is stacked"
        );
        stacked.validate().expect("stacked is always accepted");

        let tabs = ModuleDescriptor {
            layout: ModuleLayout::Tabs,
            ..two_group_descriptor()
        };
        tabs.validate()
            .expect("tabs is accepted over at least two top-level groups");

        let one_group = ModuleDescriptor {
            layout: ModuleLayout::Tabs,
            ..descriptor()
        };
        let error = one_group
            .validate()
            .expect_err("tabs needs at least two top-level groups");
        assert!(error.detail.contains("layout: tabs"), "{error}");

        let mut non_group = two_group_descriptor();
        non_group.layout = ModuleLayout::Tabs;
        non_group.controls.push(Control::Number {
            action: "set-thing".into(),
            parameter: "x".into(),
            label: "X".into(),
            style: crate::NumberStyle::Slider,
            rail: None,
            reset: None,
        });
        let error = non_group
            .validate()
            .expect_err("tabs needs every top-level control to be a group");
        assert!(error.detail.contains("layout: tabs"), "{error}");
    }

    #[test]
    fn layout_round_trips_through_json_and_rejects_an_unknown_value() {
        let tabs = ModuleDescriptor {
            layout: ModuleLayout::Tabs,
            ..two_group_descriptor()
        };
        let serialized = serde_json::to_value(&tabs).unwrap();
        assert_eq!(serialized["layout"], json!("tabs"));
        assert_eq!(ModuleDescriptor::parse(&serialized).unwrap(), tabs);
        assert_eq!(
            serde_json::to_value(descriptor())
                .unwrap()
                .get("layout")
                .cloned(),
            Some(json!("stacked")),
            "an absent layout serializes as stacked, never omitted"
        );

        let mut malformed = serialized.clone();
        malformed["layout"] = json!("floating");
        let error =
            ModuleDescriptor::parse(&malformed).expect_err("an unknown layout value is rejected");
        assert_eq!(error.kind, ErrorKind::Validation);
    }
}
