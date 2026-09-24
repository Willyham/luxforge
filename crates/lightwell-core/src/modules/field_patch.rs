//! The declarative field-patch module: a module that owns one layer of one effect, edited by a
//! `set-<name>` field patch and a `reset-<name>` action, whose payload is a JSON object of numeric
//! fields in which a missing key means that field's default.
//!
//! Basic, the colour mixer, Presence and the vignette are each a [`Spec`] — the field table, its
//! groups and the module's identity — and a [`FieldPatch::compile`]. Everything they share lives
//! here once: the descriptor built from the table, parsing, planning a commit, update or no-op,
//! payload validation, the canonical stored form, values, history labels, the recipe row and
//! neutrality.
//!
//! Every comparison is between canonical values, never between JSON maps, so `{}` and a payload
//! that spells a default out (`{"exposure": 0}`, `{"midpoint": 50}`) are the same state and are
//! never mistaken for a change. The canonical stored form holds only the fields that differ from
//! their default, so the all-default payload is exactly `{}`.
//!
//! The host checks a request's fields against the declared ranges before `parse`, on every path,
//! so planning merges the fields it is given without checking them again. A stored payload is
//! checked in full by [`ToolModule::validate_payload`] and wherever it is read.

use super::{
    ActionDescriptor, ActionInput, ActionPlan, Availability, CanvasInteraction, Control,
    EffectDescriptor, LayerUpdate, ModuleDescriptor, ModuleLayout, NewLayer, NumberStyle,
    ParameterDescriptor, ParameterKind, Processing, RailDecoration, ResetAction, Stage,
    StageContext, ToolModule,
};
use crate::{Error, ErrorKind};
use serde_json::{Map, Number, Value};

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn incompatible(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Incompatible, detail)
}

/// A finite f64 as a JSON number. Every value written here is finite, so the fallback is never
/// reached in practice and never panics if it is.
pub(crate) fn number(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// One numeric field: its payload key and parameter, the slider that sets it and the words a
/// history label uses for it.
pub struct Field {
    /// The payload key and the `set-<name>` parameter.
    pub name: &'static str,
    /// The slider's label.
    pub label: String,
    /// What a history label and the recipe row call the field: `Exposure`, `Red hue`, `Vignette
    /// amount`. A field whose range is signed shows its value with a sign (`+20`, `-35`); one
    /// whose range starts at 0 does not (`60`).
    pub history: String,
    pub min: f64,
    pub max: f64,
    /// What a missing key means, and the value a reset writes.
    pub default: f64,
    pub step: f64,
    /// Display decimals, for the slider and for the history label.
    pub precision: u8,
    pub unit: Option<&'static str>,
    /// The parameter's declared `zero` hint.
    pub zero: Option<f64>,
    pub rail: Option<RailDecoration>,
    pub notes: String,
}

impl Field {
    /// A field in the -100..100 slider range with step 1 and no decimals, the range most fields
    /// share, defaulting to 0 with no unit, rail or zero hint.
    pub fn slider(name: &'static str, label: impl Into<String>, notes: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            name,
            history: label.clone(),
            label,
            min: -100.0,
            max: 100.0,
            default: 0.0,
            step: 1.0,
            precision: 0,
            unit: None,
            zero: None,
            rail: None,
            notes: notes.into(),
        }
    }

    fn parameter(&self) -> ParameterDescriptor {
        ParameterDescriptor {
            name: self.name.into(),
            kind: ParameterKind::Number {
                min: self.min,
                max: self.max,
            },
            required: false,
            default: Some(number(self.default)),
            unit: self.unit.map(Into::into),
            step: Some(self.step),
            precision: Some(self.precision),
            notes: self.notes.clone(),
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: self.zero,
        }
    }

    /// The field and its value as a history label and the recipe row name them: the history name,
    /// the value with its declared decimals (signed for a signed range) and the declared unit.
    fn label(&self, value: f64) -> String {
        let precision = usize::from(self.precision);
        let shown = if self.min < 0.0 {
            format!("{value:+.precision$}")
        } else {
            format!("{value:.precision$}")
        };
        match self.unit {
            Some(unit) => format!("{} {shown} {unit}", self.history),
            None => format!("{} {shown}", self.history),
        }
    }
}

/// A group of sliders on the module's section. Its reset sets exactly its fields to their
/// defaults, and a patch that does so is labelled `Reset <label>` however it was sent.
pub struct Group {
    pub label: &'static str,
    pub fields: Vec<&'static str>,
    pub collapsed: bool,
    /// Controls drawn after the group's sliders, such as Basic's neutral picker.
    pub extra: Vec<Control>,
}

/// An action's identity and the words discovery shows for it.
pub struct ActionText {
    pub id: &'static str,
    pub title: &'static str,
    pub notes: &'static str,
}

/// Everything a field-patch module declares: its identity, its one effect, its two actions, the
/// field table and how the fields are grouped and laid out. The descriptor is built from it once.
pub struct Spec {
    pub id: &'static str,
    pub title: &'static str,
    pub hint: &'static str,
    /// The word payload errors use: `basic field exposure must be a finite number`.
    pub noun: &'static str,
    pub effect: EffectDescriptor,
    /// The field patch, `set-<name>`.
    pub set: ActionText,
    /// The action that returns the layer to its all-default payload, `reset-<name>`.
    pub reset: ActionText,
    /// Every field, in the payload's declared order.
    pub fields: Vec<Field>,
    pub groups: Vec<Group>,
    pub queries: Vec<ActionDescriptor>,
    pub canvas: Option<CanvasInteraction>,
    pub collapsed: bool,
    pub layout: ModuleLayout,
}

impl Spec {
    fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name == name)
    }

    fn defaults(&self) -> Vec<f64> {
        self.fields.iter().map(|field| field.default).collect()
    }

    fn descriptor(&self) -> ModuleDescriptor {
        let slider = |field: &Field| Control::Number {
            action: self.set.id.into(),
            parameter: field.name.into(),
            label: field.label.clone(),
            style: NumberStyle::Slider,
            rail: field.rail.clone(),
            reset: None,
        };
        let controls = self
            .groups
            .iter()
            .map(|group| Control::Group {
                label: group.label.into(),
                reset: Some(ResetAction {
                    action: self.set.id.into(),
                    preset: group
                        .fields
                        .iter()
                        .filter_map(|name| self.field(name))
                        .map(|field| (field.name.to_owned(), number(field.default)))
                        .collect(),
                }),
                controls: group
                    .fields
                    .iter()
                    .filter_map(|name| self.field(name))
                    .map(slider)
                    .chain(group.extra.iter().cloned())
                    .collect(),
                collapsed: group.collapsed,
            })
            .collect();
        ModuleDescriptor {
            id: self.id.into(),
            title: self.title.into(),
            hint: Some(self.hint.into()),
            effects: vec![self.effect.clone()],
            actions: vec![
                ActionDescriptor {
                    id: self.set.id.into(),
                    title: self.set.title.into(),
                    notes: self.set.notes.into(),
                    summary: None,
                    patch: true,
                    parameters: self.fields.iter().map(Field::parameter).collect(),
                },
                ActionDescriptor {
                    id: self.reset.id.into(),
                    title: self.reset.title.into(),
                    notes: self.reset.notes.into(),
                    summary: None,
                    patch: false,
                    parameters: Vec::new(),
                },
            ],
            queries: self.queries.clone(),
            controls,
            reset: Some(ResetAction {
                action: self.reset.id.into(),
                preset: Map::new(),
            }),
            canvas: self.canvas.clone(),
            developer: false,
            collapsed: self.collapsed,
            layout: self.layout,
            availability: Availability::Available,
            ..ModuleDescriptor::default()
        }
    }
}

/// The canonical values of one payload, one per field in the table's order, with every missing
/// key read as its field's default.
pub struct Values<'a> {
    fields: &'a [Field],
    values: Vec<f64>,
}

impl Values<'_> {
    /// One field's value, by name. Every name a module asks for is one of its own fields.
    pub fn get(&self, name: &str) -> f64 {
        let index = self
            .fields
            .iter()
            .position(|field| field.name == name)
            .expect("a module reads only its own declared fields");
        self.values[index]
    }

    /// Every value, in the field table's order.
    pub fn as_slice(&self) -> &[f64] {
        &self.values
    }

    /// Whether every field holds its default.
    pub fn all_default(&self) -> bool {
        self.fields
            .iter()
            .zip(&self.values)
            .all(|(field, value)| *value == field.default)
    }
}

/// What one field-patch module provides beyond its table: turning canonical values into processing,
/// and, when the rule differs from "every field at its default", which values change nothing.
pub trait FieldPatch: Send + Sync + 'static {
    /// The module's identity, effect, actions and field table.
    fn spec() -> Spec
    where
        Self: Sized;

    /// The processing these canonical values compile to at the layer's input stage.
    fn compile(&self, values: &Values<'_>, stage: Stage) -> Result<Processing, Error>;

    /// Whether these values change nothing, so a first set that reaches them commits no layer. The
    /// default is every field at its default; the vignette's is an amount of 0, whatever its shape
    /// fields hold.
    fn is_neutral(&self, values: &Values<'_>) -> bool {
        values.all_default()
    }

    /// Answer one of the queries the spec declares. Only Basic declares one.
    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        let _ = (parameters, context);
        Err(validation(format!("unknown query {query_id}")))
    }
}

/// A [`FieldPatch`] as a [`ToolModule`]: the descriptor built from its spec once, and every shared
/// behaviour of a field-patch module implemented from the table.
pub struct FieldPatchModule<M> {
    module: M,
    spec: Spec,
    descriptor: ModuleDescriptor,
}

impl<M: FieldPatch + Default> FieldPatchModule<M> {
    pub fn new() -> Self {
        let spec = M::spec();
        let descriptor = spec.descriptor();
        Self {
            module: M::default(),
            spec,
            descriptor,
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
    fn values(&self, values: Vec<f64>) -> Values<'_> {
        Values {
            fields: &self.spec.fields,
            values,
        }
    }

    /// The canonical values of a stored payload, with the effect identity and format checked
    /// first: an unsupported format is `incompatible` and is never rewritten, and every key is a
    /// declared field holding a finite number inside its declared range.
    fn read(&self, effect_id: &str, format: u32, payload: &Value) -> Result<Values<'_>, Error> {
        let spec = &self.spec;
        let noun = spec.noun;
        if effect_id != spec.effect.id {
            return Err(incompatible(format!("unavailable effect {effect_id}")));
        }
        if format != spec.effect.format {
            return Err(incompatible(format!("unsupported effect format {format}")));
        }
        let object = payload
            .as_object()
            .ok_or_else(|| validation(format!("{noun} payload must be a JSON object")))?;
        for (name, value) in object {
            let field = spec
                .field(name)
                .ok_or_else(|| validation(format!("unknown {noun} field {name}")))?;
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    validation(format!("{noun} field {name} must be a finite number"))
                })?;
            let (min, max) = (field.min, field.max);
            if number < min || number > max {
                return Err(validation(format!(
                    "{noun} field {name} must be a number within {min}..={max}"
                )));
            }
        }
        Ok(self.values(
            spec.fields
                .iter()
                .map(|field| {
                    object
                        .get(field.name)
                        .and_then(Value::as_f64)
                        .unwrap_or(field.default)
                })
                .collect(),
        ))
    }

    /// The canonical stored form of a set of values: only the fields that differ from their
    /// default, so the all-default payload is exactly `{}`.
    fn payload(&self, values: &[f64]) -> Value {
        Value::Object(
            self.spec
                .fields
                .iter()
                .zip(values)
                .filter(|(field, value)| **value != field.default)
                .map(|(field, value)| (field.name.to_owned(), number(*value)))
                .collect(),
        )
    }

    /// The group a patch returns entirely to its defaults, when it is one: a patch holding exactly
    /// one group's fields, each at its default, is that group's reset however it was sent — from
    /// the group's header, a keyboard reset or an API call.
    fn reset_group(&self, sent: &[(&String, f64)]) -> Option<&'static str> {
        self.spec
            .groups
            .iter()
            .find(|group| {
                sent.len() == group.fields.len()
                    && sent.iter().all(|(name, value)| {
                        group.fields.contains(&name.as_str())
                            && self
                                .spec
                                .field(name)
                                .is_some_and(|field| *value == field.default)
                    })
            })
            .map(|group| group.label)
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
        let parameters = if action_id == self.spec.set.id {
            // A patch stores exactly the fields the caller sent, which the generic check has
            // already validated against the declared ranges: the history entry, the label and
            // request deduplication all describe the patch, not the merged payload.
            parameters.clone()
        } else if action_id == self.spec.reset.id {
            Map::new()
        } else {
            return Err(validation(format!("unknown action {action_id}")));
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
                if let Some(value) = input.parameters.get(field.name) {
                    *slot = value.as_f64().ok_or_else(|| {
                        validation(format!(
                            "{} field {} must be a number",
                            spec.noun, field.name
                        ))
                    })?;
                }
            }
            merged
        } else if input.action_id == spec.reset.id {
            spec.defaults()
        } else {
            return Err(validation(format!("unknown action {}", input.action_id)));
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
                    self.payload(merged.as_slice()),
                )))
            }
        }
    }

    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        self.read(effect_id, format, payload).map(|_| ())
    }

    /// `Neutral` for the all-default payload, and otherwise every field that differs from its
    /// default, as its history label names it.
    fn describe_layer(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<String, Error> {
        let values = self.read(effect_id, format, payload)?;
        if values.all_default() {
            return Ok("Neutral".into());
        }
        Ok(self
            .spec
            .fields
            .iter()
            .zip(values.as_slice())
            .filter(|(field, value)| **value != field.default)
            .map(|(field, value)| field.label(*value))
            .collect::<Vec<_>>()
            .join(", "))
    }

    /// The history label a request the action's title cannot describe deserves: the one field a
    /// slider moved, the group a reset cleared, a count of the fields a larger patch set, or the
    /// module's own reset.
    fn label(&self, input: &ActionInput) -> Option<String> {
        let spec = &self.spec;
        if input.action_id == spec.reset.id {
            return Some(format!("Reset {}", spec.title));
        }
        if input.action_id != spec.set.id {
            return None;
        }
        let sent: Vec<(&String, f64)> = input
            .parameters
            .iter()
            .map(|(name, value)| {
                let default = spec.field(name).map_or(0.0, |field| field.default);
                (name, value.as_f64().unwrap_or(default))
            })
            .collect();
        if let Some(group) = self.reset_group(&sent) {
            return Some(format!("Reset {group}"));
        }
        match sent.as_slice() {
            [(name, value)] => Some(match spec.field(name) {
                Some(field) => field.label(*value),
                None => format!("{} {name}", spec.title),
            }),
            // An empty patch changes nothing and commits no entry; the host falls back to the
            // action's own title if it ever asks.
            [] => None,
            fields => Some(format!("{} ({} fields)", spec.title, fields.len())),
        }
    }

    /// Every field's value, defaults filled, named exactly as the patch's parameters are, so a
    /// client seeds its sliders from the displayed entry.
    fn values(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let values = self.read(effect_id, format, payload)?;
        Ok(self
            .spec
            .fields
            .iter()
            .zip(values.as_slice())
            .map(|(field, value)| (field.name.to_owned(), number(*value)))
            .collect())
    }

    fn query(
        &self,
        query_id: &str,
        parameters: &Map<String, Value>,
        context: &StageContext<'_>,
    ) -> Result<Value, Error> {
        if self.spec.queries.is_empty() {
            return Err(validation(format!(
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
        stage: Stage,
    ) -> Result<Processing, Error> {
        let values = self.read(effect_id, format, payload)?;
        self.module.compile(&values, stage)
    }
}
