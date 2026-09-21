//! Loopback JSON-lines transport: one line per request and response, a per-run token and a
//! session file for discovery. The same framing serves stdin/stdout for headless use.
use super::{ApiRequest, ApiResponse, ClientId, OwnerHandle, PROTOCOL};
use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
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
const MAX_CLIENTS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSessionInfo {
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
            .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?;
        let info = LocalSessionInfo {
            protocol: PROTOCOL.into(),
            address: listener
                .local_addr()
                .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?,
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
    pub fn info(&self) -> &LocalSessionInfo {
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
        std::fs::create_dir_all(parent)
            .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        Error::new(
            ErrorKind::Protocol,
            format!("cannot create live session file: {}", error.kind()),
        )
    })?;
    serde_json::to_writer(&mut file, info)
        .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?;
    file.flush()
        .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))
}

pub fn serve_json_lines(
    reader: impl Read,
    writer: impl Write,
    owner: &OwnerHandle,
) -> Result<(), Error> {
    serve(reader, writer, owner, None)
}

fn serve_stream(stream: TcpStream, owner: &OwnerHandle, token: Option<&str>) -> Result<(), Error> {
    let writer = stream
        .try_clone()
        .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?;
    serve(stream, writer, owner, token)
}

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
    mut writer: impl Write,
    owner: &OwnerHandle,
    token: Option<&str>,
) -> Result<(), Error> {
    let registration = Registration {
        owner,
        client: owner.register(),
    };
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = String::new();
        let bytes = reader
            .by_ref()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_line(&mut line)
            .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?;
        if bytes == 0 {
            return Ok(());
        }
        let response = if bytes > MAX_REQUEST_BYTES || !line.ends_with('\n') {
            ApiResponse::failure(
                "".into(),
                0,
                Error::new(ErrorKind::Protocol, "request exceeds JSON line limit"),
            )
        } else {
            match serde_json::from_str::<ApiRequest>(&line) {
                Ok(request) if token.is_some() && request.token.as_deref() != token => {
                    ApiResponse::failure(
                        request.id,
                        0,
                        Error::new(ErrorKind::Protocol, "invalid live-session token"),
                    )
                }
                Ok(request) => owner.call(registration.client, request)?,
                Err(error) => ApiResponse::failure(
                    "".into(),
                    0,
                    Error::new(ErrorKind::Protocol, error.to_string()),
                ),
            }
        };
        serde_json::to_writer(&mut writer, &response)
            .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?;
        writer
            .write_all(b"\n")
            .and_then(|_| writer.flush())
            .map_err(|error| Error::new(ErrorKind::Protocol, error.to_string()))?;
        if bytes > MAX_REQUEST_BYTES {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{
        io::Cursor,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lightwell-transport-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
    }
    fn request(id: &str, method: &str, params: Value) -> String {
        format!("{}\n", json!({"id":id,"method":method,"params":params}))
    }

    #[test]
    fn independent_json_client_drives_history_versions_and_lineage() {
        let catalog = temp("catalog.sqlite");
        // This test focuses on independent history queries; prepare the source before the owner
        // starts so a one-shot JSON stream does not discard its client-scoped import job at EOF.
        let asset = {
            let mut service = crate::EditorService::open(&catalog).unwrap();
            service.import(&fixture()).unwrap().asset.id.to_string()
        };
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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
        loop {
            let response = owner
                .call(
                    preparer,
                    ApiRequest {
                        id: "status".into(),
                        method: "job.status".into(),
                        params: json!({"job_id":job_id}),
                        token: None,
                    },
                )
                .unwrap();
            let status = response.result.unwrap();
            match status["state"].as_str() {
                Some("ready") => break,
                Some("queued" | "preparing") => std::thread::sleep(Duration::from_millis(1)),
                other => panic!("unexpected reopen preparation {other:?}: {status}"),
            }
        }
        owner.disconnect(preparer);
        let mut output = Vec::new();
        let input = [
            request("schema", "schema.list", json!({})),
            request("list", "catalog.list", json!({})),
            request("pixel", "edit.set-pixel", json!({"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"p1","actor":"api-test"},"x":0,"y":0,"rgb":[1,2,3]})),
            request("sample", "render.sample", json!({"asset_id":asset,"x":0,"y":0})),
            request("version", "version.create", json!({"asset_id":asset,"name":"Edited","actor":"api-test"})),
            request("undo", "history.undo", json!({"asset_id":asset,"mutation":{"expected_revision":1,"request_id":"u1","actor":"api-test"}})),
            request("versions", "version.list", json!({"asset_id":asset})),
            request("lineage", "history.lineage", json!({"asset_id":asset})),
        ]
        .concat();
        output.clear();
        serve_json_lines(Cursor::new(input), &mut output, &owner).unwrap();
        let responses: Vec<ApiResponse> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 8);
        assert!(responses.iter().all(|response| response.error.is_none()));
        let result = |index: usize| responses[index].result.as_ref().unwrap();
        assert_eq!(result(1)["assets"][0]["id"], json!(asset));
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
}
