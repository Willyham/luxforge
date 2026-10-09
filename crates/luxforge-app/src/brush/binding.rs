//! What one brush consumer's gesture sends, read from its declaration: the action a new path
//! drafts, the parameters its path, radius, settings and source fill, and the edit pins and
//! handles draft through.
//!
//! The mask brush is bound to the host's `mask.add-stroke`, whose path is `points` and whose radius
//! is `size`; a module's brush is bound by the `brush-mask` canvas it declares
//! ([`CanvasInteraction::BrushMask`]). Both clamp a setting to the range its own parameter declares
//! and step it by its declared step, through the one rule here, so a key, a nudge and a typed value
//! land on the same value for every consumer. Nothing here names a field of its own.
use super::{HandleEdit, StrokeCapture};
use luxforge_core::{
    ActionDescriptor, CanvasInteraction, Error, ModuleDescriptor, ParameterKind, mask::commands,
};
use serde_json::{Map, Value, json};

/// `value` as the number parameter `name` of `action` accepts it: clamped to its declared range and,
/// for an `integer`, rounded. `None` for a value that is not finite or a name that is not a declared
/// number, which leaves whatever the caller holds as it was.
pub(crate) fn clamp(action: &ActionDescriptor, name: &str, value: f64) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    match action.parameter(name)?.kind {
        ParameterKind::Number { min, max } => Some(value.clamp(min, max)),
        ParameterKind::Integer { min, max } => Some(value.round().clamp(min as f64, max as f64)),
        _ => None,
    }
}

/// The keyboard and nudge increment `name` declares, when it declares a usable one.
pub(crate) fn step(action: &ActionDescriptor, name: &str) -> Option<f64> {
    action
        .parameter(name)?
        .step
        .filter(|step| step.is_finite() && *step > 0.0)
}

/// The number `name` of `action` starts at: its declared default, or the bottom of its range.
fn initial(action: &ActionDescriptor, name: &str) -> Option<f64> {
    let declared = action.parameter(name)?;
    let floor = match declared.kind {
        ParameterKind::Number { min, .. } => min,
        ParameterKind::Integer { min, .. } => min as f64,
        _ => return None,
    };
    clamp(
        action,
        name,
        declared
            .default
            .as_ref()
            .and_then(Value::as_f64)
            .unwrap_or(floor),
    )
}

/// One number as `action` declares it: an `integer` parameter is sent as a JSON integer.
fn number_value(action: &ActionDescriptor, name: &str, value: f64) -> Value {
    match action.parameter(name).map(|declared| &declared.kind) {
        Some(ParameterKind::Integer { .. }) => json!(value.round() as i64),
        _ => json!(value),
    }
}

/// The edit a declared brush's pins and handles draft through: the field patch, the identity
/// parameter that names the operation and the query that lists the drawable ones.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EditBinding {
    pub(crate) method: String,
    pub(crate) action: ActionDescriptor,
    pub(crate) operation: String,
    pub(crate) list: String,
}

/// One consumer's brush, read once from its declaration.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrushBinding {
    /// The module that declares the brush, or `None` for the host's own mask brush.
    pub(crate) module: Option<String>,
    /// The method a new path drafts: `mask.add-stroke`, or `edit.<action>` of the module.
    pub(crate) method: String,
    /// The declaration every field is checked against.
    pub(crate) action: ActionDescriptor,
    pub(crate) points: String,
    pub(crate) radius: String,
    pub(crate) feather: Option<String>,
    pub(crate) opacity: Option<String>,
    pub(crate) mode: Option<String>,
    /// The source coordinates and the query that ranks automatic sources.
    pub(crate) source: Option<(String, String, String)>,
    pub(crate) edit: Option<EditBinding>,
}

/// The settings the next path is drawn with: the radius it is captured at, the numbers the
/// binding's settings fill and the mode. View state of the window, never a request until a path
/// is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrushSettings {
    pub(crate) radius: f64,
    pub(crate) feather: Option<f64>,
    pub(crate) opacity: Option<f64>,
    pub(crate) mode: Option<String>,
}

impl BrushBinding {
    /// The host's mask brush: `mask.add-stroke`'s path and `size`, with its feather. Flow and the
    /// colour limit are the mask brush's own and stay on its editor.
    pub(crate) fn mask() -> Self {
        let command =
            commands::find(commands::ADD_STROKE).expect("the host declares mask.add-stroke");
        Self {
            module: None,
            method: command.method.to_owned(),
            action: command.action.clone(),
            points: "points".into(),
            radius: "size".into(),
            feather: Some("feather".into()),
            opacity: None,
            mode: None,
            source: None,
            edit: None,
        }
    }

    /// The brush `module` declares, when it declares one and every action it names is declared.
    /// Registration already refused a malformed binding, so `None` here means the module declares
    /// no brush.
    pub(crate) fn declared(module: &ModuleDescriptor) -> Option<Self> {
        let Some(CanvasInteraction::BrushMask {
            action,
            points,
            radius,
            feather,
            opacity,
            mode,
            source,
            edit,
            ..
        }) = &module.canvas
        else {
            return None;
        };
        let edit = match edit {
            Some(edit) => Some(EditBinding {
                method: format!("edit.{}", edit.action),
                action: module.action(&edit.action)?.clone(),
                operation: edit.operation.clone(),
                list: edit.list.clone(),
            }),
            None => None,
        };
        Some(Self {
            module: Some(module.id.clone()),
            method: format!("edit.{action}"),
            action: module.action(action)?.clone(),
            points: points.clone(),
            radius: radius.clone(),
            feather: feather.clone(),
            opacity: opacity.clone(),
            mode: mode.clone(),
            source: source
                .as_ref()
                .map(|source| (source.x.clone(), source.y.clone(), source.query.clone())),
            edit,
        })
    }

    /// What the brush starts at: every bound setting at its declared default.
    pub(crate) fn settings(&self) -> BrushSettings {
        let number =
            |name: &Option<String>| name.as_deref().and_then(|name| initial(&self.action, name));
        BrushSettings {
            radius: initial(&self.action, &self.radius).unwrap_or(0.0),
            feather: number(&self.feather),
            opacity: number(&self.opacity),
            mode: self.mode.as_deref().and_then(|name| {
                let declared = self.action.parameter(name)?;
                let ParameterKind::Enum { options } = &declared.kind else {
                    return None;
                };
                declared
                    .default
                    .as_ref()
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| options.first().cloned())
            }),
        }
    }

    /// Set one bound number by its declared name, as the declaration accepts it. A name the brush
    /// does not bind and a value that is not finite leave the settings as they were.
    pub(crate) fn set(&self, settings: &mut BrushSettings, name: &str, value: f64) -> bool {
        let Some(value) = clamp(&self.action, name, value) else {
            return false;
        };
        let slot = if name == self.radius {
            Some(&mut settings.radius)
        } else if self.feather.as_deref() == Some(name) {
            settings.feather.as_mut()
        } else if self.opacity.as_deref() == Some(name) {
            settings.opacity.as_mut()
        } else {
            None
        };
        match slot {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }

    /// Move one bound number by `steps` of its own declared step: the brackets and a panel's nudges
    /// are the same call.
    pub(crate) fn nudge(&self, settings: &mut BrushSettings, name: &str, steps: f64) -> bool {
        let Some(step) = step(&self.action, name) else {
            return false;
        };
        let current = if name == self.radius {
            Some(settings.radius)
        } else if self.feather.as_deref() == Some(name) {
            settings.feather
        } else if self.opacity.as_deref() == Some(name) {
            settings.opacity
        } else {
            None
        };
        current.is_some_and(|value| self.set(settings, name, value + steps * step))
    }

    /// Choose the mode by one of its declared options.
    pub(crate) fn set_mode(&self, settings: &mut BrushSettings, option: &str) -> bool {
        let declared = self
            .mode
            .as_deref()
            .and_then(|name| self.action.parameter(name));
        match declared.map(|declared| &declared.kind) {
            Some(ParameterKind::Enum { options })
                if options.iter().any(|known| known == option) =>
            {
                settings.mode = Some(option.to_owned());
                true
            }
            _ => false,
        }
    }

    /// The request fields one drawn path adds through [`Self::method`]: the posted path at the
    /// settings' radius, the radius, every bound setting and, when the gesture set one, the absolute
    /// position the path's first stored point samples from. Exactly what an agent posts for the
    /// same path, because the path is the host's own decimation of the capture and every number is
    /// in its declared shape. A capture that failed, or captured nothing, is refused here and never
    /// becomes an empty or partial path.
    pub(crate) fn add_fields(
        &self,
        settings: &BrushSettings,
        capture: &StrokeCapture,
        source: Option<[f64; 2]>,
    ) -> Result<Map<String, Value>, Error> {
        if !capture.drawn() {
            return Err(Error::validation("nothing was painted"));
        }
        let points = capture.posted(settings.radius)?;
        let mut fields = Map::new();
        fields.insert(self.points.clone(), json!(&*points));
        fields.insert(
            self.radius.clone(),
            number_value(&self.action, &self.radius, settings.radius),
        );
        for (name, value) in [
            (&self.feather, settings.feather),
            (&self.opacity, settings.opacity),
        ] {
            if let (Some(name), Some(value)) = (name, value) {
                fields.insert(name.clone(), number_value(&self.action, name, value));
            }
        }
        if let (Some(name), Some(mode)) = (&self.mode, &settings.mode) {
            fields.insert(name.clone(), json!(mode));
        }
        match (&self.source, source) {
            (Some((x, y, _)), Some([sx, sy])) => {
                fields.insert(x.clone(), json!(sx));
                fields.insert(y.clone(), json!(sy));
            }
            (None, Some(_)) => {
                return Err(Error::validation(format!(
                    "{} takes no source",
                    self.method
                )));
            }
            _ => {}
        }
        Ok(fields)
    }

    /// The draft one pin, source or size-handle drag sends through the declared edit: the method,
    /// the identity parameter naming the operation, and the fields the drag changed. `None` when
    /// the brush declares no edit or the drag changed nothing — a release that moved nothing writes
    /// nothing.
    pub(crate) fn edit_fields(
        &self,
        edit: &HandleEdit,
    ) -> Option<(&str, (&str, String), Map<String, Value>)> {
        let binding = self.edit.as_ref()?;
        let mut fields = Map::new();
        match edit.change()? {
            super::HandleChange::Path(points) => {
                fields.insert(self.points.clone(), json!(points));
            }
            super::HandleChange::Source([x, y]) => {
                let (name_x, name_y, _) = self.source.as_ref()?;
                fields.insert(name_x.clone(), json!(x));
                fields.insert(name_y.clone(), json!(y));
            }
            super::HandleChange::Radius(radius) => {
                let radius = clamp(&binding.action, &self.radius, radius)?;
                fields.insert(
                    self.radius.clone(),
                    number_value(&binding.action, &self.radius, radius),
                );
            }
        }
        Some((
            binding.method.as_str(),
            (binding.operation.as_str(), edit.operation().to_owned()),
            fields,
        ))
    }
}
