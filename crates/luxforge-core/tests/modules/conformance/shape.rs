//! What the suite knows about a field-patch module, read from its registered descriptor alone.
//!
//! A module is a field patch when it declares exactly one effect and one
//! `patch` action whose every parameter is a field — a number, an integer, a boolean, an enum, a
//! colour or a curve — with a default, and a parameterless non-patch action that the module's own
//! reset names. Additional non-patch actions, such as query-choice selection, do not change that
//! shape. This is what `modules/field_patch.rs` builds from a spec, and nothing here reads a
//! spec, a module type or a module identity, so a module registered in that shape is covered the
//! day it is registered.
//!
//! Every payload the suite sends is derived from the declared field table: the neutral spellings
//! from the defaults, and the moved values from each field's own declaration — a number moved
//! within its range and rounded to its declared display precision, so that a value is one a slider
//! could produce; a boolean flipped; another option; colour channels moved; a curve's points moved
//! within the unit square without reordering them.
use luxforge_core::{
    Control, EffectDescriptor, ModuleDescriptor, ModuleRegistry, ParameterDescriptor, ParameterKind,
};
use serde_json::{Map, Value, json};

/// One declared field of the patch action.
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub kind: ParameterKind,
    pub default: Value,
    pub precision: u8,
    pub unit: Option<String>,
}

impl Field {
    /// A declared parameter of a kind the field patch holds, with the default it declares.
    pub fn read(parameter: &ParameterDescriptor) -> Option<Self> {
        let default = parameter.default.clone()?;
        Self::of(parameter, default)
    }

    /// A declared parameter of a kind the field patch holds, with `default` standing for its own.
    pub fn of(parameter: &ParameterDescriptor, default: Value) -> Option<Self> {
        match parameter.kind {
            ParameterKind::Number { .. }
            | ParameterKind::Integer { .. }
            | ParameterKind::Boolean
            | ParameterKind::Enum { .. }
            | ParameterKind::Color
            | ParameterKind::Curve { .. } => Some(Self {
                name: parameter.name.clone(),
                kind: parameter.kind.clone(),
                default,
                precision: parameter.precision.unwrap_or(0),
                unit: parameter.unit.clone(),
            }),
            _ => None,
        }
    }

    /// A number or integer field's closed range.
    fn range(&self) -> Option<(f64, f64)> {
        match self.kind {
            ParameterKind::Number { min, max } => Some((min, max)),
            ParameterKind::Integer { min, max } => Some((min as f64, max as f64)),
            _ => None,
        }
    }

    /// A numeric value as the field's kind spells it.
    fn numeric(&self, value: f64) -> Value {
        match self.kind {
            ParameterKind::Integer { .. } => json!(value as i64),
            _ => json!(value),
        }
    }

    /// `fraction` of the way from the default towards `bound`, at the field's display precision.
    fn toward(&self, bound: f64, fraction: f64) -> Value {
        let default = self.default.as_f64().unwrap_or_default();
        let scale = 10f64.powi(i32::from(self.precision));
        self.numeric(((default + (bound - default) * fraction) * scale).round() / scale)
    }

    /// A colour with every channel moved `fraction` of the way towards `bound`, or towards the
    /// other end where a channel is already at `bound`.
    fn channels(&self, bound: f64, fraction: f64) -> Value {
        let channels = self.default.as_array().cloned().unwrap_or_default();
        Value::Array(
            channels
                .iter()
                .map(|channel| {
                    let channel = channel.as_f64().unwrap_or_default();
                    let bound = if channel == bound {
                        255.0 - bound
                    } else {
                        bound
                    };
                    json!((channel + (bound - channel) * fraction).round() as u64)
                })
                .collect(),
        )
    }

    /// A curve with every point's `y` moved halfway towards `bound`, or towards the other end when
    /// every point already sits at `bound`. Moving every `y` the same way keeps a monotone curve
    /// monotone and a fixed `x` fixed.
    fn curve(&self, bound: f64) -> Value {
        let points: Vec<[f64; 2]> = self
            .default
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|point| {
                let pair = point.as_array()?;
                Some([pair.first()?.as_f64()?, pair.get(1)?.as_f64()?])
            })
            .collect();
        let bound = if points.iter().all(|[_, y]| *y == bound) {
            1.0 - bound
        } else {
            bound
        };
        json!(
            points
                .iter()
                .map(|[x, y]| [*x, y + (bound - y) / 2.0])
                .collect::<Vec<_>>()
        )
    }

    /// The options other than the default, in declared order.
    fn others(&self) -> Vec<Value> {
        match &self.kind {
            ParameterKind::Enum { options } => options
                .iter()
                .filter(|option| Some(option.as_str()) != self.default.as_str())
                .map(|option| json!(option))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A value away from the default: a number towards the maximum when the range reaches past
    /// the default there and towards the minimum otherwise, a boolean flipped, the first other
    /// option, colour channels towards white and curve points towards the top.
    pub fn high(&self) -> Value {
        match self.kind {
            ParameterKind::Boolean => json!(!self.default.as_bool().unwrap_or_default()),
            ParameterKind::Enum { .. } => self.others().first().cloned().unwrap_or(Value::Null),
            ParameterKind::Color => self.channels(255.0, 0.6),
            ParameterKind::Curve { .. } => self.curve(1.0),
            _ => {
                let (min, max) = self.range().unwrap_or_default();
                if max > self.default.as_f64().unwrap_or_default() {
                    self.toward(max, 0.6)
                } else {
                    self.toward(min, 0.6)
                }
            }
        }
    }

    /// A second value away from the default and, where the kind has one, different from
    /// [`Field::high`]: towards the other end of a range, the last other option, channels towards
    /// black and points towards the bottom. A boolean has one other value, so it is `high` again.
    pub fn low(&self) -> Value {
        match self.kind {
            ParameterKind::Boolean => self.high(),
            ParameterKind::Enum { .. } => self.others().last().cloned().unwrap_or(Value::Null),
            ParameterKind::Color => self.channels(0.0, 0.3),
            ParameterKind::Curve { .. } => self.curve(0.0),
            _ => {
                let (min, max) = self.range().unwrap_or_default();
                if min < self.default.as_f64().unwrap_or_default() {
                    self.toward(min, 0.6)
                } else {
                    self.toward(max, 0.3)
                }
            }
        }
    }

    /// Whether two values of this field are the same state: numbers, and a curve's coordinates, by
    /// the f64 they read as, whether written `1` or `1.0`; everything else by its one spelling.
    pub fn same(&self, a: Option<&Value>, b: &Value) -> bool {
        let Some(a) = a else {
            return false;
        };
        match self.kind {
            ParameterKind::Number { .. } => a.as_f64().is_some() && a.as_f64() == b.as_f64(),
            ParameterKind::Curve { .. } => match (a.as_array(), b.as_array()) {
                (Some(a), Some(b)) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|(a, b)| match (a.as_array(), b.as_array()) {
                                (Some(a), Some(b)) => {
                                    a.len() == b.len()
                                        && a.iter().zip(b).all(|(a, b)| {
                                            a.as_f64().is_some() && a.as_f64() == b.as_f64()
                                        })
                                }
                                _ => false,
                            })
                }
                _ => false,
            },
            _ => a == b,
        }
    }

    /// The same value in another spelling the declaration accepts, where the kind has one: a
    /// whole number, or a curve with whole coordinates, written as JSON integers.
    pub fn respelled(&self, value: &Value) -> Option<Value> {
        let whole = |value: &Value| {
            value
                .as_f64()
                .filter(|number| number.fract() == 0.0 && value.is_f64())
                .map(|number| json!(number as i64))
        };
        match self.kind {
            ParameterKind::Number { .. } => whole(value),
            ParameterKind::Curve { .. } => {
                let mut changed = false;
                let points = value
                    .as_array()?
                    .iter()
                    .map(|point| {
                        Value::Array(
                            point
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|coordinate| match whole(coordinate) {
                                    Some(integer) => {
                                        changed = true;
                                        integer
                                    }
                                    None => coordinate.clone(),
                                })
                                .collect(),
                        )
                    })
                    .collect();
                changed.then_some(Value::Array(points))
            }
            _ => None,
        }
    }

    /// Values the declaration accepts at its edges: both ends of a range (and the maximum as a
    /// JSON integer when it is whole), both booleans, every option, black and white, and the
    /// default and moved curves.
    pub fn accepted(&self) -> Vec<Value> {
        match &self.kind {
            ParameterKind::Number { min, max } => {
                let mut values = vec![json!(min), json!(max)];
                if max.fract() == 0.0 {
                    values.push(json!(*max as i64));
                }
                values
            }
            ParameterKind::Integer { min, max } => vec![json!(min), json!(max)],
            ParameterKind::Boolean => vec![json!(true), json!(false)],
            ParameterKind::Enum { options } => options.iter().map(|option| json!(option)).collect(),
            ParameterKind::Color => vec![json!([0, 0, 0]), json!([255, 255, 255])],
            _ => vec![self.default.clone(), self.high(), self.low()],
        }
    }

    /// Values the declaration refuses, each with the words its refusal must contain beside the
    /// field's name: just outside a range, the wrong JSON type, an undeclared option, a channel
    /// past 255 and a curve with more points than it may hold.
    pub fn refused(&self) -> Vec<(Value, String)> {
        match &self.kind {
            ParameterKind::Number { min, max } => {
                let span = max - min;
                let range = format!("within {min}..={max}");
                vec![
                    (json!(min - span * 1e-4), range.clone()),
                    (json!(max + span * 1e-4), range),
                    (json!("1"), "must be a number".into()),
                ]
            }
            ParameterKind::Integer { min, max } => {
                let range = format!("within {min}..={max}");
                vec![
                    (json!(min - 1), range.clone()),
                    (json!(max + 1), range),
                    (json!(0.5), "must be an integer".into()),
                ]
            }
            ParameterKind::Boolean => vec![(json!("true"), "must be a boolean".into())],
            ParameterKind::Enum { .. } => vec![
                (json!("conformance-undeclared"), "must be one of".into()),
                (json!(1), "must be a string".into()),
            ],
            ParameterKind::Color => vec![
                (json!([256, 0, 0]), "three sRGB channels".into()),
                (json!([0, 0]), "three sRGB channels".into()),
            ],
            ParameterKind::Curve { points_max, .. } => {
                let count = points_max + 1;
                let points: Vec<[f64; 2]> = (0..count)
                    .map(|index| {
                        let x = index as f64 / (count - 1) as f64;
                        [x, x]
                    })
                    .collect();
                vec![
                    (json!(points), "invalid curve point count".into()),
                    (json!("1"), "must be a curve point list".into()),
                ]
            }
            _ => Vec::new(),
        }
    }

    /// A value every path must refuse: the first of [`Field::refused`], just outside a range for a
    /// number.
    pub fn outside(&self) -> Value {
        self.refused()
            .into_iter()
            .next()
            .map_or(Value::Null, |(value, _)| value)
    }

    /// The value as the declared contract says a history label and a recipe row show it: a number
    /// with the declared decimals, a sign when the range is signed and the declared unit; a
    /// boolean as `on` or `off`; an option title-cased; a colour as `r,g,b`; a curve by its point
    /// count.
    pub fn shown(&self, value: &Value) -> String {
        let shown = match (&self.kind, value) {
            (ParameterKind::Boolean, Value::Bool(on)) => (if *on { "on" } else { "off" }).into(),
            (ParameterKind::Enum { .. }, Value::String(option)) => {
                let spaced = option.replace(['-', '_'], " ");
                let mut characters = spaced.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().chain(characters).collect(),
                    None => spaced,
                }
            }
            (ParameterKind::Color, Value::Array(channels)) => channels
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(","),
            (ParameterKind::Curve { .. }, Value::Array(points)) => {
                format!("{} points", points.len())
            }
            _ => {
                let number = value.as_f64().unwrap_or(f64::NAN);
                let precision = usize::from(self.precision);
                if self.range().is_some_and(|(min, _)| min < 0.0) {
                    format!("{number:+.precision$}")
                } else {
                    format!("{number:.precision$}")
                }
            }
        };
        match &self.unit {
            Some(unit) => format!("{shown} {unit}"),
            None => shown,
        }
    }
}

/// A group whose header reset is a patch of exactly its fields at their defaults.
#[derive(Clone, Debug)]
pub struct Group {
    pub label: String,
    pub preset: Map<String, Value>,
}

/// One registered field-patch module, as its descriptor declares it.
#[derive(Clone, Debug)]
pub struct FieldPatch {
    pub id: String,
    pub title: String,
    pub effect: EffectDescriptor,
    /// The patch action, `set-<name>`.
    pub set: String,
    /// The parameterless action the module reset names, `reset-<name>`.
    pub reset: String,
    pub fields: Vec<Field>,
    pub groups: Vec<Group>,
    /// Whether a layer that is not neutral changes the picture. A developer proof module declares
    /// that it is one, and its layer describes values without ever changing a pixel, so the suite
    /// proves its payload rules and that it renders nothing, not the pixel consequences and the
    /// editing journey a real tool's layer has.
    pub renders: bool,
    pub descriptor: ModuleDescriptor,
}

/// Every registered module in the field-patch shape, in registration order.
pub fn field_patches(registry: &ModuleRegistry) -> Vec<FieldPatch> {
    registry
        .descriptors()
        .into_iter()
        .filter_map(FieldPatch::read)
        .collect()
}

impl FieldPatch {
    fn read(descriptor: &ModuleDescriptor) -> Option<Self> {
        let [effect] = descriptor.effects.as_slice() else {
            return None;
        };
        let reset = descriptor.reset.as_ref()?;
        let reset_action = descriptor.action(&reset.action)?;
        if reset_action.patch || !reset_action.parameters.is_empty() {
            return None;
        }
        let mut patches = descriptor.actions.iter().filter(|action| action.patch);
        let set = patches.next()?;
        if patches.next().is_some() {
            return None;
        }
        let fields = set
            .parameters
            .iter()
            .map(Field::read)
            .collect::<Option<Vec<_>>>()?;
        if fields.is_empty() {
            return None;
        }
        let groups = descriptor
            .controls
            .iter()
            .filter_map(|control| match control {
                Control::Group(luxforge_core::GroupControl {
                    label,
                    reset: Some(reset),
                    ..
                }) if reset.action == set.id => Some(Group {
                    label: label.clone(),
                    preset: reset.preset.clone(),
                }),
                _ => None,
            })
            .collect();
        Some(Self {
            id: descriptor.id.clone(),
            title: descriptor.title.clone(),
            effect: effect.clone(),
            set: set.id.clone(),
            reset: reset_action.id.clone(),
            fields,
            groups,
            renders: !descriptor.developer,
            descriptor: descriptor.clone(),
        })
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Every spelling of the all-default state: the canonical `{}`, every field written out at its
    /// default, each field alone at its default and in another spelling of it, and every zero
    /// number default written as `-0`.
    pub fn neutral_payloads(&self) -> Vec<(String, Value)> {
        let mut payloads = vec![
            ("the empty payload".to_owned(), json!({})),
            (
                "every field at its default".to_owned(),
                self.payload(|field| Some(field.default.clone())),
            ),
        ];
        for field in &self.fields {
            payloads.push((
                format!("{} alone at its default", field.name),
                json!({field.name.clone(): field.default}),
            ));
            if let Some(respelled) = field.respelled(&field.default) {
                payloads.push((
                    format!("{} alone at its default, respelled", field.name),
                    json!({field.name.clone(): respelled}),
                ));
            }
        }
        let zero = |field: &Field| {
            matches!(field.kind, ParameterKind::Number { .. })
                && field.default.as_f64() == Some(0.0)
        };
        if self.fields.iter().any(zero) {
            payloads.push((
                "every zero default written as -0".to_owned(),
                self.payload(|field| zero(field).then(|| json!(-0.0))),
            ));
        }
        payloads
    }

    /// Each field alone, moved away from its default. Whether one changes the image is the
    /// module's own neutrality rule (the vignette's shape fields do nothing at amount 0), so the
    /// suite asks the module and checks the consequences of its answer.
    pub fn single_field_payloads(&self) -> Vec<(&Field, Value)> {
        self.fields
            .iter()
            .map(|field| (field, json!({field.name.clone(): field.high()})))
            .collect()
    }

    /// Every field moved away from its default.
    pub fn full_high(&self) -> Value {
        self.payload(|field| Some(field.high()))
    }

    /// Every field moved to a second value, so two commits of whole payloads differ.
    pub fn full_low(&self) -> Value {
        self.payload(|field| Some(field.low()))
    }

    /// Every field at its default, which is how the reset of every field as one patch reads.
    pub fn defaults(&self) -> Map<String, Value> {
        self.fields
            .iter()
            .map(|field| (field.name.clone(), field.default.clone()))
            .collect()
    }

    fn payload(&self, value: impl Fn(&Field) -> Option<Value>) -> Value {
        Value::Object(
            self.fields
                .iter()
                .filter_map(|field| value(field).map(|value| (field.name.clone(), value)))
                .collect(),
        )
    }

    /// The label a history entry for a patch of exactly these fields carries, by the declared
    /// rules: the group a patch of exactly that group's fields at their defaults resets, the group a
    /// patch of exactly its fields sets, one moved field by its value, and otherwise the module
    /// title and the field count.
    pub fn expected_label(&self, patch: &Map<String, Value>) -> Result<Label, String> {
        if let Some(group) = self.groups.iter().find(|group| {
            group.preset.len() == patch.len()
                && group.preset.iter().all(|(name, value)| {
                    self.field(name)
                        .is_some_and(|field| field.same(patch.get(name), value))
                })
        }) {
            return Ok(Label::Exactly(format!("Reset {}", group.label)));
        }
        // A patch of exactly one group's fields, more than one, reads as that group.
        if patch.len() > 1
            && let Some(group) = self.groups.iter().find(|group| {
                group.preset.len() == patch.len()
                    && patch.keys().all(|name| group.preset.contains_key(name))
            })
        {
            return Ok(Label::Exactly(group.label.clone()));
        }
        match patch.iter().collect::<Vec<_>>().as_slice() {
            [(name, value)] => {
                let field = self
                    .field(name)
                    .ok_or_else(|| format!("{name} is not a declared field"))?;
                Ok(Label::Ending(format!(" {}", field.shown(value))))
            }
            [] => Err("an empty patch has no label".into()),
            fields => Ok(Label::Exactly(format!(
                "{} ({} fields)",
                self.title,
                fields.len()
            ))),
        }
    }
}

/// What a history label must be: exactly this text, or a field name followed by this value.
#[derive(Debug)]
pub enum Label {
    Exactly(String),
    Ending(String),
}

impl Label {
    pub fn matches(&self, label: &str) -> bool {
        match self {
            Self::Exactly(expected) => label == expected,
            Self::Ending(value) => label.len() > value.len() && label.ends_with(value.as_str()),
        }
    }
}
