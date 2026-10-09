//! The declared `brush-mask` canvas, an effect's operation list and the correction identity: what
//! registration accepts, what it refuses by name, and what discovery publishes. The consumer is
//! the test-only repair brush ([`crate::modules::brush_consumer`]), so nothing here is a
//! production tool.
use super::*;
use crate::modules::brush_consumer::{self, ADD, DELETE, LIST, OPERATION, SOURCES, UPDATE};
use crate::{CorrectionId, ModuleRegistry};
use serde_json::json;

fn refused(descriptor: &ModuleDescriptor) -> String {
    descriptor
        .validate()
        .expect_err("the descriptor is refused")
        .detail
}

fn with_canvas(change: impl FnOnce(&mut CanvasInteraction)) -> ModuleDescriptor {
    let mut descriptor = brush_consumer::descriptor();
    change(descriptor.canvas.as_mut().expect("the brush canvas"));
    descriptor
}

fn with_action(id: &str, change: impl FnOnce(&mut ActionDescriptor)) -> ModuleDescriptor {
    let mut descriptor = brush_consumer::descriptor();
    change(
        descriptor
            .actions
            .iter_mut()
            .find(|action| action.id == id)
            .expect("a declared action"),
    );
    descriptor
}

fn parameter<'a>(action: &'a mut ActionDescriptor, name: &str) -> &'a mut ParameterDescriptor {
    action
        .parameters
        .iter_mut()
        .find(|parameter| parameter.name == name)
        .expect("a declared parameter")
}

/// The brush's own fields, to change one binding at a time.
macro_rules! brush {
    ($canvas:expr, $field:ident) => {
        match $canvas {
            CanvasInteraction::BrushMask { $field, .. } => $field,
            _ => panic!("a brush canvas"),
        }
    };
}

fn correction() -> ParameterDescriptor {
    ParameterDescriptor::new(
        "operation",
        ParameterKind::Identity {
            of: IdentityKind::Correction,
        },
    )
    .required(true)
    .notes("test")
}

#[test]
fn a_declared_brush_registers_and_is_published_with_its_bindings_and_operation_list() {
    let descriptor = brush_consumer::descriptor();
    descriptor.validate().expect("the consumer is valid");
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(brush_consumer::RepairBrushModule::shared())
        .expect("registration accepts the consumer");
    let published = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(
        published["canvas"],
        json!({
            "kind": "brush-mask",
            "action": ADD,
            "points": "points",
            "radius": "radius",
            "feather": "feather",
            "opacity": "opacity",
            "mode": "mode",
            "source": {"x": "source-x", "y": "source-y", "query": SOURCES},
            "edit": {"action": UPDATE, "operation": OPERATION, "list": LIST},
            "title": "Repair",
            "shortcut": null,
            "icon": "heal",
        })
    );
    assert_eq!(
        published["effects"][0]["operations"],
        json!({"identity": "correction", "max": brush_consumer::LIMIT})
    );
    // An effect without a list describes itself as it did before lists existed.
    let basic =
        serde_json::to_value(EffectDescriptor::new("x.effect", EffectStage::Color)).unwrap();
    assert!(basic.get("operations").is_none(), "{basic}");
    // The canvas round-trips exactly as module.list publishes it.
    let read: ModuleDescriptor = serde_json::from_value(published.clone()).unwrap();
    assert_eq!(read, descriptor);
    // The update's identity parameter is published as an identity of a correction, and the path's
    // bound beside the points parameter, so a client posts both without desktop code.
    let schema = crate::api::schemas(&registry);
    let update = &schema["methods"][format!("edit.{UPDATE}")];
    let operation = update["parameters"]
        .as_array()
        .expect("parameters")
        .iter()
        .find(|parameter| parameter["name"] == OPERATION)
        .expect("the operation parameter is published");
    assert_eq!(operation["kind"], json!("identity"));
    assert_eq!(operation["of"], json!("correction"));
    assert_eq!(operation["required"], json!(true));
    let points = schema["methods"][format!("edit.{ADD}")]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "points")
        .cloned()
        .expect("the path parameter");
    assert_eq!(points["kind"], json!("points"));
    assert_eq!(
        points["points_max"],
        json!(crate::path::POSTED_POINTS_PER_STROKE)
    );
}

#[test]
fn a_brush_without_settings_source_or_edit_is_still_a_brush() {
    let descriptor = with_canvas(|canvas| {
        *brush!(canvas, feather) = None;
        *brush!(canvas, opacity) = None;
        *brush!(canvas, mode) = None;
        *brush!(canvas, source) = None;
        *brush!(canvas, edit) = None;
    });
    descriptor
        .validate()
        .expect("settings, source and edit are optional");
}

#[test]
fn malformed_brush_bindings_are_refused_by_name() {
    let cases: Vec<(ModuleDescriptor, &str)> = vec![
        (
            with_canvas(|canvas| *brush!(canvas, action) = "missing".into()),
            "references undeclared action missing",
        ),
        (
            with_canvas(|canvas| *brush!(canvas, points) = "radius".into()),
            "binds parameter radius twice",
        ),
        (
            with_canvas(|canvas| *brush!(canvas, feather) = Some("opacity".into())),
            "binds parameter opacity twice",
        ),
        (
            with_canvas(|canvas| *brush!(canvas, radius) = "nothing".into()),
            "action add-repair has no parameter nothing",
        ),
        (
            with_canvas(|canvas| *brush!(canvas, mode) = Some("feather".into())),
            "binds parameter feather twice",
        ),
        (
            with_canvas(|canvas| {
                brush!(canvas, source).as_mut().unwrap().query = "missing".into();
            }),
            "references undeclared query missing",
        ),
        (
            with_canvas(|canvas| {
                brush!(canvas, edit).as_mut().unwrap().list = "missing".into();
            }),
            "references undeclared query missing",
        ),
        (
            with_canvas(|canvas| brush!(canvas, edit).as_mut().unwrap().action = DELETE.into()),
            "brush-mask edit delete-repair is not a field patch",
        ),
        (
            with_canvas(|canvas| brush!(canvas, edit).as_mut().unwrap().action = ADD.into()),
            "brush-mask edit add-repair is not a field patch",
        ),
        (
            with_canvas(|canvas| {
                brush!(canvas, edit).as_mut().unwrap().operation = "radius".into();
            }),
            "does not require parameter radius as the identity of an operation list",
        ),
        (
            with_action(ADD, |action| {
                *parameter(action, "points") = ParameterDescriptor::number("points", 0.0, 1.0);
            }),
            "canvas parameter points of action add-repair is not a points path",
        ),
        (
            with_action(ADD, |action| {
                *parameter(action, "radius") = ParameterDescriptor::integer("radius", 1, 100);
            }),
            "canvas parameter radius of action add-repair is not a number",
        ),
        (
            with_action(ADD, |action| {
                *parameter(action, "opacity") = ParameterDescriptor::boolean("opacity");
            }),
            "canvas parameter opacity of action add-repair is not a number or an integer",
        ),
        (
            with_action(ADD, |action| {
                *parameter(action, "mode") = ParameterDescriptor::integer("mode", 0, 1);
            }),
            "canvas parameter mode of action add-repair is not an enum",
        ),
        (
            with_action(ADD, |action| {
                *parameter(action, "source-x") = ParameterDescriptor::integer("source-x", 0, 1);
            }),
            "canvas parameter source-x of action add-repair is not a number",
        ),
        (
            with_action(ADD, |action| action.patch = true),
            "brush-mask action add-repair is a field patch",
        ),
        (
            with_action(ADD, |action| action.preset = true),
            "brush-mask action add-repair paints one photograph's path and must declare preset: false",
        ),
        (
            with_action(ADD, |action| action.parameters.push(correction())),
            "brush-mask action add-repair declares identity operation; a new path names no operation",
        ),
        (
            with_action(UPDATE, |action| {
                parameter(action, OPERATION).required = false
            }),
            "does not require parameter operation as the identity of an operation list",
        ),
        (
            with_action(UPDATE, |action| {
                *parameter(action, "radius") = ParameterDescriptor::number("radius", 0.0, 0.9);
            }),
            "brush-mask parameter radius of edit update-repair is not declared as action add-repair declares it",
        ),
        (
            with_action(UPDATE, |action| {
                *parameter(action, "opacity") = ParameterDescriptor::number("opacity", 0.0, 1.0);
            }),
            "brush-mask parameter opacity of edit update-repair is not declared as action add-repair declares it",
        ),
        (
            with_action(UPDATE, |action| {
                action
                    .parameters
                    .retain(|parameter| parameter.name != "source-x");
            }),
            "action update-repair has no parameter source-x",
        ),
        (
            with_canvas(|canvas| brush!(canvas, title).clear()),
            "declares a canvas interaction without a title",
        ),
    ];
    for (descriptor, expected) in cases {
        let detail = refused(&descriptor);
        assert!(
            detail.contains(expected),
            "{detail} does not say {expected}"
        );
    }
    // A brush canvas is not a pick, so a picker control cannot bind to it.
    let mut picker = brush_consumer::descriptor();
    picker.controls.push(Control::picker("Repair").into());
    let detail = refused(&picker);
    assert!(
        detail.contains("picker control without a point-pick or sample-apply canvas"),
        "{detail}"
    );
}

#[test]
fn an_operation_list_is_one_single_effects_bounded_correction_list() {
    let with_effect = |change: &dyn Fn(&mut EffectDescriptor)| {
        let mut descriptor = brush_consumer::descriptor();
        change(&mut descriptor.effects[0]);
        descriptor
    };
    for (change, expected) in [
        (
            &(|effect: &mut EffectDescriptor| effect.single = false)
                as &dyn Fn(&mut EffectDescriptor),
            "an operation list is a single effect's",
        ),
        (
            &|effect: &mut EffectDescriptor| effect.operations.as_mut().unwrap().max = 0,
            "max: 0",
        ),
        (
            &|effect: &mut EffectDescriptor| {
                effect.operations.as_mut().unwrap().max = MAX_OPERATIONS + 1
            },
            "max: 1001",
        ),
        (
            &|effect: &mut EffectDescriptor| {
                effect.operations.as_mut().unwrap().identity = IdentityKind::Mask
            },
            "identity: mask",
        ),
    ] {
        let detail = refused(&with_effect(change));
        assert!(
            detail.contains(expected),
            "{detail} does not say {expected}"
        );
    }
    with_effect(&|effect| effect.operations.as_mut().unwrap().max = MAX_OPERATIONS)
        .validate()
        .expect("the largest list");
    // Without the list, nothing the module declares may name a correction: neither its actions
    // nor its queries, and the refusal names the parameter.
    let detail = refused(&with_effect(&|effect| effect.operations = None));
    assert!(
        detail.contains(
            "parameter operation of action update-repair names a correction, but no effect of \
             module test.repairbrush declares an operation list of that identity"
        ),
        "{detail}"
    );
}

#[test]
fn only_the_correction_identity_is_admitted_on_module_actions_and_queries() {
    let module =
        |actions: Vec<ActionDescriptor>, queries: Vec<ActionDescriptor>| ModuleDescriptor {
            actions,
            queries,
            canvas: None,
            ..brush_consumer::descriptor()
        };
    let action = |parameters: Vec<ParameterDescriptor>, preset: bool| ActionDescriptor {
        parameters,
        preset,
        ..ActionDescriptor::new("edit-operation", "Edit operation", "test")
    };
    module(vec![action(vec![correction()], false)], Vec::new())
        .validate()
        .expect("an action may name a correction");
    // A query may name one too, under any name: `query.correction-sources` asks about one.
    module(
        Vec::new(),
        vec![ActionDescriptor {
            parameters: vec![ParameterDescriptor {
                name: "about".into(),
                ..correction()
            }],
            ..ActionDescriptor::new("sources", "Sources", "test")
        }],
    )
    .validate()
    .expect("a query may name a correction");
    // Every other host-only kind stays the host's: another identity, text and json.
    for (kind, name) in [
        (
            ParameterKind::Identity {
                of: IdentityKind::Mask,
            },
            "identity",
        ),
        (
            ParameterKind::Identity {
                of: IdentityKind::Stroke,
            },
            "identity",
        ),
        (ParameterKind::Text { max_bytes: 64 }, "text"),
        (ParameterKind::Json, "json"),
    ] {
        let detail = refused(&module(
            vec![action(
                vec![ParameterDescriptor::new("target", kind).notes("test")],
                false,
            )],
            Vec::new(),
        ));
        assert!(
            detail.contains(&format!(
                "declares kind {name}, which only a host command declares"
            )),
            "{detail}"
        );
    }
    // A correction is one photograph's, so an action naming one is never presettable.
    let detail = refused(&module(vec![action(vec![correction()], true)], Vec::new()));
    assert!(
        detail.contains(
            "action edit-operation names a correction through parameter operation and must \
             declare preset: false"
        ),
        "{detail}"
    );
    // A task is not an action on stored state: it may not name one.
    let detail = check_parameter_declarations("task", "test-task", &[correction()])
        .expect_err("a task names no correction")
        .detail;
    assert!(
        detail.contains("declares kind identity, which only a host command declares"),
        "{detail}"
    );
}

#[test]
fn a_correction_identity_is_its_number_spelled_without_leading_zeros() {
    let five = CorrectionId::parse("correction-5").expect("a correction");
    assert_eq!(five.number(), 5);
    assert_eq!(five.to_string(), "correction-5");
    assert_eq!(serde_json::to_value(five).unwrap(), json!("correction-5"));
    assert_eq!(
        serde_json::from_value::<CorrectionId>(json!("correction-4294967295"))
            .unwrap()
            .number(),
        u32::MAX
    );
    for valid in ["correction-1", "correction-10", "correction-4294967295"] {
        assert!(IdentityKind::Correction.accepts(valid), "{valid}");
    }
    for invalid in [
        "correction-0",
        "correction-05",
        "correction-",
        "correction-4294967296",
        "correction-+5",
        "correction-5 ",
        "correction--5",
        "Correction-5",
        "mask-5",
    ] {
        assert!(!IdentityKind::Correction.accepts(invalid), "{invalid}");
        assert!(CorrectionId::parse(invalid).is_err(), "{invalid}");
    }
    assert_eq!(
        serde_json::to_value(IdentityKind::Correction).unwrap(),
        json!("correction")
    );
    assert_eq!(IdentityKind::Correction.with_article(), "a correction");
    check_value(&correction(), &json!("correction-7")).expect("a correction identity");
    assert!(check_value(&correction(), &json!(crate::MaskId::new().as_str())).is_err());
    assert!(check_value(&correction(), &json!(7)).is_err());
}

#[test]
fn the_consumer_drafts_by_its_declared_identity_like_any_other_action() {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(brush_consumer::RepairBrushModule::shared())
        .unwrap();
    let (module, update) = registry.action(UPDATE).expect("the update is registered");
    assert_eq!(module.descriptor().id, brush_consumer::MODULE);
    let target = crate::declared_target(update, |kind| {
        (kind == IdentityKind::Correction).then(|| "correction-3".to_owned())
    });
    assert_eq!(
        target,
        crate::DraftTarget::from([(OPERATION.to_owned(), "correction-3".to_owned())])
    );
}
