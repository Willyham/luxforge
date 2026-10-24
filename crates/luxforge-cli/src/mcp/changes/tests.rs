//! List changes over scripted owners: announced on attaching only when the list differs, to each
//! listener in its era's form, from a watched event log, and after the adapter's own calls on an
//! owner that serves one connection.
use super::*;
use crate::{
    live::failure,
    mcp::{
        owner::{Attached, ListChanges, Owner},
        protocol::ClientInfo,
        tools::built_in,
    },
};
use luxforge_core::ApiFailure;
use std::{
    io::Write,
    sync::mpsc::{Receiver, Sender, channel},
};

/// Standard output as messages on a channel.
struct Lines(Sender<Value>);

impl Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let _ = self.0.send(serde_json::from_slice(line).unwrap());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn typed_changes() -> (Changes, Receiver<Value>) {
    let (written, output) = channel();
    let changes = Changes::new(
        Some(Namespaces::All),
        Output::new(Box::new(Lines(written))),
        Subscriptions::default(),
    );
    (changes, output)
}

/// The built-in registry, or it without the `history.*` methods: a registry a flag changed.
fn registry(full: bool) -> Registry {
    let mut schema = built_in().schema.clone();
    if !full {
        schema["methods"]
            .as_object_mut()
            .unwrap()
            .retain(|method, _| !method.starts_with("history."));
    }
    let inputs = MethodInputs::read(&schema);
    Registry { schema, inputs }
}

fn listed(registry: &Registry) -> Vec<Value> {
    tools::typed(
        &Namespaces::All,
        &registry.schema,
        registry.inputs.as_ref().unwrap(),
    )
    .unwrap()
}

fn tagged(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed",
           "params": {"_meta": {SUBSCRIPTION_ID: id}}})
}

#[test]
fn a_list_that_differs_on_attaching_is_announced_once_to_each_listener_in_its_era() {
    let (changes, output) = typed_changes();
    changes.listen(&json!("tools"), true);
    assert_eq!(
        output.recv().unwrap()["params"]["notifications"],
        json!({"toolsListChanged": true})
    );
    changes.listen(&json!("nothing"), false);
    assert_eq!(output.recv().unwrap()["params"]["notifications"], json!({}));
    changes.initialized(&json!({"jsonrpc": "2.0", "id": 1, "result": {}}));
    assert_eq!(output.recv().unwrap()["id"], 1, "the answer first");
    // The built-in list was offered; the live registry lacks the history methods.
    changes.offer(&listed(&registry(true)));
    changes.attached(1, &registry(false), None, "watch");
    let mut announced = [output.recv().unwrap(), output.recv().unwrap()];
    announced.sort_by_key(|message| message.get("params").is_some());
    assert_eq!(
        announced,
        [
            json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
            tagged(json!("tools")),
        ]
    );
    // The same list again, and an attachment whose list nothing was offered before, say nothing.
    changes.attached(2, &registry(false), None, "watch");
    changes.close();
    changes.join();
    assert!(output.try_recv().is_err(), "{:?}", output.try_recv());
    let (quiet, output) = typed_changes();
    quiet.listen(&json!(1), true);
    output.recv().unwrap();
    quiet.attached(1, &registry(false), None, "watch");
    quiet.close();
    quiet.join();
    assert!(output.try_recv().is_err());
}

#[test]
fn the_compact_profile_honours_no_subscription_and_never_watches() {
    let (written, output) = channel();
    let changes = Changes::new(
        None,
        Output::new(Box::new(Lines(written))),
        Subscriptions::default(),
    );
    changes.listen(&json!(3), true);
    assert_eq!(
        output.recv().unwrap(),
        json!({"jsonrpc": "2.0", "method": "notifications/subscriptions/acknowledged",
               "params": {"_meta": {SUBSCRIPTION_ID: 3}, "notifications": {}}})
    );
    changes.offer(&[json!({"name": "x"})]);
    changes.attached(1, &registry(false), None, "watch");
    assert!(output.try_recv().is_err());
    assert!(lock(&changes.0.state).watcher.is_none());
}

/// A connection whose every call the test answers in turn, seeing each request first.
struct Gated {
    requests: Sender<(String, Value)>,
    answers: Receiver<Result<Value, ApiFailure>>,
}

impl Connection for Gated {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        self.requests.send((method.to_owned(), params)).unwrap();
        self.answers.recv().unwrap()
    }

    fn closed(&self) -> bool {
        false
    }
}

/// A target that hands out one gated connection, reporting the label it was attached with.
struct OneConnection {
    connection: Option<Gated>,
    labels: Sender<String>,
}

impl Target for OneConnection {
    fn attach(&mut self, label: &str) -> Result<Attached, ApiFailure> {
        self.labels.send(label.to_owned()).unwrap();
        match self.connection.take() {
            Some(connection) => Ok(Attached {
                connection: Box::new(connection),
                schema: json!({}),
                catalog: "/catalog.sqlite".into(),
            }),
            None => Err(failure("no-session", "gone", None)),
        }
    }
}

#[test]
fn the_watcher_holds_the_event_log_and_announces_a_flag_change_that_changes_the_list() {
    let (changes, output) = typed_changes();
    let (requests, asked) = channel();
    let (answer, answers) = channel();
    let (labels, attached) = channel();
    let target = OneConnection {
        connection: Some(Gated { requests, answers }),
        labels,
    };
    changes.offer(&listed(&registry(false)));
    changes.attached(
        1,
        &registry(false),
        Some(Box::new(target)),
        "Claude Code via MCP (tool list)",
    );
    // Nothing is watched until someone listens.
    changes.listen(&json!("tools"), true);
    output.recv().unwrap();
    assert_eq!(attached.recv().unwrap(), "Claude Code via MCP (tool list)");
    // The cursor first, then the registry, so no change between them is missed.
    assert_eq!(
        asked.recv().unwrap(),
        (
            "events.wait".to_owned(),
            json!({"after": 0, "timeout_ms": 0})
        )
    );
    answer
        .send(Ok(
            json!({"events": [], "current_sequence": 5, "gap": false}),
        ))
        .unwrap();
    assert_eq!(asked.recv().unwrap().0, "schema.list");
    answer.send(Ok(registry(false).schema)).unwrap();
    // An event that changes nothing in the registry is not followed by a reading.
    assert_eq!(
        asked.recv().unwrap(),
        (
            "events.wait".to_owned(),
            json!({"after": 5, "timeout_ms": MAX_EVENT_WAIT_MS})
        )
    );
    answer
        .send(Ok(
            json!({"events": [{"sequence": 6, "method": "edit.set-basic"}],
                        "current_sequence": 6, "gap": false}),
        ))
        .unwrap();
    assert_eq!(asked.recv().unwrap().1["after"], 6);
    answer
        .send(Ok(
            json!({"events": [{"sequence": 7, "method": "flags.set"}],
                        "current_sequence": 7, "gap": false}),
        ))
        .unwrap();
    assert_eq!(asked.recv().unwrap().0, "schema.list");
    answer.send(Ok(registry(true).schema)).unwrap();
    assert_eq!(output.recv().unwrap(), tagged(json!("tools")));
    // The worker calls with the registry the watcher read, once, and only on that attachment.
    assert!(changes.adopt(2).is_none());
    assert_eq!(
        changes.adopt(1).unwrap().schema,
        registry(true).schema,
        "adopted"
    );
    assert!(changes.adopt(1).is_none());
    // The next hold; the watch ends once nothing is served, at its answer.
    assert_eq!(asked.recv().unwrap().1["after"], 7);
    changes.close();
    answer
        .send(Ok(
            json!({"events": [], "current_sequence": 7, "gap": false}),
        ))
        .unwrap();
    changes.join();
    assert!(output.try_recv().is_err(), "the list did not change again");
}

#[test]
fn a_lost_watch_waits_for_the_next_attachment_without_retrying() {
    let (changes, output) = typed_changes();
    let (requests, asked) = channel();
    let (answer, answers) = channel();
    let (labels, attached) = channel();
    changes.initialized(&json!({"jsonrpc": "2.0", "id": 1, "result": {}}));
    output.recv().unwrap();
    changes.attached(
        1,
        &registry(false),
        Some(Box::new(OneConnection {
            connection: Some(Gated { requests, answers }),
            labels,
        })),
        "watch",
    );
    attached.recv().unwrap();
    asked.recv().unwrap();
    answer
        .send(Err(failure("connection-lost", "the editor quit", None)))
        .unwrap();
    // A new attachment of the worker's is watched again.
    let (requests, asked) = channel();
    let (answer, answers) = channel();
    let (labels, attached) = channel();
    changes.attached(
        2,
        &registry(false),
        Some(Box::new(OneConnection {
            connection: Some(Gated { requests, answers }),
            labels,
        })),
        "watch",
    );
    assert_eq!(attached.recv().unwrap(), "watch");
    asked.recv().unwrap();
    changes.close();
    answer
        .send(Err(failure("connection-lost", "closing", None)))
        .unwrap();
    changes.join();
    assert!(output.try_recv().is_err());
}

/// An owner that serves one connection, as a headless child does.
struct Single {
    answers: std::collections::VecDeque<Result<Value, ApiFailure>>,
    calls: Sender<String>,
}

impl Connection for Single {
    fn call(&mut self, method: &str, _params: Value) -> Result<Value, ApiFailure> {
        self.calls.send(method.to_owned()).unwrap();
        self.answers.pop_front().unwrap()
    }

    fn closed(&self) -> bool {
        false
    }
}

struct SingleTarget(Option<Single>);

impl Target for SingleTarget {
    fn attach(&mut self, _label: &str) -> Result<Attached, ApiFailure> {
        Ok(Attached {
            connection: Box::new(self.0.take().unwrap()),
            schema: registry(false).schema,
            catalog: "/catalog.sqlite".into(),
        })
    }

    fn list_changes(&self) -> ListChanges {
        ListChanges::OwnCalls
    }

    fn again(&self) -> Option<Box<dyn Target>> {
        panic!("an owner that serves one connection is never watched")
    }
}

#[test]
fn an_owner_serving_one_connection_reads_the_registry_again_after_its_own_flag_change() {
    let (changes, output) = typed_changes();
    let (calls, called) = channel();
    let mut owner = Owner::new(Box::new(SingleTarget(Some(Single {
        answers: [
            Ok(json!({"flags": []})),
            Ok(registry(true).schema),
            Ok(json!({"name": "c"})),
        ]
        .into(),
        calls,
    }))));
    owner.report_to(changes.clone());
    changes.listen(&json!(1), true);
    output.recv().unwrap();
    let client = ClientInfo::default();
    let (session, _) = owner.session(&client).unwrap();
    changes.offer(&listed(&registry(false)));
    assert!(session.schema["methods"].get("history.list").is_none());
    owner
        .call("flags.set", json!({"flag": "ai.analysis", "value": true}))
        .unwrap();
    assert_eq!(output.recv().unwrap(), tagged(json!(1)));
    let (session, _) = owner.session(&client).unwrap();
    assert!(session.schema["methods"].get("history.list").is_some());
    // A call that cannot change the registry reads nothing more.
    owner.call("catalog.info", json!({})).unwrap();
    assert_eq!(
        called.try_iter().collect::<Vec<_>>(),
        ["flags.set", "schema.list", "catalog.info"]
    );
    changes.close();
    changes.join();
}
