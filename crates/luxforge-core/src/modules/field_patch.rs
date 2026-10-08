//! The declarative field-patch module: a module that owns one layer of one effect, edited by a
//! `set-<name>` field patch and a `reset-<name>` action, whose payload is a JSON object of declared
//! fields in which a missing key means that field's default.
//!
//! Basic, the Tone curve, Detail, the colour mixer, Presence, Perspective, the vignette and the
//! developer controls proof are each a [`Spec`] — the field table, its groups and the module's identity — and a
//! [`FieldPatch::compile`]. A field is any parameter of the field vocabulary: a number, an
//! integer, a boolean, an enum, a colour or a curve. Everything the modules share lives here once:
//! the descriptor built from the table, parsing, planning a commit, update or no-op, payload
//! validation, the canonical stored form, values, history labels, the recipe row, neutrality and
//! the neutral layer's compilation.
//!
//! [`Spec::new`] derives the actions' identities and words and fixes the effect's shared fields,
//! and building a spec refuses a malformed table — a field its control cannot draw, a missing or
//! invalid default, a group entry that names no field, a field in no group or in two — when the
//! module is constructed, so registering a built-in field-patch module is its own directory and
//! one line in the registry's `linked_modules`.
//!
//! Every comparison is between canonical values, never between JSON spellings, so `{}` and a
//! payload that spells a default out (`{"exposure": 0}`, `{"midpoint": 50}`) are the same state and
//! are never mistaken for a change. A canonical value is the value as its kind reads it: a number
//! as the finite f64 it holds, whether it was written `1` or `1.0`, and a curve's coordinates the
//! same way; an integer, a colour, a boolean and an enum option are already one spelling each. The
//! canonical stored form holds only the fields that differ from their default, so the all-default
//! payload is exactly `{}`.
//!
//! The host checks a request's fields against their declarations before `parse`, on every path. A
//! stored payload is checked in full, by the one generic parameter check ([`check_value`]), by
//! [`ToolModule::validate_payload`] and wherever it is read.

use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, ChoiceStyle,
    ColorOperation, ColorStyle, Control, ControlVariant, EffectDescriptor, EffectStage,
    GroupControl, LayerReport, LayerUpdate, ModuleDescriptor, ModuleLayout, NewLayer,
    NumberControl, NumberStyle, ParameterDescriptor, ParameterKind, Processing, RailDecoration,
    ResetAction, SpatialOperation, Stage, StageContext, ToolModule, WheelStyle, check_value,
    label_value,
};
use crate::{Error, Orientation, SourceTag};
use serde_json::{Map, Number, Value};

/// A finite f64 as a JSON number. Every value written here is finite, so the fallback is never
/// reached in practice and never panics if it is.
pub(crate) fn number(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// How a field is drawn on its group's section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldControl {
    /// A number control in this style, for a number or integer field.
    Number(NumberStyle),
    /// A toggle, for a boolean field.
    Toggle,
    /// A choice in this style, for an enum field.
    Choice(ChoiceStyle),
    /// A colour control in this style, for a colour field.
    Color(ColorStyle),
    /// No control of its own: a curve field is one channel of a curve control its group lists
    /// among [`Group::extra`].
    Channel,
}

impl FieldControl {
    /// The control a field of `kind` draws unless its spec chooses a style.
    fn of(kind: &ParameterKind) -> Self {
        match kind {
            ParameterKind::Boolean => Self::Toggle,
            ParameterKind::Enum { .. } => Self::Choice(ChoiceStyle::Automatic),
            ParameterKind::Color => Self::Color(ColorStyle::Fields),
            ParameterKind::Curve { .. } => Self::Channel,
            _ => Self::Number(NumberStyle::Slider),
        }
    }

    /// Whether this control can draw a field of `kind`.
    fn draws(self, kind: &ParameterKind) -> bool {
        matches!(
            (self, kind),
            (
                Self::Number(_),
                ParameterKind::Number { .. } | ParameterKind::Integer { .. }
            ) | (Self::Toggle, ParameterKind::Boolean)
                | (Self::Choice(_), ParameterKind::Enum { .. })
                | (Self::Color(_), ParameterKind::Color)
                | (Self::Channel, ParameterKind::Curve { .. })
        )
    }

    /// The control kind as a refusal names it.
    fn name(self) -> &'static str {
        match self {
            Self::Number(_) => "number",
            Self::Toggle => "toggle",
            Self::Choice(_) => "choice",
            Self::Color(_) => "color",
            Self::Channel => "curve channel",
        }
    }
}

/// One field: its declared parameter — the payload key, its kind, its default and its display
/// hints — the control that sets it and the words a history label uses for it.
pub(crate) struct Field {
    /// The `set-<name>` parameter: its name is the payload key and its default is what a missing
    /// key means and what a reset writes.
    pub parameter: ParameterDescriptor,
    /// The control's label.
    pub label: String,
    /// What a history label and the recipe row call the field: `Exposure`, `Red hue`, `Vignette
    /// amount`. A numeric field whose range is signed shows its value with a sign (`+20`, `-35`);
    /// one whose range starts at 0 does not (`60`).
    pub history: String,
    /// How the field is drawn: its own control, or a channel of its group's curve control.
    pub control: FieldControl,
    /// The rail a number control draws; only a number control carries one.
    pub rail: Option<RailDecoration>,
    /// The controls other modules provide in this field's control's place on a photo of one source
    /// kind ([`ControlVariant`]); only a number control carries them. On the global target of such
    /// a photo the field is superseded: the host refuses it and the variant is its one path, and
    /// the module's reset leaves it to the group reset's variant.
    pub variants: Vec<ControlVariant>,
}

impl Field {
    /// A field of `parameter`, drawn by the control its kind implies — a slider, a toggle, an
    /// automatic choice, colour fields or a curve channel — with `label` as its control's label and
    /// its history word, no rail and no variants.
    pub(crate) fn new(parameter: ParameterDescriptor, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            control: FieldControl::of(&parameter.kind),
            parameter,
            history: label.clone(),
            label,
            rail: None,
            variants: Vec::new(),
        }
    }

    /// A number field in the -100..100 slider range with step 1 and no decimals, the range most
    /// fields share, defaulting to 0 with no unit, rail or zero hint.
    pub(crate) fn slider(
        name: &'static str,
        label: impl Into<String>,
        notes: impl Into<String>,
    ) -> Self {
        Self::new(
            ParameterDescriptor::number(name, -100.0, 100.0)
                .default(0.0)
                .step(1.0)
                .precision(0)
                .notes(notes),
            label,
        )
    }

    /// A number field's closed range; a no-op on any other kind.
    pub(crate) fn range(mut self, min: f64, max: f64) -> Self {
        if let ParameterKind::Number { .. } = self.parameter.kind {
            self.parameter.kind = ParameterKind::Number { min, max };
        }
        self
    }

    pub(crate) fn default(mut self, value: impl Into<Value>) -> Self {
        self.parameter = self.parameter.default(value);
        self
    }

    pub(crate) fn step(mut self, step: f64) -> Self {
        self.parameter = self.parameter.step(step);
        self
    }

    pub(crate) fn precision(mut self, precision: u8) -> Self {
        self.parameter = self.parameter.precision(precision);
        self
    }

    pub(crate) fn unit(mut self, unit: &str) -> Self {
        self.parameter = self.parameter.unit(unit);
        self
    }

    pub(crate) fn zero(mut self, zero: f64) -> Self {
        self.parameter = self.parameter.zero(zero);
        self
    }

    pub(crate) fn history(mut self, history: impl Into<String>) -> Self {
        self.history = history.into();
        self
    }

    pub(crate) fn control(mut self, control: FieldControl) -> Self {
        self.control = control;
        self
    }

    pub(crate) fn rail(mut self, rail: RailDecoration) -> Self {
        self.rail = Some(rail);
        self
    }

    pub(crate) fn variant(mut self, variant: ControlVariant) -> Self {
        self.variants.push(variant);
        self
    }

    /// The payload key and the `set-<name>` parameter.
    pub(crate) fn name(&self) -> &str {
        &self.parameter.name
    }

    /// The declared default, which [`Field::check`] makes sure every field has.
    fn default_value(&self) -> &Value {
        const MISSING: &Value = &Value::Null;
        self.parameter.default.as_ref().unwrap_or(MISSING)
    }

    /// A field the table can hold: a kind of the field vocabulary, a valid declared default, and a
    /// control that draws its kind and carries what it declares.
    fn check(&self) -> Result<(), String> {
        let name = self.name();
        let kind = &self.parameter.kind;
        if !matches!(
            kind,
            ParameterKind::Number { .. }
                | ParameterKind::Integer { .. }
                | ParameterKind::Boolean
                | ParameterKind::Enum { .. }
                | ParameterKind::Color
                | ParameterKind::Curve { .. }
        ) {
            return Err(format!(
                "field {name}: a field patch cannot hold its {} parameter",
                kind.name()
            ));
        }
        let default = self
            .parameter
            .default
            .as_ref()
            .ok_or_else(|| format!("field {name} declares no default"))?;
        check_value(&self.parameter, default)
            .map_err(|error| format!("field {name}'s default is invalid: {}", error.detail))?;
        if !self.control.draws(kind) {
            return Err(format!(
                "field {name}: a {} control cannot draw its {} parameter",
                self.control.name(),
                kind.name()
            ));
        }
        let number = matches!(self.control, FieldControl::Number(_));
        if !self.variants.is_empty() && !number {
            return Err(format!(
                "field {name}: its {} control cannot carry variants",
                self.control.name()
            ));
        }
        if self.rail.is_some() && !number {
            return Err(format!(
                "field {name}: its {} control cannot carry a rail",
                self.control.name()
            ));
        }
        Ok(())
    }

    /// The field's own control on the `action` patch, or `None` for a curve channel.
    fn own_control(&self, action: &str) -> Option<Control> {
        let (name, label) = (self.name(), self.label.clone());
        Some(match self.control {
            FieldControl::Number(style) => {
                let control = Control::number(action, name, label).number_style(style);
                let control = match self.rail.clone() {
                    Some(rail) => control.rail(rail),
                    None => control,
                };
                self.variants
                    .iter()
                    .cloned()
                    .fold(control, NumberControl::variant)
                    .into()
            }
            FieldControl::Toggle => Control::toggle(action, name, label).into(),
            FieldControl::Choice(style) => Control::choice(action, name, label)
                .choice_style(style)
                .into(),
            FieldControl::Color(style) => Control::color_field(action, name, label)
                .color_style(style)
                .into(),
            FieldControl::Channel => return None,
        })
    }

    /// `value`, which the generic check accepts for this field, in its canonical spelling: a
    /// number, and each coordinate of a curve, as the f64 it reads as. Every other kind has one
    /// spelling already.
    fn canonical(&self, value: &Value) -> Value {
        match &self.parameter.kind {
            ParameterKind::Number { .. } => value.as_f64().map_or_else(|| value.clone(), number),
            ParameterKind::Curve { .. } => match value.as_array() {
                Some(points) => Value::Array(
                    points
                        .iter()
                        .map(|point| match point.as_array() {
                            Some(pair) => Value::Array(
                                pair.iter()
                                    .map(|coordinate| {
                                        coordinate
                                            .as_f64()
                                            .map_or_else(|| coordinate.clone(), number)
                                    })
                                    .collect(),
                            ),
                            None => point.clone(),
                        })
                        .collect(),
                ),
                None => value.clone(),
            },
            _ => value.clone(),
        }
    }

    /// The field and its value as a history label and the recipe row name them: the history word,
    /// then a number with its declared decimals (signed for a signed range) and its declared unit,
    /// a boolean as `on` or `off`, a curve by its point count, and an option or a colour as a
    /// summary shows it (`Two`, `20,40,60`).
    fn label(&self, value: &Value) -> String {
        let signed = match &self.parameter.kind {
            ParameterKind::Number { min, .. } => Some(*min < 0.0),
            ParameterKind::Integer { min, .. } => Some(*min < 0),
            _ => None,
        };
        let shown = match (signed, value.as_f64()) {
            (Some(signed), Some(value)) => {
                let precision = usize::from(self.parameter.precision.unwrap_or(0));
                if signed {
                    format!("{value:+.precision$}")
                } else {
                    format!("{value:.precision$}")
                }
            }
            _ => match (&self.parameter.kind, value) {
                (ParameterKind::Boolean, Value::Bool(on)) => {
                    (if *on { "on" } else { "off" }).into()
                }
                (ParameterKind::Curve { .. }, Value::Array(points)) => {
                    format!("{} points", points.len())
                }
                _ => label_value(value),
            },
        };
        match &self.parameter.unit {
            Some(unit) => format!("{} {shown} {unit}", self.history),
            None => format!("{} {shown}", self.history),
        }
    }
}

/// A group of controls on the module's section. Its reset sets exactly its member fields — its own
/// and every nested subgroup's — to their defaults, and a patch that does so is labelled
/// `Reset <label>` however it was sent. Every field of the table is listed exactly once in the
/// whole group tree, so it belongs to one top-level group and at most one subgroup path, which
/// [`Spec`]'s build checks.
///
/// A group draws its own fields' controls, then its subgroups, then its extra controls. A group
/// with views draws its views instead of its own fields: each view is a presentation-only tab
/// ([`View`]) that may show any member field, so one field can appear in several views, and every
/// own field must appear in at least one. A wheel declared on the group draws its bound fields in
/// place of their own controls, at the position of its hue field.
pub(crate) struct Group {
    label: &'static str,
    fields: Vec<&'static str>,
    collapsed: bool,
    /// Controls drawn after the group's own field controls, such as Basic's neutral picker or the
    /// curve control whose channels are the group's curve fields.
    extra: Vec<Control>,
    /// The resets other modules provide in the group reset's place on a photo of one source kind
    /// ([`ControlVariant::reset`]).
    reset_variants: Vec<ControlVariant>,
    /// How a client arranges this group's child groups: its views and subgroups.
    layout: ModuleLayout,
    /// Nested groups, each a reset and capture scope of its own fields within this one.
    subgroups: Vec<Group>,
    /// Presentation-only tabs over this group's member fields.
    views: Vec<View>,
    /// Wheels drawn in place of their fields' own controls in this group's own list.
    wheels: Vec<Wheel>,
}

impl Group {
    /// An expanded group of these fields, by name, with no extra controls or reset variants.
    pub(crate) fn new(label: &'static str, fields: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            label,
            fields: fields.into_iter().collect(),
            collapsed: false,
            extra: Vec::new(),
            reset_variants: Vec::new(),
            layout: ModuleLayout::Stacked,
            subgroups: Vec::new(),
            views: Vec::new(),
            wheels: Vec::new(),
        }
    }

    /// The group starts collapsed.
    pub(crate) fn collapsed(mut self) -> Self {
        self.collapsed = true;
        self
    }

    /// The group shows its child groups — its views and subgroups — as one tab row.
    pub(crate) fn tabs(mut self) -> Self {
        self.layout = ModuleLayout::Tabs;
        self
    }

    /// A nested group whose fields are members of this one: its own reset covers exactly its
    /// fields, and this group's reset covers them with the rest.
    pub(crate) fn subgroup(mut self, group: Group) -> Self {
        self.subgroups.push(group);
        self
    }

    /// A presentation-only view over this group's member fields.
    pub(crate) fn view(mut self, view: View) -> Self {
        self.views.push(view);
        self
    }

    /// A wheel drawing two or three of this group's member fields in place of their own controls.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn wheel(mut self, wheel: Wheel) -> Self {
        self.wheels.push(wheel);
        self
    }

    /// Every field of this group and of its subgroups, depth first in declared order.
    fn members(&self) -> Vec<&'static str> {
        let mut members = self.fields.clone();
        for subgroup in &self.subgroups {
            members.extend(subgroup.members());
        }
        members
    }

    /// This group and every nested subgroup, depth first, each before its subgroups.
    fn tree(&self) -> Vec<&Group> {
        let mut groups = vec![self];
        for subgroup in &self.subgroups {
            groups.extend(subgroup.tree());
        }
        groups
    }

    /// Every wheel this group or one of its views declares, not counting its subgroups'.
    fn own_wheels(&self) -> Vec<&Wheel> {
        let mut wheels: Vec<&Wheel> = self.wheels.iter().collect();
        for view in &self.views {
            wheels.extend(view.entries.iter().filter_map(|entry| match entry {
                ViewEntry::Wheel(wheel) => Some(wheel),
                ViewEntry::Field(_) => None,
            }));
        }
        wheels
    }

    /// Every wheel this group, one of its views or one of its subgroups declares.
    fn all_wheels(&self) -> Vec<&Wheel> {
        let mut wheels = self.own_wheels();
        for subgroup in &self.subgroups {
            wheels.extend(subgroup.all_wheels());
        }
        wheels
    }

    /// A control drawn after the group's own field controls.
    pub(crate) fn extra(mut self, control: impl Into<Control>) -> Self {
        self.extra.push(control.into());
        self
    }

    /// The reset another module provides in this group reset's place on a photo of one source
    /// kind.
    pub(crate) fn reset_variant(mut self, variant: ControlVariant) -> Self {
        self.reset_variants.push(variant);
        self
    }
}

/// A hue and saturation wheel over two or three number fields of the patch: its angle is the hue
/// field (declared `0..=360`), its radius the saturation field (declared from 0) and, when given,
/// its rail the luminance field. Its reset is the patch of exactly its fields to their defaults,
/// labelled `Reset <label>`, and a patch of its hue and saturation (with or without its luminance)
/// is labelled by its history words: a wheel gesture is one entry, not two.
#[derive(Clone)]
pub(crate) struct Wheel {
    label: &'static str,
    hue: &'static str,
    saturation: &'static str,
    luminance: Option<&'static str>,
    style: WheelStyle,
    history: Option<&'static str>,
}

impl Wheel {
    /// A compact wheel labelled `label` over the `hue` and `saturation` fields.
    pub(crate) fn new(label: &'static str, hue: &'static str, saturation: &'static str) -> Self {
        Self {
            label,
            hue,
            saturation,
            luminance: None,
            style: WheelStyle::Compact,
            history: None,
        }
    }

    /// The field drawn as the wheel's luminance rail.
    pub(crate) fn luminance(mut self, luminance: &'static str) -> Self {
        self.luminance = Some(luminance);
        self
    }

    /// The large style an individual view draws.
    pub(crate) fn large(mut self) -> Self {
        self.style = WheelStyle::Large;
        self
    }

    /// What a history entry setting the wheel's hue and saturation is called, `Shadows tint`;
    /// the wheel's label unless given.
    pub(crate) fn history(mut self, history: &'static str) -> Self {
        self.history = Some(history);
        self
    }

    /// The bound fields: hue, saturation and, when declared, luminance.
    fn fields(&self) -> Vec<&'static str> {
        [Some(self.hue), Some(self.saturation), self.luminance]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// One entry of a [`View`], drawn in its order: a field's own control, or a wheel.
pub(crate) enum ViewEntry {
    Field(&'static str),
    Wheel(Wheel),
}

/// A presentation-only tab of a group that declares views: what one view shows of the group's
/// member fields. A field may be shown by several views and is still captured and reset by its
/// group alone; a view has no reset and adds no history label.
pub(crate) struct View {
    label: &'static str,
    entries: Vec<ViewEntry>,
}

impl View {
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            entries: Vec::new(),
        }
    }

    /// A member field's own control.
    pub(crate) fn field(mut self, name: &'static str) -> Self {
        self.entries.push(ViewEntry::Field(name));
        self
    }

    /// A wheel over member fields.
    pub(crate) fn wheel(mut self, wheel: Wheel) -> Self {
        self.entries.push(ViewEntry::Wheel(wheel));
        self
    }

    /// Every field this view draws, through a field entry or a wheel.
    fn shown(&self) -> Vec<&'static str> {
        self.entries
            .iter()
            .flat_map(|entry| match entry {
                ViewEntry::Field(name) => vec![*name],
                ViewEntry::Wheel(wheel) => wheel.fields(),
            })
            .collect()
    }
}

/// An action's identity and the words discovery shows for it.
struct ActionText {
    id: String,
    title: String,
    notes: String,
}

/// Everything a field-patch module declares: its identity, its one effect, its two actions, the
/// field table and how the fields are grouped and laid out. The descriptor is built from it once.
///
/// A spec is made only by `Spec::new`, which derives what every field-patch module shares and
/// fixes what none may change: the effect's format is [`crate::EFFECT_FORMAT`], it declares no artifacts,
/// it is `single` (the module owns one layer per target) and it applies to every source kind.
pub(crate) struct Spec {
    id: &'static str,
    title: &'static str,
    hint: &'static str,
    /// The word payload errors use, `unknown basic field exposure`: the last segment of the id.
    noun: &'static str,
    effect: EffectDescriptor,
    /// The field patch, `set-<noun>`.
    set: ActionText,
    /// The action that returns the layer to its all-default payload, `reset-<noun>`.
    reset: ActionText,
    /// Every field, in the payload's declared order.
    fields: Vec<Field>,
    groups: Vec<Group>,
    queries: Vec<ActionDescriptor>,
    actions: Vec<ActionDescriptor>,
    canvas: Option<CanvasInteraction>,
    collapsed: bool,
    layout: ModuleLayout,
    /// Whether the module is a developer proof, listed and registered only in developer mode.
    developer: bool,
    presettable: bool,
}

impl Spec {
    /// The spec of module `id`, titled `title`, owning one layer of `effect_id` at `stage`.
    ///
    /// The noun is the id's last segment (`luxforge.basic` is `basic`); the actions are
    /// `set-<noun>`, titled `Set <title>`, and `reset-<noun>`, titled `Reset <title>`, each with
    /// the notes every field-patch module's action shares until the module says more. The effect
    /// takes order 0, is not maskable, and holds the forced fields; the module adds its fields,
    /// groups and everything else through the methods below.
    pub(crate) fn new(
        id: &'static str,
        title: &'static str,
        hint: &'static str,
        effect_id: &str,
        stage: EffectStage,
    ) -> Self {
        let noun = id.rsplit('.').next().unwrap_or(id);
        Self {
            id,
            title,
            hint,
            noun,
            effect: EffectDescriptor {
                single: true,
                ..EffectDescriptor::new(effect_id, stage)
            },
            set: ActionText {
                id: format!("set-{noun}"),
                title: format!("Set {title}"),
                notes: format!(
                    "merges the named {noun} fields into the stack's one {title} layer, which the \
                     host places by its effect's declared stage and order on the first non-neutral \
                     value and updates in place afterwards; omitted fields keep their stored values \
                     and a patch that changes nothing is a reported no-op"
                ),
            },
            reset: ActionText {
                id: format!("reset-{noun}"),
                title: format!("Reset {title}"),
                notes: format!(
                    "returns the stack's one {title} layer to its neutral payload, keeping its \
                     identity and position; a no-op without one and when it is already neutral"
                ),
            },
            fields: Vec::new(),
            groups: Vec::new(),
            queries: Vec::new(),
            actions: Vec::new(),
            canvas: None,
            collapsed: false,
            layout: ModuleLayout::Stacked,
            developer: false,
            presettable: true,
        }
    }

    /// The order the effect takes among layers of its stage.
    pub(crate) fn order(mut self, order: u16) -> Self {
        self.effect.order = order;
        self
    }

    /// Whether the patch action participates in presets.
    pub(crate) fn presettable(mut self, enabled: bool) -> Self {
        self.presettable = enabled;
        self
    }

    /// The effect may be bound to a mask, so the module's actions take a mask target.
    pub(crate) fn maskable(mut self) -> Self {
        self.effect.maskable = true;
        self
    }

    /// The field patch's notes, where the module says more than the shared sentence, such as
    /// where the host places its layer.
    pub(crate) fn set_notes(mut self, notes: impl Into<String>) -> Self {
        self.set.notes = notes.into();
        self
    }

    /// The module reset's notes, where the module says more than the shared sentence.
    pub(crate) fn reset_notes(mut self, notes: impl Into<String>) -> Self {
        self.reset.notes = notes.into();
        self
    }

    /// Every field, in the payload's declared order.
    pub(crate) fn fields(mut self, fields: impl IntoIterator<Item = Field>) -> Self {
        self.fields.extend(fields);
        self
    }

    pub(crate) fn group(mut self, group: Group) -> Self {
        self.groups.push(group);
        self
    }

    /// A read-only query the module answers through [`FieldPatch::query`].
    pub(crate) fn query(mut self, query: ActionDescriptor) -> Self {
        self.queries.push(query);
        self
    }

    /// A discrete action that the module plans through `FieldPatch::plan_extra`.
    pub(crate) fn action(mut self, action: ActionDescriptor) -> Self {
        self.actions.push(action);
        self
    }

    pub(crate) fn canvas(mut self, canvas: CanvasInteraction) -> Self {
        self.canvas = Some(canvas);
        self
    }

    /// The module's section starts collapsed.
    pub(crate) fn collapsed(mut self) -> Self {
        self.collapsed = true;
        self
    }

    pub(crate) fn layout(mut self, layout: ModuleLayout) -> Self {
        self.layout = layout;
        self
    }

    /// The module is a developer proof, listed and registered only in developer mode.
    pub(crate) fn developer(mut self) -> Self {
        self.developer = true;
        self
    }

    fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name() == name)
    }

    fn defaults(&self) -> Vec<Value> {
        self.fields
            .iter()
            .map(|field| field.default_value().clone())
            .collect()
    }

    /// Every group of the tree, depth first, each before its subgroups.
    fn all_groups(&self) -> Vec<&Group> {
        self.groups.iter().flat_map(Group::tree).collect()
    }

    /// Every wheel any group or view declares, in declaration order.
    fn wheels(&self) -> Vec<&Wheel> {
        self.groups.iter().flat_map(Group::all_wheels).collect()
    }

    /// Whether the table's fields and its groups agree: every name a group lists is a declared
    /// field, and every field is listed exactly once across the whole group tree, so each field
    /// has a control and a group reset that resets it; every view and wheel draws only member
    /// fields of the group that declares it, a group with views shows each of its own fields in
    /// at least one view, and a wheel's fields are distinct. `O(fields × entries)`, once per build.
    fn check_groups(&self) -> Result<(), String> {
        let groups = self.all_groups();
        for group in &groups {
            if let Some(name) = group.fields.iter().find(|name| self.field(name).is_none()) {
                return Err(format!(
                    "group {} lists {name}, which is not a declared field",
                    group.label
                ));
            }
        }
        for field in &self.fields {
            let name = field.name();
            match groups
                .iter()
                .flat_map(|group| &group.fields)
                .filter(|listed| **listed == name)
                .count()
            {
                1 => {}
                0 => return Err(format!("field {name} is in no group")),
                count => {
                    return Err(format!(
                        "field {name} is listed {count} times in the groups, not once"
                    ));
                }
            }
        }
        for group in &groups {
            let members = group.members();
            let member = |name: &str, what: &str| {
                if members.contains(&name) {
                    Ok(())
                } else {
                    Err(format!(
                        "{what} of group {} draws {name}, which is not a field of the group",
                        group.label
                    ))
                }
            };
            for wheel in group.own_wheels() {
                let fields = wheel.fields();
                for (at, name) in fields.iter().enumerate() {
                    member(name, &format!("wheel {}", wheel.label))?;
                    if fields[..at].contains(name) {
                        return Err(format!("wheel {} binds {name} twice", wheel.label));
                    }
                }
            }
            for view in &group.views {
                for name in view.shown() {
                    member(name, &format!("view {}", view.label))?;
                }
            }
            if !group.views.is_empty() {
                if let Some(name) = group
                    .fields
                    .iter()
                    .find(|name| !group.views.iter().any(|view| view.shown().contains(name)))
                {
                    return Err(format!(
                        "field {name} of group {} is shown by none of its views",
                        group.label
                    ));
                }
                if !group.wheels.is_empty() {
                    return Err(format!(
                        "group {} declares views, so its wheels belong in a view",
                        group.label
                    ));
                }
            }
        }
        Ok(())
    }

    /// One group's control: its own fields (or its views), its subgroups and its extra controls,
    /// with the reset that patches every member field to its default.
    fn group_control(&self, group: &Group) -> Control {
        let set = &self.set.id;
        let mut controls: Vec<Control> = Vec::new();
        if group.views.is_empty() {
            for name in &group.fields {
                match group
                    .wheels
                    .iter()
                    .find(|wheel| wheel.fields().contains(name))
                {
                    Some(wheel) if wheel.hue == *name => controls.push(self.wheel_control(wheel)),
                    Some(_) => {}
                    None => {
                        controls.extend(self.field(name).and_then(|field| field.own_control(set)))
                    }
                }
            }
        } else {
            for view in &group.views {
                let entries = view
                    .entries
                    .iter()
                    .filter_map(|entry| match entry {
                        ViewEntry::Field(name) => {
                            self.field(name).and_then(|field| field.own_control(set))
                        }
                        ViewEntry::Wheel(wheel) => Some(self.wheel_control(wheel)),
                    })
                    .collect();
                controls.push(Control::group(view.label, entries).as_view().into());
            }
        }
        controls.extend(
            group
                .subgroups
                .iter()
                .map(|subgroup| self.group_control(subgroup)),
        );
        controls.extend(group.extra.iter().cloned());
        let control = Control::group(group.label, controls)
            .reset(self.defaults_of(&group.members()))
            .collapsed(group.collapsed)
            .group_layout(group.layout);
        group
            .reset_variants
            .iter()
            .cloned()
            .fold(control, GroupControl::variant)
            .into()
    }

    /// A wheel's control, whose reset patches exactly its fields to their defaults.
    fn wheel_control(&self, wheel: &Wheel) -> Control {
        let control = Control::wheel(&self.set.id, wheel.hue, wheel.saturation, wheel.label)
            .wheel_style(wheel.style)
            .wheel_reset(self.defaults_of(&wheel.fields()));
        match wheel.luminance {
            Some(luminance) => control.luminance(luminance),
            None => control,
        }
        .into()
    }

    /// The patch of these fields to their defaults.
    fn defaults_of(&self, names: &[&str]) -> ResetAction {
        ResetAction {
            action: self.set.id.clone(),
            preset: names
                .iter()
                .filter_map(|name| self.field(name))
                .map(|field| (field.name().to_owned(), field.default_value().clone()))
                .collect(),
        }
    }

    /// The spec with every declared default in its canonical spelling, the descriptor built from
    /// it and the shape its processing takes, or the refusal of a stage no field patch can own, of
    /// the first field [`Field::check`] refuses, or of groups that do not list every field exactly
    /// once.
    fn build(mut self) -> Result<(Self, ModuleDescriptor, Shape), Error> {
        let id = self.id;
        let refused =
            |reason: String| Error::validation(format!("field-patch module {id}: {reason}"));
        let stage = self.effect.stage;
        let shape = Shape::of(stage).ok_or_else(|| {
            refused(format!(
                "a field patch cannot own a {} effect",
                stage.as_str()
            ))
        })?;
        for field in &mut self.fields {
            field.check().map_err(refused)?;
            field.parameter.default = Some(field.canonical(field.default_value()));
        }
        self.check_groups().map_err(refused)?;
        let descriptor = self.descriptor();
        // A wheel's bindings, a tab row's groups and a view's place are the control vocabulary's
        // rules, refused here as registration would refuse them, when the module is built.
        descriptor
            .validate_controls()
            .map_err(|error| refused(error.detail))?;
        Ok((self, descriptor, shape))
    }

    /// The descriptor of a checked spec: each group draws its fields' own controls then its extra
    /// controls, its reset is the patch of its fields to their defaults, and every variant a field
    /// or group declares is folded onto its control. A checked group names only declared fields.
    fn descriptor(&self) -> ModuleDescriptor {
        let controls = self
            .groups
            .iter()
            .map(|group| self.group_control(group))
            .collect();
        ModuleDescriptor {
            id: self.id.into(),
            title: self.title.into(),
            hint: Some(self.hint.into()),
            effects: vec![self.effect.clone()],
            actions: vec![
                ActionDescriptor {
                    patch: true,
                    preset: self.presettable,
                    parameters: self
                        .fields
                        .iter()
                        .map(|field| field.parameter.clone())
                        .collect(),
                    ..ActionDescriptor::new(
                        self.set.id.clone(),
                        self.set.title.clone(),
                        self.set.notes.clone(),
                    )
                },
                ActionDescriptor::new(
                    self.reset.id.clone(),
                    self.reset.title.clone(),
                    self.reset.notes.clone(),
                ),
            ]
            .into_iter()
            .chain(self.actions.iter().cloned())
            .collect(),
            queries: self.queries.clone(),
            controls,
            reset: Some(ResetAction {
                action: self.reset.id.clone(),
                preset: Map::new(),
            }),
            canvas: self.canvas.clone(),
            developer: self.developer,
            collapsed: self.collapsed,
            layout: self.layout,
            availability: Availability::Available,
            ..ModuleDescriptor::default()
        }
    }
}

/// The shape of the processing a field patch's effect compiles to, by its stage: pointwise colour
/// for a colour or finish effect, spatial processing for restoration or spatial effects,
/// and a mapping for geometry effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Color,
    Spatial,
    Geometry,
}

impl Shape {
    fn of(stage: EffectStage) -> Option<Self> {
        match stage {
            EffectStage::Color | EffectStage::Finish => Some(Self::Color),
            EffectStage::Restoration | EffectStage::Spatial => Some(Self::Spatial),
            EffectStage::Geometry => Some(Self::Geometry),
            EffectStage::Source | EffectStage::Pixel => None,
        }
    }

    /// What a neutral layer compiles to: no units, which the host drops entirely, keeping the
    /// identity byte path and the shared source buffer.
    fn neutral(self, stage: Stage) -> Processing {
        match self {
            Self::Color => Processing::Color(ColorOperation::neutral()),
            Self::Spatial => Processing::Spatial(SpatialOperation::neutral()),
            Self::Geometry => {
                Processing::ExactGeometry(super::ExactGeometry::identity(stage.width, stage.height))
            }
        }
    }
}

/// The canonical values of one payload, one per field in the table's order, with every missing
/// key read as its field's default.
pub(crate) struct Values<'a> {
    fields: &'a [Field],
    values: Vec<Value>,
}

impl Values<'_> {
    /// One field's canonical value, by name. Every name a module asks for is one of its own
    /// fields.
    fn value(&self, name: &str) -> &Value {
        let index = self
            .fields
            .iter()
            .position(|field| field.name() == name)
            .expect("a module reads only its own declared fields");
        &self.values[index]
    }

    /// One number field's value, by name: the finite f64 the stored payload holds, or the field's
    /// default.
    pub(crate) fn number(&self, name: &str) -> f64 {
        self.value(name)
            .as_f64()
            .expect("a module reads a number only from its own number fields")
    }

    /// One curve field's points, by name: the canonical `[[x, y], ...]` the stored payload holds,
    /// or the field's default, each coordinate as the finite f64 it reads as. The host has checked
    /// the list against the field's curve declaration.
    pub(crate) fn curve(&self, name: &str) -> Vec<[f64; 2]> {
        let coordinate = |value: &Value| {
            value
                .as_f64()
                .expect("a module reads a curve only from its own curve fields")
        };
        self.value(name)
            .as_array()
            .expect("a module reads a curve only from its own curve fields")
            .iter()
            .map(|point| match point.as_array().map(Vec::as_slice) {
                Some([x, y]) => [coordinate(x), coordinate(y)],
                _ => panic!("a module reads a curve only from its own curve fields"),
            })
            .collect()
    }

    /// Whether every field holds its default, by value.
    pub(crate) fn is_default(&self) -> bool {
        self.fields
            .iter()
            .zip(&self.values)
            .all(|(field, value)| value == field.default_value())
    }
}

/// What one field-patch module provides beyond its table: turning canonical values into processing,
/// and, when the rule differs from "every field at its default", which values change nothing.
pub(crate) trait FieldPatch: Send + Sync + 'static {
    /// Plan a declared discrete action alongside the patch and reset. The same host transaction
    /// rules apply to its plan, including composition through this module's own patch.
    fn plan_extra(
        &self,
        input: &ActionInput,
        context: &StageContext<'_>,
    ) -> Result<ActionPlan, Error> {
        let _ = context;
        Err(Error::validation(format!(
            "unknown action {}",
            input.action_id
        )))
    }

    /// The module's identity, effect, actions and field table.
    fn spec() -> Spec
    where
        Self: Sized;

    /// The processing these canonical values compile to at the layer's input stage. It is asked
    /// only for values [`FieldPatch::is_neutral`] calls not neutral: a neutral layer compiles to no
    /// units, in its effect stage's shape, once for every field-patch module.
    fn compile(&self, values: &Values<'_>, at: crate::CompileStage) -> Result<Processing, Error>;

    /// Re-express this payload after an exact orientation; never evaluates a raster.
    fn carry(
        &self,
        values: &Values<'_>,
        input: Stage,
        orientation: Orientation,
    ) -> Result<Option<Value>, Error> {
        let _ = (values, input, orientation);
        Ok(None)
    }

    /// Whether these values change nothing, so a first set that reaches them commits no layer and
    /// a layer holding them compiles to no units. The default is every field at its default; the
    /// vignette's is an amount of 0, whatever its shape fields hold.
    fn is_neutral(&self, values: &Values<'_>) -> bool {
        values.is_default()
    }

    /// Answer one of the queries the spec declares, with parameters the host has already checked
    /// against the query's declaration.
    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        let _ = (parameters, context);
        Err(Error::validation(format!("unknown query {query_id}")))
    }
}

/// A [`FieldPatch`] as a [`ToolModule`]: the descriptor built from its spec once, and every shared
/// behaviour of a field-patch module implemented from the table.
pub(crate) struct FieldPatchModule<M> {
    module: M,
    spec: Spec,
    descriptor: ModuleDescriptor,
    shape: Shape,
}

impl<M: FieldPatch + Default> FieldPatchModule<M> {
    /// The module built from its spec. A spec is a static table, so one its build refuses — a
    /// stage no field patch can own, a field outside the vocabulary, without a valid default or
    /// declaring a rail or variants its control cannot carry, or groups that do not list every
    /// field exactly once — is a defect the module's construction reports at once.
    pub fn new() -> Self {
        let (spec, descriptor, shape) = M::spec()
            .build()
            .unwrap_or_else(|error| panic!("{}", error.detail));
        Self {
            module: M::default(),
            spec,
            descriptor,
            shape,
        }
    }
}

impl<M: FieldPatch + Default> Default for FieldPatchModule<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> std::fmt::Debug for FieldPatchModule<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldPatchModule")
            .field("id", &self.descriptor.id)
            .finish()
    }
}

impl<M: FieldPatch> FieldPatchModule<M> {
    fn values(&self, values: Vec<Value>) -> Values<'_> {
        Values {
            fields: &self.spec.fields,
            values,
        }
    }

    /// The canonical values of a stored payload, with the effect identity and format checked
    /// first: an unsupported format is `incompatible` and is never rewritten, and every key is a
    /// declared field whose value the generic parameter check accepts.
    fn read(&self, effect_id: &str, format: u32, payload: &Value) -> Result<Values<'_>, Error> {
        let spec = &self.spec;
        let noun = spec.noun;
        if effect_id != spec.effect.id {
            return Err(Error::unavailable_effect(effect_id, &[]));
        }
        if format != spec.effect.format {
            return Err(Error::incompatible(format!(
                "unsupported effect format {format}"
            )));
        }
        let object = payload
            .as_object()
            .ok_or_else(|| Error::validation(format!("{noun} payload must be a JSON object")))?;
        for (name, value) in object {
            let field = spec
                .field(name)
                .ok_or_else(|| Error::validation(format!("unknown {noun} field {name}")))?;
            check_value(&field.parameter, value)?;
        }
        Ok(self.values(
            spec.fields
                .iter()
                .map(|field| match object.get(field.name()) {
                    Some(value) => field.canonical(value),
                    None => field.default_value().clone(),
                })
                .collect(),
        ))
    }

    /// The canonical stored form of a set of values: only the fields that differ from their
    /// default, so the all-default payload is exactly `{}`.
    fn payload(&self, values: &[Value]) -> Value {
        Value::Object(
            self.spec
                .fields
                .iter()
                .zip(values)
                .filter(|(field, value)| *value != field.default_value())
                .map(|(field, value)| (field.name().to_owned(), value.clone()))
                .collect(),
        )
    }

    /// The group a patch returns entirely to its defaults, when it is one: a patch holding exactly
    /// one group's fields, each at its default, is that group's reset however it was sent — from
    /// the group's header, a keyboard reset or an API call.
    ///
    /// Nested groups count: a patch of a subgroup's fields is that subgroup's reset (`Reset Hue`)
    /// and one of its parent's whole membership the parent's (`Reset HSL`). After the groups, a
    /// wheel's fields at their defaults are that wheel's reset (`Reset Shadows`).
    fn reset_group(&self, sent: &[(&String, Value)]) -> Option<&'static str> {
        let at_defaults = |fields: &[&'static str]| {
            sent.len() == fields.len()
                && sent.iter().all(|(name, value)| {
                    fields.contains(&name.as_str())
                        && self
                            .spec
                            .field(name)
                            .is_some_and(|field| value == field.default_value())
                })
        };
        self.spec
            .all_groups()
            .into_iter()
            .find(|group| at_defaults(&group.members()))
            .map(|group| group.label)
            .or_else(|| {
                self.spec
                    .wheels()
                    .into_iter()
                    .find(|wheel| at_defaults(&wheel.fields()))
                    .map(|wheel| wheel.label)
            })
    }

    /// The wheel a patch sets, when it is one: exactly a wheel's hue and saturation, or all its
    /// fields, read as the wheel's history words, so one wheel gesture reads as one change.
    fn whole_wheel(&self, sent: &[(&String, Value)]) -> Option<&'static str> {
        let names = |fields: &[&'static str]| {
            sent.len() == fields.len()
                && sent.iter().all(|(name, _)| fields.contains(&name.as_str()))
        };
        self.spec
            .wheels()
            .into_iter()
            .find(|wheel| names(&[wheel.hue, wheel.saturation]) || names(&wheel.fields()))
            .map(|wheel| wheel.history.unwrap_or(wheel.label))
    }

    /// The group a patch sets entirely, when it is one: a patch holding exactly one group's fields
    /// reads as that group, such as a neutral pick's temperature and tint as `White balance`.
    fn whole_group(&self, sent: &[(&String, Value)]) -> Option<&'static str> {
        self.spec
            .all_groups()
            .into_iter()
            .find(|group| {
                let members = group.members();
                sent.len() == members.len()
                    && sent
                        .iter()
                        .all(|(name, _)| members.contains(&name.as_str()))
            })
            .map(|group| group.label)
    }

    /// What the module reset runs on the global target of a photo of `kind` whose controls have
    /// variants there, or `None` when none do. A superseded field belongs to its variant, so the
    /// reset is the patch of every other field to its default composed with each group reset's
    /// variant for `kind`, one entry that still reads as the module's reset: on a RAW photo Reset
    /// Basic also returns the development to As shot. `O(fields + groups)`.
    fn variant_reset(&self, kind: SourceTag) -> Option<Vec<ActionInput>> {
        let spec = &self.spec;
        let superseded = |field: &Field| field.variants.iter().any(|v| v.source == kind);
        let resets: Vec<ActionInput> = spec
            .all_groups()
            .into_iter()
            .flat_map(|group| &group.reset_variants)
            .filter(|variant| variant.source == kind)
            .filter_map(|variant| variant.reset.as_ref())
            .map(|reset| ActionInput {
                action_id: reset.action.clone(),
                parameters: reset.preset.clone(),
            })
            .collect();
        if resets.is_empty() && !spec.fields.iter().any(superseded) {
            return None;
        }
        let defaults: Map<String, Value> = spec
            .fields
            .iter()
            .filter(|field| !superseded(field))
            .map(|field| (field.name().to_owned(), field.default_value().clone()))
            .collect();
        let own = (!defaults.is_empty()).then(|| ActionInput {
            action_id: spec.set.id.to_owned(),
            parameters: defaults,
        });
        Some(own.into_iter().chain(resets).collect())
    }
}

impl<M: FieldPatch> ToolModule for FieldPatchModule<M> {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let parameters = if action_id == self.spec.set.id
            || self
                .spec
                .actions
                .iter()
                .any(|action| action.id == action_id)
        {
            // A patch stores exactly the fields the caller sent, which the generic check has
            // already validated against their declarations: the history entry, the label and
            // request deduplication all describe the patch, not the merged payload.
            parameters.clone()
        } else if action_id == self.spec.reset.id {
            Map::new()
        } else {
            return Err(Error::validation(format!("unknown action {action_id}")));
        };
        Ok(ActionInput {
            action_id: action_id.to_owned(),
            parameters,
        })
    }

    /// The sent fields merged over the stored layer, or over the defaults when there is none. An
    /// existing layer is updated in place unless the merge changes nothing; without one, a merge
    /// that is still neutral adds no layer at all and anything else commits one, which the host
    /// places by the effect's declared stage and order.
    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let spec = &self.spec;
        if spec
            .actions
            .iter()
            .any(|action| action.id == input.action_id)
        {
            return self.module.plan_extra(input, context);
        }
        let existing = context.own_layer(&spec.effect.id)?.map(|(_, layer)| layer);
        let current = match existing {
            Some(layer) => {
                self.read(&layer.effect_id, layer.effect_format, &layer.payload)?
                    .values
            }
            None => spec.defaults(),
        };
        let merged = if input.action_id == spec.set.id {
            let mut merged = current.clone();
            for (slot, field) in merged.iter_mut().zip(&spec.fields) {
                if let Some(value) = input.parameters.get(field.name()) {
                    check_value(&field.parameter, value)?;
                    *slot = field.canonical(value);
                }
            }
            merged
        } else if input.action_id == spec.reset.id {
            // On the global target of a photo whose controls have variants, the reset composes the
            // module's own fields with the variants' group resets.
            if context.target.is_none()
                && let Some(steps) = self.variant_reset(context.kind)
            {
                return Ok(ActionPlan::Compose(steps));
            }
            spec.defaults()
        } else {
            return Err(Error::validation(format!(
                "unknown action {}",
                input.action_id
            )));
        };
        match existing {
            // Canonical comparison, so writing a field's default on a layer that stores no key at
            // all is the no-op it looks like.
            Some(_) if current == merged => Ok(ActionPlan::NoOp),
            Some(layer) => Ok(ActionPlan::Update(LayerUpdate::new(
                layer.id.clone(),
                self.payload(&merged),
            ))),
            None => {
                let merged = self.values(merged);
                if self.module.is_neutral(&merged) {
                    return Ok(ActionPlan::NoOp);
                }
                Ok(ActionPlan::Commit(NewLayer::new(
                    spec.effect.id.clone(),
                    self.payload(&merged.values),
                )))
            }
        }
    }

    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        self.read(effect_id, format, payload).map(|_| ())
    }

    /// `Neutral` for the all-default payload, and otherwise every field that differs from its
    /// default, as its history label names it; every field's canonical value, defaults filled,
    /// named exactly as the patch's parameters are, so a client seeds its controls from the
    /// displayed entry; and neutral by the module's own rule over those values: every field at its
    /// default, or the vignette's amount of 0.
    fn describe(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<LayerReport, Error> {
        let values = self.read(effect_id, format, payload)?;
        let fields = &self.spec.fields;
        let summary = if values.is_default() {
            "Neutral".into()
        } else {
            fields
                .iter()
                .zip(&values.values)
                .filter(|(field, value)| *value != field.default_value())
                .map(|(field, value)| field.label(value))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let neutral = self.module.is_neutral(&values);
        Ok(LayerReport {
            summary,
            values: fields
                .iter()
                .zip(values.values)
                .map(|(field, value)| (field.name().to_owned(), value))
                .collect(),
            neutral,
        })
    }

    /// The one field a control moved, the group a reset cleared, the group a patch of exactly its
    /// fields set, a count of the fields a larger patch set, or the module's own reset. An empty
    /// patch changes nothing and commits no entry, so it keeps the action's title.
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        let spec = &self.spec;
        if input.action_id == spec.reset.id {
            return format!("Reset {}", spec.title);
        }
        if input.action_id != spec.set.id {
            return action.title.clone();
        }
        let sent: Vec<(&String, Value)> = input
            .parameters
            .iter()
            .map(|(name, value)| {
                let value = spec
                    .field(name)
                    .map_or_else(|| value.clone(), |field| field.canonical(value));
                (name, value)
            })
            .collect();
        if let Some(group) = self.reset_group(&sent) {
            return format!("Reset {group}");
        }
        if let ([_, _, ..], Some(wheel)) = (sent.as_slice(), self.whole_wheel(&sent)) {
            return wheel.to_owned();
        }
        if let ([_, _, ..], Some(group)) = (sent.as_slice(), self.whole_group(&sent)) {
            return group.to_owned();
        }
        match sent.as_slice() {
            [(name, value)] => match spec.field(name) {
                Some(field) => field.label(value),
                None => format!("{} {name}", spec.title),
            },
            [] => action.title.clone(),
            fields => format!("{} ({} fields)", spec.title, fields.len()),
        }
    }

    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if self.spec.queries.is_empty() {
            return Err(Error::validation(format!(
                "module {} declares no queries, so it cannot answer {query_id}",
                self.spec.id
            )));
        }
        self.module.query(query_id, parameters, context)
    }

    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: crate::CompileStage,
    ) -> Result<Processing, Error> {
        let values = self.read(effect_id, format, payload)?;
        // A neutral layer, by the module's own rule, compiles to no units in its stage's shape,
        // which the host drops entirely: the identity byte path and the shared source buffer are
        // kept, and the module is never asked to compile it. A colour or spatial layer compiled in
        // its GPU shape is the exception: a GPU plan's drafted layer holds every unit it can, a
        // neutral one as its identity, so the module compiles it whatever its values
        // (`CompileStage::gpu_shape`). No CPU compile asks for that shape.
        if self.module.is_neutral(&values) && !(at.gpu_shape && self.shape != Shape::Geometry) {
            return Ok(self.shape.neutral(at.stage));
        }
        self.module.compile(&values, at)
    }

    fn carry(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        input: Stage,
        orientation: Orientation,
    ) -> Result<Option<Value>, Error> {
        let values = self.read(effect_id, format, payload)?;
        self.module.carry(&values, input, orientation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stage;
    use crate::modules::{CurveChannel, FixedStage};
    use crate::{EFFECT_FORMAT, Layer, ModuleRegistry};
    use serde_json::json;

    const EFFECT: &str = "luxforge.test-patch.adjust";
    const SET: &str = "set-test-patch";
    const RESET: &str = "reset-test-patch";
    const STAGE: Stage = Stage {
        width: 4,
        height: 4,
    };

    /// A field of every kind: a number whose control has a RAW variant, a number declared with an
    /// integer default, and a boolean, an enum, a colour and a curve in one group whose curve
    /// control draws the curve field.
    #[derive(Debug, Default)]
    struct Test;

    impl FieldPatch for Test {
        fn spec() -> Spec {
            Spec::new(
                "luxforge.test-patch",
                "Test patch",
                "Every field kind",
                EFFECT,
                EffectStage::Color,
            )
            .maskable()
            .fields([
                Field::slider("level", "Level", "").variant(ControlVariant::control(
                    SourceTag::Raw,
                    "luxforge.other",
                    Control::number("set-other", "level", "Level"),
                )),
                Field::new(
                    ParameterDescriptor::number("midpoint", 0.0, 100.0).default(50),
                    "Midpoint",
                ),
                Field::new(ParameterDescriptor::boolean("on").default(false), "Enabled"),
                Field::new(
                    ParameterDescriptor::enumeration("mode", ["one", "two"]).default("one"),
                    "Mode",
                ),
                Field::new(
                    ParameterDescriptor::color("tint").default(json!([0, 0, 0])),
                    "Tint",
                ),
                Field::new(
                    ParameterDescriptor::curve("curve", 2, 4).default(json!([[0, 0], [1, 1]])),
                    "Curve",
                ),
            ])
            .group(
                Group::new("Level", ["level", "midpoint"]).reset_variant(ControlVariant::reset(
                    SourceTag::Raw,
                    "luxforge.other",
                    ResetAction {
                        action: "reset-other".into(),
                        preset: Map::new(),
                    },
                )),
            )
            .group(
                Group::new("Look", ["on", "mode", "tint", "curve"]).extra(Control::curve(
                    SET,
                    vec![CurveChannel {
                        parameter: "curve".into(),
                        label: "Curve".into(),
                    }],
                    "Curve",
                    "sample-curve",
                )),
            )
            .query(ActionDescriptor {
                parameters: vec![ParameterDescriptor::curve("curve", 2, 4)],
                ..ActionDescriptor::new("sample-curve", "Sample curve", "")
            })
        }

        /// Only a layer the neutrality rule calls not neutral reaches the module, so this answer
        /// shows which compilations the shared short-circuit answered instead.
        fn compile(&self, _: &Values<'_>, _: crate::CompileStage) -> Result<Processing, Error> {
            Err(Error::validation("compiled by the module"))
        }
    }

    #[test]
    fn restoration_field_patch_shape_is_spatial() {
        let mut spec = Test::spec();
        spec.effect.stage = EffectStage::Restoration;
        let (_, descriptor, shape) = spec.build().unwrap();
        assert_eq!(descriptor.effects[0].stage, EffectStage::Restoration);
        assert!(matches!(shape, Shape::Spatial));
        assert!(
            matches!(shape.neutral(Stage { width: 16, height: 12 }),Processing::Spatial(op) if op.is_empty())
        );
    }

    fn module() -> FieldPatchModule<Test> {
        FieldPatchModule::new()
    }

    fn set(parameters: Value) -> ActionInput {
        ActionInput {
            action_id: SET.into(),
            parameters: parameters.as_object().cloned().unwrap(),
        }
    }

    fn plan(input: &ActionInput, layers: &[Layer], kind: SourceTag) -> ActionPlan {
        let registry = ModuleRegistry::new();
        let stage = FixedStage::new(STAGE).of_kind(kind);
        module()
            .plan(input, &stage.context(layers, &registry))
            .unwrap()
    }

    fn layer(payload: Value) -> Layer {
        Layer::new(EFFECT, payload)
    }

    #[test]
    fn the_descriptor_draws_each_kind_by_its_own_control_and_folds_variants_onto_the_number() {
        let module = module();
        let descriptor = module.descriptor();
        let [level, look] = descriptor.controls.as_slice() else {
            panic!("two groups: {:?}", descriptor.controls);
        };
        let Control::Group(GroupControl {
            controls,
            reset: Some(reset),
            variants,
            ..
        }) = level
        else {
            panic!("a resettable group");
        };
        assert_eq!(variants.len(), 1, "the group's reset variant");
        assert_eq!(controls[0].variants().len(), 1, "the number's variant");
        assert_eq!(reset.action, SET);
        // The integer default 50 of a number field is declared, and reset to, as the number 50.
        assert_eq!(
            Value::Object(reset.preset.clone()),
            json!({"level": 0.0, "midpoint": 50.0})
        );
        assert!(reset.preset["midpoint"].is_f64());
        let Control::Group(GroupControl {
            controls,
            reset: Some(reset),
            ..
        }) = look
        else {
            panic!("a resettable group");
        };
        let kinds: Vec<&str> = controls.iter().map(Control::kind_name).collect();
        assert_eq!(kinds, ["toggle", "choice", "color", "curve"]);
        assert_eq!(
            Value::Object(reset.preset.clone()),
            json!({"on": false, "mode": "one", "tint": [0, 0, 0], "curve": [[0.0, 0.0], [1.0, 1.0]]})
        );
    }

    /// Every stored key is checked by the one generic parameter check, and every comparison is by
    /// canonical value: an integer spelling of a number or of a curve's coordinates is the same
    /// state, and a field set back to its default leaves the stored form.
    #[test]
    fn a_field_of_every_kind_reads_merges_and_drops_its_default_by_value() {
        let module = module();
        for (payload, refusal) in [
            (
                json!({"mode": "three"}),
                "parameter mode must be one of one, two",
            ),
            (
                json!({"tint": [256, 0, 0]}),
                "parameter tint must be three sRGB channels 0..=255",
            ),
            (json!({"on": 1}), "parameter on must be a boolean"),
            (
                json!({"curve": [[0.5, 0.5]]}),
                "parameter curve has an invalid curve point count",
            ),
            (
                json!({"level": 101}),
                "parameter level must be a number within -100..=100",
            ),
            (json!({"other": 1}), "unknown test-patch field other"),
        ] {
            let error = module
                .validate_payload(EFFECT, EFFECT_FORMAT, &payload)
                .unwrap_err();
            assert_eq!(error.detail, refusal, "{payload}");
        }

        let jpeg = SourceTag::Jpeg;
        for neutral in [
            json!({"curve": [[0, 0], [1, 1]]}),
            json!({"midpoint": 50.0, "on": false, "tint": [0, 0, 0]}),
        ] {
            assert_eq!(
                plan(&set(neutral.clone()), &[], jpeg),
                ActionPlan::NoOp,
                "{neutral}"
            );
        }
        let ActionPlan::Commit(new) = plan(&set(json!({"on": true, "mode": "two"})), &[], jpeg)
        else {
            panic!("a first set of two fields commits");
        };
        assert_eq!(new.payload, json!({"on": true, "mode": "two"}));

        let stored = layer(json!({"on": true, "mode": "two", "level": 1.0}));
        let stack = std::slice::from_ref(&stored);
        for same in [
            json!({"level": 1}),
            json!({"mode": "two", "curve": [[0.0, 0.0], [1, 1]]}),
            json!({}),
        ] {
            assert_eq!(
                plan(&set(same.clone()), stack, jpeg),
                ActionPlan::NoOp,
                "{same}"
            );
        }
        let ActionPlan::Update(update) = plan(
            &set(json!({"on": false, "curve": [[0, 0], [0.5, 1], [1, 1]]})),
            stack,
            jpeg,
        ) else {
            panic!("an update in place");
        };
        assert_eq!(
            update.payload,
            json!({"level": 1.0, "mode": "two", "curve": [[0.0, 0.0], [0.5, 1.0], [1.0, 1.0]]})
        );
        assert_eq!(
            Value::Object(
                module
                    .describe(EFFECT, EFFECT_FORMAT, &json!({}))
                    .unwrap()
                    .values
            ),
            json!({"level": 0.0, "midpoint": 50.0, "on": false, "mode": "one", "tint": [0, 0, 0], "curve": [[0.0, 0.0], [1.0, 1.0]]})
        );
    }

    /// A curve field reads as its points in the canonical form, whether the payload spells a
    /// coordinate as an integer or a float, and as its default when the payload omits it.
    #[test]
    fn values_read_a_curve_field_in_its_canonical_form() {
        let module = module();
        let values = module
            .read(
                EFFECT,
                EFFECT_FORMAT,
                &json!({"curve": [[0, 0.125], [0.5, 0.75], [1, 1]]}),
            )
            .unwrap();
        assert_eq!(
            values.curve("curve"),
            vec![[0.0, 0.125], [0.5, 0.75], [1.0, 1.0]]
        );
        let defaults = module.read(EFFECT, EFFECT_FORMAT, &json!({})).unwrap();
        assert_eq!(defaults.curve("curve"), vec![[0.0, 0.0], [1.0, 1.0]]);
    }

    #[test]
    #[should_panic(expected = "a module reads a curve only from its own curve fields")]
    fn values_refuse_to_read_a_number_field_as_a_curve() {
        let module = module();
        let values = module.read(EFFECT, EFFECT_FORMAT, &json!({})).unwrap();
        values.curve("level");
    }

    #[test]
    fn labels_and_descriptions_name_a_field_of_every_kind() {
        let module = module();
        for (parameters, expected) in [
            (json!({"level": 20}), "Level +20"),
            (json!({"midpoint": 60}), "Midpoint 60"),
            (json!({"on": true}), "Enabled on"),
            (json!({"on": false}), "Enabled off"),
            (json!({"mode": "two"}), "Mode Two"),
            (json!({"tint": [1, 2, 3]}), "Tint 1,2,3"),
            (
                json!({"curve": [[0, 0], [0.5, 0.7], [1, 1]]}),
                "Curve 3 points",
            ),
            // A patch of exactly a group's fields at their defaults is that group's reset,
            // however each default is spelled, and one of exactly its fields is that group.
            (
                json!({"on": false, "mode": "one", "tint": [0, 0, 0], "curve": [[0, 0], [1, 1]]}),
                "Reset Look",
            ),
            (
                json!({"on": true, "mode": "two", "tint": [9, 9, 9], "curve": [[0, 1], [1, 0]]}),
                "Look",
            ),
            (json!({"level": 0, "midpoint": 50}), "Reset Level"),
            (json!({"on": true, "mode": "two"}), "Test patch (2 fields)"),
        ] {
            assert_eq!(
                module.label(&module.descriptor().actions[0], &set(parameters.clone())),
                expected,
                "{parameters}"
            );
        }
        let described = module
            .describe(
                EFFECT,
                EFFECT_FORMAT,
                &json!({"curve": [[0, 1], [1, 0]], "on": true, "mode": "two", "midpoint": 50}),
            )
            .unwrap()
            .summary;
        assert_eq!(described, "Enabled on, Mode Two, Curve 2 points");
        assert_eq!(
            module
                .describe(EFFECT, EFFECT_FORMAT, &json!({"midpoint": 50, "on": false}))
                .unwrap()
                .summary,
            "Neutral"
        );
    }

    /// On a photo whose controls have variants, the module reset writes the default of every field
    /// that is not superseded, whatever its kind, and then the group reset's variant.
    #[test]
    fn the_variant_reset_writes_every_unsuperseded_default_of_any_kind() {
        let reset = ActionInput {
            action_id: RESET.into(),
            parameters: Map::new(),
        };
        let stored = layer(json!({"on": true}));
        let stack = std::slice::from_ref(&stored);
        let ActionPlan::Compose(steps) = plan(&reset, stack, SourceTag::Raw) else {
            panic!("a composite on a RAW photo");
        };
        assert_eq!(
            steps,
            [
                set(
                    json!({"midpoint": 50.0, "on": false, "mode": "one", "tint": [0, 0, 0], "curve": [[0.0, 0.0], [1.0, 1.0]]})
                ),
                ActionInput {
                    action_id: "reset-other".into(),
                    parameters: Map::new(),
                },
            ]
        );
        assert!(matches!(
            plan(&reset, stack, SourceTag::Jpeg),
            ActionPlan::Update(_)
        ));
    }

    /// `Spec::new` derives the actions from the id and title, and every effect it makes holds the
    /// fields no field-patch module may change: the shared format, no artifacts, `single` and every
    /// source kind. Only the order and the maskable flag are the module's to set.
    #[test]
    fn a_spec_derives_its_actions_and_forces_the_effects_shared_fields() {
        let module = module();
        let descriptor = module.descriptor();
        assert_eq!(
            descriptor.effects,
            [EffectDescriptor {
                maskable: true,
                single: true,
                ..EffectDescriptor::new(EFFECT, EffectStage::Color,)
            }]
        );
        let actions: Vec<(&str, &str, &str, bool)> = descriptor
            .actions
            .iter()
            .map(|action| {
                (
                    action.id.as_str(),
                    action.title.as_str(),
                    action.notes.as_str(),
                    action.patch,
                )
            })
            .collect();
        assert_eq!(
            actions,
            [
                (
                    SET,
                    "Set Test patch",
                    "merges the named test-patch fields into the stack's one Test patch layer, \
                     which the host places by its effect's declared stage and order on the first \
                     non-neutral value and updates in place afterwards; omitted fields keep their \
                     stored values and a patch that changes nothing is a reported no-op",
                    true,
                ),
                (
                    RESET,
                    "Reset Test patch",
                    "returns the stack's one Test patch layer to its neutral payload, keeping its \
                     identity and position; a no-op without one and when it is already neutral",
                    false,
                ),
            ]
        );
        assert_eq!(
            descriptor.reset.as_ref().map(|r| r.action.as_str()),
            Some(RESET)
        );

        let spec = Spec::new(
            "luxforge.other",
            "Other",
            "",
            "luxforge.other.fx",
            EffectStage::Finish,
        )
        .order(7)
        .set_notes("its own words")
        .reset_notes("its own reset");
        assert_eq!(
            (spec.noun, spec.effect.order, spec.effect.maskable),
            ("other", 7, false)
        );
        assert_eq!(
            (spec.set.notes.as_str(), spec.reset.notes.as_str()),
            ("its own words", "its own reset")
        );
    }

    /// A layer the module's own neutrality rule calls neutral compiles to no units, in the shape
    /// its effect's stage takes, without asking the module; any other layer is the module's.
    #[test]
    fn a_neutral_layer_compiles_to_no_units_in_its_stages_shape_without_the_module() {
        let module = module();
        for neutral in [json!({}), json!({"midpoint": 50, "on": false})] {
            let Ok(Processing::Color(operation)) = module.compile(
                EFFECT,
                EFFECT_FORMAT,
                &neutral,
                crate::CompileStage::exact(STAGE),
            ) else {
                panic!("{neutral} compiles to the neutral colour operation");
            };
            assert!(operation.is_empty(), "{neutral}");
        }
        assert_eq!(
            module
                .compile(
                    EFFECT,
                    EFFECT_FORMAT,
                    &json!({"on": true}),
                    crate::CompileStage::exact(STAGE)
                )
                .err()
                .map(|error| error.detail),
            Some("compiled by the module".to_owned())
        );
        for (stage, color) in [
            (EffectStage::Color, true),
            (EffectStage::Finish, true),
            (EffectStage::Spatial, false),
        ] {
            match Shape::of(stage)
                .expect("a field patch stage")
                .neutral(STAGE)
            {
                Processing::Color(operation) if color => assert!(operation.is_empty()),
                Processing::Spatial(operation) if !color => assert!(operation.is_empty()),
                _ => panic!("{} compiles to the wrong shape", stage.as_str()),
            }
        }
    }

    /// A spec is refused when it is built, naming the field or group, when its effect is at a stage
    /// no field patch can own, a field is outside the vocabulary, lacks a valid default, or declares
    /// what its control cannot draw or carry, and when a group names a field the table does not
    /// declare or a field is in no group or in more than one.
    #[test]
    fn a_spec_is_refused_where_a_field_declares_what_its_control_cannot_carry() {
        type Change = fn(&mut Spec);
        let refused = |change: Change| {
            let mut spec = Test::spec();
            change(&mut spec);
            spec.build().err().expect("the spec is refused").detail
        };
        let cases: [(Change, &str); 12] = [
            (
                |spec| {
                    spec.fields[2].variants.push(ControlVariant::control(
                        SourceTag::Raw,
                        "luxforge.other",
                        Control::toggle("set-other", "on", "On"),
                    ))
                },
                "field on: its toggle control cannot carry variants",
            ),
            (
                |spec| spec.fields[4].rail = Some(RailDecoration::Hue),
                "field tint: its color control cannot carry a rail",
            ),
            (
                |spec| spec.fields[5].control = FieldControl::Number(NumberStyle::Slider),
                "field curve: a number control cannot draw its curve parameter",
            ),
            (
                |spec| spec.fields[3].control = FieldControl::Toggle,
                "field mode: a toggle control cannot draw its enum parameter",
            ),
            (
                |spec| {
                    spec.fields
                        .push(Field::new(ParameterDescriptor::string("name", 8), "Name"))
                },
                "field name: a field patch cannot hold its string parameter",
            ),
            (
                |spec| spec.fields[1].parameter.default = None,
                "field midpoint declares no default",
            ),
            (
                |spec| spec.fields[3].parameter.default = Some(json!("three")),
                "field mode's default is invalid: parameter mode must be one of one, two",
            ),
            (
                |spec| spec.groups[0].fields[1] = "midpiont",
                "group Level lists midpiont, which is not a declared field",
            ),
            (
                |spec| spec.groups[1].fields.retain(|name| *name != "tint"),
                "field tint is in no group",
            ),
            (
                |spec| spec.groups[1].fields.push("level"),
                "field level is listed 2 times in the groups, not once",
            ),
            (
                |spec| spec.groups[0].fields.push("level"),
                "field level is listed 2 times in the groups, not once",
            ),
            (
                |spec| spec.effect.stage = EffectStage::Pixel,
                "a field patch cannot own a pixel effect",
            ),
        ];
        for (change, expected) in cases {
            assert_eq!(
                refused(change),
                format!("field-patch module luxforge.test-patch: {expected}")
            );
        }
    }

    /// Nested groups, presentation-only views and wheels, shaped as the colour mixer's HSL and
    /// Grading tabs are: HSL's two subgroups are reset scopes of their own, and Grading's two
    /// views show the same fields, one in a compact wheel and one in a large wheel beside its
    /// numbers.
    #[derive(Debug, Default)]
    struct Graded;

    const GRADED_SET: &str = "set-graded";

    fn shadows() -> Wheel {
        Wheel::new("Shadows", "shadows-hue", "shadows-saturation")
            .luminance("shadows-luminance")
            .history("Shadows tint")
    }

    fn global() -> Wheel {
        Wheel::new("Global", "global-hue", "global-saturation")
    }

    impl FieldPatch for Graded {
        fn spec() -> Spec {
            let hue = |name: &'static str, label: &str| {
                Field::new(
                    ParameterDescriptor::number(name, 0.0, 360.0).default(0.0),
                    label,
                )
            };
            let amount = |name: &'static str, label: &str, default: f64| {
                Field::new(
                    ParameterDescriptor::number(name, 0.0, 100.0).default(default),
                    label,
                )
            };
            Spec::new(
                "luxforge.graded",
                "Graded",
                "Nested views",
                "luxforge.graded.adjust",
                EffectStage::Color,
            )
            .fields([
                Field::slider("red-hue", "Red", "").history("Red hue"),
                Field::slider("green-hue", "Green", "").history("Green hue"),
                Field::slider("red-saturation", "Red", "").history("Red saturation"),
                Field::slider("green-saturation", "Green", "").history("Green saturation"),
                hue("shadows-hue", "Hue").history("Shadows hue"),
                amount("shadows-saturation", "Saturation", 0.0),
                Field::slider("shadows-luminance", "Luminance", ""),
                hue("global-hue", "Hue"),
                amount("global-saturation", "Saturation", 0.0),
                amount("blending", "Blending", 50.0),
            ])
            .group(
                Group::new("HSL", [])
                    .tabs()
                    .subgroup(Group::new("Hue", ["red-hue", "green-hue"]))
                    .subgroup(Group::new(
                        "Saturation",
                        ["red-saturation", "green-saturation"],
                    )),
            )
            .group(
                Group::new(
                    "Grading",
                    [
                        "shadows-hue",
                        "shadows-saturation",
                        "shadows-luminance",
                        "global-hue",
                        "global-saturation",
                        "blending",
                    ],
                )
                .tabs()
                .view(
                    View::new("3-way")
                        .wheel(shadows())
                        .wheel(global())
                        .field("blending"),
                )
                .view(
                    View::new("Shadows")
                        .wheel(shadows().large())
                        .field("shadows-hue")
                        .field("shadows-saturation")
                        .field("blending"),
                ),
            )
        }

        fn compile(&self, _: &Values<'_>, _: crate::CompileStage) -> Result<Processing, Error> {
            Ok(Processing::Color(ColorOperation::neutral()))
        }
    }

    fn reset_preset(control: &Control) -> Value {
        let reset = match control {
            Control::Group(group) => group.reset.as_ref(),
            Control::Wheel(wheel) => wheel.reset.as_ref(),
            _ => None,
        };
        reset.map_or(Value::Null, |reset| Value::Object(reset.preset.clone()))
    }

    #[test]
    fn nested_groups_views_and_wheels_build_one_descriptor_with_their_own_resets() {
        let module = FieldPatchModule::<Graded>::new();
        let descriptor = module.descriptor();
        descriptor
            .validate()
            .expect("the built descriptor registers");
        let [Control::Group(hsl), Control::Group(grading)] = descriptor.controls.as_slice() else {
            panic!("two top-level groups: {:?}", descriptor.controls);
        };
        assert_eq!(hsl.layout, ModuleLayout::Tabs);
        assert_eq!(
            reset_preset(&descriptor.controls[0]),
            json!({"red-hue": 0.0, "green-hue": 0.0, "red-saturation": 0.0, "green-saturation": 0.0})
        );
        let [hue, saturation] = hsl.controls.as_slice() else {
            panic!("HSL's two subgroups");
        };
        assert_eq!(reset_preset(hue), json!({"red-hue": 0.0, "green-hue": 0.0}));
        assert_eq!(
            reset_preset(saturation),
            json!({"red-saturation": 0.0, "green-saturation": 0.0})
        );
        // Grading's reset covers every field its views show, Blending at its default of 50.
        assert_eq!(grading.layout, ModuleLayout::Tabs);
        assert_eq!(
            reset_preset(&descriptor.controls[1]),
            json!({"shadows-hue": 0.0, "shadows-saturation": 0.0, "shadows-luminance": 0.0,
                   "global-hue": 0.0, "global-saturation": 0.0, "blending": 50.0})
        );
        let [Control::Group(three), Control::Group(single)] = grading.controls.as_slice() else {
            panic!("Grading's two views");
        };
        assert!(three.view && single.view);
        assert!(
            three.reset.is_none() && single.reset.is_none(),
            "a view resets nothing"
        );
        let kinds = |group: &GroupControl| -> Vec<&str> {
            group.controls.iter().map(Control::kind_name).collect()
        };
        assert_eq!(kinds(three), ["wheel", "wheel", "number"]);
        assert_eq!(kinds(single), ["wheel", "number", "number", "number"]);
        let Control::Wheel(compact) = &three.controls[0] else {
            unreachable!()
        };
        let Control::Wheel(large) = &single.controls[0] else {
            unreachable!()
        };
        assert_eq!(
            (compact.style, large.style),
            (WheelStyle::Compact, WheelStyle::Large)
        );
        assert_eq!(compact.luminance.as_deref(), Some("shadows-luminance"));
        assert_eq!(
            reset_preset(&three.controls[0]),
            json!({"shadows-hue": 0.0, "shadows-saturation": 0.0, "shadows-luminance": 0.0}),
            "a wheel's reset is exactly its own fields"
        );
        assert_eq!(
            reset_preset(&three.controls[1]),
            json!({"global-hue": 0.0, "global-saturation": 0.0})
        );
    }

    #[test]
    fn nested_groups_and_wheels_name_their_history_entries() {
        let module = FieldPatchModule::<Graded>::new();
        let action = &module.descriptor().actions[0];
        let label = |parameters: Value| {
            module.label(
                action,
                &ActionInput {
                    action_id: GRADED_SET.into(),
                    parameters: parameters.as_object().cloned().unwrap(),
                },
            )
        };
        for (parameters, expected) in [
            (json!({"red-hue": 0, "green-hue": 0}), "Reset Hue"),
            (
                json!({"red-hue": 0, "green-hue": 0, "red-saturation": 0, "green-saturation": 0}),
                "Reset HSL",
            ),
            (
                json!({"shadows-hue": 0, "shadows-saturation": 0, "shadows-luminance": 0,
                       "global-hue": 0, "global-saturation": 0, "blending": 50}),
                "Reset Grading",
            ),
            (
                json!({"shadows-hue": 0, "shadows-saturation": 0, "shadows-luminance": 0}),
                "Reset Shadows",
            ),
            (
                json!({"global-hue": 0, "global-saturation": 0}),
                "Reset Global",
            ),
            // One wheel gesture is one entry, named by the wheel.
            (
                json!({"shadows-hue": 120, "shadows-saturation": 40}),
                "Shadows tint",
            ),
            (json!({"global-hue": 359, "global-saturation": 5}), "Global"),
            (json!({"shadows-hue": 30}), "Shadows hue 30"),
            (json!({"red-hue": 5, "green-hue": -5}), "Hue"),
        ] {
            assert_eq!(label(parameters.clone()), expected, "{parameters}");
        }
    }

    /// A hue at zero saturation is still a setting: it commits and survives as stored state.
    #[test]
    fn a_dormant_hue_is_kept() {
        let registry = ModuleRegistry::new();
        let stage = FixedStage::new(STAGE).of_kind(SourceTag::Jpeg);
        let module = FieldPatchModule::<Graded>::new();
        let input = ActionInput {
            action_id: GRADED_SET.into(),
            parameters: json!({"shadows-hue": 200}).as_object().cloned().unwrap(),
        };
        let ActionPlan::Commit(new) = module.plan(&input, &stage.context(&[], &registry)).unwrap()
        else {
            panic!("a dormant hue commits a layer");
        };
        assert_eq!(new.payload, json!({"shadows-hue": 200.0}));
    }

    /// A spec whose views or wheels draw fields their group does not own, or lose one of its
    /// fields from every view, is refused when it is built, naming the view, wheel or field.
    #[test]
    fn a_spec_is_refused_where_a_view_or_wheel_misdraws_its_group() {
        type Change = fn(&mut Spec);
        let refused = |change: Change| {
            let mut spec = Graded::spec();
            change(&mut spec);
            spec.build().err().expect("the spec is refused").detail
        };
        let cases: [(Change, &str); 6] = [
            (
                |spec| {
                    spec.groups[1].views[0]
                        .entries
                        .push(ViewEntry::Field("red-hue"))
                },
                "view 3-way of group Grading draws red-hue, which is not a field of the group",
            ),
            (
                |spec| {
                    spec.groups[1].views[1]
                        .entries
                        .push(ViewEntry::Wheel(Wheel::new(
                            "Red",
                            "red-hue",
                            "global-saturation",
                        )))
                },
                "wheel Red of group Grading draws red-hue, which is not a field of the group",
            ),
            (
                |spec| {
                    spec.groups[1].views[0].entries.retain(|entry| {
                        !matches!(entry, ViewEntry::Wheel(wheel) if wheel.label == "Global")
                    })
                },
                "field global-hue of group Grading is shown by none of its views",
            ),
            (
                |spec| {
                    spec.groups[1].views[0]
                        .entries
                        .push(ViewEntry::Wheel(Wheel::new(
                            "Twice",
                            "global-hue",
                            "global-hue",
                        )))
                },
                "wheel Twice binds global-hue twice",
            ),
            (
                |spec| spec.groups[1].wheels.push(global()),
                "group Grading declares views, so its wheels belong in a view",
            ),
            // The control vocabulary's own rules are refused at the build too: a wheel's angle
            // must be a hue of 0..=360 degrees.
            (
                |spec| {
                    spec.groups[1].views[0]
                        .entries
                        .push(ViewEntry::Wheel(Wheel::new(
                            "Saturated",
                            "global-saturation",
                            "blending",
                        )))
                },
                "wheel hue global-saturation of action set-graded declares 0..=100, not the \
                 0..=360 degrees a wheel's angle spans",
            ),
        ];
        for (change, expected) in cases {
            assert_eq!(
                refused(change),
                format!("field-patch module luxforge.graded: {expected}")
            );
        }
    }

    /// A stacked group's wheel draws its fields in place of their own controls, at its hue
    /// field's position.
    #[test]
    fn a_stacked_groups_wheel_replaces_its_fields_own_controls() {
        let mut spec = Graded::spec();
        spec.groups[1].views.clear();
        spec.groups[1].layout = ModuleLayout::Stacked;
        let grading = spec.groups.remove(1);
        spec.groups.push(grading.wheel(shadows()));
        let (_, descriptor, _) = spec.build().expect("a stacked group with a wheel");
        let Control::Group(grading) = &descriptor.controls[1] else {
            unreachable!()
        };
        let kinds: Vec<&str> = grading.controls.iter().map(Control::kind_name).collect();
        assert_eq!(kinds, ["wheel", "number", "number", "number"]);
    }
}
