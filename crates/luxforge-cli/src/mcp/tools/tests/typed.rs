//! The typed profiles: one tool per method, its name, description and annotations, the namespace
//! filter, and calls through the same path as `luxforge_call`.
use super::*;
use luxforge_core::{ModuleRegistry, schemas};
use std::collections::BTreeMap;

fn typed_tools(namespaces: Namespaces) -> Tools {
    Tools {
        profile: Profile::Typed(namespaces),
        images: true,
    }
}

/// Call typed tool `name` with `value` as its arguments, never cancelled.
fn typed_call_of(owner: &mut Owner, name: &str, value: Value) -> Result<ToolOutcome, RpcError> {
    match typed_tools(Namespaces::All).call(
        owner,
        &client(),
        name,
        &arguments(value),
        &Cancel::default(),
    ) {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(Cancelled)) => panic!("cancelled"),
        Err(error) => Err(error),
    }
}

/// Every typed tool of `schema`'s registry, by name.
fn typed_by_name(schema: &Value) -> BTreeMap<String, Value> {
    let inputs = MethodInputs::read(schema).unwrap();
    typed(&Namespaces::All, schema, &inputs)
        .unwrap()
        .into_iter()
        .map(|tool| (tool["name"].as_str().unwrap().to_owned(), tool))
        .collect()
}

#[test]
fn typed_tool_names_are_distinct_and_short_over_the_built_in_and_developer_registries() {
    let developer = schemas(&ModuleRegistry::developer());
    for schema in [&built_in().schema, &developer] {
        let methods = schema["methods"].as_object().unwrap();
        let tools = typed_by_name(schema);
        let offered: Vec<&String> = methods
            .iter()
            .filter(|(_, entry)| callable(entry))
            .map(|(method, _)| method)
            .collect();
        assert_eq!(tools.len(), offered.len(), "one tool per callable method");
        for method in offered {
            let tool = &tools[&tool_name(method)];
            assert_eq!(tool["title"], json!(method));
            assert_eq!(
                tool["annotations"],
                annotations(&methods[method]),
                "{method}"
            );
        }
        for name in tools.keys() {
            assert!(name.chars().count() <= NAME_LIMIT, "{name}");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{name} has only letters, digits, _ and -"
            );
        }
        for permission_only in ["module_permission_grant", "session_disconnect"] {
            assert!(!tools.contains_key(permission_only), "{permission_only}");
        }
    }
}

#[test]
fn a_typed_tool_describes_its_method_and_annotates_its_effects() {
    let tools = typed_by_name(&built_in().schema);
    let state = &tools["asset_state"];
    assert_eq!(state["title"], "asset.state");
    assert_eq!(
        state["description"],
        built_in().schema["methods"]["asset.state"]["notes"]
    );
    assert_eq!(
        state["annotations"],
        json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": false,
               "openWorldHint": false})
    );
    let basic = &tools["edit_set-basic"];
    assert_eq!(
        (
            &basic["annotations"]["readOnlyHint"],
            &basic["annotations"]["destructiveHint"]
        ),
        (&json!(false), &json!(false)),
        "an edit is undone through history"
    );
    let properties = basic["inputSchema"]["properties"].as_object().unwrap();
    assert!(properties.contains_key(EXPECTED_REVISION) && !properties.contains_key("mutation"));
    assert!(
        tools["draft_begin"]["description"]
            .as_str()
            .unwrap()
            .ends_with(TYPED_SESSION_NOTE)
    );
    assert!(
        !state["description"]
            .as_str()
            .unwrap()
            .contains(TYPED_SESSION_NOTE)
    );
    // The markers' contract, including combinations the built-in registry may not hold.
    for (entry, destructive, open_world) in [
        (
            json!({"read_only": false, "reversible": false}),
            true,
            false,
        ),
        (
            json!({"read_only": false, "reversible": true, "network": true}),
            false,
            true,
        ),
        (json!({}), true, false),
    ] {
        assert_eq!(
            annotations(&entry),
            json!({"readOnlyHint": false, "destructiveHint": destructive,
                   "idempotentHint": false, "openWorldHint": open_world}),
            "{entry}"
        );
    }
}

#[test]
fn a_namespace_list_offers_only_its_namespaces_and_refuses_an_unknown_one() {
    let namespaces = Namespaces::read("asset,edit,asset").unwrap();
    assert_eq!(
        namespaces,
        Namespaces::Only(vec!["asset".into(), "edit".into()])
    );
    let inputs = built_in().inputs.as_ref().unwrap();
    let listed = typed(&namespaces, &built_in().schema, inputs).unwrap();
    assert!(listed.len() > 10);
    assert!(listed.iter().all(|tool| {
        let title = tool["title"].as_str().unwrap();
        title.starts_with("asset.") || title.starts_with("edit.")
    }));
    let settings = Namespaces::read("module.settings").unwrap();
    assert!(settings.offers("module.settings.read") && !settings.offers("module.list"));
    let unknown = Namespaces::read("asset,nope").unwrap_err();
    assert!(
        unknown.contains("nope") && unknown.contains("history"),
        "{unknown}"
    );
    assert!(Namespaces::read("asset,,edit").is_err());
    assert!(
        Namespaces::read("ass").is_err(),
        "a namespace is whole segments"
    );
}

#[test]
fn a_typed_call_sends_its_method_with_the_revision_given_beside_its_parameters() {
    let script = Script::default();
    let mut owner = script.owner();
    let asset = AssetId::new();
    script.answer(Ok(json!({"outcome": "applied"})));
    let edited = typed_call_of(
        &mut owner,
        "edit_set-basic",
        json!({"asset_id": asset, "exposure": 0.5, "expected_revision": 3}),
    )
    .unwrap();
    assert_eq!(edited.value, json!({"outcome": "applied"}));
    let calls = script.state().calls.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "edit.set-basic");
    assert_eq!(calls[0].1["exposure"], 0.5);
    assert_eq!(calls[0].1["mutation"]["expected_revision"], 3);
    assert!(calls[0].1.get(EXPECTED_REVISION).is_none());
    // A refused argument is named as the tool's own.
    let refused = typed_call_of(
        &mut owner,
        "edit_set-basic",
        json!({"asset_id": asset, "exposure": "x"}),
    )
    .unwrap();
    assert_eq!(refused.value["error"]["data"]["parameter"], "exposure");
    // A name no list offers, a permission-only method's included, is an unknown tool.
    for name in ["edit.set-basic", "module_permission_grant", "nope"] {
        let unknown = typed_call_of(&mut owner, name, json!({})).unwrap_err();
        assert_eq!(unknown.code, INVALID_PARAMS, "{name}");
    }
    // A namespace list offers only its own.
    let other = typed_tools(Namespaces::Only(vec!["asset".into()]))
        .call(
            &mut owner,
            &client(),
            "catalog_info",
            &Map::new(),
            &Cancel::default(),
        )
        .err()
        .unwrap();
    assert_eq!(other.code, INVALID_PARAMS);
    assert_eq!(script.state().calls.len(), 1, "nothing more was sent");
}

#[test]
fn a_typed_list_is_the_built_in_one_without_a_session_and_a_call_says_why() {
    let script = Script::default();
    script.state().refuse = Some(failure("no-session", "open Luxforge", None));
    let mut owner = script.owner();
    let tools = typed_tools(Namespaces::All);
    let listed = tools.list(&mut owner, &client()).unwrap();
    assert_eq!(listed.len(), typed_by_name(&built_in().schema).len());
    let call = typed_call_of(&mut owner, "catalog_info", json!({})).unwrap();
    assert_eq!(error_code(&call), "no-session");
    assert_eq!(
        typed_call_of(&mut owner, "nope", json!({}))
            .unwrap_err()
            .code,
        INVALID_PARAMS
    );
    // Attached, the list is the live registry's, here the same.
    script.state().refuse = None;
    assert_eq!(tools.list(&mut owner, &client()).unwrap(), listed);
    assert_eq!(script.state().attached, 1);
}
