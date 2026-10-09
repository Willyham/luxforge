//! A draft begun on one correction of a module's own operation list: `draft.begin`'s `operation`,
//! through the one lifecycle every draft takes — begin, set, read, commit, cancel, conflict and
//! reapply — with the test-only repair brush as the consumer
//! ([`crate::modules::brush_consumer`]). Pointer motion is `draft.set`, which writes no history.
use super::methods::{Route, find};
use crate::{
    ClientSession, CorrectionId, EditorService, ModuleRegistry,
    api::ApiResponse,
    editor::mutation_json,
    modules::brush_consumer::{self, ADD, EFFECT, LIMIT, UPDATE},
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

/// The API identity of the stored operation at `index`.
fn identity(service: &mut EditorService, asset: &Value, index: usize) -> String {
    let number = operations(service, asset)[index]["id"].as_u64().unwrap();
    format!("correction-{number}")
}

fn history_len(service: &mut EditorService, asset: &Value) -> usize {
    let asset = serde_json::from_value(asset.clone()).unwrap();
    service.history(&asset, None, 100).unwrap().entries.len()
}

fn add(service: &mut EditorService, asset: &Value, revision: u64, at: [f64; 2]) {
    ok(
        service,
        &mut ClientSession::default(),
        &format!("edit.{ADD}"),
        json!({"asset_id": asset, "mutation": mutation_json(revision, &format!("add-{revision}")),
               "points": [at], "radius": 0.05}),
    );
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
    assert_eq!(added[0]["points"], json!(path));
    let operation = identity(&mut service, &asset, 0);
    assert_eq!(operation, "correction-1");

    // A pin drag: one draft begun on the correction, its translated path set per move.
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
    assert_eq!(moved[0]["id"], json!(1), "the identity is stable");
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
    add(&mut service, &asset, 0, [0.4, 0.4]);
    let operation = identity(&mut service, &asset, 0);
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
               "operation": operation, "opacity": 40}),
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

    // Reapply rebases the drafted fields and the target onto the other client's change.
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
        json!(40),
        "the other client's change is kept"
    );
    assert_eq!(
        (held["source-x"].clone(), held["source-y"].clone()),
        (json!(0.6), json!(0.4))
    );

    // A drag on a correction another client then deletes: its reapply is refused in the
    // commit's words, and the draft stays conflicted until Discard.
    let begun = ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": UPDATE, "operation": operation}),
    );
    ok(
        &mut service,
        &mut stranger,
        &format!("edit.{}", brush_consumer::DELETE),
        json!({"asset_id": asset, "mutation": mutation_json(3, "delete"), "operation": operation}),
    );
    let (code, message) = refusal(call(
        &mut service,
        &mut session,
        "draft.reapply",
        json!({"draft_id": begun["draft_id"]}),
    ));
    assert_eq!(code, "validation");
    assert!(message.contains("no repair correction-1"), "{message}");
    assert_eq!(
        ok(
            &mut service,
            &mut session,
            "draft.read",
            json!({"draft_id": begun["draft_id"]}),
        )["conflicted"],
        json!(true)
    );
    ok(
        &mut service,
        &mut session,
        "draft.cancel",
        json!({"draft_id": begun["draft_id"]}),
    );
    drop(service);
    std::fs::remove_file(catalog).unwrap();
}

#[test]
fn an_operation_target_is_refused_where_the_action_or_the_layer_has_none() {
    let (mut service, catalog, asset) = repairing("refused");
    let mut session = ClientSession::default();
    let refused = |service: &mut EditorService, session: &mut ClientSession, params: Value| {
        let refused = refusal(call(service, session, "draft.begin", params));
        assert!(session.draft.is_none(), "nothing opened");
        refused
    };
    // An action that names no correction: the add, a built-in module's action and a host gesture.
    for action in [ADD, "set-basic", "mask.create-linear"] {
        let (code, message) = refused(
            &mut service,
            &mut session,
            json!({"asset_id": asset, "action": action, "operation": "correction-1"}),
        );
        assert_eq!(code, "validation");
        assert!(
            message.contains("names no correction"),
            "{action}: {message}"
        );
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
    for wrong in [
        json!("correction-0"),
        json!("correction-01"),
        json!(crate::MaskId::new()),
        json!(7),
    ] {
        let (code, _) = refused(
            &mut service,
            &mut session,
            json!({"asset_id": asset, "action": UPDATE, "operation": wrong}),
        );
        assert_eq!(code, "validation", "{wrong}");
    }
    // A well-formed correction the layer does not hold is refused before any draft opens, in
    // the words the module's own commit uses.
    let (code, message) = refused(
        &mut service,
        &mut session,
        json!({"asset_id": asset, "action": UPDATE, "operation": "correction-9"}),
    );
    let committed = refusal(call(
        &mut service,
        &mut session,
        &format!("edit.{UPDATE}"),
        json!({"asset_id": asset, "mutation": mutation_json(0, "unknown"),
               "operation": "correction-9", "radius": 0.1}),
    ));
    assert_eq!((code, message), committed);
    assert_eq!(committed.1, "no repair correction-9 in this photograph");
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

#[test]
fn a_new_path_is_refused_at_the_declared_limit_before_any_draft_opens() {
    let (mut service, catalog, asset) = repairing("limit");
    let mut session = ClientSession::default();
    for revision in 0..u64::from(LIMIT) - 1 {
        add(&mut service, &asset, revision, [0.2, 0.2]);
    }
    // One below the limit a new path drafts and commits.
    let begun = ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": ADD}),
    );
    ok(
        &mut service,
        &mut session,
        "draft.set",
        json!({"draft_id": begun["draft_id"], "fields": {"points": [[0.5, 0.5]], "radius": 0.05}}),
    );
    ok(
        &mut service,
        &mut session,
        "draft.commit",
        json!({"draft_id": begun["draft_id"],
               "mutation": mutation_json(u64::from(LIMIT) - 1, "last")}),
    );
    assert_eq!(operations(&mut service, &asset).len(), LIMIT as usize);
    // At the limit the press is refused before a draft starts, with the commit's own refusal.
    let response = call(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": ADD}),
    );
    let error = response.error.expect("refused at the limit");
    assert!(session.draft.is_none());
    let committed = call(
        &mut service,
        &mut session,
        &format!("edit.{ADD}"),
        json!({"asset_id": asset, "mutation": mutation_json(u64::from(LIMIT), "over"),
               "points": [[0.6, 0.6]], "radius": 0.05}),
    )
    .error
    .expect("the commit is refused too");
    assert_eq!(error.code, "resource-limit");
    assert_eq!(
        (error.code, error.message),
        (committed.code, committed.message)
    );
    assert_eq!(error.data, Some(json!({"limit": LIMIT})));
    // An edit of an existing correction is not an add, and still drafts.
    let operation = identity(&mut service, &asset, 0);
    ok(
        &mut service,
        &mut session,
        "draft.begin",
        json!({"asset_id": asset, "action": UPDATE, "operation": operation}),
    );
    assert!(CorrectionId::parse(&operation).is_ok());
    drop(service);
    std::fs::remove_file(catalog).unwrap();
}
