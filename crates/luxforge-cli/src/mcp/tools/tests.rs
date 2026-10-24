//! The compact tools over a scripted owner serving the built-in registry: listing and describing
//! with and without a session, and `luxforge_call`'s check, envelopes, refusals, lost connections,
//! warnings and pictures.
use super::*;
use crate::mcp::owner::{Attached, Connection, Target};
use luxforge_core::{AssetId, DraftId};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// What the scripted owner was asked and will answer.
#[derive(Default)]
struct State {
    /// Why attaching fails; attaching succeeds while it is `None`.
    refuse: Option<ApiFailure>,
    attached: usize,
    calls: Vec<(String, Value)>,
    answers: VecDeque<Result<Value, ApiFailure>>,
    closed: bool,
}

#[derive(Clone, Default)]
struct Script(Arc<Mutex<State>>);

impl Script {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    fn answer(&self, answer: Result<Value, ApiFailure>) -> &Self {
        self.state().answers.push_back(answer);
        self
    }

    fn methods_called(&self) -> Vec<String> {
        self.state()
            .calls
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    fn owner(&self) -> Owner {
        Owner::new(Box::new(self.clone()))
    }
}

impl Connection for Script {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        let mut state = self.state();
        state.calls.push((method.to_owned(), params));
        state
            .answers
            .pop_front()
            .unwrap_or_else(|| panic!("no answer scripted for {method}"))
    }

    fn closed(&self) -> bool {
        self.state().closed
    }
}

impl Target for Script {
    fn attach(&mut self, _label: &str) -> Result<Attached, ApiFailure> {
        let mut state = self.state();
        if let Some(refusal) = &state.refuse {
            return Err(refusal.clone());
        }
        state.attached += 1;
        state.closed = false;
        Ok(Attached {
            connection: Box::new(self.clone()),
            schema: built_in().schema.clone(),
            catalog: "/catalogs/main.sqlite".into(),
        })
    }
}

fn client() -> ClientInfo {
    ClientInfo::read(&json!({"name": "claude-code", "title": "Claude Code", "version": "1"}))
}

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// `luxforge_call` with `value` as its arguments, never cancelled.
fn call(owner: &mut Owner, value: Value) -> ToolOutcome {
    call_with(owner, value, true)
}

fn call_with(owner: &mut Owner, value: Value, images: bool) -> ToolOutcome {
    let tools = Tools {
        profile: Profile::Compact,
        images,
    };
    match tools.call(
        owner,
        &client(),
        CALL,
        &arguments(value),
        &Cancel::default(),
    ) {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(Cancelled)) => panic!("cancelled"),
        Err(error) => panic!("{error:?}"),
    }
}

fn error_code(outcome: &ToolOutcome) -> &str {
    assert!(outcome.is_error, "{}", outcome.value);
    outcome.value["error"]["code"].as_str().unwrap()
}

#[test]
fn the_compact_tools_are_fixed_with_valid_input_schemas_and_names() {
    let tools = Profile::Compact.tools();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [METHODS, DESCRIBE, CALL]);
    for tool in &tools {
        jsonschema::draft202012::meta::validate(&tool["inputSchema"])
            .unwrap_or_else(|error| panic!("{}: {error}", tool["name"]));
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
    assert_eq!(
        tools[2]["annotations"],
        json!({"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false,
               "openWorldHint": true}),
        "luxforge_call can call anything"
    );
    assert!(!Profile::Compact.list_changed());
}

#[test]
fn an_unknown_tool_is_a_protocol_error() {
    let tools = Tools {
        profile: Profile::Compact,
        images: true,
    };
    let refused = tools
        .call(
            &mut Script::default().owner(),
            &client(),
            "asset_state",
            &Map::new(),
            &Cancel::default(),
        )
        .err()
        .unwrap();
    assert_eq!(refused.code, INVALID_PARAMS);
}

#[test]
fn summaries_are_the_first_clause_and_bounded() {
    assert_eq!(summary("reads the state. Then more"), "reads the state");
    assert_eq!(
        summary("{a, b}: lists things; also more. And"),
        "{a, b}: lists things"
    );
    assert_eq!(summary("one clause."), "one clause");
    let long = summary(&"x".repeat(1000));
    assert_eq!(long.chars().count(), SUMMARY_LIMIT);
    assert!(long.ends_with('…'));
}

#[test]
fn methods_and_describe_answer_from_the_built_in_registry_without_a_session() {
    let script = Script::default();
    script.state().refuse = Some(failure("no-session", "open Luxforge", None));
    let mut owner = script.owner();
    let tools = Tools {
        profile: Profile::Compact,
        images: true,
    };
    let mut run = |tool: &str, value: Value| {
        tools
            .call(
                &mut owner,
                &client(),
                tool,
                &arguments(value),
                &Cancel::default(),
            )
            .unwrap()
            .ok()
            .unwrap()
    };
    let all = run(METHODS, json!({}));
    assert!(!all.is_error);
    assert_eq!(all.value["source"], "built-in");
    assert_eq!(all.value["session"]["code"], "no-session");
    let listed = all.value["methods"].as_array().unwrap();
    assert_eq!(
        listed.len(),
        built_in().schema["methods"].as_object().unwrap().len()
    );
    let names: Vec<&str> = listed.iter().map(|m| m["name"].as_str().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "registry order is deterministic");
    let entry = |name: &str| listed.iter().find(|m| m["name"] == name).unwrap().clone();
    let basic = entry("edit.set-basic");
    assert_eq!(
        (&basic["mutates"], &basic["reversible"], &basic["callable"]),
        (&json!(true), &json!(true), &json!(true))
    );
    assert_eq!(entry("module.permission.grant")["callable"], false);
    assert_eq!(entry("draft.begin")["session_scoped"], true);
    assert_eq!(
        (
            &entry("asset.state")["mutates"],
            &entry("asset.state")["read_only"]
        ),
        (&json!(false), &json!(true))
    );
    assert_eq!(
        (&basic["read_only"], &entry("view.set")["read_only"]),
        (&json!(false), &json!(false)),
        "read_only passes through as the registry gives it"
    );
    assert_eq!(entry("session.disconnect")["callable"], false);
    assert!(
        listed
            .iter()
            .all(|m| m["summary"].as_str().unwrap().chars().count() <= SUMMARY_LIMIT)
    );

    let edits = run(METHODS, json!({"namespace": "edit"}));
    assert!(
        edits.value["methods"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"].as_str().unwrap().starts_with("edit."))
    );
    let settings = run(METHODS, json!({"namespace": "module.settings"}));
    assert!(
        settings.value["count"].as_u64().unwrap() >= 1,
        "{}",
        settings.value
    );
    let queried = run(METHODS, json!({"query": "AUTO-TONE"}));
    assert!(
        queried.value["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "edit.auto-tone")
    );
    let unknown = run(METHODS, json!({"filter": "x"}));
    assert_eq!(unknown.value["error"]["data"]["parameter"], "filter");

    let described = run(
        DESCRIBE,
        json!({"methods": ["edit.set-basic", "draft.commit", "draft.begin", "edit.set-basic"]}),
    );
    assert_eq!(described.value["source"], "built-in");
    let methods = described.value["methods"].as_array().unwrap();
    assert_eq!(methods.len(), 3, "each method once");
    let basic = &methods[0];
    assert_eq!(
        (
            &basic["envelope"],
            &basic["revision_of"],
            &basic["expected_revision"]
        ),
        (&json!("revision"), &json!("asset"), &json!("optional"))
    );
    let properties = basic["input_schema"]["properties"].as_object().unwrap();
    assert!(properties.contains_key("asset_id") && properties.contains_key("exposure"));
    assert!(!properties.contains_key("expected_revision") && !properties.contains_key("mutation"));
    assert_eq!(methods[1]["expected_revision"], "required");
    assert!(
        methods[2]["session"]
            .as_str()
            .unwrap()
            .contains("session-scoped")
    );
    assert!(methods[2].get("expected_revision").is_none());

    let refused = run(DESCRIBE, json!({"methods": ["edit.set-basic", "no.such"]}));
    assert_eq!(refused.value["error"]["code"], "validation");
    assert!(
        refused.value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no.such")
    );
    let too_many = run(
        DESCRIBE,
        json!({"methods": vec!["asset.state"; DESCRIBE_LIMIT + 1]}),
    );
    assert_eq!(too_many.value["error"]["data"]["parameter"], "methods");
    assert_eq!(script.state().attached, 0);
}

#[test]
fn methods_read_the_live_registry_when_a_session_attaches() {
    let script = Script::default();
    let mut owner = script.owner();
    let listed = Tools {
        profile: Profile::Compact,
        images: true,
    }
    .call(
        &mut owner,
        &client(),
        METHODS,
        &Map::new(),
        &Cancel::default(),
    )
    .unwrap()
    .ok()
    .unwrap();
    assert_eq!(listed.value["source"], "live");
    assert_eq!(listed.value["catalog"], "/catalogs/main.sqlite");
    assert!(
        script.methods_called().is_empty(),
        "the handshake's schema is used"
    );
}

#[test]
fn a_call_without_a_session_is_a_tool_error_saying_why() {
    let script = Script::default();
    script.state().refuse = Some(failure("no-session", "open Luxforge", None));
    let outcome = call(&mut script.owner(), json!({"method": "catalog.info"}));
    assert_eq!(error_code(&outcome), "no-session");
}

#[test]
fn a_permission_only_method_or_bad_arguments_are_refused_before_anything_is_sent() {
    let script = Script::default();
    let mut owner = script.owner();
    let grant = call(
        &mut owner,
        json!({"method": "module.permission.grant",
               "params": {"module_id": "m", "capability": "download", "scope": {}}}),
    );
    assert_eq!(error_code(&grant), "forbidden");
    let asset = AssetId::new();
    for (arguments, parameter) in [
        (
            json!({"method": "edit.set-basic", "params": {"asset_id": asset, "exposure": "x"}}),
            "params.exposure",
        ),
        (
            json!({"method": "edit.set-basic", "params": {"asset_id": asset, "mutation": {}}}),
            "params.mutation",
        ),
        (
            json!({"method": "edit.set-basic",
                   "params": {"asset_id": asset, "expected_revision": 1}}),
            "params.expected_revision",
        ),
        (
            json!({"method": "draft.commit", "params": {"draft_id": DraftId::new()}}),
            "expected_revision",
        ),
        (
            json!({"method": "catalog.info", "expected_revision": 3}),
            "expected_revision",
        ),
        (json!({"method": "no.such.method"}), "method"),
        (json!({"method": "catalog.info", "params": [1]}), "params"),
        (json!({"method": "catalog.info", "wait": true}), "wait"),
    ] {
        let refused = call(&mut owner, arguments.clone());
        assert_eq!(error_code(&refused), "validation", "{arguments}");
        assert_eq!(
            refused.value["error"]["data"]["parameter"], parameter,
            "{arguments}: {}",
            refused.value
        );
    }
    assert!(script.methods_called().is_empty());
}

#[test]
fn an_asset_edit_reads_the_revision_unless_given_and_carries_the_agents_actor() {
    let script = Script::default();
    let mut owner = script.owner();
    let asset = AssetId::new();
    script
        .answer(Ok(json!({"revision": 4})))
        .answer(Ok(json!({"outcome": "applied"})));
    let edited = call(
        &mut owner,
        json!({"method": "edit.set-basic", "params": {"asset_id": asset, "exposure": 0.5}}),
    );
    assert_eq!(edited.value, json!({"outcome": "applied"}));
    assert!(!edited.is_error && edited.warnings.is_empty());
    script.answer(Ok(json!({"outcome": "applied"})));
    call(
        &mut owner,
        json!({"method": "edit.set-basic", "params": {"asset_id": asset, "exposure": 0.25},
               "expected_revision": 9}),
    );
    let calls = script.state().calls.clone();
    assert_eq!(
        calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        ["asset.state", "edit.set-basic", "edit.set-basic"]
    );
    assert_eq!(calls[0].1, json!({"asset_id": asset}));
    let first = &calls[1].1["mutation"];
    assert_eq!(
        (&first["expected_revision"], &first["actor"]),
        (&json!(4), &json!("mcp:claude-code"))
    );
    assert_eq!(calls[2].1["mutation"]["expected_revision"], 9);
    assert_ne!(first["request_id"], calls[2].1["mutation"]["request_id"]);
    // A request-envelope method carries no revision; a read carries no envelope.
    script.answer(Ok(json!({"theme": {}})));
    call(&mut owner, json!({"method": "catalog.info"}));
    assert!(script.state().calls[3].1.get("mutation").is_none());
}

#[test]
fn a_conflict_is_returned_as_the_owner_gave_it_and_never_retried() {
    let script = Script::default();
    let mut owner = script.owner();
    let conflict = ApiFailure {
        code: "conflict".into(),
        message: "expected revision 1, current 2".into(),
        job_id: None,
        data: Some(json!({"current_revision": 2})),
    };
    script.answer(Err(conflict.clone()));
    let refused = call(
        &mut owner,
        json!({"method": "edit.set-basic", "params": {"asset_id": AssetId::new(), "exposure": 1},
               "expected_revision": 1}),
    );
    assert_eq!(refused.value, json!({"error": conflict}));
    assert_eq!(script.methods_called(), ["edit.set-basic"]);
}

#[test]
fn a_lost_connection_is_outcome_unknown_for_a_mutation_and_the_next_call_attaches_again() {
    let script = Script::default();
    let mut owner = script.owner();
    let lost = || {
        Err(failure(
            CONNECTION_LOST,
            "the editor closed the connection",
            None,
        ))
    };
    script.answer(lost());
    let unknown = call(
        &mut owner,
        json!({"method": "edit.set-basic", "params": {"asset_id": AssetId::new(), "exposure": 1},
               "expected_revision": 1}),
    );
    assert_eq!(error_code(&unknown), OUTCOME_UNKNOWN);
    let sent = script.state().calls[0].1["mutation"]["request_id"].clone();
    assert_eq!(
        unknown.value["error"]["data"],
        json!({"method": "edit.set-basic", "request_id": sent})
    );
    // A read that loses its connection is only that.
    script.answer(lost());
    let read = call(&mut owner, json!({"method": "catalog.info"}));
    assert!(
        read.warnings.iter().any(|w| w.code == "session-replaced"),
        "{:?}",
        read.warnings
    );
    assert_eq!(error_code(&read), CONNECTION_LOST);
    script.answer(Ok(json!({"photographs": 0})));
    let replaced = call(&mut owner, json!({"method": "catalog.info"}));
    assert!(!replaced.is_error);
    assert_eq!(replaced.warnings.len(), 1, "{:?}", replaced.warnings);
    assert_eq!(replaced.warnings[0].code, "session-replaced");
    assert_eq!(script.state().attached, 3);
    script.answer(Ok(json!({"photographs": 0})));
    assert!(
        call(&mut owner, json!({"method": "catalog.info"}))
            .warnings
            .is_empty()
    );
}

#[test]
fn a_connection_closed_while_idle_is_replaced_before_anything_is_sent() {
    let script = Script::default();
    let mut owner = script.owner();
    script.answer(Ok(json!({})));
    call(&mut owner, json!({"method": "catalog.info"}));
    script.state().closed = true;
    script.answer(Ok(json!({"outcome": "applied"})));
    let edited = call(
        &mut owner,
        json!({"method": "edit.set-basic", "params": {"asset_id": AssetId::new(), "exposure": 1},
               "expected_revision": 1}),
    );
    assert!(!edited.is_error, "{}", edited.value);
    assert_eq!(edited.warnings[0].code, "session-replaced");
    assert_eq!(script.state().attached, 2);
}

#[test]
fn a_job_its_clients_own_is_warned_about_and_a_catalog_job_is_not() {
    let script = Script::default();
    let mut owner = script.owner();
    let asset = AssetId::new();
    script
        .answer(Ok(json!({"job_id": "job-1", "status": "queued"})))
        .answer(Ok(json!({"kind": "source-prepare", "status": "running",
                           "ownership": "clients"})));
    let started = call(
        &mut owner,
        json!({"method": "source.prepare", "params": {"asset_id": asset}}),
    );
    assert_eq!(started.value["job_id"], "job-1");
    assert_eq!(started.warnings.len(), 1);
    assert_eq!(started.warnings[0].code, JOB_RELEASED);
    assert_eq!(started.warnings[0].job_id.as_deref(), Some("job-1"));
    script
        .answer(Ok(json!({"job_id": "job-2", "status": "queued"})))
        .answer(Ok(
            json!({"kind": "export", "status": "running", "ownership": "catalog"}),
        ));
    let export = call(
        &mut owner,
        json!({"method": "source.prepare", "params": {"asset_id": asset}}),
    );
    assert!(export.warnings.is_empty());
}

#[test]
fn a_marked_picture_is_attached_unless_images_are_off() {
    let dir = luxforge_testbase::paths::temp_dir("mcp-tools-picture");
    let picture = dir.join("large.jpg");
    std::fs::write(&picture, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
    let script = Script::default();
    let mut owner = script.owner();
    let read = json!({"method": "preview.read",
                      "params": {"item": {"kind": "photo", "asset_id": AssetId::new()},
                                 "tier": "large"}});
    let answer = json!({"preview": {"path": picture, "width": 1, "height": 1}});
    script.answer(Ok(answer.clone()));
    let attached = call(&mut owner, read.clone());
    assert_eq!(attached.images, ["/9j/2Q=="]);
    assert_eq!(attached.value, answer, "the path is returned as data too");
    script.answer(Ok(answer));
    assert!(call_with(&mut owner, read, false).images.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_cancellation_stops_the_call_at_its_next_hold() {
    let script = Script::default();
    let mut owner = script.owner();
    let cancel = Cancel::default();
    cancel.cancel();
    let mut warnings = Vec::new();
    let stopped = super::call(
        &mut owner,
        &client(),
        &arguments(json!({"method": "edit.set-basic",
                          "params": {"asset_id": AssetId::new(), "exposure": 1}})),
        &cancel,
        true,
        &mut warnings,
    );
    assert!(
        stopped.is_err(),
        "stopped after attaching, before anything was sent"
    );
    assert!(script.methods_called().is_empty());
}
