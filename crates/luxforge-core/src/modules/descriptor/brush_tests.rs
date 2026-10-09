//! The declared `brush-mask` canvas and the operation identity: what registration accepts, what it
//! refuses by name, and what discovery publishes. The consumer is the test-only repair brush
//! ([`crate::modules::brush_consumer`]), so nothing here is a production tool.
use super::*;
use crate::modules::brush_consumer::{self, ADD, DELETE, UPDATE};
use crate::{ModuleRegistry, OperationId};
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

fn bound(canvas: &mut CanvasInteraction) -> BrushFields<'_> {
    let CanvasInteraction::BrushMask {
        points,
        radius,
        source_x,
        source_y,
        update,
        action,
        ..
    } = canvas
    else {
        panic!("a brush canvas");
    };
    BrushFields {
        action,
        points,
        radius,
        source_x,
        source_y,
        update,
    }
}

struct BrushFields<'a> {
    action: &'a mut String,
    points: &'a mut String,
    radius: &'a mut String,
    source_x: &'a mut Option<String>,
    source_y: &'a mut Option<String>,
    update: &'a mut Option<String>,
}

#[test]
fn a_declared_brush_registers_and_is_published_with_its_bindings_and_operation_identity() {
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
            "source_x": "source-x",
            "source_y": "source-y",
            "update": UPDATE,
            "title": "Repair",
            "shortcut": null,
            "icon": "heal",
        })
    );
    // The canvas round-trips exactly as module.list publishes it.
    let read: ModuleDescriptor = serde_json::from_value(published.clone()).unwrap();
    assert_eq!(read, descriptor);
    // The update's operation parameter is published as an identity of an operation, with the
    // path's bound beside the points parameter, so a client posts both without desktop code.
    let schema = crate::api::schemas(&registry);
    let update = &schema["methods"][format!("edit.{UPDATE}")];
    let parameters = update["parameters"].as_array().expect("parameters");
    let operation = parameters
        .iter()
        .find(|parameter| parameter["name"] == "operation")
        .expect("the operation parameter is published");
    assert_eq!(operation["kind"], json!("identity"));
    assert_eq!(operation["of"], json!("operation"));
    assert_eq!(operation["required"], json!(true));
    let add = &schema["methods"][format!("edit.{ADD}")];
    let points = add["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "points")
        .expect("the path parameter");
    assert_eq!(points["kind"], json!("points"));
    assert_eq!(
        points["points_max"],
        json!(crate::path::POINTS_PER_STROKE),
        "{points}"
    );
}

#[test]
fn a_brush_without_a_source_pair_or_update_is_still_a_brush() {
    let descriptor = with_canvas(|canvas| {
        let fields = bound(canvas);
        *fields.source_x = None;
        *fields.source_y = None;
        *fields.update = None;
    });
    descriptor
        .validate()
        .expect("source and update are optional");
}

#[test]
fn malformed_brush_bindings_are_refused_by_name() {
    let cases: Vec<(ModuleDescriptor, &str)> = vec![
        (
            with_canvas(|canvas| *bound(canvas).action = "missing".into()),
            "references undeclared action missing",
        ),
        (
            with_canvas(|canvas| *bound(canvas).points = "radius".into()),
            "binds parameter radius twice",
        ),
        (
            with_canvas(|canvas| *bound(canvas).points = "feather".into()),
            "binds parameter feather twice",
        ),
        (
            with_canvas(|canvas| *bound(canvas).radius = "nothing".into()),
            "action add-repair has no parameter nothing",
        ),
        (
            with_canvas(|canvas| *bound(canvas).source_y = None),
            "binds one source coordinate without the other",
        ),
        (
            with_canvas(|canvas| *bound(canvas).update = Some(DELETE.into())),
            "brush-mask update delete-repair is not a field patch",
        ),
        (
            with_canvas(|canvas| *bound(canvas).update = Some(ADD.into())),
            "brush-mask update add-repair is not a field patch",
        ),
        (
            with_action(ADD, |action| {
                action.parameters[0] = ParameterDescriptor::number("points", 0.0, 1.0);
            }),
            "canvas parameter points of action add-repair is not a points path",
        ),
        (
            with_action(ADD, |action| {
                action.parameters[1] = ParameterDescriptor::integer("radius", 1, 100);
            }),
            "canvas parameter radius of action add-repair is not a number",
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
            with_action(UPDATE, |action| {
                action.parameters.remove(0);
            }),
            "brush-mask update update-repair does not require an operation identity named operation",
        ),
        (
            with_action(UPDATE, |action| action.parameters[0].required = false),
            "brush-mask update update-repair does not require an operation identity named operation",
        ),
        (
            with_action(UPDATE, |action| {
                action.parameters[2] = ParameterDescriptor::number("radius", 0.0, 0.9);
            }),
            "brush-mask parameter radius of update update-repair is not declared as action add-repair declares it",
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
            with_canvas(|canvas| {
                if let CanvasInteraction::BrushMask { title, .. } = canvas {
                    title.clear();
                }
            }),
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
    // A new path names no operation: an add that declared one could not mint it.
    let mut named = with_action(ADD, |action| {
        action.parameters.push(
            ParameterDescriptor::new(
                OPERATION_PARAMETER,
                ParameterKind::Identity {
                    of: IdentityKind::Operation,
                },
            )
            .notes("test"),
        )
    });
    assert!(
        refused(&named).contains("declares identity operation; a new path names no operation"),
        "{}",
        refused(&named)
    );
    // A brush canvas is not a pick, so a picker control cannot bind to it.
    named = brush_consumer::descriptor();
    named.controls.push(Control::picker("Repair").into());
    assert!(
        refused(&named).contains("picker control without a point-pick or sample-apply canvas"),
        "{}",
        refused(&named)
    );
}

#[test]
fn only_the_operation_identity_is_admitted_on_module_actions_and_queries() {
    let operation = ParameterDescriptor::new(
        OPERATION_PARAMETER,
        ParameterKind::Identity {
            of: IdentityKind::Operation,
        },
    )
    .required(true)
    .notes("test");
    let module = |parameters: Vec<ParameterDescriptor>, preset: bool| ModuleDescriptor {
        actions: vec![ActionDescriptor {
            parameters,
            preset,
            ..ActionDescriptor::new("edit-operation", "Edit operation", "test")
        }],
        controls: Vec::new(),
        reset: None,
        ..super::testing::descriptor()
    };
    module(vec![operation.clone()], false)
        .validate()
        .expect("an action may name an operation");
    // A query may name one too: `query.correction-sources` asks about one operation.
    ModuleDescriptor {
        queries: vec![ActionDescriptor {
            parameters: vec![operation.clone()],
            ..ActionDescriptor::new("sources", "Sources", "test")
        }],
        ..module(Vec::new(), true)
    }
    .validate()
    .expect("a query may name an operation");
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
            vec![ParameterDescriptor::new("target", kind).notes("test")],
            false,
        ));
        assert!(
            detail.contains(&format!(
                "declares kind {name}, which only a host command declares"
            )),
            "{detail}"
        );
    }
    // The operation identity has one name, which is what a draft targets it by.
    let detail = refused(&module(
        vec![ParameterDescriptor {
            name: "correction".into(),
            ..operation.clone()
        }],
        false,
    ));
    assert!(
        detail.contains("declares an operation identity, which is named operation"),
        "{detail}"
    );
    // An operation is one photograph's, so an action naming one is never presettable.
    let detail = refused(&module(vec![operation.clone()], true));
    assert!(
        detail.contains(
            "names an operation through parameter operation and must declare preset: false"
        ),
        "{detail}"
    );
    // A task is not an action on stored state: it may not name one.
    let detail = check_parameter_declarations("task", "test-task", &[operation])
        .expect_err("a task names no operation")
        .detail;
    assert!(
        detail.contains("declares kind identity, which only a host command declares"),
        "{detail}"
    );
}

#[test]
fn an_operation_identity_has_its_own_minted_shape() {
    let minted = OperationId::new();
    assert!(minted.as_str().starts_with("operation-"));
    assert!(IdentityKind::Operation.accepts(minted.as_str()));
    for other in [
        crate::MaskId::new().as_str().to_owned(),
        "operation-".to_owned(),
        "operation-short".to_owned(),
        format!("{} ", minted.as_str()),
    ] {
        assert!(!IdentityKind::Operation.accepts(&other), "{other}");
    }
    assert_eq!(
        serde_json::to_value(IdentityKind::Operation).unwrap(),
        json!("operation")
    );
    assert_eq!(IdentityKind::Operation.with_article(), "an operation");
    let declared = ParameterDescriptor::new(
        OPERATION_PARAMETER,
        ParameterKind::Identity {
            of: IdentityKind::Operation,
        },
    );
    check_value(&declared, &json!(minted.as_str())).expect("a minted operation");
    let detail = check_value(&declared, &json!(crate::MaskId::new().as_str()))
        .expect_err("a mask is not an operation")
        .detail;
    assert!(detail.contains("operation"), "{detail}");
}

#[test]
fn the_consumer_module_is_a_shared_registry_entry_like_any_other() {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(brush_consumer::RepairBrushModule::shared())
        .unwrap();
    let (module, update) = registry.action(UPDATE).expect("the update is registered");
    assert_eq!(module.descriptor().id, brush_consumer::MODULE);
    let target = crate::declared_target(update, |kind| {
        (kind == IdentityKind::Operation).then(|| "operation-0123456789".to_owned())
    });
    assert_eq!(
        target,
        crate::DraftTarget::from([("operation".to_owned(), "operation-0123456789".to_owned())])
    );
}
