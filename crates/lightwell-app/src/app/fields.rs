//! The text typed into every generated control, and the rules that read it back. Validation always
//! runs against the declared parameter, never against a parsed copy, so an invalid field keeps what
//! was typed and commits nothing.
use crate::state::tools::{Rendered, classify, declared_parameter};
use lightwell_core::{
    ActionDescriptor, Control, ModuleDescriptor, ParameterDescriptor, ParameterKind, RAW_EFFECT,
    RawPayload, Recipe, WhiteBalanceMode,
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
    pub(crate) fn seeded(modules: &[ModuleDescriptor]) -> Self {
        let mut fields = Self::default();
        for module in modules {
            seed_controls(module, &module.controls, &mut fields);
        }
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

    /// Reflect the displayed RAW history entry. While a person edits one field, keep their text;
    /// all other controls follow authoritative recipe state across undo, redo and reopen.
    pub(crate) fn bind_raw(
        &mut self,
        recipe: &Recipe,
        editing: Option<&(String, String)>,
        dragging: Option<&(String, String)>,
    ) {
        let Some(layer) = recipe
            .layers
            .first()
            .filter(|layer| layer.effect_id == RAW_EFFECT)
        else {
            return;
        };
        let Ok(payload) = RawPayload::from_layer(layer) else {
            return;
        };
        let gains = match payload.wb_mode {
            WhiteBalanceMode::AsShot => payload.as_shot_gains,
            WhiteBalanceMode::Custom => payload.gains,
        };
        for (action, parameter, text) in [
            ("set-raw-exposure", "ev", number_text(payload.exposure_ev)),
            (
                "set-raw-temperature",
                "kelvin",
                number_text(payload.temperature_kelvin.unwrap_or(6504.0)),
            ),
            (
                "set-raw-tint",
                "tint",
                number_text(payload.tint.unwrap_or(0.0)),
            ),
            ("set-raw-red-gain", "gain", gains[0].to_string()),
            ("set-raw-blue-gain", "gain", gains[2].to_string()),
        ] {
            if editing.is_some_and(|field| field.0 == action && field.1 == parameter)
                || dragging.is_some_and(|field| field.0 == action && field.1 == parameter)
            {
                continue;
            }
            self.set(action, parameter, text);
        }
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

fn seed_controls(module: &ModuleDescriptor, controls: &[Control], fields: &mut Fields) {
    for control in controls {
        match classify(control) {
            Rendered::Group { controls, .. } => seed_controls(module, controls, fields),
            Rendered::Number {
                action, parameter, ..
            }
            | Rendered::Color {
                action, parameter, ..
            } => {
                if let Some(declared) = declared_parameter(module, action, parameter) {
                    fields.set(action, parameter, seed_text(declared));
                }
            }
            // Neither carries a field of its own: an action button submits the fields already
            // seeded, and a picker only enters its module's canvas mode.
            Rendered::Action { .. } | Rendered::Picker { .. } | Rendered::Unsupported(_) => {}
        }
    }
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
        ParameterKind::Number { min, .. } => number_text(
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

/// A number as a field would hold it: `0`, `-3.5`, no trailing zeros or exponent noise.
pub(crate) fn number_text(value: f64) -> String {
    format!("{value}")
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
    }
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

/// A control label carries the parameter's declared unit, e.g. `X (px)`.
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

/// What a double-click on a label submits: that one field at its declared default.
///
/// It runs as one action exactly where one field is already a whole request — a patch action's
/// field, which the module merges, or the only parameter its action declares. An action with a
/// second parameter has no way to send one field alone, so the double-click only refills the text
/// there, as it has always done. A non-patch action's default is the value that is sent, so a
/// parameter that declares none cannot be reset this way either; `seed_text` would invent its
/// minimum, and inventing a value to commit is not a reset.
pub(crate) fn reset_field_preset(
    modules: &[ModuleDescriptor],
    action: &str,
    parameter: &str,
) -> Option<Map<String, Value>> {
    let declared = crate::state::tools::declared_action(modules, action)?;
    if !declared.patch && !crate::state::tools::drafts_alone(modules, action, parameter) {
        return None;
    }
    let declared = declared.parameter(parameter)?;
    if !crate::state::tools::is_patch(modules, action) && declared.default.is_none() {
        return None;
    }
    let value = parse_field(declared, &seed_text(declared)).ok()?;
    Some([(parameter.to_owned(), value)].into_iter().collect())
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
            notes: "test".into(),
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
                "set-pixel.rgb",
                "set-pixel.x",
                "set-pixel.y",
                "set-raw-exposure.ev",
                "set-raw-temperature.kelvin",
                "set-raw-tint.tint"
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
        assert_eq!(seed_text(&number_parameter(Some(json!(-3.5)))), "-3.5");
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
        let preset = reset_field_preset(&modules, &patch.id, &parameter.name)
            .expect("a patch action resets one field as one action");
        assert_eq!(preset.len(), 1);
        assert_eq!(
            preset[&parameter.name],
            parameter.default.clone().expect("a declared default")
        );
        // A non-patch action cannot send one field alone, so the double-click only refills text.
        let (action, x, _) = point_pick(&modules).expect("a canvas pick");
        assert_eq!(reset_field_preset(&modules, action, x), None);
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
            },
            Control::Number {
                action: "act".into(),
                parameter: "x".into(),
                label: "X".into(),
            },
            Control::Color {
                action: "act".into(),
                parameter: "rgb".into(),
                label: "RGB".into(),
            },
            Control::Action {
                action: "act".into(),
                label: "Apply".into(),
                preset: Map::new(),
            },
        ];
        for (control, kind) in controls.iter().zip(["group", "number", "color", "action"]) {
            assert_eq!(control_kind(control), kind);
            assert!(
                !matches!(classify(control), Rendered::Unsupported(_)),
                "{kind} is rendered"
            );
        }
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
