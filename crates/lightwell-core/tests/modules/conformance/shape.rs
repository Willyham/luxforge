//! What the suite knows about a field-patch module, read from its registered descriptor alone.
//!
//! A module is a field patch when it declares exactly one effect and exactly two actions: one
//! `patch` action whose every parameter is a number with a default, and one parameterless action
//! that the module's own reset names. That is the shape `modules/field_patch.rs` builds from a
//! spec, and nothing here reads a spec, a module type or a module identity, so a module added to
//! [`lightwell_core::builtin_modules`] in that shape is covered the day it is registered.
//!
//! Every payload the suite sends is derived from the declared field table: the neutral spellings
//! from the defaults, and the moved values from each field's own range, rounded to its declared
//! display precision so that a value is one a slider could produce.
use lightwell_core::{
    Control, EffectDescriptor, ModuleDescriptor, ModuleRegistry, ParameterDescriptor, ParameterKind,
};
use serde_json::{Map, Value, json};

/// One declared numeric field of the patch action.
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub precision: u8,
    pub unit: Option<String>,
}

impl Field {
    fn read(parameter: &ParameterDescriptor) -> Option<Self> {
        let ParameterKind::Number { min, max } = parameter.kind else {
            return None;
        };
        Some(Self {
            name: parameter.name.clone(),
            min,
            max,
            default: parameter.default.as_ref()?.as_f64()?,
            precision: parameter.precision.unwrap_or(0),
            unit: parameter.unit.clone(),
        })
    }

    /// `fraction` of the way from the default towards `bound`, at the field's display precision.
    fn toward(&self, bound: f64, fraction: f64) -> f64 {
        let scale = 10f64.powi(i32::from(self.precision));
        ((self.default + (bound - self.default) * fraction) * scale).round() / scale
    }

    /// A value away from the default, towards the maximum when the range reaches past the default
    /// there and towards the minimum otherwise.
    pub fn high(&self) -> f64 {
        if self.max > self.default {
            self.toward(self.max, 0.6)
        } else {
            self.toward(self.min, 0.6)
        }
    }

    /// A second value away from the default and different from [`Field::high`], towards the other
    /// end of the range when there is one.
    pub fn low(&self) -> f64 {
        if self.min < self.default {
            self.toward(self.min, 0.6)
        } else {
            self.toward(self.max, 0.3)
        }
    }

    /// A value just outside the declared range, which every path must refuse.
    pub fn outside(&self) -> f64 {
        self.max + (self.max - self.min).max(1.0)
    }

    /// The value as the declared contract says a history label and a recipe row show it: the
    /// declared decimals, a sign when the range is signed, and the declared unit.
    pub fn shown(&self, value: f64) -> String {
        let precision = usize::from(self.precision);
        let number = if self.min < 0.0 {
            format!("{value:+.precision$}")
        } else {
            format!("{value:.precision$}")
        };
        match &self.unit {
            Some(unit) => format!("{number} {unit}"),
            None => number,
        }
    }
}

/// A group of sliders whose header reset is a patch of exactly its fields at their defaults.
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
        let [first, second] = descriptor.actions.as_slice() else {
            return None;
        };
        let (set, reset_action) = match (first.patch, second.patch) {
            (true, false) => (first, second),
            (false, true) => (second, first),
            _ => return None,
        };
        if reset_action.id != reset.action || !reset_action.parameters.is_empty() {
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
                Control::Group {
                    label,
                    reset: Some(reset),
                    ..
                } if reset.action == set.id => Some(Group {
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
            descriptor: descriptor.clone(),
        })
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Every spelling of the all-default state: the canonical `{}`, every field written out at its
    /// default, each field alone at its default, and every zero default written as `-0`.
    pub fn neutral_payloads(&self) -> Vec<(String, Value)> {
        let mut payloads = vec![
            ("the empty payload".to_owned(), json!({})),
            (
                "every field at its default".to_owned(),
                self.payload(|field| Some(field.default)),
            ),
        ];
        for field in &self.fields {
            payloads.push((
                format!("{} alone at its default", field.name),
                json!({field.name.clone(): field.default}),
            ));
        }
        if self.fields.iter().any(|field| field.default == 0.0) {
            payloads.push((
                "every zero default written as -0".to_owned(),
                self.payload(|field| (field.default == 0.0).then_some(-0.0)),
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

    /// Every field moved towards one end of its range.
    pub fn full_high(&self) -> Value {
        self.payload(|field| Some(field.high()))
    }

    /// Every field moved to a second value, so two commits of whole payloads differ everywhere.
    pub fn full_low(&self) -> Value {
        self.payload(|field| Some(field.low()))
    }

    /// Every field at its default, which is how the reset of every field as one patch reads.
    pub fn defaults(&self) -> Map<String, Value> {
        self.fields
            .iter()
            .map(|field| (field.name.clone(), json!(field.default)))
            .collect()
    }

    fn payload(&self, value: impl Fn(&Field) -> Option<f64>) -> Value {
        Value::Object(
            self.fields
                .iter()
                .filter_map(|field| value(field).map(|value| (field.name.clone(), json!(value))))
                .collect(),
        )
    }

    /// The label a history entry for a patch of exactly these fields carries, by the declared
    /// rules: the group a patch of exactly that group's fields at their defaults resets, one moved
    /// field by its value, and otherwise the module title and the field count.
    pub fn expected_label(&self, patch: &Map<String, Value>) -> Result<Label, String> {
        if let Some(group) = self.groups.iter().find(|group| {
            group.preset.len() == patch.len()
                && group
                    .preset
                    .iter()
                    .all(|(name, value)| patch.get(name).and_then(Value::as_f64) == value.as_f64())
        }) {
            return Ok(Label::Exactly(format!("Reset {}", group.label)));
        }
        match patch.iter().collect::<Vec<_>>().as_slice() {
            [(name, value)] => {
                let field = self
                    .field(name)
                    .ok_or_else(|| format!("{name} is not a declared field"))?;
                let value = value
                    .as_f64()
                    .ok_or_else(|| format!("{name} is not a number in {value}"))?;
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
