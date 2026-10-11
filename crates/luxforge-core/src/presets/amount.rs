//! Bounded settings-only Amount planning. The module's descriptors supply the neutral values
//! and bounds; no image is read and no new rendering operation is introduced.
use crate::{Error, ModuleDescriptor, ParameterKind, check_parameters};
use serde_json::{Map, Value, json};

/// Scale named preset fields from their module defaults. 100 preserves the settings verbatim.
/// Numbers clamp to their declared bounds; identity-default curves blend y from x, clamping and
/// preserving declared monotonicity on extrapolation. Other shapes and analysis steps explicitly
/// refuse a non-100 amount. Clients can call this with 0 to discover whether Amount is supported.
pub fn settings_at_amount(
    modules: &[impl std::borrow::Borrow<ModuleDescriptor>],
    settings: &Map<String, Value>,
    amount: f64,
) -> Result<Map<String, Value>, Error> {
    if !amount.is_finite() || !(0.0..=200.0).contains(&amount) {
        return Err(Error::validation(
            "preset amount must be finite and within 0..=200",
        ));
    }
    if amount == 100.0 {
        return Ok(settings.clone());
    }
    let fraction = amount / 100.0;
    let mut scaled = Map::new();
    for (id, fields) in settings {
        let action = modules
            .iter()
            .map(std::borrow::Borrow::borrow)
            .filter(|m| m.is_available())
            .find_map(|m| m.action(id))
            .ok_or_else(|| Error::validation(format!("Amount unavailable for {id}")))?;
        if !action.patch || action.analysis.is_some() {
            return Err(Error::validation(format!("Amount unavailable for {id}")));
        }
        let fields = check_parameters(action, fields)?;
        let mut result = Map::new();
        for (name, value) in fields {
            let parameter = action
                .parameter(&name)
                .ok_or_else(|| Error::validation("unknown preset field"))?;
            let unsupported = || Error::validation(format!("Amount unavailable for {id}.{name}"));
            let value = match &parameter.kind {
                ParameterKind::Number { min, max } => {
                    let neutral = parameter
                        .default
                        .as_ref()
                        .and_then(Value::as_f64)
                        .ok_or_else(unsupported)?;
                    json!(
                        (neutral + (value.as_f64().ok_or_else(unsupported)? - neutral) * fraction)
                            .clamp(*min, *max)
                    )
                }
                ParameterKind::Curve { monotone, .. } => {
                    let neutral = parameter
                        .default
                        .as_ref()
                        .and_then(Value::as_array)
                        .ok_or_else(unsupported)?;
                    if !neutral.iter().all(|p| p[0].as_f64() == p[1].as_f64()) {
                        return Err(unsupported());
                    }
                    let mut previous = 0.0_f64;
                    let points = value
                        .as_array()
                        .ok_or_else(unsupported)?
                        .iter()
                        .map(|p| {
                            let x = p[0].as_f64().expect("validated curve x");
                            let y = p[1].as_f64().expect("validated curve y");
                            let mut y = (x + (y - x) * fraction).clamp(0.0, 1.0);
                            if *monotone {
                                y = y.max(previous);
                            }
                            previous = y;
                            json!([x, y])
                        })
                        .collect::<Vec<_>>();
                    Value::Array(points)
                }
                _ => return Err(unsupported()),
            };
            result.insert(name, value);
        }
        check_parameters(action, &Value::Object(result.clone()))?;
        scaled.insert(id.clone(), Value::Object(result));
    }
    Ok(scaled)
}

/// Whether a preset can use the shared draft path. Analysis steps cannot be drafted.
pub fn settings_can_preview(
    modules: &[impl std::borrow::Borrow<ModuleDescriptor>],
    settings: &Map<String, Value>,
) -> bool {
    settings.keys().all(|id| {
        modules
            .iter()
            .map(std::borrow::Borrow::borrow)
            .filter(|m| m.is_available())
            .find_map(|m| m.action(id))
            .is_some_and(|action| action.patch && action.analysis.is_none())
    })
}
