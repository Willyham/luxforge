use super::{CONNECTION_LOST, LiveSession, STALE_SESSION, failure};
use luxforge_core::{ApiFailure, ApiRequest, ApiResponse};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

/// How long connecting may take. A live session is on this computer, so an answer that has not come
/// within it means nothing is serving the address.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// The longest request line the session accepts, its newline included.
pub const REQUEST_LIMIT: usize = 1024 * 1024;
/// The longest answer line this client reads, its newline included, so its memory stays bounded.
pub const RESPONSE_LIMIT: usize = 64 * 1024 * 1024;

/// One connection to a live session: one API client of the catalog owner, with edit authority, for
/// as long as it is open. Calls are answered in order; there is no deadline on an answer, since a
/// legitimate call, such as a held `job.wait` or a large pixel read, can take as long as it takes.
pub struct LiveClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    token: String,
}

impl LiveClient {
    /// Connect to `session` within [`CONNECT_TIMEOUT`]. An address nothing answers at is a stale
    /// session, never a reason to start another owner.
    pub fn connect(session: &LiveSession) -> Result<Self, ApiFailure> {
        let writer = TcpStream::connect_timeout(&SocketAddr::V4(session.address), CONNECT_TIMEOUT)
            .map_err(|error| {
                failure(
                    STALE_SESSION,
                    format!(
                        "the live session for {} does not answer ({error}); the editor that \
                         opened it may have quit",
                        session.catalog.display()
                    ),
                    Some(json!({"catalog": session.catalog})),
                )
            })?;
        let _ = writer.set_nodelay(true);
        let reader = writer.try_clone().map(BufReader::new).map_err(|error| {
            failure(
                CONNECTION_LOST,
                format!("cannot read the session: {error}"),
                None,
            )
        })?;
        Ok(Self {
            reader,
            writer,
            token: session.token.clone(),
        })
    }

    /// Send one request with a new request identity and the session's token, and read its answer.
    /// An API failure comes back exactly as the owner answered it. A request line over
    /// [`REQUEST_LIMIT`] is refused before anything is written; an answer over [`RESPONSE_LIMIT`]
    /// fails without being kept. A connection that fails once the request is on its way is
    /// [`CONNECTION_LOST`]: whether the owner received the request is unknown.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiFailure> {
        let request = ApiRequest {
            id: uuid::Uuid::new_v4().to_string(),
            method: method.into(),
            params,
            token: Some(self.token.clone()),
        };
        let mut line = serde_json::to_vec(&request)
            .map_err(|error| failure("internal", error.to_string(), None))?;
        line.push(b'\n');
        if line.len() > REQUEST_LIMIT {
            return Err(failure(
                "resource-limit",
                format!(
                    "the {method} request is {} bytes; a request is at most {REQUEST_LIMIT}",
                    line.len()
                ),
                None,
            ));
        }
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
            return Err(failure(
                "resource-limit",
                format!("the {method} answer is longer than {RESPONSE_LIMIT} bytes"),
                None,
            ));
        }
        if answer.last() != Some(&b'\n') {
            return Err(lost("the editor closed the connection".into()));
        }
        let response: ApiResponse = serde_json::from_slice(&answer).map_err(|error| {
            failure(
                "protocol",
                format!("the {method} answer is malformed: {error}"),
                None,
            )
        })?;
        // A request the session could not parse is answered with an empty identity.
        if response.id != request.id && !response.id.is_empty() {
            return Err(failure(
                "protocol",
                format!("the {method} answer is for another request"),
                None,
            ));
        }
        match response.error {
            Some(error) => Err(error),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }
}
