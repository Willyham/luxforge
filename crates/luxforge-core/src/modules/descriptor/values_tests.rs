//! Tests of the checks a request's values meet against their declarations.
use super::testing::*;
use super::*;
use crate::ErrorKind;
use serde_json::{Value, json};

/// A string is bounded in characters, not bytes, refuses control characters and leaves
/// emptiness to the module; it serializes flat like every other kind.
#[test]
fn string_parameters_bound_characters_and_refuse_control_characters() {
    let parameter = string("name", 4, true);
    assert_eq!(
        serde_json::to_value(string("name", 128, true)).unwrap(),
        json!({
            "name": "name",
            "kind": "string",
            "max_length": 128,
            "required": true,
            "default": null,
            "unit": null,
            "step": null,
            "precision": null,
            "notes": "test",
        })
    );
    assert_eq!(
        serde_json::from_value::<ParameterDescriptor>(serde_json::to_value(&parameter).unwrap())
            .unwrap(),
        parameter
    );
    assert!(
        with_hints(None, None, string("name", 256, true))
            .validate()
            .is_ok(),
        "256 characters is the longest bound"
    );
    for accepted in ["", "abcd", "ééé", "日本語だ", "a b "] {
        check_value(&parameter, &json!(accepted))
            .unwrap_or_else(|error| panic!("{accepted:?}: {error}"));
    }
    assert_eq!("日本語だ".len(), 12, "four characters are twelve bytes");
    for (case, value, fragment) in [
        ("a number", json!(4), "parameter name must be a string"),
        ("null", Value::Null, "parameter name must be a string"),
        (
            "an array",
            json!(["abcd"]),
            "parameter name must be a string",
        ),
        (
            "five characters",
            json!("abcde"),
            "parameter name must be at most 4 characters",
        ),
        (
            "five two-byte characters",
            json!("ééééé"),
            "parameter name must be at most 4 characters",
        ),
        (
            "a newline",
            json!("a\nb"),
            "parameter name must not contain control characters",
        ),
        (
            "a bell",
            json!("\u{7}"),
            "parameter name must not contain control characters",
        ),
        (
            "a C1 control",
            json!("a\u{85}"),
            "parameter name must not contain control characters",
        ),
    ] {
        let error = check_value(&parameter, &value).expect_err(case);
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert_eq!(error.detail, fragment, "{case}");
    }
}

/// An identity is validated by its own type's parser and nothing else, serializes flat with the
/// object it names, and stays required on a patch, which demands no other field.
#[test]
fn identity_parameters_accept_only_their_own_kind_and_stay_required_on_a_patch() {
    let mask = ParameterDescriptor::identity("mask", IdentityKind::Mask).required(true);
    let component =
        ParameterDescriptor::identity("component", IdentityKind::Component).required(true);
    let stroke = ParameterDescriptor::identity("stroke", IdentityKind::Stroke);
    assert_eq!(
        serde_json::to_value(&mask).unwrap(),
        json!({
            "name": "mask",
            "kind": "identity",
            "of": "mask",
            "required": true,
            "default": null,
            "unit": null,
            "step": null,
            "precision": null,
            "notes": "",
        })
    );
    assert_eq!(
        serde_json::from_value::<ParameterDescriptor>(serde_json::to_value(&stroke).unwrap())
            .unwrap(),
        stroke
    );
    let mask_id = crate::MaskId::new();
    let component_id = crate::ComponentId::new();
    let stroke_id = "0".repeat(32);
    check_value(&mask, &json!(mask_id.as_str())).unwrap();
    check_value(&component, &json!(component_id.as_str())).unwrap();
    check_value(&stroke, &json!(stroke_id)).unwrap();
    for (parameter, value, detail) in [
        (
            &mask,
            json!(component_id.as_str()),
            "parameter mask must be a mask identity",
        ),
        (&mask, json!(7), "parameter mask must be a mask identity"),
        (
            &component,
            json!(mask_id.as_str()),
            "parameter component must be a component identity",
        ),
        (
            &stroke,
            json!("not-hex"),
            "parameter stroke must be a stroke identity",
        ),
    ] {
        let error = check_value(parameter, &value).expect_err(detail);
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(error.detail, detail);
    }
    // Only the host declares an identity, for its own objects: a module's action refuses one.
    assert_eq!(
        with_hints(None, None, mask.clone())
            .validate()
            .unwrap_err()
            .detail,
        "parameter mask of action set-thing declares kind identity, which only a host command declares"
    );
    // And, like every other non-numeric kind, it carries no numeric hints.
    assert!(check_declaration(&mask.clone().step(1.0)).is_err());
    // A patch demands its required identities and nothing else.
    let patch = [
        mask.clone(),
        component.clone(),
        ParameterDescriptor::number("x0", 0.0, 1.0).required(true),
    ];
    let checked = check_declared_values(
        "action",
        "patch",
        &patch,
        true,
        &json!({"mask": mask_id.as_str(), "component": component_id.as_str()}),
    )
    .unwrap();
    assert_eq!(
        checked.len(),
        2,
        "no default and no other field is demanded"
    );
    assert_eq!(
        check_declared_values(
            "action",
            "patch",
            &patch,
            true,
            &json!({"mask": mask_id.as_str(), "x0": 0.5}),
        )
        .unwrap_err()
        .detail,
        "missing required parameter component for action patch"
    );
}

/// The generic settings check validates the shape of a set and nothing else: the actions and
/// fields it names are the host's to check against the registry.
#[test]
fn settings_parameters_check_only_the_shape_of_a_settings_set() {
    let parameter = settings("settings");
    assert_eq!(
        serde_json::to_value(&parameter).unwrap()["kind"],
        json!("settings")
    );
    assert!(
        serde_json::to_value(&parameter)
            .unwrap()
            .get("max_length")
            .is_none(),
        "a settings kind carries nothing but its tag"
    );
    assert_eq!(
        serde_json::from_value::<ParameterDescriptor>(serde_json::to_value(&parameter).unwrap())
            .unwrap(),
        parameter
    );
    let fields = |count: usize| -> Value {
        Value::Object(
            (0..count)
                .map(|index| (format!("field-{index}"), json!(index)))
                .collect(),
        )
    };
    let actions = |count: usize| -> Value {
        Value::Object(
            (0..count)
                .map(|index| (format!("set-thing-{index}"), fields(MAX_SETTINGS_FIELDS)))
                .collect(),
        )
    };
    for (case, accepted) in [
        ("one action", json!({"set-basic": {"exposure": 0.35}})),
        (
            "an action no module declares, which is the host's to refuse",
            json!({"set-anything": {"any-field": "any value"}}),
        ),
        ("the most actions and fields", actions(MAX_SETTINGS_ACTIONS)),
    ] {
        check_value(&parameter, &accepted).unwrap_or_else(|error| panic!("{case}: {error}"));
    }
    for (case, value, fragment) in [
        (
            "an array",
            json!([]),
            "parameter settings must be a settings object",
        ),
        (
            "a string",
            json!("set-basic"),
            "parameter settings must be a settings object",
        ),
        (
            "null",
            Value::Null,
            "parameter settings must be a settings object",
        ),
        (
            "no actions",
            json!({}),
            "parameter settings must name 1..=16 actions",
        ),
        (
            "seventeen actions",
            actions(MAX_SETTINGS_ACTIONS + 1),
            "parameter settings must name 1..=16 actions",
        ),
        (
            "a dotted action identity",
            json!({"set.basic": {"exposure": 1}}),
            "parameter settings names invalid action identity set.basic",
        ),
        (
            "an upper-case action identity",
            json!({"Set-Basic": {"exposure": 1}}),
            "parameter settings names invalid action identity Set-Basic",
        ),
        (
            "an empty field object",
            json!({"set-basic": {}}),
            "parameter settings must give action set-basic a non-empty object of fields",
        ),
        (
            "fields that are not an object",
            json!({"set-basic": 1}),
            "parameter settings must give action set-basic a non-empty object of fields",
        ),
        (
            "sixty-five fields",
            json!({ "set-basic": fields(MAX_SETTINGS_FIELDS + 1) }),
            "parameter settings gives action set-basic more than 64 fields",
        ),
        (
            "an invalid field name",
            json!({"set-basic": {"Exposure": 1}}),
            "parameter settings gives action set-basic invalid field name Exposure",
        ),
    ] {
        let error = check_value(&parameter, &value).expect_err(case);
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert_eq!(error.detail, fragment, "{case}");
    }
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
