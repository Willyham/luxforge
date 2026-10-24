//! A typed profile's list changes ([the typed profiles](../../../../docs/design/mcp.md#the-typed-profiles)).
//!
//! The typed tool list is generated from the registry the adapter is attached to, or the built-in
//! one without a session, so it changes when the adapter attaches and when a flag or a module
//! changes the live registry. [`Changes`] keeps the list last offered, compares each newly
//! generated list with it, and announces a difference to whoever listens: each open modern
//! subscription that asked for `toolsListChanged`, with its tag, and a legacy client after
//! `initialize`, untagged.
//!
//! How a change is learnt is the target's [`ListChanges`](super::owner::ListChanges). For the
//! running desktop (`Watch`), once someone listens, a watcher thread attaches a second connection
//! to the same session ([`Target::again`]), under the adapter's label marked "(tool list)", and
//! holds `events.wait` (at most 30 s a hold, held again; no polling). After a
//! [`REGISTRY_CHANGES`] event, or a gap in the log, it reads `schema.list` again and publishes it:
//! the worker adopts it before its next call, and the list is announced if it differs. The
//! watcher attaches once per attachment of the worker's own connection, so a desktop that quits
//! leaves it waiting, without retrying, for the next one. A headless owner (`OwnCalls`) serves
//! one connection and is never watched: [`Owner`](super::owner::Owner) reads its registry again
//! after the adapter's own calls and reports it through [`Changes::read_again`].
use super::{
    owner::{Connection, REGISTRY_CHANGES, Target},
    protocol::{self, SUBSCRIPTION_ID},
    schema::{MethodInputs, SchemaError},
    serve::{Output, Subscriptions},
    tools::{self, Namespaces},
};
use luxforge_core::MAX_EVENT_WAIT_MS;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
};

/// One registry reading: `schema.list` and its generated inputs.
pub struct Registry {
    pub schema: Value,
    pub inputs: Result<MethodInputs, SchemaError>,
}

/// The list changes of one launch. A clone shares them. In the compact profile it only
/// acknowledges subscriptions and writes the legacy handshake's answer: the list never changes.
#[derive(Clone)]
pub struct Changes(Arc<Inner>);

struct Inner {
    /// The typed profile's namespaces; `None` for the compact profile.
    namespaces: Option<Namespaces>,
    output: Output,
    subscriptions: Subscriptions,
    state: Mutex<State>,
    /// Wakes the watcher when there is something new to watch or someone new listens.
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// The typed list last offered: answered to `tools/list`, or generated from an attachment's
    /// registry. `None` until there is one.
    offered: Option<Vec<Value>>,
    /// A legacy client initialized: it is told of changes untagged.
    legacy: bool,
    /// The worker's newest attachment.
    attachment: u64,
    /// How that attachment's changes are learnt on a second connection, and the label it names
    /// itself with; `None` when the owner serves one connection, or before any attachment.
    target: Option<(Box<dyn Target>, String)>,
    /// A registry the watcher read for an attachment, until the worker adopts it.
    pending: Option<(u64, Registry)>,
    watcher: Option<JoinHandle<()>>,
    closed: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Changes {
    /// The changes of a launch offering `namespaces` (a typed profile) or the compact tools
    /// (`None`), written to `output`, to the open `subscriptions`.
    pub fn new(
        namespaces: Option<Namespaces>,
        output: Output,
        subscriptions: Subscriptions,
    ) -> Self {
        Self(Arc::new(Inner {
            namespaces,
            output,
            subscriptions,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }))
    }

    /// Whether the list can change: a typed profile.
    pub fn typed(&self) -> bool {
        self.0.namespaces.is_some()
    }

    /// `subscriptions/listen` `id`, asking for `toolsListChanged` when `tools`: acknowledged with
    /// what is honoured, `toolsListChanged` only in a typed profile, and open until the client
    /// cancels it. Nothing on it is written before its acknowledgement.
    pub fn listen(&self, id: &Value, tools: bool) {
        let honoured = tools && self.typed();
        let acknowledged = protocol::notification(
            "notifications/subscriptions/acknowledged",
            json!({
                "_meta": {SUBSCRIPTION_ID: id},
                "notifications": if honoured { json!({"toolsListChanged": true}) } else { json!({}) },
            }),
        );
        self.0
            .subscriptions
            .open(id, honoured, || self.0.output.write(&acknowledged));
        if honoured {
            self.listening();
        }
    }

    /// Write `initialize`'s `answer`; a typed profile then tells this legacy client of changes,
    /// never before its answer.
    pub fn initialized(&self, answer: &Value) {
        let mut state = lock(&self.0.state);
        self.0.output.write(answer);
        if self.typed() {
            state.legacy = true;
            drop(state);
            self.listening();
        }
    }

    /// Someone listens: start the watcher, once, and wake it.
    fn listening(&self) {
        let mut state = lock(&self.0.state);
        if state.watcher.is_none() && !state.closed {
            let inner = self.0.clone();
            state.watcher = Some(
                std::thread::Builder::new()
                    .name("luxforge-mcp-watch".into())
                    .spawn(move || watch(&inner))
                    .expect("the watcher thread starts"),
            );
        }
        self.0.wake.notify_all();
    }

    /// The list `tools/list` answered: the one the client now holds.
    pub fn offer(&self, tools: &[Value]) {
        if self.typed() {
            lock(&self.0.state).offered = Some(tools.to_vec());
        }
    }

    /// The worker attached (its `attachment`th time) and read `registry`; `again` reaches the same
    /// owner for the watch's connection, which names itself `label`, and is `None` where nothing
    /// is watched. A list that differs from the one offered is announced.
    pub fn attached(
        &self,
        attachment: u64,
        registry: &Registry,
        again: Option<Box<dyn Target>>,
        label: &str,
    ) {
        if !self.typed() {
            return;
        }
        let tools = self.0.generate(registry);
        let mut state = lock(&self.0.state);
        state.attachment = attachment;
        state.pending = None;
        state.target = again.map(|target| (target, label.to_owned()));
        self.0.compare(&mut state, tools);
        self.0.wake.notify_all();
    }

    /// The newer registry the watcher read for `attachment`, for the worker to call with.
    pub fn adopt(&self, attachment: u64) -> Option<Registry> {
        let mut state = lock(&self.0.state);
        if state
            .pending
            .as_ref()
            .is_some_and(|(read, _)| *read == attachment)
        {
            state.pending.take().map(|(_, registry)| registry)
        } else {
            None
        }
    }

    /// The worker read `registry` again on `attachment`'s own connection, after a call of its own
    /// that can change it.
    pub fn read_again(&self, attachment: u64, registry: &Registry) {
        let tools = self.0.generate(registry);
        let mut state = lock(&self.0.state);
        if state.attachment == attachment {
            self.0.compare(&mut state, tools);
        }
    }

    /// No more is served: the watcher ends at its next wake, or with the process while it holds a
    /// wait.
    pub fn close(&self) {
        lock(&self.0.state).closed = true;
        self.0.wake.notify_all();
    }

    /// Wait for the watcher to end, after [`Self::close`] and its connection's last answer.
    #[cfg(test)]
    pub fn join(&self) {
        let watcher = lock(&self.0.state).watcher.take();
        if let Some(watcher) = watcher {
            watcher.join().unwrap();
        }
    }
}

impl Inner {
    /// The typed list `registry` generates; a registry the adapter cannot offer generates none.
    fn generate(&self, registry: &Registry) -> Vec<Value> {
        let Some(namespaces) = &self.namespaces else {
            return Vec::new();
        };
        match &registry.inputs {
            Ok(inputs) => tools::typed(namespaces, &registry.schema, inputs).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Offer `tools`, announcing them when they differ from the list offered before.
    fn compare(&self, state: &mut State, tools: Vec<Value>) {
        let changed = state
            .offered
            .as_ref()
            .is_some_and(|offered| *offered != tools);
        state.offered = Some(tools);
        if changed {
            if state.legacy {
                self.output.write(&protocol::tools_changed(None));
            }
            self.subscriptions
                .each_tools(|id| self.output.write(&protocol::tools_changed(Some(id))));
        }
    }

    fn listening(&self, state: &State) -> bool {
        state.legacy || self.subscriptions.tools()
    }

    /// The watcher read `registry` for `attachment`: the worker adopts it, and the list is
    /// compared.
    fn publish(&self, attachment: u64, registry: Registry) {
        let tools = self.generate(&registry);
        let mut state = lock(&self.state);
        if state.attachment != attachment {
            return;
        }
        state.pending = Some((attachment, registry));
        self.compare(&mut state, tools);
    }

    /// Whether the watcher should keep watching `attachment`.
    fn current(&self, attachment: u64) -> bool {
        let state = lock(&self.state);
        !state.closed && state.attachment == attachment && self.listening(&state)
    }
}

/// The watcher: for each attachment of the worker while someone listens, a second connection
/// holding `events.wait`.
fn watch(inner: &Inner) {
    // The attachment whose second connection failed, which is not tried again.
    let mut failed = 0;
    loop {
        let (attachment, mut target, label) = {
            let mut state = lock(&inner.state);
            loop {
                if state.closed {
                    return;
                }
                if state.attachment != failed
                    && inner.listening(&state)
                    && let Some((target, label)) = state.target.take()
                {
                    break (state.attachment, target, label);
                }
                state = inner
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let attached = target.attach(&label);
        {
            let mut state = lock(&inner.state);
            if state.attachment == attachment && state.target.is_none() {
                state.target = Some((target, label));
            }
        }
        let mut connection = match attached {
            Ok(attached) => attached.connection,
            Err(error) => {
                eprintln!(
                    "luxforge-mcp: the tool list cannot be watched: {}",
                    error.message
                );
                failed = attachment;
                continue;
            }
        };
        if follow(inner, attachment, connection.as_mut()).is_err() {
            failed = attachment;
        }
    }
}

/// Follow the event log on `connection` while `attachment` is current and someone listens. The
/// cursor is read first and the registry after it, so a change between the two is in the
/// registry read and none after it is missed. `Err` when the connection fails.
fn follow(inner: &Inner, attachment: u64, connection: &mut dyn Connection) -> Result<(), ()> {
    let failed = |error: luxforge_core::ApiFailure| {
        eprintln!(
            "luxforge-mcp: the tool list's watch ended: {}",
            error.message
        );
    };
    let now = connection
        .call("events.wait", json!({"after": 0, "timeout_ms": 0}))
        .map_err(failed)?;
    let mut after = now["current_sequence"].as_u64().unwrap_or_default();
    read_registry(inner, attachment, connection).map_err(failed)?;
    while inner.current(attachment) {
        let waited = connection
            .call(
                "events.wait",
                json!({"after": after, "timeout_ms": MAX_EVENT_WAIT_MS}),
            )
            .map_err(failed)?;
        after = waited["current_sequence"].as_u64().unwrap_or(after);
        let relevant = waited["gap"] == json!(true)
            || waited["events"].as_array().is_some_and(|events| {
                events.iter().any(|event| {
                    event["method"]
                        .as_str()
                        .is_some_and(|method| REGISTRY_CHANGES.contains(&method))
                })
            });
        if relevant && inner.current(attachment) {
            read_registry(inner, attachment, connection).map_err(failed)?;
        }
    }
    Ok(())
}

fn read_registry(
    inner: &Inner,
    attachment: u64,
    connection: &mut dyn Connection,
) -> Result<(), luxforge_core::ApiFailure> {
    let schema = connection.call("schema.list", json!({}))?;
    let inputs = MethodInputs::read(&schema);
    inner.publish(attachment, Registry { schema, inputs });
    Ok(())
}

#[cfg(test)]
mod tests;
