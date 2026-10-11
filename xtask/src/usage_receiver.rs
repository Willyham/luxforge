//! Usage statistics in a timing run's editor launch: an isolated configuration whose stored choice
//! is a refusal (`off`) or an opt-in, and a loopback receiver the launch's statistics worker sends
//! to through `--usage-test-receiver` — one that accepts at once (`fast`), one that holds each
//! request open past the transport's timeouts (`slow`), or none at all, so each request is refused
//! at connection (`offline`). Every launch is eligible through explicit test eligibility, so `off`
//! measures an opted-out person's editor and the others an opted-in one's; none of them contacts
//! the production destination.
use crate::*;
use luxforge_testbase::{Request, TestServer, respond};
use std::{
    io::Write,
    net::TcpListener,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// The path the receivers serve, as the production destination's.
const PATH: &str = "/v1/usage/batches";
/// How long the slow receiver holds a request, past the transport's five-second total timeout.
const HOLD: Duration = Duration::from_secs(10);
/// How often the slow receiver writes one header line while it holds a request.
const TRICKLE: Duration = Duration::from_millis(250);
/// The installation the opted-in configuration names.
const INSTALLATION: &str = "c9a10716-1b78-4cbe-8d1c-b8309e5e7247";

/// What the launch's stored choice and receiver are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageMode {
    /// Opted out: the supervisor exists, no worker runs and nothing is sent.
    Off,
    /// Opted in, and the receiver answers `202` at once.
    Fast,
    /// Opted in, and the receiver sends its status line and then one header line every 250 ms
    /// for ten seconds, so the transport's own timeout ends each request.
    Slow,
    /// Opted in, and nothing listens: every request is refused at connection.
    Offline,
}

impl UsageMode {
    pub const ALL: [Self; 4] = [Self::Off, Self::Fast, Self::Slow, Self::Offline];

    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "off" => Self::Off,
            "fast" => Self::Fast,
            "slow" => Self::Slow,
            "offline" => Self::Offline,
            other => {
                return Err(format!("--usage is off, fast, slow or offline, not {other}").into());
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Fast => "fast",
            Self::Slow => "slow",
            Self::Offline => "offline",
        }
    }

    /// Whether the person opted in.
    pub fn collects(self) -> bool {
        self != Self::Off
    }
}

/// One launch's statistics setup: its configuration directory, to pass with `--evidence-config`,
/// the receiver's URL, to pass with `--usage-test-receiver`, and every request the receiver saw.
pub struct UsageSetup {
    pub mode: UsageMode,
    pub url: String,
    pub config: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    /// Kept for the run: dropping it stops the receiver.
    _server: Option<TestServer>,
}

impl UsageSetup {
    /// The configuration under `dir/host/config` and the receiver for `mode`.
    pub fn start(dir: &Path, mode: UsageMode) -> Result<Self> {
        let config = dir.join("host").join("config");
        fs::create_dir_all(&config)?;
        let record = if mode.collects() {
            json!({"prompt_shown": true, "enabled": true, "disclosure_version": 1,
                "installation_id": INSTALLATION, "image_key": "ab".repeat(32)})
        } else {
            json!({"prompt_shown": true, "enabled": false, "disclosure_version": 1})
        };
        write_json(
            &config.join("preferences.json"),
            &json!({"format": 1, "usage_statistics": record}),
        )?;
        let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
        let started = Instant::now();
        let (url, server) = match mode {
            UsageMode::Offline => {
                // A port that was free a moment ago and has nothing listening on it now.
                let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
                (format!("http://127.0.0.1:{port}{PATH}"), None)
            }
            _ => {
                let seen = requests.clone();
                let server = TestServer::http(move |request: &Request, out: &mut dyn Write| {
                    serve(mode, request, out, &seen, started);
                })?;
                (server.url(PATH), Some(server))
            }
        };
        Ok(Self {
            mode,
            url,
            config,
            requests,
            _server: server,
        })
    }

    /// Every request the receiver saw: when, how large, and how it ended. No body is kept.
    pub fn requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The report's record of the setup.
    pub fn record(&self) -> Value {
        json!({"mode": self.mode.name(), "url": self.url, "requests": self.requests()})
    }
}

fn serve(
    mode: UsageMode,
    request: &Request,
    out: &mut dyn Write,
    requests: &Mutex<Vec<Value>>,
    started: Instant,
) {
    let index = {
        let mut seen = requests.lock().unwrap_or_else(PoisonError::into_inner);
        seen.push(json!({
            "received_ms": started.elapsed().as_secs_f64() * 1000.0,
            "method": request.method,
            "path": request.path,
            "body_bytes": request.body.len(),
            "outcome": "answered",
        }));
        seen.len() - 1
    };
    if mode != UsageMode::Slow {
        respond(out, "202 Accepted", "", b"");
        return;
    }
    let held = Instant::now();
    let mut write = |bytes: &[u8]| out.write_all(bytes).and_then(|()| out.flush()).is_ok();
    let mut outcome = "closed-by-client";
    if write(b"HTTP/1.1 202 Accepted\r\n") {
        outcome = "held-to-the-end";
        while held.elapsed() < HOLD {
            std::thread::sleep(TRICKLE);
            if !write(b"X-Held: 1\r\n") {
                outcome = "closed-by-client";
                break;
            }
        }
        if outcome != "closed-by-client" {
            write(b"Content-Length: 0\r\nConnection: close\r\n\r\n");
        }
    }
    let mut seen = requests.lock().unwrap_or_else(PoisonError::into_inner);
    seen[index]["outcome"] = json!(outcome);
    seen[index]["held_ms"] = json!(held.elapsed().as_secs_f64() * 1000.0);
}
