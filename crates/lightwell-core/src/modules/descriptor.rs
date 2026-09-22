//! Plain-data module descriptors: one serializable source for API discovery, generated controls
//! and every validation limit a module declares.
use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Groups nest for layout only; a descriptor deeper than this is rejected rather than walked.
const MAX_CONTROL_DEPTH: usize = 8;

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

/// Where an effect acts, and so where the host puts a new layer: a geometry effect changes the
/// stage and extends the tail at the end of the stack; a pixel effect addresses its input stage and
/// joins the stack before that tail, so the geometry after it carries the edit. A colour effect is
/// placed by the same rule as a pixel effect, because it addresses the content stage too and
/// changes no dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectStage {
    Source,
    Geometry,
    Pixel,
    /// Pointwise colour over the whole stage, compiled into [`crate::Processing::Color`].
    Color,
}

/// A durable effect identity stored in every layer, with its internal payload format marker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectDescriptor {
    pub id: String,
    pub format: u32,
    pub stage: EffectStage,
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
    /// Ordered [x, y] fractions. Interpolation belongs to the module.
    Curve {
        points_min: usize,
        points_max: usize,
        #[serde(default)]
        monotone: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fixed_x: Option<Vec<f64>>,
    },
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
    /// A pointer pick on the image runs a module query at the picked content pixel and, when the
    /// query answers, submits its numeric result fields to `action` once. `query` names a query this
    /// module declares and `x`/`y` name that query's integer coordinate parameters; the fields
    /// submitted are every top-level number field of the result whose name is a parameter of
    /// `action`. A refused query commits nothing and its reason is shown instead.
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

/// An unavailable provider keeps its descriptor and effect identities so stored data stays readable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub availability: Availability,
}

impl ModuleDescriptor {
    /// Read a descriptor from JSON, reporting a missing or malformed field as a validation error.
    pub fn parse(value: &Value) -> Result<Self, Error> {
        if let Some(controls) = value.get("controls").and_then(Value::as_array) {
            for control in controls {
                check_raw_control_hints(control)?;
            }
        }
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
        for control in &self.controls {
            self.check_control(control, 1)?;
        }
        // One picker stands for one pick mode, so two would be two ways into the same mode and a
        // panel could not say which is selected.
        let pickers = Self::pickers(&self.controls);
        if pickers > 1 {
            return Err(validation(format!(
                "module {} declares {pickers} picker controls; a module declares at most one",
                self.id
            )));
        }
        self.check_reset(self.reset.as_ref())?;
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
                ..
            } => {
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
        }
        Ok(())
    }

    /// How many picker controls this module declares, at any depth.
    fn pickers(controls: &[Control]) -> usize {
        controls
            .iter()
            .map(|control| match control {
                Control::Picker { .. } => 1,
                Control::Group { controls, .. } => Self::pickers(controls),
                _ => 0,
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
    let mut parameters = HashSet::with_capacity(declared.parameters.len());
    for parameter in &declared.parameters {
        if !valid_name(&parameter.name) {
            return Err(validation(format!(
                "invalid parameter name {} of {kind} {}",
                parameter.name, declared.id
            )));
        }
        if !parameters.insert(parameter.name.as_str()) {
            return Err(validation(format!(
                "duplicate parameter {} of {kind} {}",
                parameter.name, declared.id
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
    check_summary(declared)
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

/// `rotate-left` reads as `Rotate left`; `16:9` and other punctuated options keep their shape.
fn title_case(text: &str) -> String {
    let spaced = text.replace('-', " ");
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
    let empty = Map::new();
    let object = match input {
        Value::Object(object) => object,
        Value::Null => &empty,
        _ => {
            return Err(validation(format!(
                "parameters of action {} must be a JSON object",
                action.id
            )));
        }
    };
    for name in object.keys() {
        if action.parameter(name).is_none() {
            return Err(validation(format!(
                "unknown parameter {name} for action {}",
                action.id
            )));
        }
    }
    let mut checked = Map::new();
    if action.patch {
        for (name, value) in object {
            let parameter = action
                .parameter(name)
                .expect("every key was matched to a declared parameter above");
            check_value(parameter, value)?;
            checked.insert(name.clone(), value.clone());
        }
        return Ok(checked);
    }
    for parameter in &action.parameters {
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
                    "missing required parameter {} for action {}",
                    parameter.name, action.id
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

    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "test.module".into(),
            title: "Test".into(),
            hint: Some("A test module".into()),
            effects: vec![EffectDescriptor {
                id: "test.module.effect".into(),
                format: 1,
                stage: EffectStage::Pixel,
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
            availability: Availability::Available,
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
        ];
        for (case, descriptor) in cases {
            let error = descriptor
                .validate()
                .expect_err(&format!("{case} must be rejected"));
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
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
    /// colour-stage module declares itself exactly as a pixel or geometry one does.
    #[test]
    fn effect_stages_keep_their_serialized_names_and_validate() {
        for (stage, name) in [
            (EffectStage::Geometry, "geometry"),
            (EffectStage::Pixel, "pixel"),
            (EffectStage::Color, "color"),
        ] {
            let effect = EffectDescriptor {
                id: "test.module.effect".into(),
                format: 1,
                stage,
            };
            assert_eq!(serde_json::to_value(stage).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<EffectDescriptor>(serde_json::to_value(&effect).unwrap())
                    .unwrap(),
                effect
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
}
