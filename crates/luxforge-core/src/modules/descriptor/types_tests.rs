//! Tests of the descriptor shapes: how each serializes, what it defaults to and that it reads
//! back as written.
use super::testing::*;
use super::*;
use serde::Deserialize;
use serde_json::json;

/// Every declared stage keeps its wire name and is accepted by descriptor validation, so a
/// colour-stage module declares itself exactly as a pixel or geometry one does, and the order a
/// module takes among the layers of its stage travels with the effect.
#[test]
fn effect_stages_keep_their_serialized_names_and_validate() {
    for (stage, name, order) in [
        (EffectStage::Geometry, "geometry", 0),
        (EffectStage::Pixel, "pixel", 0),
        (EffectStage::Color, "color", 10),
        (EffectStage::Spatial, "spatial", 0),
        (EffectStage::Finish, "finish", 65535),
    ] {
        let effect = EffectDescriptor {
            order,
            ..EffectDescriptor::new("test.module.effect", stage)
        };
        assert_eq!(serde_json::to_value(stage).unwrap(), json!(name));
        // `order` is always serialized, so `module.list` reports it for every effect.
        assert_eq!(
            serde_json::to_value(&effect).unwrap(),
            json!({"id": "test.module.effect", "format": 1, "stage": name, "order": order})
        );
        assert_eq!(
            serde_json::from_value::<EffectDescriptor>(serde_json::to_value(&effect).unwrap())
                .unwrap(),
            effect
        );
        // An effect written without an order is the default earliest position of its stage.
        assert_eq!(
            serde_json::from_value::<EffectDescriptor>(
                json!({"id": "test.module.effect", "format": 1, "stage": name})
            )
            .unwrap()
            .order,
            0
        );
        let descriptor = ModuleDescriptor {
            effects: vec![effect],
            ..descriptor()
        };
        assert!(descriptor.validate().is_ok(), "{name}");
        assert_eq!(
            ModuleDescriptor::deserialize(&serde_json::to_value(&descriptor).unwrap()).unwrap(),
            descriptor,
            "{name} round-trips through JSON"
        );
    }
}

#[test]
fn number_parameters_and_the_crop_frame_canvas_keep_their_serialized_form() {
    assert_eq!(
        serde_json::to_value(number("angle", -45.0, 45.0)).unwrap(),
        json!({
            "name": "angle",
            "kind": "number",
            "min": -45.0,
            "max": 45.0,
            "required": true,
            "default": null,
            "unit": null,
            "step": null,
            "precision": null,
            "notes": "test",
        })
    );
    // The decimal hints a slider needs travel with the parameter and survive a round trip.
    let exposure = ParameterDescriptor {
        step: Some(0.01),
        precision: Some(2),
        unit: Some("EV".into()),
        ..number("exposure", -5.0, 5.0)
    };
    assert_eq!(
        serde_json::to_value(&exposure).unwrap(),
        json!({
            "name": "exposure",
            "kind": "number",
            "min": -5.0,
            "max": 5.0,
            "required": true,
            "default": null,
            "unit": "EV",
            "step": 0.01,
            "precision": 2,
            "notes": "test",
        })
    );
    assert_eq!(
        serde_json::from_value::<ParameterDescriptor>(serde_json::to_value(&exposure).unwrap())
            .unwrap(),
        exposure
    );
    // A descriptor written before the hints existed still reads, with neither hint declared.
    let without = serde_json::from_value::<ParameterDescriptor>(json!({
        "name": "exposure",
        "kind": "number",
        "min": -5.0,
        "max": 5.0,
        "required": true,
        "default": null,
        "unit": null,
        "notes": "test",
    }))
    .expect("the hints are optional");
    assert_eq!(without.step, None);
    assert_eq!(without.precision, None);
    let canvas = serde_json::to_value(frame_canvas("set-frame", "fit-frame")).unwrap();
    assert_eq!(
        canvas,
        json!({
            "kind": "crop-frame",
            "effect": "test.module.effect",
            "action": "set-frame",
            "angle": "angle",
            "x": "x",
            "y": "y",
            "width": "width",
            "height": "height",
            "fit_action": "fit-frame",
            "aspect": "aspect",
            "title": "Frame",
            "shortcut": "R",
        })
    );
    let descriptor = frame_descriptor();
    assert_eq!(
        ModuleDescriptor::deserialize(&serde_json::to_value(&descriptor).unwrap()).unwrap(),
        descriptor,
        "a crop-frame descriptor round-trips through JSON"
    );
}

/// A canvas mode's optional icon is a name from the vocabulary an action control's `icon` uses:
/// a declared one is validated and reported, and a mode without one serializes as before.
#[test]
fn a_canvas_mode_icon_is_a_validated_name_reported_only_when_declared() {
    let with_icon = |name: Option<&str>| {
        let mut descriptor = frame_descriptor();
        if let Some(CanvasInteraction::CropFrame { icon, .. }) = &mut descriptor.canvas {
            *icon = name.map(str::to_owned);
        }
        descriptor
    };
    let declared = with_icon(Some("crop"));
    assert!(declared.validate().is_ok(), "a kebab-case name is accepted");
    let canvas = declared.canvas.as_ref().expect("the frame canvas");
    assert_eq!(canvas.icon(), Some("crop"));
    assert_eq!(serde_json::to_value(canvas).unwrap()["icon"], json!("crop"));
    assert_eq!(
        ModuleDescriptor::deserialize(&serde_json::to_value(&declared).unwrap()).unwrap(),
        declared,
        "the icon round-trips through JSON"
    );
    let plain = with_icon(None);
    assert_eq!(
        plain.canvas.as_ref().and_then(CanvasInteraction::icon),
        None
    );
    assert!(
        serde_json::to_value(plain.canvas.as_ref().unwrap())
            .unwrap()
            .get("icon")
            .is_none(),
        "a mode without an icon reports none"
    );
    for bad in ["Bad_Icon", "crop frame", "", "-crop"] {
        let error = with_icon(Some(bad))
            .validate()
            .expect_err("an icon outside the name vocabulary is refused");
        assert!(
            error.to_string().contains("invalid icon name"),
            "{bad:?}: {error}"
        );
    }
    // Every kind of canvas mode checks it the same way.
    let mut pick = sample_descriptor();
    if let Some(CanvasInteraction::SampleApply { icon, .. }) = &mut pick.canvas {
        *icon = Some("Picker!".into());
    }
    assert!(pick.validate().is_err());
}

#[test]
fn new_control_kinds_and_hints_round_trip_and_validate() {
    let descriptor = controls_descriptor();
    descriptor.validate().unwrap();
    let mut stepped_integer = integer("count");
    stepped_integer.step = Some(1.0);
    stepped_integer.fine_step = Some(0.1);
    stepped_integer.precision = Some(0);
    assert!(with_hints(None, None, stepped_integer).validate().is_ok());
    let mut stepped_curve = descriptor.actions[0].parameter("curve").unwrap().clone();
    stepped_curve.step = Some(0.01);
    stepped_curve.fine_step = Some(0.001);
    assert!(with_hints(None, None, stepped_curve).validate().is_ok());
    let serialized = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(serialized["controls"][0]["kind"], "toggle");
    assert_eq!(serialized["controls"][1]["style"], "menu");
    assert_eq!(serialized["controls"][2]["rail"], "hue");
    assert_eq!(serialized["controls"][3]["sample_query"], "sample-curve");
    assert_eq!(serialized["controls"][5]["style"], "picker");
    assert_eq!(serialized["controls"][6]["collapsed"], true);
    assert_eq!(serialized["actions"][0]["parameters"][3]["soft_min"], -5.0);
    assert_eq!(
        ModuleDescriptor::deserialize(&serialized).unwrap(),
        descriptor
    );
    let minimal: Control = serde_json::from_value(
        json!({"kind":"number","action":"set-controls","parameter":"amount","label":"Amount"}),
    )
    .unwrap();
    assert!(matches!(
        minimal,
        Control::Number(NumberControl {
            style: NumberStyle::Slider,
            rail: None,
            ..
        })
    ));
}

#[test]
fn layout_round_trips_through_json_and_rejects_an_unknown_value() {
    let tabs = ModuleDescriptor {
        layout: ModuleLayout::Tabs,
        ..two_group_descriptor()
    };
    let serialized = serde_json::to_value(&tabs).unwrap();
    assert_eq!(serialized["layout"], json!("tabs"));
    assert_eq!(ModuleDescriptor::deserialize(&serialized).unwrap(), tabs);
    assert_eq!(
        serde_json::to_value(descriptor())
            .unwrap()
            .get("layout")
            .cloned(),
        Some(json!("stacked")),
        "an absent layout serializes as stacked, never omitted"
    );

    let mut malformed = serialized.clone();
    malformed["layout"] = json!("floating");
    assert!(
        ModuleDescriptor::deserialize(&malformed).is_err(),
        "an unknown layout value is rejected"
    );
}
