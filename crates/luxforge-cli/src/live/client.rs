use super::{
    CONNECTION_LOST, INTERNAL, LiveSession, PROTOCOL_ERROR, STALE_SESSION, UNSUPPORTED_PROTOCOL,
    failure,
};
use luxforge_core::{ApiFailure, ApiRequest, ApiResponse, MAX_REQUEST_BYTES, PROTOCOL};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

/// How long connecting may take. A live session is on this computer, so an answer that has not come
/// within it means nothing is serving the address.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the first answer, the handshake's `schema.list`, may take. The catalog owner answers it
/// at once, so an address that has not answered within it is not that session, as when another
/// program took the address after the editor quit.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest answer line this client reads, its newline included, so its memory stays bounded.
pub const RESPONSE_LIMIT: usize = 64 * 1024 * 1024;
/// What this client names itself with `session.identify` as it connects, so the desktop lists it.
pub const LABEL: &str = "luxforge-ctl";

/// One connection to a live session: one API client of the catalog owner, with edit authority, for
/// as long as it is open. Calls are answered in order. Only the handshake has a deadline: after it,
/// a legitimate call, such as a held `job.wait` or a large pixel read, can take as long as it
/// takes.
pub struct LiveClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    token: String,
}

impl LiveClient {
    /// Connect to `session` within [`CONNECT_TIMEOUT`] and shake hands: `schema.list` answered
    /// within [`HANDSHAKE_TIMEOUT`] naming this client's protocol, then `session.identify` naming
    /// this client [`LABEL`], answered within the same time. Answers the connection and that
    /// schema. An address nothing answers at, or that does not answer as the session, is a stale
    /// session, never a reason to start another owner; a session that answers another protocol is
    /// [`UNSUPPORTED_PROTOCOL`]. A session that refuses another client answers why, which is kept.
    pub fn connect(session: &LiveSession) -> Result<(Self, Value), ApiFailure> {
        Self::connect_as(session, LABEL)
    }

    /// [`Self::connect`], naming this client `label` with `session.identify` instead of [`LABEL`]:
    /// 1 to 64 characters, as the MCP adapter names itself after the agent it serves.
    pub fn connect_as(session: &LiveSession, label: &str) -> Result<(Self, Value), ApiFailure> {
        Self::connect_within(session, HANDSHAKE_TIMEOUT, label)
    }

    /// [`Self::connect_as`] with the handshake answered within `handshake`.
    pub(super) fn connect_within(
        session: &LiveSession,
        handshake: Duration,
        label: &str,
    ) -> Result<(Self, Value), ApiFailure> {
        let stale = |detail: String| {
            failure(
                STALE_SESSION,
                format!(
                    "the live session for {} does not answer ({detail}); the editor that opened it \
                     may have quit",
                    session.catalog.display()
                ),
                Some(json!({"catalog": session.catalog})),
            )
        };
        let writer = TcpStream::connect_timeout(&SocketAddr::V4(session.address), CONNECT_TIMEOUT)
            .map_err(|error| stale(error.to_string()))?;
        let _ = writer.set_nodelay(true);
        let reader = writer
            .try_clone()
            .map(BufReader::new)
            .map_err(|error| stale(error.to_string()))?;
        let mut client = Self {
            reader,
            writer,
            token: session.token.clone(),
        };
        client
            .writer
            .set_read_timeout(Some(handshake))
            .map_err(|error| stale(error.to_string()))?;
        // The owner's own refusal, such as a session already serving every client it may, is an
        // answer from the session and is kept; anything else is not the session talking.
        let answered = |error: ApiFailure| {
            if error.code == CONNECTION_LOST || error.code == PROTOCOL_ERROR {
                stale(error.message)
            } else {
                error
            }
        };
        let schema = client.call("schema.list", json!({})).map_err(answered)?;
        if schema["protocol"] != PROTOCOL {
            return Err(failure(
                UNSUPPORTED_PROTOCOL,
                format!(
                    "the live session speaks {}; this luxforge-ctl speaks {PROTOCOL}",
                    schema["protocol"]
                ),
                Some(json!({"catalog": session.catalog})),
            ));
        }
        // Named in the desktop's list of connected clients for as long as the connection is open.
        client
            .call("session.identify", json!({"label": label}))
            .map_err(answered)?;
        client
            .writer
            .set_read_timeout(None)
            .map_err(|error| stale(error.to_string()))?;
        Ok((client, schema))
    }

    /// Whether the session has closed this connection while it was idle, as when the editor quit
    /// or restarted since the last answer: read without waiting, so nothing is sent to learn it. A
    /// connection that cannot be inspected counts as closed.
    pub fn closed(&self) -> bool {
        if !self.reader.buffer().is_empty() {
            return false;
        }
        if self.writer.set_nonblocking(true).is_err() {
            return true;
        }
        let peeked = self.writer.peek(&mut [0; 1]);
        let restored = self.writer.set_nonblocking(false).is_ok();
        match peeked {
            Ok(0) => true,
            Ok(_) => !restored,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => !restored,
            Err(_) => true,
        }
    }

    /// Send one request with a new request identity and the session's token, and read its answer.
    /// An API failure comes back exactly as the owner answered it. A request line over
    /// [`MAX_REQUEST_BYTES`] is refused before anything is written; an answer over
    /// [`RESPONSE_LIMIT`] fails without being kept. A connection that fails once the request is on
    /// its way is [`CONNECTION_LOST`]: whether the owner received the request is unknown.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        let (id, line) = request_line(method, params, Some(self.token.clone()))?;
        let lost = |detail: String| {
            failure(
                CONNECTION_LOST,
                format!("the connection to the live session failed during {method}: {detail}"),
                None,
            )
        };
        self.writer
            .write_all(&line)
            .and_then(|()| self.writer.flush())
            .map_err(|error| lost(error.to_string()))?;
        let mut answer = Vec::new();
        self.reader
            .by_ref()
            .take(RESPONSE_LIMIT as u64 + 1)
            .read_until(b'\n', &mut answer)
            .map_err(|error| lost(error.to_string()))?;
        if answer.len() > RESPONSE_LIMIT {
            return Err(too_long(method));
        }
        if answer.last() != Some(&b'\n') {
            return Err(lost("the editor closed the connection".into()));
        }
        read_answer(method, &id, &answer)
    }
}

/// One request line for `method` with a new request identity and, over a live session, its token:
/// the identity and the line, its newline included. A line over [`MAX_REQUEST_BYTES`] is refused
/// before anything is written.
pub(crate) fn request_line(
    method: &str,
    params: Value,
    token: Option<String>,
) -> Result<(String, Vec<u8>), ApiFailure> {
    let request = ApiRequest {
        id: uuid::Uuid::new_v4().to_string(),
        method: method.into(),
        params,
        token,
    };
    let mut line =
        serde_json::to_vec(&request).map_err(|error| failure(INTERNAL, error.to_string(), None))?;
    line.push(b'\n');
    if line.len() > MAX_REQUEST_BYTES {
        return Err(failure(
            "resource-limit",
            format!(
                "the {method} request is {} bytes; a request is at most {MAX_REQUEST_BYTES}",
                line.len()
            ),
            None,
        ));
    }
    Ok((request.id, line))
}

/// The failure for an answer to `method` longer than [`RESPONSE_LIMIT`], which is not kept.
pub(crate) fn too_long(method: &str) -> ApiFailure {
    failure(
        "resource-limit",
        format!("the {method} answer is longer than {RESPONSE_LIMIT} bytes"),
        None,
    )
}

/// Read `answer`, one whole line, as the owner's answer to the request `id` for `method`: its
/// result, or its failure exactly as the owner gave it. A malformed answer, or one for another
/// request, is [`PROTOCOL_ERROR`].
pub(crate) fn read_answer(method: &str, id: &str, answer: &[u8]) -> Result<Value, ApiFailure> {
    let response: ApiResponse = serde_json::from_slice(answer).map_err(|error| {
        failure(
            PROTOCOL_ERROR,
            format!("the {method} answer is malformed: {error}"),
            None,
        )
    })?;
    // A request the owner could not parse, or a connection it refused, is answered with an empty
    // identity.
    if response.id != id && !response.id.is_empty() {
        return Err(failure(
            PROTOCOL_ERROR,
            format!("the {method} answer is for another request"),
            None,
        ));
    }
    match response.error {
        Some(error) => Err(error),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}
