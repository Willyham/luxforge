//! The checks a request's values meet against their declarations: one value against one
//! parameter, a whole action's parameters with its defaults, a settings set and the typed decode a
//! module reads its checked fields through.
use super::types::{ActionDescriptor, MAX_ENDPOINT_BYTES, ParameterDescriptor, ParameterKind};
use super::validate::valid_name;
use crate::{Error, capabilities::endpoint::parse_endpoint};
use serde::Deserialize;
use serde_json::{Map, Value};

/// The most field-patch actions one `settings` value names.
pub(crate) const MAX_SETTINGS_ACTIONS: usize = 16;

/// The most fields one action of a `settings` value sets.
pub(crate) const MAX_SETTINGS_FIELDS: usize = 64;

/// One value against one declared parameter: the check every caller of an action gets, exposed so
/// a draft can validate a single field without assembling a whole request.
pub fn check_value(parameter: &ParameterDescriptor, value: &Value) -> Result<(), Error> {
    let name = &parameter.name;
    match &parameter.kind {
        ParameterKind::Integer { min, max } => {
            let number = value
                .as_i64()
                .ok_or_else(|| Error::validation(format!("parameter {name} must be an integer")))?;
            if number < *min || number > *max {
                return Err(Error::validation(format!(
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
                .ok_or_else(|| Error::validation(format!("parameter {name} must be a number")))?;
            if number < *min || number > *max {
                return Err(Error::validation(format!(
                    "parameter {name} must be a number within {min}..={max}"
                )));
            }
        }
        ParameterKind::Enum { options } => {
            let text = value
                .as_str()
                .ok_or_else(|| Error::validation(format!("parameter {name} must be a string")))?;
            if !options.iter().any(|option| option == text) {
                return Err(Error::validation(format!(
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
                return Err(Error::validation(format!(
                    "parameter {name} must be three sRGB channels 0..=255"
                )));
            }
        }
        ParameterKind::Boolean => {
            if !value.is_boolean() {
                return Err(Error::validation(format!(
                    "parameter {name} must be a boolean"
                )));
            }
        }
        ParameterKind::Points {
            points_min,
            points_max,
        } => {
            use crate::path::{COORDINATE_MAX, COORDINATE_MIN};
            let Some(points) = value.as_array() else {
                return Err(Error::validation(format!(
                    "parameter {name} must be a path"
                )));
            };
            // The count is refused as a resource limit when it is over the bound and as a
            // validation error when it is under one, because the two are different facts: a path
            // longer than a build will store names the limit it exceeded, and a path too short to
            // be a gesture is a malformed request.
            if points.len() > *points_max {
                return Err(Error::resource_limit(format!(
                    "parameter {name} has {} positions; the limit is {points_max} positions \
                         posted per path",
                    points.len()
                )));
            }
            if points.len() < *points_min {
                return Err(Error::validation(format!(
                    "parameter {name} must hold at least {points_min} positions"
                )));
            }
            for (index, point) in points.iter().enumerate() {
                let Some(pair) = point.as_array().filter(|pair| pair.len() == 2) else {
                    return Err(Error::validation(format!(
                        "parameter {name} has a malformed position {index}"
                    )));
                };
                let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) else {
                    return Err(Error::validation(format!(
                        "parameter {name} has a malformed position {index}"
                    )));
                };
                if !x.is_finite()
                    || !y.is_finite()
                    || !(COORDINATE_MIN..=COORDINATE_MAX).contains(&x)
                    || !(COORDINATE_MIN..=COORDINATE_MAX).contains(&y)
                {
                    return Err(Error::validation(format!(
                        "parameter {name} position {index} must hold two numbers within \
                         {COORDINATE_MIN:.0}..={COORDINATE_MAX:.0}"
                    )));
                }
            }
        }
        ParameterKind::Artifact => {
            let valid = value
                .as_str()
                .is_some_and(|text| crate::ArtifactId::parse(text).is_ok());
            if !valid {
                return Err(Error::validation(format!(
                    "parameter {name} must be an artifact identity"
                )));
            }
        }
        ParameterKind::Curve {
            points_min,
            points_max,
            monotone,
            fixed_x,
        } => {
            let Some(points) = value.as_array() else {
                return Err(Error::validation(format!(
                    "parameter {name} must be a curve point list"
                )));
            };
            if points.len() < *points_min
                || points.len() > *points_max
                || fixed_x.as_ref().is_some_and(|xs| xs.len() != points.len())
            {
                return Err(Error::validation(format!(
                    "parameter {name} has an invalid curve point count"
                )));
            }
            let mut previous = None;
            for (index, point) in points.iter().enumerate() {
                let Some(pair) = point.as_array().filter(|pair| pair.len() == 2) else {
                    return Err(Error::validation(format!(
                        "parameter {name} has a malformed curve point {index}"
                    )));
                };
                let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) else {
                    return Err(Error::validation(format!(
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
                    return Err(Error::validation(format!(
                        "parameter {name} has an invalid curve point {index}"
                    )));
                }
                previous = Some((x, y));
            }
        }
        ParameterKind::String { max_length } => {
            let text = value
                .as_str()
                .ok_or_else(|| Error::validation(format!("parameter {name} must be a string")))?;
            // Characters, not bytes: the bound is what a person reads and types.
            if text.chars().count() > *max_length {
                return Err(Error::validation(format!(
                    "parameter {name} must be at most {max_length} characters"
                )));
            }
            if text.chars().any(char::is_control) {
                return Err(Error::validation(format!(
                    "parameter {name} must not contain control characters"
                )));
            }
        }
        ParameterKind::Settings => check_settings(
            name,
            value.as_object().ok_or_else(|| {
                Error::validation(format!("parameter {name} must be a settings object"))
            })?,
        )?,
        ParameterKind::SettingsOrigin => {
            crate::SettingsOrigin::read(value).map_err(|error| {
                Error::validation(format!("parameter {name}: {}", error.detail))
            })?;
        }
        ParameterKind::Endpoint { classes } => {
            let text = value
                .as_str()
                .filter(|text| text.len() <= MAX_ENDPOINT_BYTES)
                .ok_or_else(|| {
                    Error::validation(format!(
                        "parameter {name} must be a string of at most {MAX_ENDPOINT_BYTES} bytes"
                    ))
                })?;
            parse_endpoint(text, classes).map_err(|error| {
                Error::validation(format!("parameter {name}: {}", error.detail))
            })?;
        }
        // The one request that carries a secret's value is `module.settings.set-secret`, which
        // hands it to the secret store without it ever being a JSON value here.
        ParameterKind::Secret { .. } => {
            return Err(Error::validation(format!(
                "parameter {name} is a secret, which is never a plain value"
            )));
        }
        ParameterKind::Identity { of } => {
            if !value.as_str().is_some_and(|text| of.accepts(text)) {
                return Err(Error::validation(format!(
                    "parameter {name} must be {} identity",
                    of.with_article()
                )));
            }
        }
        ParameterKind::Text { max_bytes } => {
            let text = value
                .as_str()
                .ok_or_else(|| Error::validation(format!("parameter {name} must be a string")))?;
            if text.len() > *max_bytes {
                return Err(Error::validation(format!(
                    "parameter {name} must be at most {max_bytes} bytes"
                )));
            }
        }
        // Any value: the field's own type checks its shape when the request is parsed.
        ParameterKind::Json => {}
    }
    Ok(())
}

/// The shape of a settings set, and nothing more: 1 to 16 action identities, each giving a
/// non-empty object of at most 64 fields with valid parameter names. The one shape check: a
/// `settings` parameter gets it from the generic check, and the preset library's
/// [`crate::presets::validate_settings`] runs it under the `settings` parameter's name. Whether
/// each action is presettable and accepts each value is the host's check against the registry
/// ([`crate::ModuleRegistry::patch_action`]), because a descriptor cannot see other modules.
pub(crate) fn check_settings(name: &str, actions: &Map<String, Value>) -> Result<(), Error> {
    if actions.is_empty() || actions.len() > MAX_SETTINGS_ACTIONS {
        return Err(Error::validation(format!(
            "parameter {name} must name 1..={MAX_SETTINGS_ACTIONS} actions"
        )));
    }
    for (action, fields) in actions {
        if !valid_name(action) {
            return Err(Error::validation(format!(
                "parameter {name} names invalid action identity {action}"
            )));
        }
        let fields = fields.as_object().ok_or_else(|| {
            Error::validation(format!(
                "parameter {name} must give action {action} an object of fields"
            ))
        })?;
        if fields.len() > MAX_SETTINGS_FIELDS {
            return Err(Error::validation(format!(
                "parameter {name} gives action {action} more than {MAX_SETTINGS_FIELDS} fields"
            )));
        }
        if let Some(field) = fields.keys().find(|field| !valid_name(field)) {
            return Err(Error::validation(format!(
                "parameter {name} gives action {action} invalid field name {field}"
            )));
        }
    }
    Ok(())
}

/// Apply declared defaults and reject anything an action did not declare, so every caller of an
/// action gets the same structured validation error before the module sees the request.
///
/// A patch action is checked differently: the fields the caller sent are validated and returned as
/// sent, no declared default is applied and no required parameter is demanded, so the module
/// receives exactly the named fields and merges them over the state it already holds. The one
/// exception is a required [`ParameterKind::Identity`]: it says *which* state the patch is merged
/// over, so a patch still demands it.
pub fn check_parameters(
    action: &ActionDescriptor,
    input: &Value,
) -> Result<Map<String, Value>, Error> {
    check_declared_values(
        "action",
        &action.id,
        &action.parameters,
        action.patch,
        input,
    )
}

/// An action's checked parameters as the typed request its module reads. The generic check has
/// already refused an unknown or missing required field, a value of the wrong kind and one outside
/// its declared range or options, and filled every declared default, so a module decodes rather
/// than checks again, and keeps only the rules across fields that no one declaration can state.
pub(crate) fn decode_parameters<'a, T: Deserialize<'a>>(
    action_id: &str,
    parameters: &'a Map<String, Value>,
) -> Result<T, Error> {
    T::deserialize(parameters).map_err(|error| {
        Error::validation(format!(
            "invalid parameters for action {action_id}: {error}"
        ))
    })
}

/// Check the objects a request addresses before any value it sets: every field named is a declared
/// parameter with a valid value, and every required identity is named, exactly as a patch is
/// checked. A gesture's target is checked this way when it begins (`draft.begin`), so it is refused
/// in the words its commit would use, before the fields it drafts exist.
pub(crate) fn check_target(action: &ActionDescriptor, input: &Value) -> Result<(), Error> {
    check_declared_values("action", &action.id, &action.parameters, true, input).map(|_| ())
}

/// The generic check behind [`check_parameters`], for anything that declares parameters the way an
/// action does: `what` and `id` name it in every refusal, e.g. `task generate-proof-tint`.
pub(crate) fn check_declared_values(
    what: &str,
    id: &str,
    parameters: &[ParameterDescriptor],
    patch: bool,
    input: &Value,
) -> Result<Map<String, Value>, Error> {
    let object = checked_object(what, id, parameters, patch, input)?;
    Ok(declared_values(
        parameters,
        patch,
        object.cloned().unwrap_or_default(),
    ))
}

/// [`check_parameters`] of a request the caller gives up, as a commit and a draft's plan prepare
/// theirs: the same checks, refusing exactly as it refuses, with each value moved into the answer
/// rather than copied, so a brush stroke's path is not copied again.
pub(crate) fn take_parameters(
    action: &ActionDescriptor,
    input: Value,
) -> Result<Map<String, Value>, Error> {
    checked_object(
        "action",
        &action.id,
        &action.parameters,
        action.patch,
        &input,
    )?;
    let object = match input {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    Ok(declared_values(&action.parameters, action.patch, object))
}

/// Every check [`check_declared_values`] makes, in its order, copying nothing: the object the
/// values come from, `None` for `null`.
fn checked_object<'a>(
    what: &str,
    id: &str,
    parameters: &[ParameterDescriptor],
    patch: bool,
    input: &'a Value,
) -> Result<Option<&'a Map<String, Value>>, Error> {
    let declared = |name: &str| parameters.iter().find(|parameter| parameter.name == name);
    let object = match input {
        Value::Object(object) => Some(object),
        Value::Null => None,
        _ => {
            return Err(Error::validation(format!(
                "parameters of {what} {id} must be a JSON object"
            )));
        }
    };
    let named = || object.into_iter().flatten();
    let named_key = |name: &str| object.is_some_and(|object| object.contains_key(name));
    for (name, _) in named() {
        if declared(name).is_none() {
            return Err(Error::validation(format!(
                "unknown parameter {name} for {what} {id}"
            )));
        }
    }
    if patch {
        for (name, value) in named() {
            let parameter =
                declared(name).expect("every key was matched to a declared parameter above");
            check_value(parameter, value)?;
        }
        if let Some(missing) = parameters.iter().find(|parameter| {
            parameter.required && parameter.kind.is_identity() && !named_key(&parameter.name)
        }) {
            return Err(Error::validation(format!(
                "missing required parameter {} for {what} {id}",
                missing.name
            )));
        }
        return Ok(object);
    }
    for parameter in parameters {
        match (
            object.and_then(|object| object.get(&parameter.name)),
            &parameter.default,
        ) {
            (Some(value), _) => check_value(parameter, value)?,
            (None, None) if parameter.required => {
                return Err(Error::validation(format!(
                    "missing required parameter {} for {what} {id}",
                    parameter.name
                )));
            }
            (None, _) => {}
        }
    }
    Ok(object)
}

/// The checked values of an object [`checked_object`] accepted: a patch's fields as sent, otherwise
/// every declared parameter sent or defaulted.
fn declared_values(
    parameters: &[ParameterDescriptor],
    patch: bool,
    mut object: Map<String, Value>,
) -> Map<String, Value> {
    if patch {
        return object;
    }
    parameters
        .iter()
        .filter_map(|parameter| {
            object
                .remove(&parameter.name)
                .or_else(|| parameter.default.clone())
                .map(|value| (parameter.name.clone(), value))
        })
        .collect()
}
