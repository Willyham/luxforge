//! The owner over a scripted target: a target that is its owner's only client reads the registry
//! again after its own registry-changing calls, and one that others share never does.
use super::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct State {
    calls: Vec<String>,
    answers: VecDeque<Result<Value, ApiFailure>>,
}

#[derive(Clone)]
struct Script {
    state: Arc<Mutex<State>>,
    list_changes: ListChanges,
}

impl Connection for Script {
    fn call(&mut self, method: &str, _params: Value) -> Result<Value, ApiFailure> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(method.to_owned());
        state
            .answers
            .pop_front()
            .unwrap_or_else(|| panic!("no answer scripted for {method}"))
    }

    fn closed(&self) -> bool {
        false
    }
}

impl Target for Script {
    fn attach(&mut self, _label: &str) -> Result<Attached, ApiFailure> {
        Ok(Attached {
            connection: Box::new(self.clone()),
            schema: registry(0),
            catalog: "/catalogs/main.sqlite".into(),
        })
    }

    fn list_changes(&self) -> ListChanges {
        self.list_changes
    }
}

/// A registry with no methods, told apart by `generation`.
fn registry(generation: u64) -> Value {
    json!({"paths": {"range": [0, 100]}, "methods": {}, "generation": generation})
}

fn attached(list_changes: ListChanges) -> (Owner, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State::default()));
    let mut owner = Owner::new(Box::new(Script {
        state: state.clone(),
        list_changes,
    }));
    owner.session("Claude Code via MCP").unwrap();
    (owner, state)
}

#[test]
fn an_only_client_reads_the_registry_again_after_its_own_flag_change() {
    let (mut owner, state) = attached(ListChanges::OwnCalls);
    state.lock().unwrap().answers.extend([
        Ok(json!({"flags": []})),
        Ok(registry(1)),
        Ok(json!({})),
        Err(failure("validation", "no such flag", None)),
    ]);
    assert_eq!(
        owner
            .call("flags.set", json!({"flag": "f", "value": true}))
            .unwrap(),
        json!({"flags": []}),
        "the call's own answer is kept"
    );
    let (session, warning) = owner.session("Claude Code via MCP").unwrap();
    assert!(warning.is_none());
    assert_eq!(session.schema, registry(1));
    assert!(session.inputs.is_ok());
    // Other calls, and a refused flag change, read nothing again.
    owner.call("catalog.info", json!({})).unwrap();
    owner.call("flags.set", json!({"flag": "g"})).unwrap_err();
    assert_eq!(
        state.lock().unwrap().calls,
        ["flags.set", "schema.list", "catalog.info", "flags.set"]
    );
}

#[test]
fn a_shared_owner_leaves_list_changes_to_the_watch() {
    let (mut owner, state) = attached(ListChanges::Watch);
    state
        .lock()
        .unwrap()
        .answers
        .push_back(Ok(json!({"flags": []})));
    owner.call("flags.set", json!({"flag": "f"})).unwrap();
    assert_eq!(state.lock().unwrap().calls, ["flags.set"]);
    assert_eq!(owner.session("x").unwrap().0.schema, registry(0));
}
