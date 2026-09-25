//! The text typed into every generated control, and the rules that read it back. Validation always
//! runs against the declared parameter, never against a parsed copy, so an invalid field keeps what
//! was typed and commits nothing.
use crate::state::tools::{ControlOwner, Rendered, classify};
use lightwell_core::{
    ActionDescriptor, Control, ModuleDescriptor, ParameterDescriptor, ParameterKind, check_value,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The channels of a color parameter, in declared order.
pub(crate) const CHANNELS: [&str; 3] = ["R", "G", "B"];

/// The text typed into each generated field, by (action id, parameter name).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fields(BTreeMap<(String, String), String>);

impl Fields {
    /// Seed every declared field from its parameter's default, else from the limit it accepts.
    ///
    /// The host's own `mask.*` controls are seeded beside the modules', from the same declarations
    /// through the same walk: they are declared with the same types, so a mask's amount field and a
    /// gradient endpoint arrive here exactly as a module's slider does.
    pub(crate) fn seeded(modules: &[ModuleDescriptor]) -> Self {
        let mut fields = Self::default();
        for module in modules {
            seed_controls(ControlOwner::Module(module), &module.controls, &mut fields);
        }
        seed_controls(
            ControlOwner::Host,
            lightwell_core::mask::commands::controls(),
            &mut fields,
        );
        fields
    }

    pub(crate) fn get(&self, action: &str, parameter: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|((declared, name), _)| declared == action && name == parameter)
            .map(|(_, text)| text.as_str())
    }

    pub(crate) fn set(&mut self, action: &str, parameter: &str, text: String) {
        self.0
            .insert((action.to_owned(), parameter.to_owned()), text);
    }

    pub(crate) fn get_value(
        &self,
        action: &str,
        parameter: &ParameterDescriptor,
    ) -> Result<Value, String> {
        parse_field(
            parameter,
            self.get(action, &parameter.name).unwrap_or_default(),
        )
    }

    pub(crate) fn set_value(
        &mut self,
        action: &str,
        parameter: &ParameterDescriptor,
        value: &Value,
    ) -> Result<(), String> {
        let text = value_text(parameter, value)?;
        self.set(action, &parameter.name, text);
        Ok(())
    }

    /// Correlated evidence: what every generated control held when a frame was captured.
    pub(crate) fn summary(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|((action, parameter), text)| {
                    (format!("{action}.{parameter}"), Value::from(text.clone()))
                })
                .collect(),
        )
    }
}

fn seed_controls(owner: ControlOwner<'_>, controls: &[Control], fields: &mut Fields) {
    for control in controls {
        match classify(control) {
            Rendered::Group { controls, .. } => seed_controls(owner, controls, fields),
            Rendered::Number {
                action, parameter, ..
            }
            | Rendered::Color {
                action, parameter, ..
            }
            | Rendered::Toggle {
                action, parameter, ..
            }
            | Rendered::Choice {
                action, parameter, ..
            } => {
                if let Some(declared) = owner.parameter(action, parameter) {
                    fields.set(action, parameter, seed_text(declared));
                }
            }
            Rendered::Curve {
                action, channels, ..
            } => {
                for channel in channels {
                    if let Some(declared) = owner.parameter(action, &channel.parameter) {
                        fields.set(action, &channel.parameter, seed_text(declared));
                    }
                }
            }
            // None carries a field of its own: an action button submits the fields already
            // seeded, a picker only enters its module's canvas mode, a preset row submits a library
            // preset's own settings, name and identity, and a task sends the open asset and a
            // profile.
            Rendered::Action { .. }
            | Rendered::Picker { .. }
            | Rendered::Presets { .. }
            | Rendered::Task { .. }
            | Rendered::Unsupported(_) => {}
        }
    }
}

/// One action's declared parameter, wherever the registered modules declare that action.
pub(crate) fn declared<'a>(
    modules: &'a [ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<&'a ParameterDescriptor> {
    crate::state::tools::declared_action(modules, action)?.parameter(parameter)
}

/// A field starts at the declared default; without one it starts at the lowest accepted value.
pub(crate) fn seed_text(parameter: &ParameterDescriptor) -> String {
    match &parameter.kind {
        ParameterKind::Integer { min, .. } => parameter
            .default
            .as_ref()
            .and_then(Value::as_i64)
            .unwrap_or(*min)
            .to_string(),
        ParameterKind::Number { min, .. } => format_number(
            parameter,
            parameter
                .default
                .as_ref()
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())
                .unwrap_or(*min),
        ),
        ParameterKind::Color => parameter
            .default
            .as_ref()
            .and_then(Value::as_array)
            .filter(|channels| channels.len() == CHANNELS.len())
            .map(|channels| {
                channels
                    .iter()
                    .map(|channel| channel.as_u64().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| "0,0,0".into()),
        ParameterKind::Enum { options } => parameter
            .default
            .as_ref()
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| options.first().cloned())
            .unwrap_or_default(),
        ParameterKind::Boolean => parameter
            .default
            .as_ref()
            .and_then(Value::as_bool)
            .unwrap_or(false)
            .to_string(),
        // An artifact identity has no default and nothing sensible to seed.
        ParameterKind::Artifact => String::new(),
        ParameterKind::Curve {
            fixed_x,
            points_min,
            ..
        } => parameter
            .default
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_else(|| {
                let xs = fixed_x.clone().unwrap_or_else(|| {
                    (0..*points_min)
                        .map(|index| index as f64 / (*points_min - 1) as f64)
                        .collect()
                });
                Value::Array(xs.into_iter().map(|x| serde_json::json!([x, x])).collect())
                    .to_string()
            }),
        ParameterKind::String { .. } => parameter
            .default
            .as_ref()
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default(),
        ParameterKind::Settings => parameter
            .default
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_else(|| "{}".into()),
        // A path has no seed to start from: nothing here draws one, and an empty list is what a
        // field shows until a gesture or a client supplies one.
        ParameterKind::Points { .. } => parameter
            .default
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_else(|| "[]".into()),
        // A setting's endpoint and secret never have a default: a destination is the person's
        // choice, and a secret is never shown.
        ParameterKind::Endpoint { .. } | ParameterKind::Secret { .. } => String::new(),
    }
}

fn range_message(name: &str, min: i64, max: i64) -> String {
    format!("{name} must be an integer within {min}..={max}")
}

fn number_range_message(name: &str, min: f64, max: f64) -> String {
    format!(
        "{name} must be a number from {} to {}",
        number_text(min),
        number_text(max)
    )
}

/// A number as text, with no declared parameter to say how it should read: `0`, `-3.5`, no
/// trailing zeros or exponent noise.
///
/// This is for numbers that are not a declared parameter's value — a range message's limits, the
/// crop draft's own readout. Every number that **is** one goes through [`format_number`], which
/// shows the decimals the parameter declares and never leaves `1.7000000000000002` on screen.
pub(crate) fn number_text(value: f64) -> String {
    format!("{value}")
}

/// How many decimals a declared parameter's value is shown with.
///
/// In order: the declared `precision`, else the decimals of the declared `step`, else the decimals
/// of the generic step the panel derives from the range. An integer parameter is shown as an
/// integer, whatever else it declares.
pub(crate) fn decimals_for(parameter: &ParameterDescriptor) -> usize {
    let (min, max) = match &parameter.kind {
        ParameterKind::Integer { .. } => return 0,
        ParameterKind::Number { min, max } => (*min, *max),
        ParameterKind::Color
        | ParameterKind::Enum { .. }
        | ParameterKind::Boolean
        | ParameterKind::Artifact
        | ParameterKind::Curve { .. }
        | ParameterKind::Points { .. }
        | ParameterKind::String { .. }
        | ParameterKind::Settings
        | ParameterKind::Endpoint { .. }
        | ParameterKind::Secret { .. } => return 0,
    };
    if let Some(precision) = parameter.precision {
        return usize::from(precision).min(lightwell_ui::geometry::MAX_DECIMALS);
    }
    let step = parameter
        .step
        .filter(|step| step.is_finite() && *step > 0.0)
        .unwrap_or_else(|| crate::state::tools::generic_step(min, max));
    decimals_of(step)
}

/// The decimals one increment needs: `0.01` → 2, `0.5` → 1, `10` → 0. Capped at the largest
/// precision a descriptor may declare, so an unrepresentable step cannot ask for endless digits.
pub(crate) fn decimals_of(step: f64) -> usize {
    if !step.is_finite() || step <= 0.0 {
        return 0;
    }
    (0..=lightwell_ui::geometry::MAX_DECIMALS)
        .find(|decimals| {
            let scaled = step * 10f64.powi(*decimals as i32);
            (scaled - scaled.round()).abs() <= 1e-9 * scaled.abs().max(1.0)
        })
        .unwrap_or(lightwell_ui::geometry::MAX_DECIMALS)
}

/// One declared parameter's value as its field shows it: a fixed number of decimals, so a control
/// reads `1.70`, `0.00` or `-3.50` rather than whatever the last arithmetic left behind.
///
/// A value that rounds to zero is always `0` or `0.00`, never `-0.00`: the sign of a zero is an
/// artefact of the arithmetic, not something the person did.
pub(crate) fn format_number(parameter: &ParameterDescriptor, value: f64) -> String {
    let ordinary = decimals_for(parameter);
    let fine = fine_decimals_for(parameter);
    let factor = 10f64.powi(ordinary as i32);
    let ordinary_value = (value * factor).round() / factor;
    // A saved sensor value may be fractionally off the visible grid. Only expose the fine digits
    // when they distinguish a real fine nudge rather than a conversion or floating-point residue.
    let fine_quantum = 10f64.powi(-(fine as i32));
    let decimals = if value.is_finite()
        && (value - ordinary_value).abs() > (fine_quantum * 0.49).max(1e-9 * value.abs().max(1.0))
    {
        fine
    } else {
        ordinary
    };
    format_decimals(value, decimals)
}

/// Precision needed for an Option nudge (or fine rail drag), including an implicit tenth-step.
pub(crate) fn fine_decimals_for(parameter: &ParameterDescriptor) -> usize {
    let ordinary = decimals_for(parameter);
    if matches!(&parameter.kind, ParameterKind::Integer { .. }) {
        return 0;
    }
    let step = parameter.step.unwrap_or_else(|| match &parameter.kind {
        ParameterKind::Number { min, max } => crate::state::tools::generic_step(*min, *max),
        _ => 1.0,
    });
    ordinary.max(decimals_of(parameter.fine_step.unwrap_or(step / 10.0)))
}

/// `value` with exactly `decimals` decimals, and no negative zero.
fn format_decimals(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return number_text(value);
    }
    let text = format!("{value:.decimals$}");
    match text.strip_prefix('-') {
        // "-0", "-0.00": the digits are all zeros, so the sign says nothing.
        Some(rest) if rest.bytes().all(|byte| byte == b'0' || byte == b'.') => rest.to_owned(),
        _ => text,
    }
}

/// One field's text read as the value its parameter declares, or the message naming what it needs.
pub(crate) fn parse_field(parameter: &ParameterDescriptor, text: &str) -> Result<Value, String> {
    let name = &parameter.name;
    match &parameter.kind {
        ParameterKind::Integer { min, max } => text
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|value| (min..=max).contains(&value))
            .map(Value::from)
            .ok_or_else(|| range_message(name, *min, *max)),
        ParameterKind::Number { min, max } => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && (min..=max).contains(&value))
            .map(Value::from)
            .ok_or_else(|| number_range_message(name, *min, *max)),
        ParameterKind::Color => parse_color(text)
            .map(|rgb| Value::from(rgb.to_vec()))
            .ok_or_else(|| format!("{name} must be three channels 0..=255")),
        ParameterKind::Enum { options } => options
            .iter()
            .find(|option| *option == text.trim())
            .map(|option| Value::from(option.clone()))
            .ok_or_else(|| format!("{name} must be one of {}", options.join(", "))),
        ParameterKind::Boolean => text
            .trim()
            .parse::<bool>()
            .map(Value::from)
            .map_err(|_| format!("{name} must be a boolean")),
        ParameterKind::Artifact => lightwell_core::ArtifactId::parse(text.trim())
            .map(|id| Value::from(id.as_str()))
            .map_err(|_| format!("{name} must be an artifact identity")),
        ParameterKind::Curve { .. } => serde_json::from_str::<Value>(text.trim())
            .map_err(|_| format!("{name} must be a JSON curve point list"))
            .and_then(|value| {
                check_value(parameter, &value)
                    .map_err(|error| error.detail)
                    .map(|_| value)
            }),
        // Text is taken as typed, untrimmed: the parameter's own check decides what it accepts.
        ParameterKind::String { .. } => {
            let value = Value::from(text);
            check_value(parameter, &value)
                .map_err(|error| error.detail)
                .map(|_| value)
        }
        ParameterKind::Settings => serde_json::from_str::<Value>(text.trim())
            .map_err(|_| format!("{name} must be a JSON settings object"))
            .and_then(|value| {
                check_value(parameter, &value)
                    .map_err(|error| error.detail)
                    .map(|_| value)
            }),
        // No panel widget edits a path: a path is drawn on the canvas, so this exists only so a
        // path a client posted can be shown and read back through the same generic check.
        ParameterKind::Points { .. } => serde_json::from_str::<Value>(text.trim())
            .map_err(|_| format!("{name} must be a JSON list of [x, y] positions"))
            .and_then(|value| {
                check_value(parameter, &value)
                    .map_err(|error| error.detail)
                    .map(|_| value)
            }),
        ParameterKind::Endpoint { .. } => {
            let value = Value::from(text.trim());
            check_value(parameter, &value)
                .map_err(|error| error.detail)
                .map(|_| value)
        }
        // A secret is typed into its own masked field and sent only by `set-secret`.
        ParameterKind::Secret { .. } => Err(format!("{name} is a secret and is set on its own")),
    }
}

/// One authoritative recipe or API value as this parameter's editable field text.
pub(crate) fn value_text(parameter: &ParameterDescriptor, value: &Value) -> Result<String, String> {
    check_value(parameter, value).map_err(|error| error.detail)?;
    Ok(match &parameter.kind {
        ParameterKind::Integer { .. } => value.as_i64().unwrap().to_string(),
        ParameterKind::Number { .. } => format_number(parameter, value.as_f64().unwrap()),
        ParameterKind::Enum { .. } => value.as_str().unwrap().to_owned(),
        ParameterKind::Color => value
            .as_array()
            .unwrap()
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(","),
        ParameterKind::Boolean => value.as_bool().unwrap().to_string(),
        ParameterKind::Artifact => value.as_str().unwrap().to_owned(),
        ParameterKind::Curve { .. } | ParameterKind::Points { .. } | ParameterKind::Settings => {
            value.to_string()
        }
        ParameterKind::String { .. } | ParameterKind::Endpoint { .. } => {
            value.as_str().unwrap().to_owned()
        }
        // No plain value of a secret passes the check above.
        ParameterKind::Secret { .. } => unreachable!("a secret has no plain value"),
    })
}

fn parse_color(text: &str) -> Option<[u8; 3]> {
    let mut channels = text.split(',');
    let mut rgb = [0u8; 3];
    for slot in rgb.iter_mut() {
        *slot = channels.next()?.trim().parse().ok()?;
    }
    channels.next().is_none().then_some(rgb)
}

pub(crate) fn channel_text(value: &str, index: usize) -> &str {
    value.split(',').nth(index).unwrap_or_default().trim()
}

/// One channel of a color field replaced, keeping the other two as typed.
pub(crate) fn replace_channel(current: &str, index: usize, text: &str) -> String {
    let mut channels: Vec<&str> = (0..CHANNELS.len())
        .map(|channel| channel_text(current, channel))
        .collect();
    let trimmed = text.trim();
    if let Some(slot) = channels.get_mut(index) {
        *slot = trimmed;
    }
    channels.join(",")
}

/// A control label carries the parameter's declared unit, e.g. `X (px)`, for the kinds whose
/// value cannot: an enum's chips or a colour's channels. A slider shows the unit after its value.
pub(crate) fn labelled(label: &str, parameter: &ParameterDescriptor) -> String {
    match &parameter.unit {
        Some(unit) => format!("{label} ({unit})"),
        None => label.to_owned(),
    }
}

/// A stable widget identity per generated field, so focus survives a redraw.
pub(crate) fn field_id(action: &str, parameter: &str, channel: Option<&str>) -> String {
    match channel {
        Some(channel) => format!("lightwell.field.{action}.{parameter}.{channel}"),
        None => format!("lightwell.field.{action}.{parameter}"),
    }
}

pub(crate) fn undeclared_label(action: &str, parameter: &str) -> String {
    format!("Unsupported control: {action} declares no parameter {parameter}")
}

pub(crate) fn unsupported_label(kind: &str) -> String {
    format!("Unsupported control: {kind}")
}

/// The request fields for one action.
///
/// A **patch** action sends exactly the fields it was given and nothing else: a generated control
/// submits its own parameter, and a reset or action control submits its declared preset. Filling
/// the other declared parameters would turn one slider's move into a patch over the whole module,
/// which is the difference between "set Exposure" and "set every Basic field to whatever the panel
/// happens to show".
///
/// Every other action keeps sending every parameter it declares: preset values merged over the
/// parsed field text, preset wins, so crop and pixel requests are unchanged. A parameter with a
/// declared default is left out so the host applies that default.
pub(crate) fn action_params(
    action: &ActionDescriptor,
    preset: &Map<String, Value>,
    fields: &Fields,
) -> Result<Map<String, Value>, String> {
    if action.patch {
        return Ok(preset.clone());
    }
    let mut params = Map::new();
    for parameter in &action.parameters {
        if let Some(value) = preset.get(&parameter.name) {
            params.insert(parameter.name.clone(), value.clone());
        } else if let Some(text) = fields.get(&action.id, &parameter.name) {
            params.insert(parameter.name.clone(), parse_field(parameter, text)?);
        } else if parameter.default.is_none() && parameter.required {
            return Err(format!("{} requires {}", action.title, parameter.name));
        }
    }
    Ok(params)
}

/// What one generated control submits when it is released, or when Enter is pressed in its field.
///
/// A control of a patch action submits its own parameter alone, read from the field exactly as it
/// is displayed; every other control keeps submitting the preset of the first control that invokes
/// the action, which [`action_params`] then merges over the remaining fields.
pub(crate) fn submit_preset(
    modules: &[ModuleDescriptor],
    action: &str,
    parameter: Option<&str>,
    fields: &Fields,
) -> Result<Map<String, Value>, String> {
    let declared = crate::state::tools::declared_action(modules, action)
        .ok_or_else(|| format!("No module declares the action {action}"))?;
    if declared.patch {
        let Some(parameter) = parameter else {
            // A patch action reached without a field is a control that carries its own preset,
            // such as a group reset; the preset is the whole request.
            return Ok(control_preset_of(modules, action));
        };
        let declared = declared
            .parameter(parameter)
            .ok_or_else(|| undeclared_label(action, parameter))?;
        let text = fields.get(action, parameter).unwrap_or_default();
        let value = parse_field(declared, text)?;
        return Ok([(parameter.to_owned(), value)].into_iter().collect());
    }
    Ok(control_preset_of(modules, action))
}

/// The preset of the first generated control that invokes this action, if any.
fn control_preset_of(modules: &[ModuleDescriptor], action: &str) -> Map<String, Value> {
    modules
        .iter()
        .find_map(|module| control_preset(&module.controls, action))
        .cloned()
        .unwrap_or_default()
}

/// What resetting one field runs — a double-click on its label or rail, or its field's own reset —
/// as the action and the parameters it is sent with.
///
/// A number control that declares its own reset runs exactly that: an action of its module with
/// fixed parameters, such as RAW's custom temperature and tint returning the development to As
/// shot. Otherwise the field returns to its declared default, which runs as one action exactly
/// where one field is already a whole request — a patch action's field, which the module merges,
/// or the only parameter its action declares. An action with a second parameter has no way to send
/// one field alone, so the reset only refills the text there, as it has always done. A non-patch
/// action's default is the value that is sent, so a parameter that declares none cannot be reset
/// this way either; `seed_text` would invent its minimum, and inventing a value to commit is not a
/// reset.
pub(crate) fn field_reset(
    modules: &[ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<(String, Map<String, Value>)> {
    if let Some(reset) = crate::state::tools::declared_field_reset(modules, action, parameter) {
        return Some((reset.action.clone(), reset.preset.clone()));
    }
    let declared = crate::state::tools::declared_action(modules, action)?;
    if !declared.patch && !crate::state::tools::drafts_alone(modules, action, parameter) {
        return None;
    }
    let declared = declared.parameter(parameter)?;
    if !crate::state::tools::is_patch(modules, action) && declared.default.is_none() {
        return None;
    }
    let value = parse_field(declared, &seed_text(declared)).ok()?;
    Some((
        action.to_owned(),
        [(parameter.to_owned(), value)].into_iter().collect(),
    ))
}

fn control_preset<'a>(controls: &'a [Control], action: &str) -> Option<&'a Map<String, Value>> {
    controls.iter().find_map(|control| match classify(control) {
        Rendered::Group { controls, .. } => control_preset(controls, action),
        Rendered::Action {
            action: declared,
            preset,
            ..
        } if declared == action => Some(preset),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tools::{control_kind, declared_action, point_pick};
    use serde_json::json;

    #[test]
    fn boolean_color_and_curve_fields_reflect_exact_authoritative_values() {
        let descriptor = crate::app::testing::controls_descriptor();
        let action = descriptor.action("fixture-set").unwrap();
        let mut fields = Fields::seeded(std::slice::from_ref(&descriptor));
        for (name, value, expected_text) in [
            ("enabled", json!(true), "true"),
            ("rgb", json!([12, 34, 56]), "12,34,56"),
            (
                "master",
                json!([[0.0, 0.0], [0.25, 0.37], [1.0, 1.0]]),
                "[[0.0,0.0],[0.25,0.37],[1.0,1.0]]",
            ),
        ] {
            let parameter = action.parameter(name).unwrap();
            fields.set_value(&action.id, parameter, &value).unwrap();
            assert_eq!(fields.get(&action.id, name), Some(expected_text));
            assert_eq!(fields.get_value(&action.id, parameter).unwrap(), value);
        }
        let curve = action.parameter("master").unwrap();
        assert!(
            fields
                .set_value(
                    &action.id,
                    curve,
                    &json!([[0.0, 0.0], [0.0, 0.5], [1.0, 1.0]])
                )
                .is_err()
        );
        assert_eq!(
            fields.get_value(&action.id, curve).unwrap(),
            json!([[0.0, 0.0], [0.25, 0.37], [1.0, 1.0]])
        );
    }

    /// The descriptors the desktop would fetch through `module.list`.
    fn descriptors() -> Vec<ModuleDescriptor> {
        lightwell_core::ModuleRegistry::builtin()
            .descriptors()
            .into_iter()
            .cloned()
            .collect()
    }

    fn parameter_of<'a>(
        modules: &'a [ModuleDescriptor],
        action: &str,
        name: &str,
    ) -> &'a ParameterDescriptor {
        declared_action(modules, action)
            .and_then(|declared| declared.parameter(name))
            .expect("the declared parameter")
    }

    /// A declared number parameter: the kind the crop module's angle and rectangle use.
    fn number_parameter(default: Option<Value>) -> ParameterDescriptor {
        ParameterDescriptor {
            name: "angle".into(),
            kind: ParameterKind::Number {
                min: -45.0,
                max: 45.0,
            },
            required: true,
            default,
            unit: Some("deg".into()),
            step: None,
            precision: None,
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
            notes: "test".into(),
        }
    }

    /// The same parameter with the hints a module can declare for it.
    fn hinted(
        default: Option<Value>,
        step: Option<f64>,
        precision: Option<u8>,
    ) -> ParameterDescriptor {
        ParameterDescriptor {
            step,
            precision,
            ..number_parameter(default)
        }
    }

    #[test]
    fn fields_are_seeded_from_declared_defaults_and_limits() {
        let modules = descriptors();
        let fields = Fields::seeded(&modules);
        let (action, x, y) = point_pick(&modules).expect("the pixel module declares a canvas pick");
        assert_eq!(
            fields.get(action, x),
            Some("0"),
            "integers seed at their min"
        );
        assert_eq!(fields.get(action, y), Some("0"));
        let color = declared_action(&modules, action)
            .expect("the declared action")
            .parameters
            .iter()
            .find(|parameter| matches!(parameter.kind, ParameterKind::Color))
            .expect("the pixel action declares a color");
        assert_eq!(fields.get(action, &color.name), Some("0,0,0"));
        // The host's own mask controls are seeded from the same declarations through the same
        // walk, so a gradient endpoint and a mask's amount are fields exactly as a module's are.
        assert_eq!(fields.get("mask.set-amount", "amount"), Some("0"));
        assert_eq!(fields.get("mask.set-component-mode", "mode"), Some("add"));
        assert_eq!(fields.get("mask.set-linear", "x0"), Some("-1.0000"));
        // Only declared fields exist: an action driven by presets alone has none, and every
        // declared number, integer and colour parameter of a built-in has exactly one.
        assert_eq!(
            fields
                .summary()
                .as_object()
                .expect("an object")
                .keys()
                .collect::<Vec<_>>(),
            [
                "mask.set-amount.amount",
                "mask.set-colour-range.refine",
                "mask.set-component-invert.invert",
                "mask.set-component-mode.mode",
                "mask.set-invert.invert",
                "mask.set-linear.x0",
                "mask.set-linear.x1",
                "mask.set-linear.y0",
                "mask.set-linear.y1",
                "mask.set-luminance-range.high",
                "mask.set-luminance-range.high_feather",
                "mask.set-luminance-range.low",
                "mask.set-luminance-range.low_feather",
                "mask.set-radial.angle",
                "mask.set-radial.feather",
                "mask.set-radial.radius_x",
                "mask.set-radial.radius_y",
                "mask.set-radial.x",
                "mask.set-radial.y",
                "set-basic.blacks",
                "set-basic.contrast",
                "set-basic.exposure",
                "set-basic.highlights",
                "set-basic.saturation",
                "set-basic.shadows",
                "set-basic.temperature",
                "set-basic.tint",
                "set-basic.vibrance",
                "set-basic.whites",
                "set-mixer.aqua-hue",
                "set-mixer.aqua-luminance",
                "set-mixer.aqua-saturation",
                "set-mixer.blue-hue",
                "set-mixer.blue-luminance",
                "set-mixer.blue-saturation",
                "set-mixer.green-hue",
                "set-mixer.green-luminance",
                "set-mixer.green-saturation",
                "set-mixer.magenta-hue",
                "set-mixer.magenta-luminance",
                "set-mixer.magenta-saturation",
                "set-mixer.orange-hue",
                "set-mixer.orange-luminance",
                "set-mixer.orange-saturation",
                "set-mixer.purple-hue",
                "set-mixer.purple-luminance",
                "set-mixer.purple-saturation",
                "set-mixer.red-hue",
                "set-mixer.red-luminance",
                "set-mixer.red-saturation",
                "set-mixer.yellow-hue",
                "set-mixer.yellow-luminance",
                "set-mixer.yellow-saturation",
                "set-pixel.rgb",
                "set-pixel.x",
                "set-pixel.y",
                "set-presence.clarity",
                "set-presence.dehaze",
                "set-presence.texture",
                "set-raw-exposure.ev",
                "set-raw-temperature.kelvin",
                "set-raw-tint.tint",
                "set-vignette.amount",
                "set-vignette.feather",
                "set-vignette.midpoint",
                "set-vignette.roundness"
            ],
            "{}",
            fields.summary()
        );
        assert_eq!(
            seed_text(&ParameterDescriptor {
                default: Some(json!(7)),
                ..parameter_of(&modules, action, x).clone()
            }),
            "7",
            "a declared default wins over the minimum"
        );
        assert_eq!(
            seed_text(&number_parameter(None)),
            "-45",
            "a number without a default seeds at its min"
        );
        assert_eq!(
            seed_text(&number_parameter(Some(json!(0)))),
            "0",
            "a whole number default seeds without trailing noise"
        );
        // A parameter that declares nothing is shown with the decimals of the generic step the
        // panel derives from its range, which over -45..45 is whole degrees.
        assert_eq!(seed_text(&number_parameter(Some(json!(-3.5)))), "-3.5");
        // Declaring a precision is how a module asks for the digits it cares about.
        assert_eq!(
            seed_text(&hinted(Some(json!(-3.5)), Some(0.1), Some(1))),
            "-3.5"
        );
        assert_eq!(
            seed_text(&hinted(Some(json!(-3.5)), Some(0.01), Some(2))),
            "-3.50"
        );
    }

    /// Ordinary values use the declared decimals without float noise. A value reached through a
    /// fine step shows the extra digits it needs, so its field remains editable without losing it.
    #[test]
    fn a_declared_parameters_value_is_shown_with_the_decimals_it_declares() {
        // A declared precision wins outright.
        let declared = hinted(None, Some(0.01), Some(2));
        assert_eq!(decimals_for(&declared), 2);
        assert_eq!(format_number(&declared, 0.001), "0.001");
        assert_eq!(
            parse_field(&declared, &format_number(&declared, 0.001)),
            Ok(json!(0.001))
        );
        for (value, text) in [
            (1.7000000000000002, "1.70"),
            (0.0, "0.00"),
            (-3.5, "-3.50"),
            (-0.0, "0.00"),
            (-0.004, "-0.004"),
            (-0.006, "-0.006"),
        ] {
            assert_eq!(format_number(&declared, value), text, "{value}");
        }
        // Without a precision the declared step decides: 0.5 is one decimal, 10 is none.
        assert_eq!(decimals_for(&hinted(None, Some(0.5), None)), 1);
        assert_eq!(format_number(&hinted(None, Some(0.5), None), 2.25), "2.25");
        assert_eq!(decimals_for(&hinted(None, Some(10.0), None)), 0);
        assert_eq!(format_number(&hinted(None, Some(10.0), None), 40.0), "40");
        assert_eq!(decimals_for(&hinted(None, Some(0.001), None)), 3);
        // Without either, the generic step over the range does: 90 degrees give whole degrees.
        assert_eq!(decimals_for(&number_parameter(None)), 0);
        assert_eq!(format_number(&number_parameter(None), -0.4), "-0.4");
        // An integer parameter is an integer whatever else it says.
        let modules = descriptors();
        let (action, x, _) = point_pick(&modules).expect("a canvas pick");
        let coordinate = parameter_of(&modules, action, x);
        assert_eq!(decimals_for(coordinate), 0);
        assert_eq!(format_number(coordinate, 12.0), "12");
        // A step no control could use asks for no decimals rather than endless ones.
        assert_eq!(decimals_for(&hinted(None, Some(1.0 / 3.0), None)), 6);
        assert_eq!(decimals_for(&hinted(None, Some(f64::NAN), None)), 0);
    }

    /// Every number parameter a registered module puts behind a generated control declares its
    /// own step and precision, so no slider on screen falls back to the generic rule. Parameters
    /// the host drives itself (the crop frame's rectangle and angle, which the crop panel edits)
    /// are not generated controls and are not covered by this.
    #[test]
    fn every_generated_number_control_of_a_built_in_names_its_step_and_precision() {
        let modules = descriptors();
        fn walk(module: &ModuleDescriptor, controls: &[Control], checked: &mut usize) {
            for control in controls {
                match classify(control) {
                    Rendered::Group { controls, .. } => walk(module, controls, checked),
                    Rendered::Number {
                        action, parameter, ..
                    } => {
                        let declared = ControlOwner::Module(module)
                            .parameter(action, parameter)
                            .expect("a generated control names a declared parameter");
                        if !matches!(declared.kind, ParameterKind::Number { .. }) {
                            continue;
                        }
                        assert!(
                            declared.step.is_some() && declared.precision.is_some(),
                            "{action}.{parameter} declares no step or precision"
                        );
                        *checked += 1;
                    }
                    _ => {}
                }
            }
        }
        let mut checked = 0;
        for module in &modules {
            walk(module, &module.controls, &mut checked);
        }
        assert!(checked >= 10, "only {checked} generated number controls");
    }

    #[test]
    fn field_text_is_validated_against_the_declared_parameter() {
        let modules = descriptors();
        let (action, x, _) = point_pick(&modules).expect("a canvas pick");
        let coordinate = parameter_of(&modules, action, x);
        assert_eq!(parse_field(coordinate, " 12 ").unwrap(), json!(12));
        for text in ["", "1.5", "-1", "16384", "twelve"] {
            let message = parse_field(coordinate, text).expect_err(text);
            assert!(message.contains("0..=16383"), "{text}: {message}");
        }
        let color = declared_action(&modules, action)
            .expect("the declared action")
            .parameters
            .iter()
            .find(|parameter| matches!(parameter.kind, ParameterKind::Color))
            .expect("a color parameter");
        assert_eq!(parse_field(color, "255,0,0").unwrap(), json!([255, 0, 0]));
        for text in ["255,0", "256,0,0", "255,0,0,0", "a,b,c", ""] {
            let message = parse_field(color, text).expect_err(text);
            assert!(message.contains("0..=255"), "{text}: {message}");
        }
        let angle = number_parameter(None);
        for (text, expected) in [
            (" -3.5 ", json!(-3.5)),
            ("0", json!(0.0)),
            ("45", json!(45.0)),
        ] {
            assert_eq!(parse_field(&angle, text).unwrap(), expected, "{text}");
        }
        for text in ["", "45.1", "-45.1", "three", "1e400", "nan", "inf"] {
            let message = parse_field(&angle, text).expect_err(text);
            assert_eq!(
                message, "angle must be a number from -45 to 45",
                "{text}: {message}"
            );
        }
        let choice = modules
            .iter()
            .flat_map(|module| module.actions.iter())
            .flat_map(|action| action.parameters.iter())
            .find(|parameter| matches!(parameter.kind, ParameterKind::Enum { .. }))
            .expect("the transform module declares an enum");
        let ParameterKind::Enum { options } = &choice.kind else {
            unreachable!("filtered above")
        };
        assert_eq!(
            parse_field(choice, &options[0]).unwrap(),
            json!(options[0].clone())
        );
        let message = parse_field(choice, "sideways").expect_err("an undeclared option");
        assert!(message.contains(&options[0]), "{message}");
    }

    #[test]
    fn colour_channels_are_edited_one_at_a_time() {
        assert_eq!(channel_text("1,2,3", 1), "2");
        assert_eq!(channel_text("1,2", 2), "");
        assert_eq!(replace_channel("1,2,3", 1, " 200 "), "1,200,3");
        assert_eq!(replace_channel("", 0, "5"), "5,,");
    }

    #[test]
    fn action_parameters_merge_presets_over_field_values() {
        let modules = descriptors();
        let (action, x, y) = point_pick(&modules).expect("a canvas pick");
        let declared = declared_action(&modules, action).expect("the declared action");
        let mut fields = Fields::seeded(&modules);
        fields.set(action, x, "4".into());
        fields.set(action, y, "5".into());
        let params = action_params(declared, &Map::new(), &fields).unwrap();
        assert_eq!(params[x], json!(4));
        assert_eq!(params[y], json!(5));
        let preset = json!({ x: 9 }).as_object().expect("an object").clone();
        let params = action_params(declared, &preset, &fields).unwrap();
        assert_eq!(params[x], json!(9), "the preset wins over the field");
        assert_eq!(params[y], json!(5));
        fields.set(action, x, "nine".into());
        let message = action_params(declared, &Map::new(), &fields)
            .expect_err("an unparsable field stops the request");
        assert!(message.contains("0..=16383"), "{message}");
        assert!(
            action_params(declared, &preset, &fields).is_ok(),
            "a preset supplies the parameter the field cannot"
        );
    }

    #[test]
    fn an_action_runs_only_when_every_required_parameter_is_supplied() {
        let modules = descriptors();
        let fields = Fields::seeded(&modules);
        let runnable = |action: &str, preset: &Map<String, Value>| {
            action_params(
                declared_action(&modules, action).expect("the declared action"),
                preset,
                &fields,
            )
            .is_ok()
        };
        for module in &modules {
            for control in &module.controls {
                let Rendered::Group { controls, .. } = classify(control) else {
                    continue;
                };
                for child in controls {
                    if let Rendered::Action { action, preset, .. } = classify(child) {
                        assert!(
                            runnable(action, preset),
                            "{action} is not runnable from its declared control"
                        );
                        assert!(
                            preset.is_empty() || !runnable(action, &Map::new()),
                            "{action} needs its preset to run"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn enter_in_a_field_runs_the_first_control_that_invokes_the_action() {
        let modules = descriptors();
        let fields = Fields::seeded(&modules);
        let (action, x, _) = point_pick(&modules).expect("a canvas pick");
        assert_eq!(
            submit_preset(&modules, action, Some(x), &fields),
            Ok(Map::new()),
            "the pixel action is not a patch, so its fields all travel together"
        );
        // The transform module's controls are action buttons rather than fields, which is the
        // shape this submit rule is about; Basic's are sliders of a patch action.
        let choice = modules
            .iter()
            .find(|module| module.id == "lightwell.transform")
            .expect("the transform module");
        let Some(Rendered::Action { action, preset, .. }) =
            choice.controls.first().map(classify).map(|control| {
                let Rendered::Group { controls, .. } = control else {
                    unreachable!("transform controls are grouped")
                };
                classify(&controls[0])
            })
        else {
            unreachable!("the first transform control invokes an action")
        };
        assert_eq!(
            submit_preset(&modules, action, None, &fields).as_ref(),
            Ok(preset)
        );
        assert!(submit_preset(&modules, "no-such-action", None, &fields).is_err());
    }

    /// The generic submit rule, proved on the descriptors the desktop actually fetches: a control
    /// of a patch action submits its own parameter alone, a reset control of one submits its
    /// declared preset alone, and every other control keeps sending the action's whole parameter
    /// list, so crop and pixel requests are unchanged.
    #[test]
    fn a_patch_actions_control_submits_its_own_field_and_nothing_else() {
        let modules = descriptors();
        let mut fields = Fields::seeded(&modules);
        let patch = modules
            .iter()
            .flat_map(|module| module.actions.iter())
            .find(|action| action.patch)
            .expect("a built-in declares a field patch");
        let parameter = patch.parameters.first().expect("a declared field");
        fields.set(&patch.id, &parameter.name, "1.25".into());

        let preset = submit_preset(&modules, &patch.id, Some(&parameter.name), &fields).unwrap();
        assert_eq!(
            preset,
            json!({ parameter.name.clone(): 1.25 })
                .as_object()
                .cloned()
                .unwrap(),
            "a slider of a patch action submits its own parameter only"
        );
        assert_eq!(
            action_params(patch, &preset, &fields).unwrap(),
            preset,
            "and the request is exactly that patch, with no declared default filled in"
        );

        // A reset control of the same action submits its declared preset and nothing else.
        let reset = modules
            .iter()
            .flat_map(|module| group_resets(&module.controls))
            .find(|reset| reset.action == patch.id)
            .expect("the patch action declares a group reset");
        assert_eq!(
            action_params(patch, &reset.preset, &fields).unwrap(),
            reset.preset,
            "a group reset submits its preset alone"
        );
        assert!(
            !reset.preset.is_empty(),
            "a group reset names the fields it neutralizes"
        );

        // An unreadable field stops the submit with the range it needs, and mutates nothing.
        fields.set(&patch.id, &parameter.name, "sideways".into());
        let message = submit_preset(&modules, &patch.id, Some(&parameter.name), &fields)
            .expect_err("an unparsable field commits nothing");
        assert!(message.contains(&parameter.name), "{message}");

        // Every non-patch action still sends its whole declared parameter list.
        let (action, x, y) = point_pick(&modules).expect("a canvas pick");
        let declared = declared_action(&modules, action).expect("the declared action");
        assert!(!declared.patch);
        let fields = Fields::seeded(&modules);
        let preset = submit_preset(&modules, action, Some(x), &fields).unwrap();
        let params = action_params(declared, &preset, &fields).unwrap();
        for name in [x, y] {
            assert!(
                params.contains_key(name),
                "{action} must keep sending {name}: {params:?}"
            );
        }
    }

    /// A double-click on a patch action's label is one action setting that field to its default.
    #[test]
    fn a_double_click_resets_one_field_of_a_patch_action() {
        let modules = descriptors();
        let patch = modules
            .iter()
            .flat_map(|module| module.actions.iter())
            .find(|action| action.patch)
            .expect("a built-in declares a field patch");
        let parameter = patch.parameters.first().expect("a declared field");
        let (action, preset) = field_reset(&modules, &patch.id, &parameter.name)
            .expect("a patch action resets one field as one action");
        assert_eq!(action, patch.id);
        assert_eq!(preset.len(), 1);
        assert_eq!(
            preset[&parameter.name],
            parameter.default.clone().expect("a declared default")
        );
        // A non-patch action cannot send one field alone, so the double-click only refills text.
        let (action, x, _) = point_pick(&modules).expect("a canvas pick");
        assert_eq!(field_reset(&modules, action, x), None);
    }

    /// A number control that declares its own reset runs that action with its preset, and no
    /// field default; the same field without the declaration resets to its default again.
    #[test]
    fn a_declared_field_reset_runs_its_own_action() {
        let mut descriptor = crate::app::testing::controls_descriptor();
        // The declared reset sends another field, never the Amount default.
        let reset = lightwell_core::ResetAction {
            action: "fixture-set".into(),
            preset: json!({"mode": "two"}).as_object().unwrap().clone(),
        };
        let (action, parameter) = declare_amount_reset(&mut descriptor, Some(reset.clone()));
        let modules = [descriptor.clone()];
        assert_eq!(
            field_reset(&modules, &action, &parameter),
            Some((reset.action.clone(), reset.preset.clone()))
        );
        declare_amount_reset(&mut descriptor, None);
        let modules = [descriptor];
        let (sent, preset) = field_reset(&modules, &action, &parameter)
            .expect("a patch field resets to its default");
        assert_eq!(sent, action);
        assert_eq!(preset.keys().collect::<Vec<_>>(), [&parameter]);
    }

    /// Set the controls fixture's Amount slider's declared field reset, returning its field.
    pub(crate) fn declare_amount_reset(
        descriptor: &mut ModuleDescriptor,
        declared: Option<lightwell_core::ResetAction>,
    ) -> (String, String) {
        fn walk(
            controls: &mut [Control],
            declared: &Option<lightwell_core::ResetAction>,
        ) -> Option<(String, String)> {
            controls.iter_mut().find_map(|control| match control {
                Control::Group { controls, .. } => walk(controls, declared),
                Control::Number {
                    action,
                    parameter,
                    reset,
                    ..
                } if parameter == "amount" => {
                    *reset = declared.clone();
                    Some((action.clone(), parameter.clone()))
                }
                _ => None,
            })
        }
        let field = walk(&mut descriptor.controls, &declared).expect("the Amount slider");
        descriptor
            .validate()
            .expect("the fixture validates with its reset");
        field
    }

    /// Every group reset a module's controls declare, in order.
    fn group_resets(controls: &[Control]) -> Vec<lightwell_core::ResetAction> {
        controls
            .iter()
            .flat_map(|control| match classify(control) {
                Rendered::Group {
                    controls, reset, ..
                } => reset
                    .cloned()
                    .into_iter()
                    .chain(group_resets(controls))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn an_unrenderable_control_kind_is_named_not_dropped() {
        assert_eq!(
            unsupported_label("gradient"),
            "Unsupported control: gradient"
        );
        let controls = [
            Control::Group {
                label: "Group".into(),
                controls: Vec::new(),
                reset: None,
                collapsed: false,
            },
            Control::Number {
                action: "act".into(),
                parameter: "x".into(),
                label: "X".into(),
                style: lightwell_core::NumberStyle::Slider,
                rail: None,
                reset: None,
            },
            Control::Color {
                action: "act".into(),
                parameter: "rgb".into(),
                label: "RGB".into(),
                style: lightwell_core::ColorStyle::Fields,
            },
            Control::Action {
                action: "act".into(),
                label: "Apply".into(),
                preset: Map::new(),
                style: lightwell_core::ActionStyle::Default,
                icon: None,
            },
        ];
        for (control, kind) in controls.iter().zip(["group", "number", "color", "action"]) {
            assert_eq!(control_kind(control), kind);
            assert!(
                !matches!(classify(control), Rendered::Unsupported(_)),
                "{kind} is rendered"
            );
        }
        // The host renders the preset library where a module declares its presets control.
        let presets = Control::Presets {
            action: "apply-preset".into(),
        };
        assert_eq!(control_kind(&presets), "presets");
        assert!(matches!(
            classify(&presets),
            Rendered::Presets { action } if action == "apply-preset"
        ));
        // Every control the registered modules declare has a real rendering.
        for module in descriptors() {
            let mut queue: Vec<&Control> = module.controls.iter().collect();
            while let Some(control) = queue.pop() {
                match classify(control) {
                    Rendered::Group { controls, .. } => queue.extend(controls),
                    Rendered::Unsupported(kind) => panic!("{} declares {kind}", module.id),
                    _ => {}
                }
            }
        }
        assert_eq!(
            undeclared_label("act", "z"),
            "Unsupported control: act declares no parameter z"
        );
    }
}
