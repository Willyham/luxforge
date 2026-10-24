//! The owner's registered clients: who is connected, through what, how busy each has been and what
//! each calls itself (`docs/design/mcp.md`, "Core additions"). The owner records a client as it
//! registers and forgets it as it disconnects. `session.identify` names the calling client,
//! `session.clients` lists the others and `session.disconnect` closes one, for the desktop's
//! Settings › Agents tab. Only metadata is kept: never what a request asked or answered.
//!
//! A connection through a transport (the loopback listener, a JSON-lines stream) is recorded in the
//! event log as it opens and as it closes, as every identification and every `session.disconnect`
//! is, so a reader follows them with `events.wait` and no polling. A client the host runs in its own
//! process registers no event: the desktop's own client is never listed, and the others are tests'.
use super::{Call, ClientId, Owner};
use crate::{
    Error,
    api::{
        ClientAuthority, ClientTransport, Origin,
        params::{NoParams, host_params},
    },
    editor::now_ms,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// The longest label `session.identify` takes, in characters.
pub(crate) const MAX_CLIENT_LABEL: usize = 64;

/// The event a client's connection is recorded under. No method has this name: no request makes a
/// connection, so its request identity is empty.
pub(crate) const CONNECTED: &str = "session.connect";
/// The event a disconnection is recorded under: with the `session.disconnect` request that closed
/// the client, or with an empty request identity when the client closed its own connection.
pub(crate) const DISCONNECTED: &str = "session.disconnect";

/// What the owner calls to close a transport client's connection, on its own thread, when
/// `session.disconnect` names it. It must not block: a loopback connection shuts its socket down.
pub(crate) type Closer = Box<dyn FnOnce() + Send>;

/// How a client registers: its authority, how it reaches the owner, whether it is the desktop's own
/// client and how its connection is closed.
pub(crate) struct Enrolment {
    pub authority: ClientAuthority,
    pub transport: ClientTransport,
    /// The desktop's own client, which `session.clients` never lists.
    pub desktop: bool,
    /// How to close the client's connection; `None` for a client that has no connection the owner
    /// can close, whose next request is refused instead.
    pub close: Option<Closer>,
}

/// One registered client, as the owner keeps it until the client disconnects.
struct Registered {
    authority: ClientAuthority,
    transport: ClientTransport,
    desktop: bool,
    label: Option<String>,
    connected_at: i64,
    last_request_at: Option<i64>,
    requests: u64,
    close: Option<Closer>,
    /// Closed by `session.disconnect`: its session has ended and it is no longer listed, and every
    /// request it still sends is refused until its transport reports the disconnection.
    closed: bool,
}

impl Registered {
    /// Whether the client's connection and disconnection are recorded in the event log.
    fn announced(&self) -> bool {
        self.transport != ClientTransport::DesktopInternal
    }
}

/// The owner's registered clients.
#[derive(Default)]
pub(super) struct Clients {
    registered: HashMap<ClientId, Registered>,
}

/// One row of `session.clients`.
#[derive(Serialize)]
struct Listed<'a> {
    client_id: ClientId,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    authority: ClientAuthority,
    transport: ClientTransport,
    connected_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_request_at: Option<i64>,
    requests: u64,
}

/// The `data.reason` of the refusal a client closed by `session.disconnect` is answered with for
/// every request it still sends. A JSON-lines stream ends after answering it.
pub(crate) const DISCONNECTED_REASON: &str = "disconnected";

/// Record a client as it registers, and its connection in the event log when a transport made it.
pub(super) fn register(owner: &mut Owner, client: ClientId, enrolment: Enrolment) {
    let Enrolment {
        authority,
        transport,
        desktop,
        close,
    } = enrolment;
    if authority != ClientAuthority::Edit {
        owner.sessions.entry(client).or_default().authority = authority;
    }
    let registered = Registered {
        authority,
        transport,
        desktop,
        label: None,
        connected_at: now_ms(),
        last_request_at: None,
        requests: 0,
        close,
        closed: false,
    };
    if registered.announced() {
        owner.log.record(&Origin::new(CONNECTED, "").client(client));
    }
    owner.clients.registered.insert(client, registered);
}

/// Count one request a client sent, as the owner takes it. A client `session.disconnect` closed is
/// refused instead, with [`DISCONNECTED_REASON`].
pub(super) fn received(owner: &mut Owner, client: ClientId) -> Result<(), Error> {
    let Some(registered) = owner.clients.registered.get_mut(&client) else {
        return Ok(());
    };
    if registered.closed {
        return Err(Error::protocol(
            "this client was disconnected by session.disconnect; open a new connection",
        )
        .with_data(json!({"reason": DISCONNECTED_REASON})));
    }
    registered.requests = registered.requests.saturating_add(1);
    registered.last_request_at = Some(now_ms());
    Ok(())
}

/// Forget a client whose transport closed, or which its host let go, recording its disconnection
/// when its connection was recorded and `session.disconnect` has not already recorded it.
pub(super) fn forget(owner: &mut Owner, client: ClientId) {
    if let Some(registered) = owner.clients.registered.remove(&client)
        && registered.announced()
        && !registered.closed
    {
        owner
            .log
            .record(&Origin::new(DISCONNECTED, "").client(client));
    }
}

host_params! {
    /// `session.identify`.
    pub(in crate::api) struct SessionIdentify {
        label: String = string(MAX_CLIENT_LABEL).notes("what the desktop lists this client as, 1 to 64 characters"),
    }
}

host_params! {
    /// `session.disconnect`.
    pub(in crate::api) struct SessionDisconnect {
        client_id: u64 = sequence().notes("the client to disconnect, as session.clients lists it"),
    }
}

/// `session.identify`: name the calling client for as long as it is connected.
pub(in crate::api) fn identify(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SessionIdentify,
) -> Result<Value, Error> {
    if params.label.trim().is_empty() {
        return Err(Error::validation(
            "parameter label must hold at least one character that is not whitespace",
        ));
    }
    let Some(registered) = owner.clients.registered.get_mut(&call.client) else {
        return Err(Error::protocol(
            "this client is not registered with the owner",
        ));
    };
    registered.label = Some(params.label.clone());
    owner.log.record(&call.origin.clone().client(call.client));
    Ok(json!({"client_id": call.client, "label": params.label}))
}

/// `session.clients`: every other client the owner serves, in the order they connected, without
/// the desktop's own client or one `session.disconnect` has closed.
pub(in crate::api) fn list(
    owner: &mut Owner,
    call: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    let mut listed: Vec<Listed<'_>> = owner
        .clients
        .registered
        .iter()
        .filter(|(client, registered)| {
            **client != call.client && !registered.desktop && !registered.closed
        })
        .map(|(client, registered)| Listed {
            client_id: *client,
            label: registered.label.as_deref(),
            authority: registered.authority,
            transport: registered.transport,
            connected_at: registered.connected_at,
            last_request_at: registered.last_request_at,
            requests: registered.requests,
        })
        .collect();
    listed.sort_unstable_by_key(|row| row.client_id);
    Ok(json!({ "clients": listed }))
}

/// `session.disconnect`: close another client's connection, ending its session as its own
/// disconnection would. Only permission authority may.
pub(in crate::api) fn disconnect(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SessionDisconnect,
) -> Result<Value, Error> {
    if owner.authority(call.client) != ClientAuthority::Permissions {
        return Err(Error::forbidden(
            "disconnecting a client needs permission authority",
        ));
    }
    let target = ClientId(params.client_id);
    if target == call.client {
        return Err(Error::validation(format!(
            "client {} is the calling client; close this connection instead",
            params.client_id
        )));
    }
    let Some(registered) = owner
        .clients
        .registered
        .get_mut(&target)
        .filter(|registered| !registered.desktop && !registered.closed)
    else {
        return Err(Error::validation(format!(
            "unknown client {}; session.clients lists the connected clients",
            params.client_id
        )));
    };
    registered.closed = true;
    // The connection closes first, so nothing more is written to it, not even the failure of a
    // held wait the session's end drops.
    if let Some(close) = registered.close.take() {
        close();
    }
    // The session ends here, exactly as the transport's disconnection ends it: its draft, the
    // `clients` jobs it alone wanted and its held waits go now, whatever the connection does next.
    owner.end_session(target);
    owner.log.record(&call.origin.clone().client(target));
    Ok(json!({ "client_id": target }))
}
