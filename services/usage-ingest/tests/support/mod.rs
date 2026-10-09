//! Test support: a fake downstream collector, a raw HTTP client, a manual clock and sample batches.
#![allow(dead_code)]

use luxforge_usage_ingest::config::Config;
use luxforge_usage_ingest::limits::Clock;
use luxforge_usage_ingest::server::Server;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const INSTALLATION: &str = "c9a10716-1b78-4cbe-8d1c-b8309e5e7247";
pub const SESSION: &str = "5f0c2e3a-8d47-4b6e-9a1f-2c7d9e4b1a60";
pub const IMAGE: &str = "3f9c2a7e5b1d48c09e6f1a2b3c4d5e6f";
/// 2026-10-09T14:03:12Z
pub const NOW: u64 = 1_791_554_592;

pub struct ManualClock {
    pub unix: AtomicU64,
    pub mono: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Arc<ManualClock> {
        Arc::new(ManualClock {
            unix: AtomicU64::new(NOW),
            mono: AtomicU64::new(1_000),
        })
    }

    pub fn advance_ms(&self, ms: u64) {
        self.mono.fetch_add(ms, SeqCst);
    }
}

impl Clock for ManualClock {
    fn unix_secs(&self) -> u64 {
        self.unix.load(SeqCst)
    }

    fn now_ms(&self) -> u64 {
        self.mono.load(SeqCst)
    }
}

/// What the fake collector received.
#[derive(Debug, Clone)]
pub struct Received {
    pub path: String,
    /// Header names (lowercase) and values.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Ok,
    /// Accept requests and never answer them.
    Hang,
    /// Answer 500.
    Fail,
}

pub struct Downstream {
    pub addr: SocketAddr,
    pub received: Arc<Mutex<Vec<Received>>>,
    pub connections: Arc<AtomicU64>,
}

impl Downstream {
    pub async fn start(mode: Mode) -> Downstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicU64::new(0));
        let (log, count) = (received.clone(), connections.clone());
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                count.fetch_add(1, SeqCst);
                let log = log.clone();
                tokio::spawn(async move {
                    let mut stream = stream;
                    let mut buffer = Vec::new();
                    loop {
                        let head_end = loop {
                            if let Some(i) = find(&buffer, b"\r\n\r\n") {
                                break i;
                            }
                            let mut chunk = [0u8; 4096];
                            match stream.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                            }
                        };
                        let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
                        let mut lines = head.split("\r\n");
                        let path = lines.next().unwrap().split(' ').nth(1).unwrap().to_owned();
                        let headers: Vec<(String, String)> = lines
                            .filter_map(|l| l.split_once(':'))
                            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                            .collect();
                        let length: usize = headers
                            .iter()
                            .find(|(k, _)| k == "content-length")
                            .map_or(0, |(_, v)| v.parse().unwrap());
                        let total = head_end + 4 + length;
                        while buffer.len() < total {
                            let mut chunk = [0u8; 8192];
                            match stream.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let body = buffer[head_end + 4..total].to_vec();
                        buffer.drain(..total);
                        log.lock().unwrap().push(Received {
                            path,
                            headers,
                            body,
                        });
                        match mode {
                            Mode::Ok => {
                                let _ = stream
                                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                                    .await;
                            }
                            Mode::Fail => {
                                let _ = stream
                                    .write_all(
                                        b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n",
                                    )
                                    .await;
                            }
                            Mode::Hang => std::future::pending::<()>().await,
                        }
                    }
                });
            }
        });
        Downstream {
            addr,
            received,
            connections,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/ingest", self.addr)
    }

    pub fn records(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    /// Wait until `count` requests have arrived (or fail after ten seconds).
    pub async fn wait_for(&self, count: usize) -> Vec<Received> {
        for _ in 0..1000 {
            let records = self.records();
            if records.len() >= count {
                return records;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "downstream received {} of {count} requests",
            self.records().len()
        );
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// An address on which nothing listens.
pub async fn closed_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

pub fn test_config(collector: &str) -> Config {
    let mut config = Config::new(collector);
    config.listen = "127.0.0.1:0".parse().unwrap();
    config.global_per_second = 1000;
    config.global_burst = 1000;
    config.id_interval_secs = 0;
    config.request_timeout = Duration::from_secs(3);
    config.forward_timeout = Duration::from_secs(30);
    config.stats_interval = Duration::from_secs(3600);
    config
}

pub type Log = Arc<Mutex<Vec<String>>>;

pub async fn start(config: Config, clock: Arc<ManualClock>) -> (Server, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let log = log.clone();
        Arc::new(move |line: &str| log.lock().unwrap().push(line.to_owned()))
    };
    let server = Server::start(config, clock, sink)
        .await
        .expect("server starts");
    (server, log)
}

pub struct Response {
    pub status: u16,
    /// Header names (lowercase) and values.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Send raw bytes and read the whole reply (the service closes every connection).
pub async fn raw(addr: SocketAddr, request: &[u8]) -> Option<Response> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let _ = stream.write_all(request).await;
    read_response(&mut stream).await
}

pub async fn read_response(stream: &mut TcpStream) -> Option<Response> {
    let mut reply = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut reply)).await;
    let text = String::from_utf8_lossy(&reply).into_owned();
    let (head, body) = text.split_once("\r\n\r\n")?;
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(Response {
        status,
        headers,
        body: body.to_owned(),
    })
}

pub fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("{method} {path} HTTP/1.1\r\nhost: ingest.test\r\n").into_bytes();
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        && !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
    {
        out.extend_from_slice(format!("content-length: {}\r\n", body.len()).as_bytes());
    }
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(body);
    out
}

pub const JSON: (&str, &str) = ("content-type", "application/json");

pub async fn post(addr: SocketAddr, body: &[u8]) -> Response {
    raw(addr, &request("POST", "/v1/usage/batches", &[JSON], body))
        .await
        .expect("a response")
}

pub async fn post_json(addr: SocketAddr, body: &Value) -> Response {
    post(addr, body.to_string().as_bytes()).await
}

/// The design's example batch.
pub fn batch() -> Value {
    json!({
      "schema": 1,
      "disclosure_version": 1,
      "release": "0.0.0+9d4138ca1f2e",
      "installation_id": INSTALLATION,
      "session_id": SESSION,
      "session_start": "2026-10-09T14:03:12Z",
      "batch_seq": 2,
      "dropped": 0,
      "device": {
        "os": {"family": "macos", "version": [15, 5]},
        "arch": "arm64",
        "cpu": {"vendor": "apple", "generation": 4, "tier": "pro",
                "physical_cores": 12, "logical_cores": 12},
        "memory_gib_band": 32,
        "gpus": [{"vendor": "apple", "backend": "metal", "type": "integrated",
                  "generation": 4, "tier": "pro", "unified_memory": true}],
        "renderer": "gpu"
      },
      "events": [
        {"seq": 41, "t": 1203400, "name": "workspace.enter", "workspace": "develop"},
        {"seq": 42, "t": 1219050, "name": "tool.commit", "tool": "basic", "image": IMAGE},
        {"seq": 43, "t": 1260000, "name": "process.sample", "cpu_percent": 37,
         "memory_mib": 2140, "memory_kind": "footprint"},
        {"seq": 44, "t": 1284711, "name": "operation.end", "operation": "export",
         "outcome": "completed", "duration_ms": 812, "count": 1, "image": IMAGE}
      ]
    })
}

/// A batch like [`batch`] with a different installation ID.
pub fn batch_for(installation: &str, seq: u64) -> Value {
    let mut b = batch();
    b["installation_id"] = installation.into();
    b["events"][0]["seq"] = seq.into();
    b["events"].as_array_mut().unwrap().truncate(1);
    b
}

/// The n-th distinct canonical UUIDv4.
pub fn uuid(n: u64) -> String {
    format!("{:08x}-1b78-4cbe-8d1c-{:012x}", n, n)
}
