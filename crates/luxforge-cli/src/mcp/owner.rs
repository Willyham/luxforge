//! The catalog owner the adapter serves its calls through ([attaching](../../../../docs/design/mcp.md#attaching-to-luxforge)).
//!
//! A [`Target`] says how to reach an owner and a [`Connection`] carries the calls of one attachment.
//! The running desktop's live session is [`LiveTarget`], over a [`LiveClient`]; a headless owner,
//! a `luxforge-json` child spoken to over its standard streams, is another `Target` behind the same
//! two traits. [`Owner`] attaches lazily, at the first call that needs the owner, keeps one
//! connection for as long as it lasts, so session-scoped state carries from call to call, and
//! attaches again after a lost one, warning that the old session's state is gone.
//!
//! Each target also says how its registry's changes are learnt ([`ListChanges`]): on a second
//! connection to the same owner ([`Target::again`]), or, for an owner that serves one connection,
//! after the adapter's own calls that can change it. A typed profile's [`Changes`] announces them.
use super::{
    changes::{Changes, Registry},
    protocol::ClientInfo,
    schema::{MethodInputs, SchemaError},
};
use crate::{
    Paths,
    live::{CONNECTION_LOST, LiveClient, LiveSession, PROTOCOL_ERROR, failure, find_session},
};
use luxforge_core::ApiFailure;
use serde_json::{Value, json};
use std::path::PathBuf;

/// The warning code on the first result after the adapter attached again: the session it had
/// before, with its drafts, previews, selections and `clients` jobs, ended with its connection.
pub const SESSION_REPLACED: &str = "session-replaced";

/// One connection to a catalog owner, answering calls in order.
pub trait Connection: Send {
    /// Send one request and read its answer, exactly as the owner gives it. A connection that
    /// fails once the request is on its way is [`CONNECTION_LOST`].
    fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure>;

    /// Whether the owner closed the connection while it was idle, learnt without sending
    /// anything.
    fn closed(&self) -> bool;
}

impl Connection for LiveClient {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        LiveClient::call(self, method, params)
    }

    fn closed(&self) -> bool {
        LiveClient::closed(self)
    }
}

/// A new connection and what its handshake read.
pub struct Attached {
    pub connection: Box<dyn Connection>,
    /// The owner's `schema.list`.
    pub schema: Value,
    /// The catalog the owner has open.
    pub catalog: PathBuf,
}

/// The methods whose success can change the owner's registry, and so the tools generated from it
/// ([list changes](../../../../docs/design/mcp.md#the-typed-profiles)). The registry has no
/// module-enablement method today.
pub const REGISTRY_CHANGES: &[&str] = &["flags.set"];

/// How the adapter learns that the owner's registry changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListChanges {
    /// Other clients share the owner and can change its registry: the adapter watches the owner's
    /// event log, on a connection of its own from another [`Target::attach`].
    Watch,
    /// The adapter is the owner's only client and the owner serves no second connection: the
    /// registry changes only through the adapter's own calls, so [`Owner::call`] reads
    /// `schema.list` again after each of its own successful calls to a [`REGISTRY_CHANGES`]
    /// method, and nothing watches.
    OwnCalls,
}

/// How the adapter reaches a catalog owner.
pub trait Target: Send {
    /// Connect, naming the adapter `label` to the owner, or say why there is nothing to connect
    /// to.
    fn attach(&mut self, label: &str) -> Result<Attached, ApiFailure>;

    /// How changes to this owner's registry reach the adapter.
    fn list_changes(&self) -> ListChanges {
        ListChanges::Watch
    }

    /// With [`ListChanges::Watch`], a target that reaches the owner of the last attachment again,
    /// for the watch's own connection: the same owner, never another found since. `None` when
    /// there is none to reach, and nothing is watched.
    fn again(&self) -> Option<Box<dyn Target>> {
        None
    }
}

/// The running desktop's live session: the one the registry of running sessions lists, or the
/// one beside `catalog`, as `luxforge-ctl` finds it.
pub struct LiveTarget {
    pub catalog: Option<PathBuf>,
    pub paths: Option<Paths>,
    /// The session the last attachment found.
    pub found: Option<LiveSession>,
}

impl Target for LiveTarget {
    fn attach(&mut self, label: &str) -> Result<Attached, ApiFailure> {
        let session = find_session(self.catalog.as_deref(), self.paths.as_ref())?;
        self.found = Some(session.clone());
        SessionTarget(session).attach(label)
    }

    fn again(&self) -> Option<Box<dyn Target>> {
        let found = self.found.clone()?;
        Some(Box::new(SessionTarget(found)))
    }
}

/// One live session found already: the second connection of an attachment reaches the same
/// session the first did, never another the registry lists by then.
struct SessionTarget(LiveSession);

impl Target for SessionTarget {
    fn attach(&mut self, label: &str) -> Result<Attached, ApiFailure> {
        let (client, schema) = LiveClient::connect_as(&self.0, label)?;
        Ok(Attached {
            connection: Box::new(client),
            schema,
            catalog: self.0.catalog.clone(),
        })
    }
}

/// One attachment: its connection, the registry it read and that registry's generated inputs.
pub struct Session {
    pub connection: Box<dyn Connection>,
    pub schema: Value,
    /// The generated inputs, or why the registry cannot be offered: one entry the adapter cannot
    /// read refuses the whole answer.
    pub inputs: Result<MethodInputs, SchemaError>,
    pub catalog: PathBuf,
    /// Which attachment this is, counted from 1 for the life of the adapter.
    attachment: u64,
}

impl Session {
    /// The registry's entry for `method`.
    pub fn entry(&self, method: &str) -> Option<&Value> {
        self.schema["methods"].get(method)
    }

    fn adopt(&mut self, registry: Registry) {
        self.schema = registry.schema;
        self.inputs = registry.inputs;
    }
}

/// The adapter's owner: at most one session at a time, attached when first needed.
pub struct Owner {
    target: Box<dyn Target>,
    session: Option<Session>,
    /// A session was attached before and has ended: the next attachment replaces it.
    ended: bool,
    /// How many attachments there have been.
    attachments: u64,
    /// Where a typed profile learns of each attachment's registry and of its changes; `None`
    /// where nothing listens, as in the compact tools' own tests.
    changes: Option<Changes>,
    /// Warnings an answer that carries none (a tool list) left for the next result.
    kept: Vec<ApiFailure>,
}

impl Owner {
    pub fn new(target: Box<dyn Target>) -> Self {
        Self {
            target,
            session: None,
            ended: false,
            attachments: 0,
            changes: None,
            kept: Vec::new(),
        }
    }

    /// Report each attachment's registry, and the newer ones learnt while it lasts, to `changes`.
    pub fn report_to(&mut self, changes: Changes) {
        self.changes = Some(changes);
    }

    /// The session, attaching first when there is none or the owner has closed the open one,
    /// naming the adapter after `client`. With it, the [`SESSION_REPLACED`] warning when this
    /// attachment replaces an ended one, and any [kept](Self::keep_warnings) before.
    pub fn session(
        &mut self,
        client: &ClientInfo,
    ) -> Result<(&mut Session, Vec<ApiFailure>), ApiFailure> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.connection.closed())
        {
            eprintln!("luxforge-mcp: the catalog owner closed this connection");
            self.end();
        }
        let mut warnings = std::mem::take(&mut self.kept);
        if self.session.is_none() {
            let attached = match self.target.attach(&client.label()) {
                Ok(attached) => attached,
                Err(error) => {
                    self.kept = warnings;
                    return Err(error);
                }
            };
            eprintln!(
                "luxforge-mcp: attached to the session of {}",
                attached.catalog.display()
            );
            if self.ended {
                self.ended = false;
                warnings.push(ApiFailure {
                    code: SESSION_REPLACED.into(),
                    message: "the connection to Luxforge ended and this call attached again: the \
                              drafts, previews, selections and jobs owned by clients of the old \
                              connection are gone; read state again before relying on it"
                        .into(),
                    job_id: None,
                    data: Some(json!({"catalog": attached.catalog})),
                });
            }
            let inputs = MethodInputs::read(&attached.schema);
            if let Err(error) = &inputs {
                eprintln!("luxforge-mcp: the session's registry cannot be offered: {error}");
            }
            self.attachments += 1;
            let registry = Registry {
                schema: attached.schema,
                inputs,
            };
            if let Some(changes) = &self.changes {
                // An owner the adapter alone calls is never watched: its own calls are read.
                let again = match self.target.list_changes() {
                    ListChanges::Watch => self.target.again(),
                    ListChanges::OwnCalls => None,
                };
                changes.attached(self.attachments, &registry, again, &client.watch_label());
            }
            self.session = Some(Session {
                connection: attached.connection,
                schema: registry.schema,
                inputs: registry.inputs,
                catalog: attached.catalog,
                attachment: self.attachments,
            });
        }
        let session = self.session.as_mut().expect("attached above");
        // A newer registry the second connection read since the last call.
        if let Some(registry) = self
            .changes
            .as_ref()
            .and_then(|changes| changes.adopt(session.attachment))
        {
            session.adopt(registry);
        }
        Ok((session, warnings))
    }

    /// Keep `warnings` for the next result that can carry them.
    pub fn keep_warnings(&mut self, warnings: Vec<ApiFailure>) {
        self.kept.extend(warnings);
    }

    /// End the session: its connection is lost or cannot carry another request. The next call
    /// attaches again.
    pub fn end(&mut self) {
        if self.session.take().is_some() {
            self.ended = true;
        }
    }

    /// Call `method` on the open session, ending the session when its connection fails. With
    /// [`ListChanges::OwnCalls`], a successful call to a [`REGISTRY_CHANGES`] method reads the
    /// registry again.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        let Some(session) = self.session.as_mut() else {
            return Err(failure(
                CONNECTION_LOST,
                format!("no connection to Luxforge to send {method} on"),
                None,
            ));
        };
        let answer = session.connection.call(method, params);
        match &answer {
            Err(error) if error.code == CONNECTION_LOST || error.code == PROTOCOL_ERROR => {
                eprintln!("luxforge-mcp: the connection to Luxforge failed during {method}");
                self.end();
            }
            Ok(_)
                if self.target.list_changes() == ListChanges::OwnCalls
                    && REGISTRY_CHANGES.contains(&method) =>
            {
                self.read_registry(method);
            }
            _ => {}
        }
        answer
    }

    /// Read the open session's registry again after its own `cause` succeeded, keeping the one it
    /// had when the read fails; a failed connection ends the session as any call's does.
    fn read_registry(&mut self, cause: &str) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.connection.call("schema.list", json!({})) {
            Ok(schema) => {
                if schema != session.schema {
                    eprintln!("luxforge-mcp: the registry changed after {cause}");
                    let inputs = MethodInputs::read(&schema);
                    if let Err(error) = &inputs {
                        eprintln!(
                            "luxforge-mcp: the session's registry cannot be offered: {error}"
                        );
                    }
                    let registry = Registry { schema, inputs };
                    // A typed profile announces the list when it changed.
                    if let Some(changes) = &self.changes {
                        changes.read_again(session.attachment, &registry);
                    }
                    session.adopt(registry);
                }
            }
            Err(error) => {
                eprintln!(
                    "luxforge-mcp: the registry could not be read again after {cause}: {}",
                    error.message
                );
                if error.code == CONNECTION_LOST || error.code == PROTOCOL_ERROR {
                    self.end();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
