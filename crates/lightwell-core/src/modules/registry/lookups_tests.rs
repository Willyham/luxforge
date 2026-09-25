//! Tests of the registry's lookups.
use super::{tests::*, *};
use crate::{
    ActionDescriptor, BASIC_EFFECT, CROP_EFFECT, EFFECT_FORMAT, Layer, ORIENTATION_EFFECT,
    Orientation, PIXEL_EFFECT, RAW_EFFECT,
    modules::{ActionInput, Availability, ModuleDescriptor},
};
use serde_json::{Value, json};

/// A query identity is unique across the whole registry, so `query.<id>` can never resolve to
/// two providers; it is its own namespace, so it does not collide with an action of that name.
#[test]
fn one_query_identity_resolves_to_one_provider_across_the_registry() {
    let mut registry = ModuleRegistry::builtin();
    let (module, query) = registry
        .query("neutral-sample")
        .expect("the Basic module declares the neutral picker");
    assert_eq!(module.descriptor().id, "lightwell.basic");
    assert_eq!(query.id, "neutral-sample");
    assert!(!query.patch);
    assert!(
        registry.query("set-basic").is_none(),
        "queries are separate"
    );
    assert!(registry.action("neutral-sample").is_none());

    let with_query = |id: &str, effect: &str, action: &str, query: &str| {
        let mut descriptor = TestModule::new(id, effect, action, Availability::Available).0;
        descriptor.queries = vec![ActionDescriptor {
            id: query.into(),
            title: "Test query".into(),
            notes: "test".into(),
            summary: None,
            patch: false,
            parameters: Vec::new(),
        }];
        TestModule::from_descriptor(descriptor)
    };
    let error = registry
        .register(with_query(
            "test.one",
            "test.one.effect",
            "test-one",
            "neutral-sample",
        ))
        .expect_err("the Basic module already provides that query");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(
        error
            .detail
            .contains("query neutral-sample is already provided by lightwell.basic"),
        "{error}"
    );
    // An action may still be named after a query of another module: different namespaces.
    registry
        .register(with_query(
            "test.two",
            "test.two.effect",
            "neutral-sample",
            "test-query",
        ))
        .expect("an action named after another module's query is free");
    assert_eq!(
        registry
            .query("test-query")
            .expect("the new query")
            .0
            .descriptor()
            .id,
        "test.two"
    );
    assert_eq!(
        registry
            .query("neutral-sample")
            .expect("still the Basic module's")
            .0
            .descriptor()
            .id,
        "lightwell.basic"
    );
}

/// A descriptor carrying queries and a sample-apply canvas round-trips through JSON, so a
/// client reads exactly what the registry validated.
#[test]
fn queries_and_the_sample_apply_canvas_survive_a_json_round_trip() {
    let registry = ModuleRegistry::builtin();
    let basic = registry
        .effect(BASIC_EFFECT)
        .expect("the Basic module")
        .0
        .descriptor();
    let encoded = serde_json::to_value(basic).expect("a serializable descriptor");
    assert_eq!(encoded["queries"][0]["id"], json!("neutral-sample"));
    assert_eq!(
        encoded["canvas"],
        json!({
            "kind": "sample-apply",
            "query": "neutral-sample",
            "x": "x",
            "y": "y",
            "action": "set-basic",
            "title": "Neutral picker",
            "shortcut": "W",
        })
    );
    assert_eq!(
        &ModuleDescriptor::parse(&encoded).expect("a valid descriptor"),
        basic
    );
    // A descriptor written before queries existed still reads, with none declared.
    let mut without = encoded.clone();
    let object = without.as_object_mut().expect("an object");
    object.remove("queries");
    object.insert("canvas".into(), Value::Null);
    assert!(
        serde_json::from_value::<ModuleDescriptor>(without)
            .expect("queries are optional")
            .queries
            .is_empty()
    );
}

/// The generic check in front of a patch action: the module is handed exactly the fields the
/// caller named, with no declared default filled in and no required parameter demanded, and it
/// merges them over the state it already holds.
#[test]
fn a_patch_action_is_checked_field_by_field_and_fills_no_defaults() {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(PatchModule::shared())
        .expect("a patch descriptor is valid");
    let (module, action) = registry.action(PATCH_ACTION).expect("the patch action");
    assert!(action.patch);
    let checked = crate::check_parameters(action, &json!({"red": 12})).expect("one field");
    assert_eq!(
        checked,
        json!({"red": 12}).as_object().unwrap().clone(),
        "only the field that was sent, exactly as it was sent"
    );
    assert_eq!(
        crate::check_parameters(action, &json!({}))
            .expect("an empty patch")
            .len(),
        0,
        "a patch fills no declared default"
    );
    for (case, sent, fragment) in [
        (
            "unknown field",
            json!({"blue": 1}),
            "unknown parameter blue",
        ),
        (
            "out of range",
            json!({"red": 300}),
            "parameter red must be a number within 0..=255",
        ),
        (
            "wrong kind",
            json!({"red": "12"}),
            "parameter red must be a number",
        ),
    ] {
        let error = crate::check_parameters(action, &sent).expect_err(case);
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert!(error.detail.contains(fragment), "{case}: {error}");
    }
    // The module merges what it was handed over what it already stores.
    let input = ActionInput {
        action_id: PATCH_ACTION.into(),
        parameters: checked,
    };
    let parsed = module.parse(PATCH_ACTION, &input.parameters).unwrap();
    assert_eq!(parsed.parameters, input.parameters);
    assert_eq!(
        module
            .values(PATCH_EFFECT, EFFECT_FORMAT, &json!({"red": 12.0}))
            .unwrap(),
        json!({"red": 12.0, "green": 0.0})
            .as_object()
            .unwrap()
            .clone(),
        "a stored layer reports every parameter it represents, neutral fields included"
    );
    assert_eq!(
        module.label(&parsed).as_deref(),
        Some("Patch red 12"),
        "one changed field labels its own entry"
    );
    assert_eq!(
        module.label(&ActionInput {
            action_id: PATCH_ACTION.into(),
            parameters: json!({"red": 1, "green": 2}).as_object().unwrap().clone(),
        }),
        None,
        "a module that has nothing to add leaves the label to the host"
    );
}

#[test]
fn built_in_modules_describe_their_stored_layers() {
    let registry = ModuleRegistry::builtin();
    let described = |layer: &Layer| -> String {
        let (module, _) = registry.effect(&layer.effect_id).expect("a provider");
        module
            .describe_layer(&layer.effect_id, layer.effect_format, &layer.payload)
            .expect("a stored payload")
    };
    assert_eq!(
        described(&Layer::pixel(3, 4, [1, 2, 3])),
        "Pixel 3, 4 → 1,2,3"
    );
    // An orientation layer holds a composed state, so its row names the orientation it is in,
    // not the actions that reached it. All eight are named and the neutral one says so.
    let orientation = |mirror: bool, turns: u8| -> String {
        described(&Layer::orientation(Orientation { mirror, turns }))
    };
    assert_eq!(orientation(false, 0), "Upright");
    assert_eq!(orientation(false, 1), "Rotate right");
    assert_eq!(orientation(false, 2), "Rotate 180°");
    assert_eq!(orientation(false, 3), "Rotate left");
    assert_eq!(orientation(true, 0), "Mirror horizontal");
    assert_eq!(orientation(true, 1), "Mirror horizontal · Rotate right");
    assert_eq!(orientation(true, 2), "Flip vertical");
    assert_eq!(orientation(true, 3), "Mirror horizontal · Rotate left");
    assert_eq!(
        described(&Layer::crop(crate::CropPayload::NEUTRAL)),
        "Whole image"
    );
}

/// The three effects the design names declare the flag and nothing else does, and the field
/// follows the module that declares one: `edit.set-basic` accepts a target, `edit.set-crop` does
/// not, and a client reads both from the registry rather than from a hand-maintained list.
#[test]
fn exactly_the_three_delivered_effects_are_maskable() {
    let registry = ModuleRegistry::builtin();
    for effect in [BASIC_EFFECT, crate::MIXER_EFFECT, crate::PRESENCE_EFFECT] {
        assert!(registry.effect_maskable(effect), "{effect}");
    }
    for effect in [
        PIXEL_EFFECT,
        RAW_EFFECT,
        CROP_EFFECT,
        ORIENTATION_EFFECT,
        crate::VIGNETTE_EFFECT,
        "test.absent",
    ] {
        assert!(!registry.effect_maskable(effect), "{effect}");
    }
    for action in [
        "set-basic",
        "reset-basic",
        "set-mixer",
        "reset-mixer",
        "set-presence",
        "reset-presence",
    ] {
        assert!(registry.action_accepts_mask(action), "{action}");
    }
    for action in [
        "set-pixel",
        "set-crop",
        "rotate-left",
        "set-vignette",
        "set-raw",
        "no-such-action",
    ] {
        assert!(!registry.action_accepts_mask(action), "{action}");
    }
}
