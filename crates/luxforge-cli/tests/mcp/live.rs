//! A catalog owner serving a live session from this test process, as the desktop serves one, and
//! a person's own client of it; and a proxy in front of a session that can cut a connection.
use luxforge_core::{
    ApiRequest, ClientAuthority, ClientId, LocalServer, LocalSessionInfo, OwnerHandle,
    live_session_file,
};
use luxforge_testbase::{Gate, paths, wait_until};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread::JoinHandle,
};

pub struct Live {
    pub catalog: PathBuf,
    pub scratch: PathBuf,
    owner: OwnerHandle,
    person: ClientId,
    pub server: Option<LocalServer>,
    join: Option<JoinHandle<()>>,
    /// The gate every source job passes before its work, open unless a test shuts it.
    pub sources: Arc<Gate>,
}

impl Live {
    /// Start the owner and its session, the session file at `session_file` (beside the catalog
    /// when `None`). `None` where the host forbids a loopback listener and the run declares it.
    pub fn start(label: &str, session_file: Option<&Path>) -> Option<Self> {
        let scratch = paths::temp_dir(label);
        let catalog = scratch.join("catalog.sqlite");
        let sources = Arc::new(Gate::new());
        let hold = sources.clone();
        let (owner, join) =
            OwnerHandle::start_holding_sources(&catalog, Arc::new(move || hold.pass())).unwrap();
        let file = session_file.map_or_else(|| live_session_file(&catalog), Path::to_path_buf);
        let server = match LocalServer::start(owner.clone(), &file) {
            Ok(server) => server,
            Err(error) if luxforge_testbase::loopback_forbidden(&error.detail) => {
                owner.stop();
                join.join().unwrap();
                return None;
            }
            Err(error) => panic!("cannot start the live session: {error}"),
        };
        let person = owner.register_with(ClientAuthority::Edit);
        Some(Self {
            catalog,
            scratch,
            owner,
            person,
            server: Some(server),
            join: Some(join),
            sources,
        })
    }

    /// The person's own call, answering its result or its failure.
    pub fn try_ask(&self, method: &str, params: Value) -> Result<Value, Value> {
        let response = self
            .owner
            .call(
                self.person,
                ApiRequest {
                    id: "person".into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        match response.error {
            Some(error) => Err(serde_json::to_value(error).unwrap()),
            None => Ok(response.result.unwrap()),
        }
    }

    pub fn ask(&self, method: &str, params: Value) -> Value {
        self.try_ask(method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    /// Follow a job the person started to its end, as the person.
    fn finish_job(&self, job_id: &Value) -> Value {
        loop {
            let waited = self.ask("job.wait", json!({"job_id": job_id, "timeout_ms": 30_000}));
            if !matches!(waited["job"]["status"].as_str(), Some("queued" | "running")) {
                return waited["job"].clone();
            }
        }
    }

    /// Develop a copy of the JPEG fixture as the person, answering the new photograph's identity.
    /// Its source is prepared unless `prepare` is false.
    pub fn develop(&self, prepare: bool) -> String {
        let original = self.scratch.join("original.jpg");
        std::fs::copy(paths::jpeg(), &original).unwrap();
        let started = self.ask(
            "pick.develop",
            json!({"targets": {"kind": "paths", "paths": [original]}, "into": [],
                   "confirm_removable": true,
                   "mutation": {"request_id": "develop", "actor": "person"}}),
        );
        let developed = self.finish_job(&started["job_id"]);
        assert_eq!(developed["status"], "ready", "{developed}");
        let asset = developed["result"]["developed"][0]["asset_id"]
            .as_str()
            .unwrap()
            .to_owned();
        if prepare {
            let prepared = self.ask("source.prepare", json!({"asset_id": asset}));
            let prepared = self.finish_job(&prepared["job_id"]);
            assert_eq!(prepared["status"], "ready", "{prepared}");
        }
        asset
    }

    pub fn revision(&self, asset: &str) -> u64 {
        self.ask("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// The asset's history, newest first.
    pub fn history(&self, asset: &str) -> Vec<Value> {
        self.ask("history.list", json!({"asset_id": asset, "limit": 50}))["entries"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// The live clients the person sees, the adapter among them.
    pub fn clients(&self) -> Vec<Value> {
        self.ask("session.clients", json!({}))["clients"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// How many requests the one live client has sent the owner.
    pub fn requests(&self) -> u64 {
        let clients = self.clients();
        assert_eq!(clients.len(), 1, "{clients:?}");
        clients[0]["requests"].as_u64().unwrap()
    }

    /// Wait until the one live client has sent `count` requests: the owner has taken the last.
    pub fn wait_for_requests(&self, count: u64) {
        wait_until("the adapter's request to reach the owner", || {
            self.requests() >= count
        });
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.sources.open();
        drop(self.server.take());
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Run `command` with `home` as its home directory and every platform's configuration variable
/// under it, answering where it finds the registry of running live sessions.
pub fn home_registry(command: &mut std::process::Command, home: &Path) -> PathBuf {
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("APPDATA", home.join("roaming"))
        .env("LOCALAPPDATA", home.join("local"));
    registry_under(home)
}

/// The registry of running live sessions a process run by [`home_registry`] reads.
pub fn registry_under(home: &Path) -> PathBuf {
    let config = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Luxforge")
    } else if cfg!(windows) {
        home.join("roaming").join("Luxforge")
    } else {
        home.join("xdg").join("luxforge")
    };
    config.join(luxforge_core::LIVE_SESSIONS_DIR)
}

/// A proxy in front of a live session, whose own address the catalog's session file names. It
/// forwards each connection's requests one at a time and reports each request's method. Once,
/// for the method `cut` names, it forwards the request and reads the owner's answer, then closes
/// the client's connection without passing the answer on, as a desktop that quit mid-answer
/// would.
pub struct Proxy {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    pub methods: Receiver<String>,
}

impl Proxy {
    /// Stand a proxy in front of `live`, whose session file is at `real`, and point the catalog's
    /// session file at it.
    pub fn start(live: &Live, real: &Path, cut: Option<&'static str>) -> Self {
        let info: LocalSessionInfo = serde_json::from_slice(&std::fs::read(real).unwrap()).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        std::fs::write(
            live_session_file(&live.catalog),
            serde_json::to_vec(&LocalSessionInfo {
                address,
                ..info.clone()
            })
            .unwrap(),
        )
        .unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let (report, methods) = channel();
        let upstream = info.address;
        let cutting = Arc::new(AtomicBool::new(cut.is_some()));
        let accept = {
            let stopping = stopping.clone();
            std::thread::spawn(move || {
                for client in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let client = client.unwrap();
                    let report = report.clone();
                    let cutting = cutting.clone();
                    std::thread::spawn(move || forward(client, upstream, report, cut, &cutting));
                }
            })
        };
        Self {
            address,
            stopping,
            accept: Some(accept),
            methods,
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn forward(
    client: TcpStream,
    upstream: SocketAddr,
    report: Sender<String>,
    cut: Option<&'static str>,
    cutting: &AtomicBool,
) {
    let mut to_client = client.try_clone().unwrap();
    let owner = TcpStream::connect(upstream).unwrap();
    let mut to_owner = owner.try_clone().unwrap();
    let mut from_owner = BufReader::new(owner);
    for line in BufReader::new(client).lines() {
        let Ok(line) = line else { break };
        let request: Value = serde_json::from_str(&line).unwrap();
        let method = request["method"].as_str().unwrap().to_owned();
        let _ = report.send(method.clone());
        writeln!(to_owner, "{line}").unwrap();
        let mut answer = String::new();
        if from_owner.read_line(&mut answer).unwrap_or(0) == 0 {
            break;
        }
        if Some(method.as_str()) == cut && cutting.swap(false, Ordering::SeqCst) {
            let _ = to_client.shutdown(Shutdown::Both);
            break;
        }
        if to_client.write_all(answer.as_bytes()).is_err() {
            break;
        }
    }
    let _ = to_owner.shutdown(Shutdown::Both);
}
