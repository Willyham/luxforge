//! The descriptor shapes a module declares and a client reads: effects, parameters, actions, the
//! control vocabulary with its variants and their resolution, canvas interactions and the module
//! descriptor itself. Plain data, serialized exactly as `module.list` publishes it.
use super::labels::not_applicable;
use crate::{
    Error, ErrorKind, SourceTag,
    capabilities::{
        descriptor::{
            CapabilityDescriptor, ResourceDescriptor, SettingsDescriptor, TaskDescriptor,
        },
        endpoint::EndpointClass,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The parameters a `presets` control submits its action with: the settings set, the preset's name
/// and, optionally, the library identity it came from.
pub(crate) const PRESET_SETTINGS: &str = "settings";
pub(crate) const PRESET_NAME: &str = "name";
pub(crate) const PRESET_ID: &str = "preset-id";

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

/// Where an effect acts, and so where the host puts a new layer. The stages run in this order:
/// a source effect prepares the content stage at index zero; pixel and colour effects address that
/// content stage and join the stack before everything that follows; a spatial effect reads a
/// bounded neighbourhood of the content stage, so it follows the pointwise work; a geometry effect
/// changes the stage and extends the geometry tail; and a finish effect is evaluated last, in the
/// output coordinates the tail produced. `crate::ModuleRegistry::insertion_index` states the
/// placement rule each stage gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectStage {
    Source,
    Geometry,
    Pixel,
    /// Bounded pre-tone restoration in content coordinates.
    Restoration,
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
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Geometry => "geometry",
            Self::Pixel => "pixel",
            Self::Restoration => "restoration",
            Self::Color => "color",
            Self::Spatial => "spatial",
            Self::Finish => "finish",
        }
    }
}

/// A durable effect identity stored in every layer, with its internal payload format marker and the
/// order it takes among layers of its own stage.
/// Which pixels a settled Fit preview presents for a non-empty operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FitSettle {
    #[default]
    Proxy,
    Exact,
}

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
    #[serde(default, skip_serializing_if = "is_default")]
    pub fit_settle: FitSettle,
    /// The source kinds a layer of this effect may exist on, named by the `kind` tags `asset.state`
    /// reports for a photo's source (`jpeg`, `raw`). Empty, the default, is every kind, and is not
    /// serialized, so an effect that exists on every photo describes itself exactly as it did
    /// before kinds were declared. A module applies to a photo when any of its effects does
    /// ([`ModuleDescriptor::applies_to`]); the host refuses an action of a module that does not
    /// apply, and admission refuses a layer on a kind its effect does not list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceTag>,
}

impl EffectDescriptor {
    /// An effect of `stage` at the current payload format ([`crate::EFFECT_FORMAT`]) and order
    /// `0`, on every source kind, neither maskable nor single and with no artifacts: a base for
    /// struct update, which names each field the effect declares otherwise.
    pub(crate) fn new(id: impl Into<String>, stage: EffectStage) -> Self {
        Self {
            id: id.into(),
            format: crate::EFFECT_FORMAT,
            stage,
            order: 0,
            maskable: false,
            artifacts: false,
            single: false,
            fit_settle: FitSettle::default(),
            sources: Vec::new(),
        }
    }

    /// Whether a layer of this effect may exist on a photo of `kind`: `O(sources)`, no allocation.
    pub(crate) fn applies_to(&self, kind: SourceTag) -> bool {
        self.sources.is_empty() || self.sources.contains(&kind)
    }
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
    /// A network destination: a URL of at most `MAX_ENDPOINT_BYTES` that the capability
    /// transport's policy accepts as one of `classes`. Only a module setting declares one, and only
    /// a provider profile's, because only the host contacts anything and a destination is the
    /// person's choice: an action, query or task that declared one would let a request name where
    /// the host sends data. It never has a default.
    Endpoint {
        classes: Vec<EndpointClass>,
    },
    /// A credential of 1..=`max_length` characters (at most `MAX_SECRET_LENGTH`), held only by the
    /// OS secret store and reported only as present or absent. Only a module setting declares one,
    /// so the one request that carries a secret is `module.settings.set-secret`, which is what
    /// makes redaction structural: no recipe, history entry, draft or job parameter can hold one.
    /// No plain value is ever valid for it, and it never has a default.
    Secret {
        max_length: usize,
    },
    /// The identity of one host object a request addresses — a mask, a component inside one, or a
    /// stroke by its content address — as the opaque string the host minted for it. The generic
    /// check validates its syntax; the command that resolves it refuses one the stack does not hold.
    /// An identity addresses state rather than setting it, so a required one stays required on a
    /// patch, which demands no other field.
    Identity {
        of: IdentityKind,
    },
    /// UTF-8 text of at most `max_bytes` bytes that, unlike a [`ParameterKind::String`], may hold
    /// control characters such as line breaks: a file's contents or a filesystem path. Only a host
    /// method declares one, because only the host reads files.
    Text {
        max_bytes: usize,
    },
    /// Any JSON value: a structured field of a host method, such as a zoom, an analysis target or
    /// a draft's fields, whose shape the parameter's notes give and whose own type checks it when
    /// the request is parsed. The one kind for a structure no other kind describes, so a host
    /// method never needs a second vocabulary; only a host method declares one, because a module's
    /// parameters are checked here and a value this kind accepts would reach the module unchecked.
    Json,
}

/// Which host object a [`ParameterKind::Identity`] parameter names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdentityKind {
    /// A photo in the catalog, `asset-…`.
    Asset,
    /// One history entry of a photo, `entry-…`.
    Entry,
    /// A client's open draft, `draft-…`.
    Draft,
    /// A job of the one job table, `job-…`.
    Job,
    /// A preset of the library, `preset-…`.
    Preset,
    /// A mask of the recipe, `mask-…`.
    Mask,
    /// A component of one mask, `component-…`.
    Component,
    /// One stroke of a brush component, by its content address.
    Stroke,
}

impl IdentityKind {
    /// The object with its article, as a refusal names it: `an asset`, `a mask`.
    pub(crate) fn with_article(self) -> &'static str {
        match self {
            Self::Asset => "an asset",
            Self::Entry => "an entry",
            Self::Draft => "a draft",
            Self::Job => "a job",
            Self::Preset => "a preset",
            Self::Mask => "a mask",
            Self::Component => "a component",
            Self::Stroke => "a stroke",
        }
    }

    /// Whether `text` is an identity of this kind, by the check the identity type owns, so the
    /// generic check and the command that resolves the identity cannot disagree about its shape.
    /// The minted identities are checked in place, without allocating, because every host request
    /// that names an asset, entry or draft is checked here.
    pub(crate) fn accepts(self, text: &str) -> bool {
        match self {
            Self::Asset => crate::AssetId::is_valid(text),
            Self::Entry => crate::EntryId::is_valid(text),
            Self::Draft => crate::DraftId::is_valid(text),
            Self::Job => crate::JobId::is_valid(text),
            Self::Preset => crate::PresetId::is_valid(text),
            Self::Mask => crate::MaskId::is_valid(text),
            Self::Component => crate::ComponentId::is_valid(text),
            Self::Stroke => crate::path::StrokeId::parse(text).is_ok(),
        }
    }
}

/// The longest endpoint URL a value may be before it is parsed, in bytes.
pub(super) const MAX_ENDPOINT_BYTES: usize = 4096;
/// The longest secret a module may declare, in characters.
pub(crate) const MAX_SECRET_LENGTH: usize = 4096;

impl ParameterKind {
    /// The kind's tag as it is serialized, for messages.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Integer { .. } => "integer",
            Self::Number { .. } => "number",
            Self::Enum { .. } => "enum",
            Self::Color => "color",
            Self::Boolean => "boolean",
            Self::Points { .. } => "points",
            Self::Artifact => "artifact",
            Self::Curve { .. } => "curve",
            Self::String { .. } => "string",
            Self::Settings => "settings",
            Self::Endpoint { .. } => "endpoint",
            Self::Secret { .. } => "secret",
            Self::Identity { .. } => "identity",
            Self::Text { .. } => "text",
            Self::Json => "json",
        }
    }

    /// Whether only a module setting may declare this kind: see [`Self::Endpoint`] and
    /// [`Self::Secret`].
    pub(crate) fn setting_only(&self) -> bool {
        matches!(self, Self::Endpoint { .. } | Self::Secret { .. })
    }

    /// Whether only the host declares this kind, for its own objects' commands and its own
    /// methods: see [`Self::Identity`], [`Self::Text`] and [`Self::Json`]. A module's action,
    /// query or task, and a module setting, refuse it.
    pub(crate) fn host_only(&self) -> bool {
        matches!(self, Self::Identity { .. } | Self::Text { .. } | Self::Json)
    }

    /// Whether this parameter names an object rather than setting a value, which is what keeps a
    /// required one required on a patch.
    pub fn is_identity(&self) -> bool {
        matches!(self, Self::Identity { .. })
    }
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

/// The largest pixel coordinate a parameter can address: the decoder accepts at most 16384 pixels
/// per side, so no stage has a larger one.
pub(super) const MAX_COORDINATE: i64 = 16383;

/// A descriptor is built from one kind constructor — [`Self::number`], [`Self::integer`] and the
/// rest, each starting optional with every hint unset and empty notes — and chained hints:
/// `ParameterDescriptor::number("exposure", -5.0, 5.0).required(true).unit("EV").step(0.1)`.
impl ParameterDescriptor {
    /// A parameter of `kind`: optional, every hint unset, empty notes.
    pub fn new(name: impl Into<String>, kind: ParameterKind) -> Self {
        Self {
            name: name.into(),
            kind,
            required: false,
            default: None,
            unit: None,
            step: None,
            precision: None,
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
            notes: String::new(),
        }
    }

    pub fn number(name: impl Into<String>, min: f64, max: f64) -> Self {
        Self::new(name, ParameterKind::Number { min, max })
    }

    pub fn integer(name: impl Into<String>, min: i64, max: i64) -> Self {
        Self::new(name, ParameterKind::Integer { min, max })
    }

    pub fn enumeration<S: Into<String>>(
        name: impl Into<String>,
        options: impl IntoIterator<Item = S>,
    ) -> Self {
        Self::new(
            name,
            ParameterKind::Enum {
                options: options.into_iter().map(Into::into).collect(),
            },
        )
    }

    pub fn color(name: impl Into<String>) -> Self {
        Self::new(name, ParameterKind::Color)
    }

    pub fn boolean(name: impl Into<String>) -> Self {
        Self::new(name, ParameterKind::Boolean)
    }

    pub(crate) fn points(name: impl Into<String>, points_min: usize, points_max: usize) -> Self {
        Self::new(
            name,
            ParameterKind::Points {
                points_min,
                points_max,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn artifact(name: impl Into<String>) -> Self {
        Self::new(name, ParameterKind::Artifact)
    }

    /// A non-monotone curve with no fixed `x`s; chain [`Self::monotone`] to make it monotone.
    pub fn curve(name: impl Into<String>, points_min: usize, points_max: usize) -> Self {
        Self::new(
            name,
            ParameterKind::Curve {
                points_min,
                points_max,
                monotone: false,
                fixed_x: None,
            },
        )
    }

    pub(crate) fn string(name: impl Into<String>, max_length: usize) -> Self {
        Self::new(name, ParameterKind::String { max_length })
    }

    pub(crate) fn settings(name: impl Into<String>) -> Self {
        Self::new(name, ParameterKind::Settings)
    }

    pub(crate) fn endpoint(
        name: impl Into<String>,
        classes: impl IntoIterator<Item = EndpointClass>,
    ) -> Self {
        Self::new(
            name,
            ParameterKind::Endpoint {
                classes: classes.into_iter().collect(),
            },
        )
    }

    pub(crate) fn secret(name: impl Into<String>, max_length: usize) -> Self {
        Self::new(name, ParameterKind::Secret { max_length })
    }

    /// The identity of one host object of kind `of`.
    pub(crate) fn identity(name: impl Into<String>, of: IdentityKind) -> Self {
        Self::new(name, ParameterKind::Identity { of })
    }

    /// A pixel coordinate of a stage, `0..=MAX_COORDINATE` px, required: a descriptor cannot know
    /// the stage a particular asset produces, so a point outside it is refused when it is asked.
    pub(crate) fn pixel_coordinate(name: impl Into<String>) -> Self {
        Self::integer(name, 0, MAX_COORDINATE)
            .required(true)
            .unit("px")
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    pub fn default(mut self, value: impl Into<Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn step(mut self, step: f64) -> Self {
        self.step = Some(step);
        self
    }

    pub fn precision(mut self, precision: u8) -> Self {
        self.precision = Some(precision);
        self
    }

    pub fn soft_min(mut self, soft_min: f64) -> Self {
        self.soft_min = Some(soft_min);
        self
    }

    pub fn soft_max(mut self, soft_max: f64) -> Self {
        self.soft_max = Some(soft_max);
        self
    }

    pub fn fine_step(mut self, fine_step: f64) -> Self {
        self.fine_step = Some(fine_step);
        self
    }

    pub fn zero(mut self, zero: f64) -> Self {
        self.zero = Some(zero);
        self
    }

    pub fn notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = notes.into();
        self
    }

    /// Only meaningful on a [`ParameterKind::Curve`]; a no-op on any other kind.
    pub fn monotone(mut self) -> Self {
        if let ParameterKind::Curve { monotone, .. } = &mut self.kind {
            *monotone = true;
        }
        self
    }
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

/// The control another module provides in a control's place on a photo of one source kind, or, on a
/// group, the reset it provides in the group's. A number, action or picker control carries a
/// `control` of its own shape over the named module's own actions or pick canvas; a group carries a
/// `reset` of that module. A variant applies only on the global target of a photo of its kind: a
/// mask target always uses the base control ([`resolve_control`]). Registration checks each one
/// against the complete registry (`crate::ModuleRegistry::check_complete`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlVariant {
    /// The source kind this variant applies to, named by the `kind` tag `asset.state` reports.
    pub source: SourceTag,
    /// The module whose control this is: another registered module that applies to `source`.
    pub module: String,
    /// The replacement of a number, action or picker control, of the same kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Box<Control>>,
    /// The replacement of a group's reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetAction>,
}

impl ControlVariant {
    /// A control `module` provides in a control's place on a photo of `source`.
    pub(crate) fn control(
        source: SourceTag,
        module: impl Into<String>,
        control: impl Into<Control>,
    ) -> Self {
        Self {
            source,
            module: module.into(),
            control: Some(Box::new(control.into())),
            reset: None,
        }
    }

    /// A reset `module` provides in a group's place on a photo of `source`.
    pub(crate) fn reset(source: SourceTag, module: impl Into<String>, reset: ResetAction) -> Self {
        Self {
            source,
            module: module.into(),
            control: None,
            reset: Some(reset),
        }
    }
}

/// A control as it applies to one photo and target: the module that provides it and the control
/// itself, the base or one of its variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedControl<'d> {
    /// The identity of the module whose actions or canvas the control addresses.
    pub module: &'d str,
    pub control: &'d Control,
    /// Whether this is a variant rather than the declaring module's own control.
    pub variant: bool,
}

/// A group reset as it applies to one photo and target, and the module whose action it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedReset<'d> {
    pub module: &'d str,
    pub reset: &'d ResetAction,
    pub variant: bool,
}

/// The one rule that resolves a control for a photo and a target: `control`, declared by the module
/// `owner`, as it applies to a photo of `kind` edited through `mask`.
///
/// A variant applies on the global target (`mask` is `None`) of a photo of its kind; with no photo
/// (`kind` is `None`), on a mask target, or without a variant for the kind, the base control applies
/// and `owner` provides it. Every client resolves through this — the desktop's sections, the parity
/// test and the host's own derivations of superseded fields — so nothing names a module to choose.
/// `O(variants)`; reads no stack.
pub fn resolve_control<'d>(
    owner: &'d str,
    control: &'d Control,
    kind: Option<SourceTag>,
    mask: Option<&crate::MaskId>,
) -> ResolvedControl<'d> {
    let variant = match (kind, mask) {
        (Some(kind), None) => control
            .variants()
            .iter()
            .find(|variant| variant.source == kind)
            .and_then(|variant| Some((variant.module.as_str(), variant.control.as_deref()?))),
        _ => None,
    };
    match variant {
        Some((module, control)) => ResolvedControl {
            module,
            control,
            variant: true,
        },
        None => ResolvedControl {
            module: owner,
            control,
            variant: false,
        },
    }
}

/// [`resolve_control`] for a group's reset: the reset `group`, declared by `owner`, runs for a photo
/// of `kind` edited through `mask`, or `None` when `group` is not a group or declares no reset.
pub fn resolve_group_reset<'d>(
    owner: &'d str,
    group: &'d Control,
    kind: Option<SourceTag>,
    mask: Option<&crate::MaskId>,
) -> Option<ResolvedReset<'d>> {
    let Control::Group(GroupControl {
        reset, variants, ..
    }) = group
    else {
        return None;
    };
    if let (Some(kind), None) = (kind, mask)
        && let Some((module, reset)) = variants
            .iter()
            .find(|variant| variant.source == kind)
            .and_then(|variant| Some((variant.module.as_str(), variant.reset.as_ref()?)))
    {
        return Some(ResolvedReset {
            module,
            reset,
            variant: true,
        });
    }
    reset.as_ref().map(|reset| ResolvedReset {
        module: owner,
        reset,
        variant: false,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDescriptor {
    pub id: String,
    pub title: String,
    pub notes: String,
    /// A field patch: the generic check validates the fields the caller sent and fills no declared
    /// defaults, so the module receives exactly those fields and merges them over its own stored
    /// state. Every parameter of a patch action is optional, whatever it declares.
    #[serde(default)]
    pub patch: bool,
    /// Whether this action may be captured or applied by the preset service.
    #[serde(default = "preset_default", skip_serializing_if = "preset_enabled")]
    pub preset: bool,
    pub parameters: Vec<ParameterDescriptor>,
}

fn preset_default() -> bool {
    true
}
fn preset_enabled(value: &bool) -> bool {
    *value
}

impl ActionDescriptor {
    /// An action with no parameters that is not a field patch: a base for struct update, which
    /// names `parameters` and `patch` when the action declares them.
    pub fn new(id: impl Into<String>, title: impl Into<String>, notes: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            notes: notes.into(),
            patch: false,
            preset: true,
            parameters: Vec::new(),
        }
    }

    pub fn parameter(&self, name: &str) -> Option<&ParameterDescriptor> {
        self.parameters
            .iter()
            .find(|parameter| parameter.name == name)
    }
}

/// Ordered semantic controls. A client renders them; it never invents an operation of its own.
///
/// Each kind is its own struct, built by its constructor here (`Control::number`,
/// `Control::choice`, …) and completed by the setters that kind declares, then made a `Control`
/// with `.into()`. A setter of another kind is not there to call:
///
/// ```
/// use luxforge_core::{ChoiceStyle, Control, NumberStyle};
/// let field: Control = Control::number("set-x", "x", "X").number_style(NumberStyle::Field).into();
/// let menu: Control = Control::choice("set-x", "mode", "Mode").choice_style(ChoiceStyle::Menu).into();
/// # let _ = (field, menu);
/// ```
///
/// ```compile_fail,E0599
/// use luxforge_core::{Control, NumberStyle};
/// // A choice control has no number style.
/// let menu: Control = Control::choice("set-x", "mode", "Mode").number_style(NumberStyle::Field).into();
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Control {
    Group(GroupControl),
    Number(NumberControl),
    Toggle(ToggleControl),
    Choice(ChoiceControl),
    QueryChoice(QueryChoiceControl),
    Color(ColorControl),
    Curve(CurveControl),
    Range(RangeControl),
    Action(ActionControl),
    Picker(PickerControl),
    Task(TaskControl),
    Presets(PresetsControl),
}

/// A titled group of controls, with the reset that returns its fields to neutral.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupControl {
    pub label: String,
    pub controls: Vec<Control>,
    /// The action that returns this group to its neutral values, shown on the group header.
    #[serde(default)]
    pub reset: Option<ResetAction>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub collapsed: bool,
    /// The group reset another module provides on a photo of one source kind: each variant
    /// carries a `reset` and applies on the global target of a photo of its kind
    /// ([`resolve_group_reset`]). Listed only when declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<ControlVariant>,
}

impl GroupControl {
    /// The action the group header's reset runs.
    pub(crate) fn reset(self, reset: ResetAction) -> Self {
        Self {
            reset: Some(reset),
            ..self
        }
    }

    pub(crate) fn collapsed(self, collapsed: bool) -> Self {
        Self { collapsed, ..self }
    }

    /// A replacement reset for one source kind ([`ControlVariant::reset`]).
    pub(crate) fn variant(mut self, variant: ControlVariant) -> Self {
        self.variants.push(variant);
        self
    }
}

/// One integer or number parameter, drawn as a slider, a field or a stepper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumberControl {
    pub action: String,
    pub parameter: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: NumberStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail: Option<RailDecoration>,
    /// What resetting this one field runs — a double-click on its label or rail, or its field's
    /// own reset — when that is not the field's declared default: an action of this module with
    /// fixed parameters, validated like a group's reset. Without it the field resets to its
    /// parameter's declared default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetAction>,
    /// The control another module provides in this place on a photo of one source kind
    /// (`ControlVariant`). Listed only when declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<ControlVariant>,
}

impl NumberControl {
    pub fn number_style(self, style: NumberStyle) -> Self {
        Self { style, ..self }
    }

    pub fn rail(self, rail: RailDecoration) -> Self {
        Self {
            rail: Some(rail),
            ..self
        }
    }

    /// What resetting this one field runs in place of its declared default.
    pub(crate) fn field_reset(self, reset: ResetAction) -> Self {
        Self {
            reset: Some(reset),
            ..self
        }
    }

    /// A replacement number control for one source kind ([`ControlVariant::control`]).
    pub(crate) fn variant(mut self, variant: ControlVariant) -> Self {
        self.variants.push(variant);
        self
    }
}

/// One boolean parameter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToggleControl {
    pub action: String,
    pub parameter: String,
    pub label: String,
}

/// One enum parameter, drawn as segments, chips or a menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceControl {
    pub action: String,
    pub parameter: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: ChoiceStyle,
}

impl ChoiceControl {
    pub fn choice_style(self, style: ChoiceStyle) -> Self {
        Self { style, ..self }
    }
}

/// One colour parameter, drawn as three fields or a picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorControl {
    pub action: String,
    pub parameter: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: ColorStyle,
}

impl ColorControl {
    pub fn color_style(self, style: ColorStyle) -> Self {
        Self { style, ..self }
    }
}

/// Curve parameters of one action, one per channel, edited on one curve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveControl {
    pub action: String,
    pub channels: Vec<CurveChannel>,
    pub label: String,
    /// Query receiving the active channel's point list and returning sampled fractions.
    pub sample_query: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub background: CurveBackground,
}

impl CurveControl {
    pub fn background(self, background: CurveBackground) -> Self {
        Self { background, ..self }
    }
}

/// A band on one axis: two thumbs for its `low` and `high` edges and, when declared, a shoulder
/// grip outside each for its `low_feather` and `high_feather` widths. All are `number` parameters
/// of `action`; `low` and `high` declare the same range, which is the axis the rail spans, and each
/// feather is a width on that axis. Every thumb or grip edits its own parameter exactly as a slider
/// edits its one: it drafts while dragged and commits once on release, so the band is four ordinary
/// fields and never a request of its own. A client draws the four parameters' own number fields
/// under it, because a typed value is exact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeControl {
    pub action: String,
    pub low: String,
    pub high: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_feather: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_feather: Option<String>,
    pub label: String,
    /// The rail under the band, as a number control's `rail` decorates its rail: a luminance
    /// band's is black to white.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail: Option<RailDecoration>,
}

impl RangeControl {
    /// The two shoulder parameters.
    pub(crate) fn feathers(
        self,
        low_feather: impl Into<String>,
        high_feather: impl Into<String>,
    ) -> Self {
        Self {
            low_feather: Some(low_feather.into()),
            high_feather: Some(high_feather.into()),
            ..self
        }
    }

    pub(crate) fn rail(self, rail: RailDecoration) -> Self {
        Self {
            rail: Some(rail),
            ..self
        }
    }
}

/// A button that submits one action with fixed parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionControl {
    pub action: String,
    pub label: String,
    #[serde(default)]
    pub preset: Map<String, Value>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: ActionStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The control another module provides in this place on a photo of one source kind
    /// (`ControlVariant`). Listed only when declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<ControlVariant>,
}

impl ActionControl {
    /// The parameters the button submits.
    pub fn preset(self, preset: Map<String, Value>) -> Self {
        Self { preset, ..self }
    }

    pub fn action_style(self, style: ActionStyle) -> Self {
        Self { style, ..self }
    }

    pub fn icon(self, icon: impl Into<String>) -> Self {
        Self {
            icon: Some(icon.into()),
            ..self
        }
    }

    /// A replacement action control for one source kind ([`ControlVariant::control`]).
    pub(crate) fn variant(mut self, variant: ControlVariant) -> Self {
        self.variants.push(variant);
        self
    }
}

/// The module's own canvas pick, offered beside the controls that pick fills rather than in a
/// mode strip. It binds to the [`CanvasInteraction`] this module declares — a `point-pick` or a
/// `sample-apply`, never a `crop-frame` — and carries no action of its own: entering and leaving
/// the mode is `workspace.set`, and the pick itself is what the canvas declares. A module declares
/// at most one. A module that declares a pick canvas declares one, or is reached by another
/// module's picker variant, so every pick mode is reachable from the panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickerControl {
    pub label: String,
    /// The picker another module provides in this place on a photo of one source kind, which
    /// enters that module's own pick canvas (`ControlVariant`). Listed only when declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<ControlVariant>,
}

impl PickerControl {
    /// A replacement picker for one source kind ([`ControlVariant::control`]).
    pub(crate) fn variant(mut self, variant: ControlVariant) -> Self {
        self.variants.push(variant);
        self
    }
}

/// A button that runs one of this module's own worker tasks through the client's consent and
/// progress flow; when the task declares `apply`, the client offers Apply with its result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskControl {
    pub task: String,
    pub label: String,
}

/// The host's preset library. Choosing a preset submits `action` once with that preset's
/// `settings`, `name` and `preset-id`, so applying one is the same API call every client makes.
/// The action is this module's own, with a required `settings` parameter of kind `settings`, a
/// required `name` of kind `string`, optionally a `preset-id` of kind `string` and nothing else. A
/// module declares at most one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetsControl {
    pub action: String,
}

/// Search and paging supplied by this module's query; an eligible row submits its key to this
/// module's non-patch action. Shared inputs have equal kinds in both declarations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryChoiceControl {
    pub label: String,
    pub query: String,
    pub text: String,
    pub page: String,
    pub action: String,
    pub key: String,
    #[serde(default)]
    pub shared: Vec<String>,
}

macro_rules! control_kinds {
    ($($kind:ident($control:ident)),* $(,)?) => {
        $(
            impl From<$control> for Control {
                fn from(control: $control) -> Self {
                    Self::$kind(control)
                }
            }
        )*
    };
}

control_kinds!(
    Group(GroupControl),
    Number(NumberControl),
    Toggle(ToggleControl),
    Choice(ChoiceControl),
    QueryChoice(QueryChoiceControl),
    Color(ColorControl),
    Curve(CurveControl),
    Range(RangeControl),
    Action(ActionControl),
    Picker(PickerControl),
    Task(TaskControl),
    Presets(PresetsControl),
);

impl Control {
    /// A `number` control styled as a slider, the style every field-patch module's fields use.
    /// Chain [`NumberControl::number_style`] for `field`/`stepper`, [`NumberControl::rail`] and
    /// `NumberControl::field_reset`.
    pub fn number(
        action: impl Into<String>,
        parameter: impl Into<String>,
        label: impl Into<String>,
    ) -> NumberControl {
        NumberControl {
            action: action.into(),
            parameter: parameter.into(),
            label: label.into(),
            style: NumberStyle::Slider,
            rail: None,
            reset: None,
            variants: Vec::new(),
        }
    }

    /// A `range` control over the band edges `low` and `high` of `action`, without shoulders.
    /// Chain [`RangeControl::feathers`] for the shoulder grips and [`RangeControl::rail`] for the
    /// rail.
    pub(crate) fn range(
        action: impl Into<String>,
        low: impl Into<String>,
        high: impl Into<String>,
        label: impl Into<String>,
    ) -> RangeControl {
        RangeControl {
            action: action.into(),
            low: low.into(),
            high: high.into(),
            low_feather: None,
            high_feather: None,
            label: label.into(),
            rail: None,
        }
    }

    pub fn toggle(
        action: impl Into<String>,
        parameter: impl Into<String>,
        label: impl Into<String>,
    ) -> ToggleControl {
        ToggleControl {
            action: action.into(),
            parameter: parameter.into(),
            label: label.into(),
        }
    }

    pub fn choice(
        action: impl Into<String>,
        parameter: impl Into<String>,
        label: impl Into<String>,
    ) -> ChoiceControl {
        ChoiceControl {
            action: action.into(),
            parameter: parameter.into(),
            label: label.into(),
            style: ChoiceStyle::default(),
        }
    }

    pub fn color_field(
        action: impl Into<String>,
        parameter: impl Into<String>,
        label: impl Into<String>,
    ) -> ColorControl {
        ColorControl {
            action: action.into(),
            parameter: parameter.into(),
            label: label.into(),
            style: ColorStyle::default(),
        }
    }

    pub fn curve(
        action: impl Into<String>,
        channels: Vec<CurveChannel>,
        label: impl Into<String>,
        sample_query: impl Into<String>,
    ) -> CurveControl {
        CurveControl {
            action: action.into(),
            channels,
            label: label.into(),
            sample_query: sample_query.into(),
            background: CurveBackground::default(),
        }
    }

    pub fn action(action: impl Into<String>, label: impl Into<String>) -> ActionControl {
        ActionControl {
            action: action.into(),
            label: label.into(),
            preset: Map::new(),
            style: ActionStyle::default(),
            icon: None,
            variants: Vec::new(),
        }
    }

    pub fn group(label: impl Into<String>, controls: Vec<Control>) -> GroupControl {
        GroupControl {
            label: label.into(),
            controls,
            reset: None,
            collapsed: false,
            variants: Vec::new(),
        }
    }

    pub fn picker(label: impl Into<String>) -> PickerControl {
        PickerControl {
            label: label.into(),
            variants: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn task(task: impl Into<String>, label: impl Into<String>) -> TaskControl {
        TaskControl {
            task: task.into(),
            label: label.into(),
        }
    }

    pub(crate) fn presets(action: impl Into<String>) -> PresetsControl {
        PresetsControl {
            action: action.into(),
        }
    }

    /// The variants this control declares; empty for a kind that cannot carry any.
    pub fn variants(&self) -> &[ControlVariant] {
        match self {
            Self::Group(GroupControl { variants, .. })
            | Self::Number(NumberControl { variants, .. })
            | Self::Action(ActionControl { variants, .. })
            | Self::Picker(PickerControl { variants, .. }) => variants,
            _ => &[],
        }
    }

    /// The control kind as it is serialized, for a refusal that names a shape.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Group(_) => "group",
            Self::Number(_) => "number",
            Self::Toggle(_) => "toggle",
            Self::Choice(_) => "choice",
            Self::QueryChoice(_) => "query-choice",
            Self::Color(_) => "color",
            Self::Curve(_) => "curve",
            Self::Range(_) => "range",
            Self::Action(_) => "action",
            Self::Picker(_) => "picker",
            Self::Task(_) => "task",
            Self::Presets(_) => "presets",
        }
    }
}

/// How a module lets the canvas drive its action. A point pick commits only when it declares
/// `commit`; a sample-apply pick submits its query's answer once; a crop frame commits on Apply.
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
        /// The mode's icon in a client's mode strip; see [`CanvasInteraction::icon`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        /// The pick is the whole request: a client submits `action` with the picked `x` and `y`
        /// at once, as one commit, instead of filling its fields for the person to submit. Only an
        /// action that declares no other parameter may commit on a pick, which
        /// [`ModuleDescriptor::validate`] checks. RAW's sensor neutral pick commits; the pixel
        /// proof's pick does not.
        #[serde(default)]
        commit: bool,
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
        /// The mode's icon in a client's mode strip; see [`CanvasInteraction::icon`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
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
        /// The mode's icon in a client's mode strip; see [`CanvasInteraction::icon`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
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

    /// The name of the icon a client's mode strip draws for this mode, when the module declares
    /// one, from the same vocabulary an action control's `icon` names (`crop`, `mask`). A hint: a
    /// client that does not know the name, or a mode without one, shows the title instead.
    pub fn icon(&self) -> Option<&str> {
        match self {
            Self::PointPick { icon, .. }
            | Self::SampleApply { icon, .. }
            | Self::CropFrame { icon, .. } => icon.as_deref(),
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
    /// What the module may be granted: sending an asset's data to a profile's endpoint, or
    /// installing a declared resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilityDescriptor>,
    /// Pinned files the host may install for this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceDescriptor>,
    /// Worker tasks, each reached through the generated `task.<id>` method. A task identity is
    /// unique across the registry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskDescriptor>,
}

impl ModuleDescriptor {
    pub fn action(&self, id: &str) -> Option<&ActionDescriptor> {
        self.actions.iter().find(|action| action.id == id)
    }

    /// The read-only query this module declares under that identity. Queries have their own
    /// namespace: `query.<id>` and `edit.<id>` are different methods, so a module may name a query
    /// after the action its result feeds without either shadowing the other.
    pub(crate) fn query(&self, id: &str) -> Option<&ActionDescriptor> {
        self.queries.iter().find(|query| query.id == id)
    }

    pub(crate) fn capability(&self, id: &str) -> Option<&CapabilityDescriptor> {
        self.capabilities
            .iter()
            .find(|capability| capability.id == id)
    }

    pub(crate) fn resource(&self, id: &str) -> Option<&ResourceDescriptor> {
        self.resources.iter().find(|resource| resource.id == id)
    }

    pub fn task(&self, id: &str) -> Option<&TaskDescriptor> {
        self.tasks.iter().find(|task| task.id == id)
    }

    pub fn is_available(&self) -> bool {
        matches!(self.availability, Availability::Available)
    }

    /// Whether this module applies to a photo of `kind`: when any of its effects may exist on that
    /// kind (`EffectDescriptor::applies_to`), or when it declares no effect at all, since a module
    /// that writes no layer has nothing a kind could refuse. The one answer to "does this module
    /// apply to this photo" — the host's action refusal, `module.list` and every client surface read
    /// it. `O(effects × sources)`, no allocation.
    pub fn applies_to(&self, kind: SourceTag) -> bool {
        self.effects.is_empty() || self.effects.iter().any(|effect| effect.applies_to(kind))
    }

    /// [`Self::is_available`] as a refusal: `incompatible: unavailable module <id>`. A provider
    /// registered unavailable keeps its descriptor so its stored layers stay readable, but the host
    /// never plans, queries or commits through it, and no preset may name its actions.
    pub(crate) fn check_available(&self) -> Result<(), Error> {
        if self.is_available() {
            return Ok(());
        }
        Err(Error::incompatible(format!(
            "unavailable module {}",
            self.id
        )))
    }

    /// [`Self::applies_to`] as a refusal: `validation: RAW does not apply to a JPEG photo`, worded
    /// from this module's title and the kind.
    pub(crate) fn check_applies_to(&self, kind: SourceTag) -> Result<(), Error> {
        if self.applies_to(kind) {
            return Ok(());
        }
        Err(self.not_applicable_refusal(ErrorKind::Validation, kind))
    }

    /// The refusal of this module on a photo of `kind`, of kind `error`: its message worded from
    /// the title and the kind ([`not_applicable`]), and its data `{source, module_id}` naming them,
    /// so a client acts on the kind and the module without reading the message.
    pub(crate) fn not_applicable_refusal(&self, error: ErrorKind, kind: SourceTag) -> Error {
        Error::new(error, not_applicable(&self.title, kind))
            .with_data(serde_json::json!({"source": kind, "module_id": self.id}))
    }
}
