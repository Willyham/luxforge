//! Loopback JSON-lines transport: one line per request and response, a per-run token and a
//! session file for discovery. The same framing serves stdin/stdout for headless use.
use super::{ApiRequest, ApiResponse, ClientAuthority, ClientId, OwnerHandle, PROTOCOL};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// How many loopback connections are served at once; the tile service's queue is sized from it.
pub(super) const MAX_CLIENTS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalSessionInfo {
    pub protocol: String,
    pub address: SocketAddr,
    pub token: String,
}

pub struct LocalServer {
    info: LocalSessionInfo,
    session_file: PathBuf,
    stopping: Arc<AtomicBool>,
    /// The same counter the accept loop keeps, so the status bar can report live clients.
    clients: Arc<AtomicUsize>,
    join: Option<JoinHandle<()>>,
}

impl LocalServer {
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
        // Blocking accept keeps the idle listener asleep; Drop wakes it with one loopback connection.
        let join = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { break };
                if clients.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
                    clients.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let owner = owner.clone();
                let token = token.clone();
                let clients = clients.clone();
                std::thread::spawn(move || {
                    let _ = serve_stream(stream, &owner, Some(&token));
                    clients.fetch_sub(1, Ordering::AcqRel);
                });
            }
        });
        Ok(Self {
            info,
            session_file: session_file.into(),
            stopping,
            clients: connected,
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
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(join) = self.join.take()
            && TcpStream::connect_timeout(&self.info.address, Duration::from_millis(250)).is_ok()
        {
            let _ = join.join();
        }
        let _ = std::fs::remove_file(&self.session_file);
    }
}

fn write_session_file(path: &Path, info: &LocalSessionInfo) -> Result<(), Error> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| Error::protocol(error.to_string()))?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        Error::protocol(format!("cannot create live session file: {}", error.kind()))
    })?;
    serde_json::to_writer(&mut file, info).map_err(|error| Error::protocol(error.to_string()))?;
    file.flush()
        .map_err(|error| Error::protocol(error.to_string()))
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
    let registration = Registration {
        owner,
        client: owner.register(),
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
    let registration = Registration {
        owner,
        client: owner.register_with(authority),
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
        if bytes > MAX_REQUEST_BYTES {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModuleRegistry;
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
            request("pixel", "edit.set-pixel", json!({"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"p1","actor":"api-test"},"x":0,"y":0,"rgb":[1,2,3]})),
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
        assert_eq!(result(3)["rgba"], json!([1, 2, 3, 255]));
        assert_eq!(result(4)["outcome"], json!("applied"));
        assert_eq!(result(6)["versions"][0]["name"], json!("Edited"));
        assert_eq!(result(6)["versions"][0]["entry_sequence"], json!(1));
        // After undo the lineage from current is only Original; the version keeps the edit reachable.
        assert_eq!(result(7)["steps"].as_array().unwrap().len(), 1);
        assert_eq!(result(7)["steps"][0]["sequence"], json!(0));
        // The three catalog mutations advanced the sequence; source preparation did not.
        assert_eq!(responses[5].sequence, 3);
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
                Err(error) if error.detail.contains("Operation not permitted") => {
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
    use luxforge_testbase::paths::temp_path;
    use serde_json::json;

    #[test]
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
