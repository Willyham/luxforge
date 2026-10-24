//! The catalog owner the adapter serves its calls through ([attaching](../../../../docs/design/mcp.md#attaching-to-luxforge)).
//!
//! A [`Target`] says how to reach an owner and a [`Connection`] carries the calls of one attachment.
//! The running desktop's live session is [`LiveTarget`], over a [`LiveClient`]; a headless owner,
//! a `luxforge-json` child spoken to over its standard streams, is another `Target` behind the same
//! two traits. [`Owner`] attaches lazily, at the first call that needs the owner, keeps one
//! connection for as long as it lasts, so session-scoped state carries from call to call, and
//! attaches again after a lost one, warning that the old session's state is gone.
use super::schema::{MethodInputs, SchemaError};
use crate::{
    Paths,
    live::{CONNECTION_LOST, LiveClient, PROTOCOL_ERROR, failure, find_session},
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

/// How the adapter reaches a catalog owner.
pub trait Target: Send {
    /// Connect, naming the adapter `label` to the owner, or say why there is nothing to connect
    /// to.
    fn attach(&mut self, label: &str) -> Result<Attached, ApiFailure>;
}

/// The running desktop's live session: the one the registry of running sessions lists, or the
/// one beside `catalog`, as `luxforge-ctl` finds it.
pub struct LiveTarget {
    pub catalog: Option<PathBuf>,
    pub paths: Option<Paths>,
}

impl Target for LiveTarget {
    fn attach(&mut self, label: &str) -> Result<Attached, ApiFailure> {
        let session = find_session(self.catalog.as_deref(), self.paths.as_ref())?;
        let (client, schema) = LiveClient::connect_as(&session, label)?;
        Ok(Attached {
            connection: Box::new(client),
            schema,
            catalog: session.catalog,
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
}

impl Session {
    /// The registry's entry for `method`.
    pub fn entry(&self, method: &str) -> Option<&Value> {
        self.schema["methods"].get(method)
    }
}

/// The adapter's owner: at most one session at a time, attached when first needed.
pub struct Owner {
    target: Box<dyn Target>,
    session: Option<Session>,
    /// A session was attached before and has ended: the next attachment replaces it.
    ended: bool,
}

impl Owner {
    pub fn new(target: Box<dyn Target>) -> Self {
        Self {
            target,
            session: None,
            ended: false,
        }
    }

    /// The session, attaching first when there is none or the owner has closed the open one.
    /// With it, the [`SESSION_REPLACED`] warning when this attachment replaces an ended one.
    pub fn session(
        &mut self,
        label: &str,
    ) -> Result<(&mut Session, Option<ApiFailure>), ApiFailure> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.connection.closed())
        {
            eprintln!("luxforge-mcp: the live session closed this connection");
            self.end();
        }
        let mut warning = None;
        if self.session.is_none() {
            let attached = self.target.attach(label)?;
            eprintln!(
                "luxforge-mcp: attached to the session of {}",
                attached.catalog.display()
            );
            if self.ended {
                self.ended = false;
                warning = Some(ApiFailure {
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
            self.session = Some(Session {
                connection: attached.connection,
                schema: attached.schema,
                inputs,
                catalog: attached.catalog,
            });
        }
        let session = self.session.as_mut().expect("attached above");
        Ok((session, warning))
    }

    /// End the session: its connection is lost or cannot carry another request. The next call
    /// attaches again.
    pub fn end(&mut self) {
        if self.session.take().is_some() {
            self.ended = true;
        }
    }

    /// Call `method` on the open session, ending the session when its connection fails.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        let Some(session) = self.session.as_mut() else {
            return Err(failure(
                CONNECTION_LOST,
                format!("no connection to Luxforge to send {method} on"),
                None,
            ));
        };
        let answer = session.connection.call(method, params);
        if let Err(error) = &answer
            && (error.code == CONNECTION_LOST || error.code == PROTOCOL_ERROR)
        {
            eprintln!("luxforge-mcp: the connection to Luxforge failed during {method}");
            self.end();
        }
        answer
    }
}
