//! Loopback JSON-lines transport: one line per request and response, a per-run token and a
//! session file for discovery. The same framing serves stdin/stdout for headless use.
use super::{
    ApiRequest, ApiResponse, ClientAuthority, ClientId, ClientTransport, OwnerHandle, PROTOCOL,
    live_sessions::{RegistryEntry, write_private},
    owner::clients::{Closer, DISCONNECTED_REASON},
};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, BufWriter, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

/// The longest request line a session reads, its newline included; a longer one is refused.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// How many loopback connections are served at once; the tile service's queue is sized from it.
pub(super) const MAX_CLIENTS: usize = 8;
/// How long the listener spends telling a connection past [`MAX_CLIENTS`] why it is refused.
const REFUSAL_WRITE: Duration = Duration::from_millis(250);

/// What a live-session file records: the protocol, the loopback address and the per-run token a
/// client sends with every request. The desktop writes it; a live client such as `luxforge-ctl`
/// reads it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSessionInfo {
    pub protocol: String,
    pub address: SocketAddr,
    pub token: String,
}

/// The live-session file beside `catalog`, which the owner of that catalog's loopback session writes
/// and removes: `catalog.sqlite` has `catalog.live-session.json`.
pub fn live_session_file(catalog: &Path) -> PathBuf {
    catalog.with_extension("live-session.json")
}

struct Connection {
    socket: Arc<TcpStream>,
    join: JoinHandle<()>,
}

pub struct LocalServer {
    info: LocalSessionInfo,
    session_file: PathBuf,
    /// The session's entry in the per-user registry of running sessions, once registered.
    registration: Option<RegistryEntry>,
    stopping: Arc<AtomicBool>,
    /// The same counter the accept loop keeps, so the status bar can report live clients.
    clients: Arc<AtomicUsize>,
    /// Sockets and workers still served by this listener, stopped and joined before it drops.
    connections: Arc<Mutex<Vec<Connection>>>,
    join: Option<JoinHandle<()>>,
}

impl LocalServer {
    /// Serve `owner` on a new loopback address with a new token, recorded in `session_file`, which
    /// is written whole and owner-only and replaces any file there; dropping the server removes it.
    pub fn start(owner: OwnerHandle, session_file: &Path) -> Result<Self, Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| Error::protocol(error.to_string()))?;
        let info = LocalSessionInfo {
            protocol: PROTOCOL.into(),
            address: listener
                .local_addr()
                .map_err(|error| Error::protocol(error.to_string()))?,
            token: uuid::Uuid::new_v4().simple().to_string(),
        };
        write_session_file(session_file, &info)?;
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let token = info.token.clone();
        let clients = Arc::new(AtomicUsize::new(0));
        let connected = clients.clone();
        let connections = Arc::new(Mutex::new(Vec::<Connection>::new()));
        let served = connections.clone();
        // Blocking accept keeps the idle listener asleep; Drop wakes it with one loopback connection.
        let join = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { break };
                // Completed workers keep no socket or thread handle across further accepts.
                let mut connections = served
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                connections.retain(|connection| !connection.join.is_finished());
                // The display counter reaches zero just before a worker returns. Admission uses
                // retained handles instead, so even that finish race keeps the shutdown set bounded.
                if connections.len() >= MAX_CLIENTS {
                    drop(connections);
                    refuse(stream);
                    continue;
                }
                clients.fetch_add(1, Ordering::AcqRel);
                let Ok(socket) = stream.try_clone() else {
                    clients.fetch_sub(1, Ordering::AcqRel);
                    continue;
                };
                let socket = Arc::new(socket);
                let closing = socket.clone();
                let owner = owner.clone();
                let token = token.clone();
                let clients = clients.clone();
                let join = std::thread::spawn(move || {
                    let _ = serve_stream(stream, &owner, Some(&token));
                    // The listener retains a clone for shutdown; it must not keep a completed
                    // connection open until the next accept reaps its worker.
                    let _ = closing.shutdown(std::net::Shutdown::Both);
                    clients.fetch_sub(1, Ordering::AcqRel);
                });
                connections.push(Connection { socket, join });
            }
        });
        Ok(Self {
            info,
            session_file: session_file.into(),
            registration: None,
            stopping,
            clients: connected,
            connections,
            join: Some(join),
        })
    }
    #[cfg(test)]
    pub(crate) fn info(&self) -> &LocalSessionInfo {
        &self.info
    }
    /// Live connections served by this listener, counted as they are accepted and released.
    pub fn connected(&self) -> usize {
        self.clients.load(Ordering::Acquire)
    }

    /// Register this session, serving `catalog`, in the per-user registry `dir`
    /// (`<config>/live-sessions`), so a live client finds the running desktop's catalog without
    /// naming it ([`super::running_sessions`]). The entry is removed when the server drops. A
    /// failure leaves the session served and findable through its session file alone.
    pub fn register(&mut self, dir: &Path, catalog: &Path) -> Result<(), Error> {
        self.registration = Some(RegistryEntry::register(dir, catalog, &self.session_file)?);
        Ok(())
    }
}

/// Answer a connection past [`MAX_CLIENTS`] with one `resource-limit` failure naming the limit
/// before closing it, so its client can tell a full session from one that has gone. The write is
/// bounded, and a client that reads nothing only loses its answer. What the client already sent is
/// read and dropped until it closes, for at most [`REFUSAL_WRITE`] in all: closing a socket with
/// unread input resets it, which can discard the answer before the client reads it.
fn refuse(mut stream: TcpStream) {
    let failure = Error::resource_limit(format!(
        "the live session already serves {MAX_CLIENTS} clients; try again once one disconnects"
    ))
    .with_data(serde_json::json!({"max_clients": MAX_CLIENTS}));
    let Ok(mut line) = serde_json::to_vec(&ApiResponse::failure(String::new(), 0, failure)) else {
        return;
    };
    line.push(b'\n');
    let deadline = std::time::Instant::now() + REFUSAL_WRITE;
    let _ = stream.set_write_timeout(Some(REFUSAL_WRITE));
    if stream.write_all(&line).is_err() || stream.shutdown(std::net::Shutdown::Write).is_err() {
        return;
    }
    let mut discard = [0_u8; 4096];
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now())
        && !left.is_zero()
        && stream.set_read_timeout(Some(left)).is_ok()
        && matches!(stream.read(&mut discard), Ok(count) if count > 0)
    {}
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(join) = self.join.take()
            && TcpStream::connect_timeout(&self.info.address, Duration::from_millis(250)).is_ok()
        {
            let _ = join.join();
        }
        let connections = std::mem::take(
            &mut *self
                .connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        // EOF wakes both idle readers and connections parked on an owner reply. Shutdown also
        // releases a writer whose client stopped reading, before any worker is joined.
        for connection in &connections {
            let _ = connection.socket.shutdown(std::net::Shutdown::Both);
        }
        for connection in connections {
            let _ = connection.join.join();
        }
        // The registry entry goes first, so no reader finds an entry whose session file is gone.
        drop(self.registration.take());
        let _ = std::fs::remove_file(&self.session_file);
    }
}

fn write_session_file(path: &Path, info: &LocalSessionInfo) -> Result<(), Error> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| Error::protocol(error.to_string()))?;
    }
    let bytes = serde_json::to_vec(info).map_err(|error| Error::protocol(error.to_string()))?;
    write_private(path, &bytes).map_err(|error| {
        Error::protocol(format!(
            "cannot write the live session file: {}",
            error.kind()
        ))
    })
}

/// Serve one client over JSON lines with the authority its process was started with. Only a local
/// process the person started explicitly for setup, such as `luxforge-json
/// --permission-authority`, passes anything but `Edit`; the loopback listener never does.
pub fn serve_json_lines_with(
    reader: impl Read,
    writer: impl Write,
    owner: &OwnerHandle,
    authority: ClientAuthority,
) -> Result<(), Error> {
    serve(reader, writer, owner, None, authority)
}

fn serve_stream(stream: TcpStream, owner: &OwnerHandle, token: Option<&str>) -> Result<(), Error> {
    stream
        .set_nodelay(true)
        .map_err(|error| Error::protocol(error.to_string()))?;
    stream
        .set_nonblocking(true)
        .map_err(|error| Error::protocol(error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| Error::protocol(error.to_string()))?;
    runtime.block_on(serve_socket(stream, owner, token))
}

/// A connection's existing thread multiplexes owner replies with reads. Buffered pipelined input
/// has the same one-line byte bound as ordinary input; EOF cancels a quiet held wait immediately.
async fn serve_socket(
    stream: TcpStream,
    owner: &OwnerHandle,
    token: Option<&str>,
) -> Result<(), Error> {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // `session.disconnect` closes the connection by shutting its socket down, which the read below
    // sees as the client closing it.
    let close = stream.try_clone().ok().map(|socket| -> Closer {
        Box::new(move || {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        })
    });
    let registration = Registration {
        owner,
        client: owner.register_connection(ClientAuthority::Edit, ClientTransport::Loopback, close),
    };
    let mut stream = tokio::net::TcpStream::from_std(stream)
        .map_err(|error| Error::protocol(error.to_string()))?;
    let mut buffered = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let line = loop {
            if let Some(end) = buffered.iter().position(|byte| *byte == b'\n') {
                if end >= MAX_REQUEST_BYTES {
                    return socket_framing_failure(&mut stream, "request exceeds JSON line limit")
                        .await;
                }
                break buffered.drain(..=end).collect::<Vec<_>>();
            }
            if buffered.len() >= MAX_REQUEST_BYTES {
                return socket_framing_failure(&mut stream, "request exceeds JSON line limit")
                    .await;
            }
            let count = stream
                .read(&mut chunk)
                .await
                .map_err(|error| Error::protocol(error.to_string()))?;
            if count == 0 {
                return Ok(());
            }
            buffered.extend_from_slice(&chunk[..count]);
        };
        let response = match serde_json::from_slice::<ApiRequest>(&line) {
            Ok(request) if token.is_some() && request.token.as_deref() != token => {
                ApiResponse::failure(request.id, 0, Error::protocol("invalid live-session token"))
            }
            Ok(request) => {
                let response_id = request.id.clone();
                let mut answer = std::pin::pin!(owner.call_async(registration.client, request));
                loop {
                    enum Event {
                        Answer(Result<ApiResponse, Error>),
                        Input(std::io::Result<usize>),
                    }
                    let event = {
                        let mut input = std::pin::pin!(stream.read(&mut chunk));
                        poll_fn(|cx| {
                            if let Poll::Ready(answer) = answer.as_mut().poll(cx) {
                                return Poll::Ready(Event::Answer(answer));
                            }
                            if let Poll::Ready(input) = input.as_mut().poll(cx) {
                                return Poll::Ready(Event::Input(input));
                            }
                            Poll::Pending
                        })
                        .await
                    };
                    match event {
                        Event::Answer(Ok(response)) => break response,
                        Event::Answer(Err(error)) => {
                            break ApiResponse::failure(response_id.clone(), 0, error);
                        }
                        Event::Input(Ok(0)) => return Ok(()),
                        Event::Input(Ok(count)) => {
                            buffered.extend_from_slice(&chunk[..count]);
                            if buffered.len() > MAX_REQUEST_BYTES {
                                return socket_framing_failure(
                                    &mut stream,
                                    "pipelined request exceeds JSON line limit",
                                )
                                .await;
                            }
                        }
                        Event::Input(Err(error)) => return Err(Error::protocol(error.to_string())),
                    }
                }
            }
            Err(error) => {
                ApiResponse::failure(String::new(), 0, Error::protocol(error.to_string()))
            }
        };
        let mut line =
            serde_json::to_vec(&response).map_err(|error| Error::protocol(error.to_string()))?;
        line.push(b'\n');
        stream
            .write_all(&line)
            .await
            .map_err(|error| Error::protocol(error.to_string()))?;
    }
}

/// A framing violation answers explicitly before closing the connection and its held interest.
async fn socket_framing_failure(
    stream: &mut tokio::net::TcpStream,
    reason: &str,
) -> Result<(), Error> {
    use tokio::io::AsyncWriteExt;
    let response = ApiResponse::failure(String::new(), 0, Error::protocol(reason));
    let mut line =
        serde_json::to_vec(&response).map_err(|error| Error::protocol(error.to_string()))?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .await
        .map_err(|error| Error::protocol(error.to_string()))?;
    Err(Error::protocol(reason))
}

/// The two ends one served connection reads and writes through. Answers are one line each, so
/// `TCP_NODELAY` is set: with Nagle's algorithm on, a line's last bytes can wait for the peer's
/// acknowledgement of the bytes before it. The option belongs to the socket, so both ends have it.
#[cfg(test)]
fn session_halves(stream: TcpStream) -> Result<(TcpStream, TcpStream), Error> {
    stream
        .set_nodelay(true)
        .map_err(|error| Error::protocol(error.to_string()))?;
    let writer = stream
        .try_clone()
        .map_err(|error| Error::protocol(error.to_string()))?;
    Ok((stream, writer))
}

/// The buffer a session's answers are written through, large enough that a typical answer and
/// its newline reach the socket (or the pipe) in one `write` instead of one per JSON fragment.
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Registers a client for the connection's lifetime and forgets its session on every exit path.
struct Registration<'a> {
    owner: &'a OwnerHandle,
    client: ClientId,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.owner.disconnect(self.client);
    }
}

fn serve(
    reader: impl Read,
    writer: impl Write,
    owner: &OwnerHandle,
    token: Option<&str>,
    authority: ClientAuthority,
) -> Result<(), Error> {
    // A stream cannot be closed from outside while it is read, so a client `session.disconnect`
    // closed is told so by its next answer, after which the stream ends.
    let registration = Registration {
        owner,
        client: owner.register_connection(authority, ClientTransport::Stdio, None),
    };
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::with_capacity(WRITE_BUFFER_BYTES, writer);
    loop {
        let mut line = String::new();
        let bytes = reader
            .by_ref()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_line(&mut line)
            .map_err(|error| Error::protocol(error.to_string()))?;
        if bytes == 0 {
            return Ok(());
        }
        let response = if bytes > MAX_REQUEST_BYTES || !line.ends_with('\n') {
            ApiResponse::failure(
                "".into(),
                0,
                Error::protocol("request exceeds JSON line limit"),
            )
        } else {
            match serde_json::from_str::<ApiRequest>(&line) {
                Ok(request) if token.is_some() && request.token.as_deref() != token => {
                    ApiResponse::failure(
                        request.id,
                        0,
                        Error::protocol("invalid live-session token"),
                    )
                }
                Ok(request) => owner.call(registration.client, request)?,
                Err(error) => {
                    ApiResponse::failure("".into(), 0, Error::protocol(error.to_string()))
                }
            }
        };
        serde_json::to_writer(&mut writer, &response)
            .map_err(|error| Error::protocol(error.to_string()))?;
        writer
            .write_all(b"\n")
            .and_then(|_| writer.flush())
            .map_err(|error| Error::protocol(error.to_string()))?;
        if bytes > MAX_REQUEST_BYTES || disconnected(&response) {
            return Ok(());
        }
    }
}

/// Whether `response` is the refusal of a client `session.disconnect` closed.
fn disconnected(response: &ApiResponse) -> bool {
    response
        .error
        .as_ref()
        .and_then(|error| error.data.as_ref())
        .is_some_and(|data| data["reason"] == DISCONNECTED_REASON)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModuleRegistry;
    use luxforge_test_macros::slow;
    use luxforge_testbase::paths::{jpeg as fixture, temp_path as temp};
    use serde_json::{Value, json};
    use std::io::Cursor;
    fn request(id: &str, method: &str, params: Value) -> String {
        format!("{}\n", json!({"id":id,"method":method,"params":params}))
    }

    #[test]
    fn independent_json_client_drives_history_versions_and_lineage() {
        let catalog = temp("catalog.sqlite");
        // This test focuses on independent history queries; prepare the source before the owner
        // starts so a one-shot JSON stream does not discard its client-scoped import job at EOF.
        let asset = {
            let mut service =
                crate::EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer()))
                    .unwrap();
            service.import(&fixture()).unwrap().asset.id.to_string()
        };
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let preparer = owner.register();
        let queued = owner
            .call(
                preparer,
                ApiRequest {
                    id: "prepare".into(),
                    method: "source.prepare".into(),
                    params: json!({"asset_id":asset}),
                    token: None,
                },
            )
            .unwrap()
            .result
            .unwrap();
        let job_id = queued["job_id"].as_str().unwrap();
        luxforge_testbase::wait_until("the reopen preparation to finish", || {
            let response = owner
                .call(
                    preparer,
                    ApiRequest {
                        id: "status".into(),
                        method: "job.read".into(),
                        params: json!({"job_id":job_id}),
                        token: None,
                    },
                )
                .unwrap();
            let status = response.result.unwrap();
            match status["status"].as_str() {
                Some("ready") => true,
                Some("queued" | "running") => false,
                other => panic!("unexpected reopen preparation {other:?}: {status}"),
            }
        });
        owner.disconnect(preparer);
        let mut output = Vec::new();
        let input = [
            request("schema", "schema.list", json!({})),
            request("info", "catalog.info", json!({})),
            request("pixel", "edit.set-basic", json!({"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"p1","actor":"api-test"},"exposure": 0.5})),
            request("sample", "render.sample", json!({"asset_id":asset,"x":0,"y":0})),
            request("version", "version.create", json!({"asset_id":asset,"name":"Edited","mutation":{"request_id":"v1","actor":"api-test"}})),
            request("undo", "history.undo", json!({"asset_id":asset,"mutation":{"expected_revision":1,"request_id":"u1","actor":"api-test"}})),
            request("versions", "version.list", json!({"asset_id":asset})),
            request("lineage", "history.lineage", json!({"asset_id":asset})),
        ]
        .concat();
        output.clear();
        serve_json_lines_with(
            Cursor::new(input),
            &mut output,
            &owner,
            ClientAuthority::Edit,
        )
        .unwrap();
        let responses: Vec<ApiResponse> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 8);
        assert!(responses.iter().all(|response| response.error.is_none()));
        let result = |index: usize| responses[index].result.as_ref().unwrap();
        assert_eq!(result(1)["counts"]["photographs"], json!(1));
        assert_ne!(
            result(3)["rgba"],
            json!([220, 34, 45, 255]),
            "the edit changed the pixel the fixture holds"
        );
        assert_eq!(result(4)["outcome"], json!("applied"));
        assert_eq!(result(6)["versions"][0]["name"], json!("Edited"));
        assert_eq!(result(6)["versions"][0]["entry_sequence"], json!(1));
        // After undo the lineage from current is only Original; the version keeps the edit reachable.
        assert_eq!(result(7)["steps"].as_array().unwrap().len(), 1);
        assert_eq!(result(7)["steps"][0]["sequence"], json!(0));
        // The stream's connection and the three catalog mutations advanced the sequence; source
        // preparation did not.
        assert_eq!(responses[5].sequence, 4);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn local_server_requires_token_and_removes_session_file() {
        let catalog = temp("live.sqlite");
        let session_file = temp("session.json");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        {
            let server = match LocalServer::start(owner.clone(), &session_file) {
                Ok(server) => server,
                Err(error) if luxforge_testbase::loopback_forbidden(&error.detail) => {
                    owner.stop();
                    join.join().unwrap();
                    std::fs::remove_file(catalog).unwrap();
                    return;
                }
                Err(error) => panic!("cannot start local server: {error}"),
            };
            assert_eq!(server.connected(), 0, "no client has connected yet");
            let mut stream = TcpStream::connect(server.info().address).unwrap();
            stream
                .write_all(request("bad", "schema.list", json!({})).as_bytes())
                .unwrap();
            let mut line = String::new();
            // The stream stays open while the count is read: closing it releases the connection.
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            reader.read_line(&mut line).unwrap();
            let response: ApiResponse = serde_json::from_str(&line).unwrap();
            assert_eq!(response.error.unwrap().code, "protocol");
            assert_eq!(
                server.connected(),
                1,
                "the answering connection is counted while it is live"
            );
        }
        assert!(!session_file.exists());
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// The session file is written whole and owner-only over whatever was there, the session is
    /// registered while it runs, and a client past the limit is told why it is refused.
    #[test]
    #[slow]
    fn local_server_registers_itself_and_answers_a_client_past_its_limit() {
        let root = luxforge_testbase::paths::temp_dir("live-limit");
        let catalog = root.join("catalog.sqlite");
        let session_file = live_session_file(&catalog);
        std::fs::write(&session_file, b"left by an earlier process").unwrap();
        let registry = root.join("config").join(super::super::LIVE_SESSIONS_DIR);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let mut server = match LocalServer::start(owner.clone(), &session_file) {
            Ok(server) => server,
            Err(error) if luxforge_testbase::loopback_forbidden(&error.detail) => {
                owner.stop();
                join.join().unwrap();
                return;
            }
            Err(error) => panic!("cannot start local server: {error}"),
        };
        let written: LocalSessionInfo =
            serde_json::from_slice(&std::fs::read(&session_file).unwrap()).unwrap();
        assert_eq!(written.address, server.info().address);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&session_file)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        server.register(&registry, &catalog).unwrap();
        let running = super::super::running_sessions(&registry).unwrap();
        assert_eq!(running.len(), 1);
        let entry = running[0].entry.as_ref().unwrap();
        assert_eq!(
            (&entry.catalog, &entry.session_file),
            (&catalog, &session_file)
        );
        // Every client the session serves, each answered once so it is surely counted.
        let token = server.info().token.clone();
        let ask = |stream: &mut TcpStream| {
            let line = json!({"id":"c","method":"catalog.info","params":{},"token":token});
            writeln!(stream, "{line}").unwrap();
            let mut answer = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut answer)
                .unwrap();
            serde_json::from_str::<ApiResponse>(&answer).unwrap()
        };
        let mut served: Vec<TcpStream> = (0..MAX_CLIENTS)
            .map(|_| TcpStream::connect(server.info().address).unwrap())
            .collect();
        for stream in &mut served {
            assert!(ask(stream).error.is_none());
        }
        assert_eq!(server.connected(), MAX_CLIENTS);
        let mut refused = TcpStream::connect(server.info().address).unwrap();
        refused
            .set_read_timeout(Some(luxforge_testbase::HANG))
            .unwrap();
        let answer = ask(&mut refused).error.unwrap();
        assert_eq!(answer.code, "resource-limit", "{answer:?}");
        assert_eq!(answer.data, Some(json!({"max_clients": MAX_CLIENTS})));
        drop(served);
        drop(server);
        assert!(!session_file.exists());
        assert!(
            super::super::running_sessions(&registry)
                .unwrap()
                .is_empty()
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An inner writer that records the length of every `write` call and counts flushes, so a test
    /// sees exactly what a session hands the socket.
    #[derive(Default)]
    struct CountingWriter {
        writes: Vec<usize>,
        flushes: usize,
        bytes: Vec<u8>,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes.push(buf.len());
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn each_answer_line_reaches_the_inner_writer_in_one_write_and_a_big_one_in_few() {
        let catalog = temp("buffered.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();

        // Small answers: three requests, three lines, and exactly one `write` and one flush each.
        let mut small = CountingWriter::default();
        let input = [
            request("a", "activity.list", json!({})),
            request("b", "activity.list", json!({})),
            request("c", "activity.list", json!({})),
        ]
        .concat();
        serve_json_lines_with(
            Cursor::new(input),
            &mut small,
            &owner,
            ClientAuthority::Edit,
        )
        .unwrap();
        let lines: Vec<&[u8]> = small.bytes.split_inclusive(|byte| *byte == b'\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.ends_with(b"\n")));
        assert_eq!(
            small.writes,
            lines.iter().map(|line| line.len()).collect::<Vec<_>>(),
            "one write per answer line, newline included"
        );
        assert_eq!(small.flushes, 3, "one flush per answer line");

        // The same answer written straight into an unbuffered writer is many fragment writes, so
        // the equality above does measure the buffer.
        let answer: ApiResponse = serde_json::from_slice(lines[0]).unwrap();
        let mut straight = CountingWriter::default();
        serde_json::to_writer(&mut straight, &answer).unwrap();
        assert!(
            straight.writes.len() > 1,
            "an unbuffered answer is written fragment by fragment, got {:?}",
            straight.writes
        );

        // An answer that can exceed the buffer: the schema list. Each write fills the buffer, so
        // the count follows the answer's size and not its number of JSON fragments.
        let mut big = CountingWriter::default();
        serve_json_lines_with(
            Cursor::new(request("schema", "schema.list", json!({}))),
            &mut big,
            &owner,
            ClientAuthority::Edit,
        )
        .unwrap();
        assert!(big.bytes.ends_with(b"\n"));
        assert_eq!(
            big.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1,
            "one answer, one line"
        );
        let most = big.bytes.len().div_ceil(WRITE_BUFFER_BYTES) + 1;
        assert!(
            big.writes.len() <= most,
            "{} bytes took {} writes, expected at most {most}: {:?}",
            big.bytes.len(),
            big.writes.len(),
            big.writes
        );
        assert_eq!(big.flushes, 1);

        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A JSON-lines stream is listed as `stdio` with its label. It cannot be closed from outside
    /// while it is read, so once `session.disconnect` names it, its session has ended, its next
    /// request is answered `protocol` with `data.reason` `disconnected` and the stream ends, though
    /// its input is still open; the log records its connection and one disconnection.
    #[test]
    fn a_disconnected_json_lines_stream_answers_its_next_request_and_ends() {
        let catalog = temp("stdio-disconnect.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let desktop = owner.register_desktop();
        let (input, mut to_stream) = std::io::pipe().unwrap();
        let (from_stream, output) = std::io::pipe().unwrap();
        let served = std::thread::spawn({
            let owner = owner.clone();
            move || serve_json_lines_with(input, output, &owner, ClientAuthority::Edit)
        });
        let mut answers = BufReader::new(from_stream);
        let mut ask = |id: &str, method: &str, params: Value| {
            to_stream
                .write_all(request(id, method, params).as_bytes())
                .unwrap();
            let mut line = String::new();
            answers.read_line(&mut line).unwrap();
            serde_json::from_str::<ApiResponse>(&line).unwrap()
        };
        let named = ask("name", "session.identify", json!({"label": "script"}));
        let client = named.result.unwrap()["client_id"].clone();
        let desktop_call = |id: &str, method: &str, params: Value| {
            owner
                .call(
                    desktop,
                    ApiRequest {
                        id: id.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap()
        };
        let listed = desktop_call("list", "session.clients", json!({}))
            .result
            .unwrap();
        assert_eq!(
            listed["clients"],
            json!([{"client_id": client, "label": "script", "authority": "edit",
                    "transport": "stdio", "connected_at": listed["clients"][0]["connected_at"],
                    "last_request_at": listed["clients"][0]["last_request_at"], "requests": 1}])
        );
        let closed = desktop_call("close", "session.disconnect", json!({"client_id": client}));
        assert!(closed.error.is_none(), "{:?}", closed.error);
        let refused = ask("after", "catalog.info", json!({})).error.unwrap();
        assert_eq!(refused.code, "protocol", "{refused:?}");
        assert_eq!(refused.data, Some(json!({"reason": "disconnected"})));
        served.join().unwrap().unwrap();
        let events = desktop_call("events", "events.since", json!({"after": 0}))
            .result
            .unwrap();
        let session: Vec<(&str, &str)> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                assert_eq!(event["client_id"], client);
                (
                    event["method"].as_str().unwrap(),
                    event["request_id"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            session,
            [
                ("session.connect", ""),
                ("session.identify", "name"),
                ("session.disconnect", "close"),
            ]
        );
        drop(to_stream);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_served_tcp_stream_has_nodelay_set_on_both_ends() {
        let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else {
            eprintln!("skipped: this environment does not allow a loopback listener");
            return;
        };
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        assert!(
            !accepted.nodelay().unwrap(),
            "a fresh connection has Nagle's algorithm on"
        );
        let (reader, writer) = session_halves(accepted).unwrap();
        assert!(reader.nodelay().unwrap());
        assert!(writer.nodelay().unwrap());
        drop(client);
    }
}

#[cfg(test)]
mod monitoring_transport_tests {
    use super::*;
    use luxforge_test_macros::slow;
    use luxforge_testbase::paths::temp_path;
    use serde_json::json;

    #[test]
    #[slow]
    fn dropping_the_local_server_closes_and_joins_an_idle_connection() {
        let catalog = temp_path("server-drop-idle.sqlite");
        let session_file = temp_path("server-drop-idle-session.json");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let server = LocalServer::start(owner.clone(), &session_file).unwrap();
        let mut stream = TcpStream::connect(server.info().address).unwrap();
        stream
            .set_read_timeout(Some(luxforge_testbase::HANG))
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({"id":"idle", "method":"catalog.info", "params":{}, "token":server.info().token})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let response: ApiResponse = serde_json::from_str(&line).unwrap();
        assert!(response.error.is_none());
        let connected = server.clients.clone();
        assert_eq!(connected.load(Ordering::Acquire), 1);

        drop(server);

        assert_eq!(
            connected.load(Ordering::Acquire),
            0,
            "the worker has joined"
        );
        assert_eq!(stream.read(&mut [0]).unwrap(), 0, "the peer has closed");
        assert!(!session_file.exists());
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    #[slow]
    fn dropping_the_local_server_releases_a_held_request() {
        let dir = luxforge_testbase::paths::temp_dir("server-drop-wait")
            .canonicalize()
            .unwrap();
        let catalog = temp_path("server-drop-wait.sqlite");
        let session_file = temp_path("server-drop-wait-session.json");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let gate = Arc::new(luxforge_testbase::Gate::new());
        gate.shut();
        owner.hold_listings(gate.clone(), dir.clone());
        let invoke = |method: &str, params: serde_json::Value| {
            owner
                .call(
                    client,
                    ApiRequest {
                        id: method.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap()
                .result
                .unwrap()
        };
        let refreshed = invoke(
            "index.refresh",
            json!({"source":{"kind":"folder", "path":dir}}),
        );
        let job = refreshed["job_id"].clone();
        gate.wait_reached(1, "held index listing");
        let observed = invoke("job.wait", json!({"job_id":job}));
        let server = LocalServer::start(owner.clone(), &session_file).unwrap();
        let mut stream = TcpStream::connect(server.info().address).unwrap();
        stream
            .set_read_timeout(Some(luxforge_testbase::HANG))
            .unwrap();
        writeln!(stream, "{}", json!({"id":"held", "method":"job.wait", "params":{"job_id":job, "after":observed["change"]}, "token":server.info().token})).unwrap();
        luxforge_testbase::wait_until("one held request", || owner.job_monitor_stats().held == 1);
        let connected = server.clients.clone();

        drop(server);

        assert_eq!(
            connected.load(Ordering::Acquire),
            0,
            "the worker has joined"
        );
        luxforge_testbase::wait_until("the server's held request released", || {
            owner.job_monitor_stats().held == 0
        });
        assert_eq!(stream.read(&mut [0]).unwrap(), 0, "the peer has closed");
        gate.open();
        owner.disconnect(client);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    #[slow]
    fn a_remote_close_releases_a_quiet_job_wait_and_pipeline_preserves_order() {
        // An ordinary owner's catalog refresh is held before disk work; its globally readable
        // queued/running job gives the transport a quiet indefinite wait.
        let dir = luxforge_testbase::paths::temp_dir("remote-job-wait")
            .canonicalize()
            .unwrap();
        let catalog = temp_path("remote-job-wait.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let gate = Arc::new(luxforge_testbase::Gate::new());
        gate.shut();
        owner.hold_listings(gate.clone(), dir.clone());
        let invoke = |method: &str, params: serde_json::Value| {
            owner
                .call(
                    client,
                    ApiRequest {
                        id: method.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap()
                .result
                .unwrap()
        };
        let refreshed = invoke(
            "index.refresh",
            json!({"source":{"kind":"folder","path":dir}}),
        );
        let job = refreshed["job_id"].clone();
        assert!(job.is_string(), "{refreshed}");
        gate.wait_reached(1, "held index listing");
        let observed = invoke("job.wait", json!({"job_id":job}));
        let session_file = temp_path("remote-job-wait-session.json");
        let server = LocalServer::start(owner.clone(), &session_file).unwrap();
        let mut stream = TcpStream::connect(server.info().address).unwrap();
        let request = |id: &str, method: &str, params: serde_json::Value| {
            format!(
                "{}\n",
                json!({"id":id,"method":method,"params":params,"token":server.info().token})
            )
        };
        stream
            .write_all(
                request(
                    "quiet",
                    "job.wait",
                    json!({"job_id":job,"after":observed["change"]}),
                )
                .as_bytes(),
            )
            .unwrap();
        luxforge_testbase::wait_until("one remote held job wait", || {
            owner.job_monitor_stats().held == 1
        });
        drop(stream);
        luxforge_testbase::wait_until("the remote quiet wait released on EOF", || {
            owner.job_monitor_stats().held == 0 && server.connected() == 0
        });
        let mut stream = TcpStream::connect(server.info().address).unwrap();
        stream
            .set_read_timeout(Some(luxforge_testbase::HANG))
            .unwrap();
        let pipelined =
            request("one", "catalog.info", json!({})) + &request("two", "schema.list", json!({}));
        stream.write_all(pipelined.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        for expected in ["one", "two"] {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let response: ApiResponse = serde_json::from_str(&line).unwrap();
            assert_eq!(response.id, expected);
            assert!(response.error.is_none());
        }
        drop(reader);
        let mut oversized = TcpStream::connect(server.info().address).unwrap();
        oversized
            .set_read_timeout(Some(luxforge_testbase::HANG))
            .unwrap();
        oversized
            .write_all(&vec![b' '; MAX_REQUEST_BYTES + 1])
            .unwrap();
        let mut reader = BufReader::new(oversized);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let refused: ApiResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(refused.error.unwrap().code, "protocol");
        drop(reader);
        gate.open();
        owner.disconnect(client);
        drop(server);
        owner.stop();
        join.join().unwrap();
    }
}
