//! A draft begun on one operation of a module's own list: the `operation` target of `draft.begin`,
//! through the one lifecycle every draft takes — begin, set, read, commit, cancel, conflict and
//! reapply — with the test-only repair brush as the consumer
//! ([`crate::modules::brush_consumer`]). Pointer motion is `draft.set`, which writes no history.
use super::methods::{Route, find};
use crate::{
    ClientSession, EditorService, ModuleRegistry, OperationId,
    api::ApiResponse,
    editor::mutation_json,
    modules::brush_consumer::{self, ADD, EFFECT, UPDATE},
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn call(
    service: &mut EditorService,
    session: &mut ClientSession,
    method: &str,
    params: Value,
) -> ApiResponse {
    let result = match find(service, method) {
        Some(resolved) => {
            assert!(
                matches!(resolved.route(), Route::Service),
                "{method} is the owner's"
            );
            resolved.serve(service, session, &params)
        }
        None => Err(crate::Error::protocol(format!("unknown method {method}"))),
    };
    match result {
        Ok(result) => ApiResponse::value(method.into(), 0, result),
        Err(error) => ApiResponse::failure(method.into(), 0, error),
    }
}

fn ok(
    service: &mut EditorService,
    session: &mut ClientSession,
    method: &str,
    params: Value,
) -> Value {
    let response = call(service, session, method, params);
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("a result")
}

fn refusal(response: ApiResponse) -> (String, String) {
    let error = response.error.expect("refused");
    (error.code, error.message)
}

/// A catalog with the repair brush registered beside the built-in modules, and one photograph.
fn repairing(name: &str) -> (EditorService, std::path::PathBuf, Value) {
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-operation-draft-{name}-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(brush_consumer::RepairBrushModule::shared())
        .unwrap();
    let mut service = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
    let asset = json!(service.import(&fixture).unwrap().asset.id);
    (service, catalog, asset)
}

/// The operations the stored repair layer holds, in order.
fn operations(service: &mut EditorService, asset: &Value) -> Vec<Value> {
    let asset = serde_json::from_value(asset.clone()).unwrap();
    let state = service.state(&asset).unwrap();
    state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|layer| layer.effect_id == EFFECT)
        .map(|layer| layer.payload["operations"].as_array().unwrap().clone())
        .unwrap_or_default()
}

fn history_len(service: &mut EditorService, asset: &Value) -> usize {
    let asset = serde_json::from_value(asset.clone()).unwrap();
    service.history(&asset, None, 100).unwrap().entries.len()
}

#[test]
fn an_operation_target_drafts_through_the_one_lifecycle_and_motion_writes_no_history() {
    let (mut service, catalog, asset) = repairing("lifecycle");
    let mut session = ClientSession::default();
    // The canvas is a mode a session enters by the module's identity, like any canvas.
    ok(
        &mut service,
        &mut session,
        "workspace.set",
        json!({"mode": brush_consumer::MODULE}),
    );

    // A painted path: one draft, many ticks, one entry.
    let begun = ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": ADD}),
    );
    let draft_id = begun["draft_id"].clone();
    assert_eq!(begun["target"], json!(null), "a new path targets nothing");
    let before = history_len(&mut service, &asset);
    let mut path = vec![[0.2, 0.3]];
    for step in 1..6 {
        path.push([0.2 + 0.05 * f64::from(step), 0.3]);
        ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"points": path, "radius": 0.05}}),
        );
    }
    assert_eq!(
        history_len(&mut service, &asset),
        before,
        "pointer motion writes no history"
    );
    assert!(operations(&mut service, &asset).is_empty());
    ok(
        &mut service,
        &mut session,
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": mutation_json(0, "add")}),
    );
    assert_eq!(history_len(&mut service, &asset), before + 1, "one entry");
    let added = operations(&mut service, &asset);
    assert_eq!(added.len(), 1);
    let operation = added[0]["operation"].as_str().unwrap().to_owned();
    OperationId::parse(operation.clone()).expect("a minted operation identity");
    assert_eq!(added[0]["points"], json!(path));

    // A pin drag: one draft begun on the operation, its translated path set per move.
    let begun = ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": UPDATE, "operation": operation}),
    );
    assert_eq!(begun["target"], json!({"operation": operation}));
    let draft_id = begun["draft_id"].clone();
    for offset in [0.01, 0.02, 0.03] {
        let moved: Vec<[f64; 2]> = path.iter().map(|[x, y]| [x + offset, *y]).collect();
        let set = ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"points": moved}}),
        );
        assert_eq!(set["target"], json!({"operation": operation}));
        assert!(
            set["fields"].get("operation").is_none(),
            "the target is not a field"
        );
    }
    assert_eq!(history_len(&mut service, &asset), before + 1);
    // The drafted recipe is the commit's own plan: the named operation moved, nothing written.
    let held = session.draft.clone().unwrap();
    let asset_id = serde_json::from_value(asset.clone()).unwrap();
    let (drafted, _) = service.draft_recipe(&asset_id, &held).unwrap();
    let drafted_points = &drafted
        .layers
        .iter()
        .find(|layer| layer.effect_id == EFFECT)
        .unwrap()
        .payload["operations"][0]["points"];
    assert!((drafted_points[0][0].as_f64().unwrap() - 0.23).abs() < 1e-12);
    assert_eq!(operations(&mut service, &asset)[0]["points"], json!(path));
    ok(
        &mut service,
        &mut session,
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": mutation_json(1, "move")}),
    );
    assert_eq!(history_len(&mut service, &asset), before + 2);
    let moved = operations(&mut service, &asset);
    assert_eq!(
        moved[0]["operation"],
        json!(operation),
        "the identity is stable"
    );
    assert!((moved[0]["points"][0][0].as_f64().unwrap() - 0.23).abs() < 1e-12);

    // Cancel ends a drag with no entry.
    let begun = ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": UPDATE, "operation": operation}),
    );
    ok(
        &mut service,
        &mut session,
        "draft.set",
        json!({"draft_id": begun["draft_id"], "fields": {"radius": 0.2}}),
    );
    ok(
        &mut service,
        &mut session,
        "draft.cancel",
        json!({"draft_id": begun["draft_id"]}),
    );
    assert_eq!(history_len(&mut service, &asset), before + 2);
    assert_eq!(operations(&mut service, &asset)[0]["radius"], json!(0.05));
    drop(service);
    std::fs::remove_file(catalog).unwrap();
}

#[test]
fn an_operation_draft_keeps_the_one_draft_and_conflict_contract() {
    let (mut service, catalog, asset) = repairing("conflict");
    let mut session = ClientSession::default();
    let mut stranger = ClientSession::default();
    ok(
        &mut service,
        &mut stranger,
        &format!("edit.{ADD}"),
        json!({"asset_id": asset, "mutation": mutation_json(0, "seed"),
               "points": [[0.4, 0.4]], "radius": 0.05}),
    );
    let operation = operations(&mut service, &asset)[0]["operation"].clone();
    let begun = ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": UPDATE, "operation": operation}),
    );
    let draft_id = begun["draft_id"].clone();
    ok(
        &mut service,
        &mut session,
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"source-x": 0.6, "source-y": 0.4}}),
    );
    // One draft per client: a second begin is a conflict and the first is kept.
    let (code, message) = refusal(call(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": UPDATE, "operation": operation}),
    ));
    assert_eq!(code, "conflict");
    assert!(message.contains("already holds draft"), "{message}");

    // Another client moves the asset: the draft is conflicted, its commit is refused and kept.
    ok(
        &mut service,
        &mut stranger,
        &format!("edit.{UPDATE}"),
        json!({"asset_id": asset, "mutation": mutation_json(1, "external"),
               "operation": operation, "opacity": 40.0}),
    );
    let read = ok(
        &mut service,
        &mut session,
        "draft.read",
        json!({"draft_id": draft_id}),
    );
    assert_eq!(read["conflicted"], json!(true));
    let (code, _) = refusal(call(
        &mut service,
        &mut session,
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": mutation_json(2, "late")}),
    ));
    assert_eq!(code, "conflict");
    assert!(session.draft.is_some(), "a refused commit keeps the draft");

    // Reapply rebases the drafted fields and its target onto the other client's change.
    let reapplied = ok(
        &mut service,
        &mut session,
        "draft.reapply",
        json!({"draft_id": draft_id}),
    );
    assert_eq!(reapplied["base_revision"], json!(2));
    assert_eq!(reapplied["target"], json!({"operation": operation}));
    assert_eq!(
        reapplied["fields"],
        json!({"source-x": 0.6, "source-y": 0.4})
    );
    ok(
        &mut service,
        &mut session,
        "draft.commit",
        json!({"draft_id": draft_id, "mutation": mutation_json(2, "source")}),
    );
    let held = &operations(&mut service, &asset)[0];
    assert_eq!(
        held["opacity"],
        json!(40.0),
        "the other client's change is kept"
    );
    assert_eq!(
        (held["source-x"].clone(), held["source-y"].clone()),
        (json!(0.6), json!(0.4))
    );
    drop(service);
    std::fs::remove_file(catalog).unwrap();
}

#[test]
fn an_operation_target_is_refused_where_the_action_does_not_declare_one() {
    let (mut service, catalog, asset) = repairing("refused");
    let mut session = ClientSession::default();
    let operation = OperationId::new();
    let refused = |service: &mut EditorService, session: &mut ClientSession, params: Value| {
        let refused = refusal(call(service, session, "draft.begin", params));
        assert!(session.draft.is_none(), "nothing opened");
        refused
    };
    // An action that declares no operation: the add, and a built-in module's action.
    for action in [ADD, "set-basic"] {
        let (code, message) = refused(
            &mut service,
            &mut session,
            json!({"asset_id": asset, "action": action, "operation": operation}),
        );
        assert_eq!(code, "validation");
        assert!(message.contains("operation"), "{action}: {message}");
    }
    // The update requires one.
    let (code, message) = refused(
        &mut service,
        &mut session,
        json!({"asset_id": asset, "action": UPDATE}),
    );
    assert_eq!(code, "validation");
    assert!(message.contains("operation"), "{message}");
    // A malformed identity, and another kind's, are refused when the request is parsed.
    for wrong in [json!("operation-"), json!(crate::MaskId::new()), json!(7)] {
        let (code, _) = refused(
            &mut service,
            &mut session,
            json!({"asset_id": asset, "action": UPDATE, "operation": wrong}),
        );
        assert_eq!(code, "validation", "{wrong}");
    }
    // A host mask gesture takes no operation either.
    let (code, _) = refused(
        &mut service,
        &mut session,
        json!({"asset_id": asset, "action": "mask.create-linear", "operation": operation}),
    );
    assert_eq!(code, "validation");
    // A well-formed operation the layer does not hold is refused by the module that owns the
    // list, at the commit that would resolve it, and writes nothing.
    let begun = ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": UPDATE, "operation": operation}),
    );
    let set = call(
        &mut service,
        &mut session,
        "draft.set",
        json!({"draft_id": begun["draft_id"], "fields": {"radius": 0.1}}),
    );
    let commit = call(
        &mut service,
        &mut session,
        "draft.commit",
        json!({"draft_id": begun["draft_id"], "mutation": mutation_json(0, "unknown")}),
    );
    let message = set
        .error
        .or(commit.error)
        .expect("an unknown operation is refused")
        .message;
    assert!(message.contains("unknown operation"), "{message}");
    assert!(operations(&mut service, &asset).is_empty());
    // The schema publishes the field.
    let schema = ok(&mut service, &mut session, "schema.list", json!({}));
    assert!(
        schema["methods"]["draft.begin"]["optional"]["operation"].is_string(),
        "{}",
        schema["methods"]["draft.begin"]
    );
    drop(service);
    std::fs::remove_file(catalog).unwrap();
}
