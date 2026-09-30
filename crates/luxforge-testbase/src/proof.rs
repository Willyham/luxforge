//! [`ProofEndpoint`], the capability proof's fake provider: a developer test fixture, never part of
//! the editor. Tests and the rendered smoke start one, point the proof module's resource and a
//! profile at it, and read back what it received. A started endpoint is a [`TestServer`] that keeps
//! no copy of the requests, since they carry the key; the endpoint keeps its own record without the
//! credential. The core's own tests, which link no network transport, answer an endpoint in
//! process instead, through [`ProofEndpoint::answer`].
//!
//! The exchange itself (the paths, the pinned palette, the size of a sample grid) is the core's,
//! which this crate may not depend on, so whoever starts an endpoint hands it a [`ProofProtocol`]
//! built from the core's constants: `luxforge_testkit::proof_protocol` for every crate but the
//! core, and the core's own capability test harness for its unit tests. The endpoint's tests are
//! `luxforge-testkit`'s, where that protocol is built.
use crate::{
    Gate,
    server::{Options, TestServer, respond},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// The requests the endpoint remembers, newest last.
const MAX_RECORDED: usize = 64;

/// The capability proof module's side of the exchange, as `luxforge-core` defines it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofProtocol {
    /// Where the endpoint serves the palette, under its base URL.
    pub palette_path: &'static str,
    /// Where the endpoint answers a sample grid, under its base URL.
    pub generate_path: &'static str,
    /// The palette the resource is pinned at.
    pub palette: Vec<u8>,
    /// A palette of the pinned length whose bytes do not match the pin.
    pub wrong_palette: Vec<u8>,
    /// The exact length of every `sample-grid-8` body.
    pub grid_bytes: usize,
    /// The samples in one grid.
    pub grid_samples: usize,
}

/// One request the endpoint answered, without its credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofRequest {
    pub method: String,
    pub path: String,
    /// Header fields with lowercased names. An `authorization` value is never kept: it reads
    /// `<redacted>`.
    pub headers: Vec<(String, String)>,
    /// The request carried `Authorization: Bearer` with the expected key.
    pub authorized: bool,
    pub body_bytes: usize,
    /// The 64 samples of a valid `sample-grid-8` body, row by row.
    pub samples: Option<Vec<[u8; 3]>>,
    /// The status the endpoint answered with.
    pub status: u16,
    /// The tint a successful `POST /generate` answered.
    pub rgb: Option<[u8; 3]>,
}

impl ProofRequest {
    /// The first value of one header, by its lowercased name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }
}

struct State {
    /// The longest a held answer to `POST /generate` or `GET /proof-palette.bin` waits at its
    /// gate, when [`ProofEndpoint::set_delay`] or [`ProofEndpoint::set_palette_delay`] set one.
    delay: Option<Duration>,
    palette_delay: Option<Duration>,
    fail_next: Option<u16>,
    wrong_palette: bool,
    recorded: VecDeque<ProofRequest>,
}

struct Shared {
    api_key: String,
    protocol: ProofProtocol,
    state: Mutex<State>,
    /// The gates every answer to `POST /generate` and to `GET /proof-palette.bin` passes once its
    /// request is recorded.
    generation: Gate,
    palette: Gate,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The fake provider the capability proof sends to: `GET /proof-palette.bin` serves the pinned
/// palette, and `POST /generate` checks `Authorization: Bearer <key>` (401 otherwise), validates a
/// `sample-grid-8` body (400 otherwise) and answers `{"rgb": [r, g, b]}` with each channel
/// `192 + mean / 4` of that channel's samples. Knobs let a test hold answers at a gate, fail the
/// next generation and serve a palette that does not match its pin.
pub struct ProofEndpoint {
    shared: Arc<Shared>,
    /// The loopback server, or `None` for an endpoint answered in process.
    server: Option<TestServer>,
}

/// The origin an endpoint answered in process names. Nothing resolves it: only a test's in-memory
/// transport, which hands each request to [`ProofEndpoint::answer`], reaches it.
const IN_PROCESS_ORIGIN: &str = "https://proof.example";

/// One answer of the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofAnswer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl ProofEndpoint {
    /// Listen on a free loopback port and answer requests authorized with `api_key`.
    // `..Options::default()` fills the `tls` feature's fields; without that feature it is empty.
    #[allow(clippy::needless_update)]
    pub fn start(api_key: &str, protocol: ProofProtocol) -> io::Result<Self> {
        let mut endpoint = Self::in_process(api_key, protocol);
        let answering = endpoint.shared.clone();
        endpoint.server = Some(TestServer::start(
            Options {
                unrecorded: true,
                ..Options::default()
            },
            move |request, out| {
                let answer = reply(
                    &answering,
                    &request.method,
                    &request.path,
                    &request.headers,
                    &request.body,
                );
                respond(
                    out,
                    &status_line(answer.status),
                    &format!("Content-Type: {}\r\n", answer.content_type),
                    &answer.body,
                );
            },
        )?);
        Ok(endpoint)
    }

    /// An endpoint with no server, answering requests authorized with `api_key` only through
    /// [`Self::answer`]. Its URLs name an origin nothing resolves.
    pub fn in_process(api_key: &str, protocol: ProofProtocol) -> Self {
        let shared = Arc::new(Shared {
            api_key: api_key.to_owned(),
            protocol,
            state: Mutex::new(State {
                delay: None,
                palette_delay: None,
                fail_next: None,
                wrong_palette: false,
                recorded: VecDeque::new(),
            }),
            generation: Gate::new(),
            palette: Gate::new(),
        });
        Self {
            shared,
            server: None,
        }
    }

    /// `http://127.0.0.1:<port>`, or the in-process origin: the base the proof module's resource
    /// URL is built on.
    pub fn base_url(&self) -> String {
        match &self.server {
            Some(server) => server.origin(),
            None => IN_PROCESS_ORIGIN.to_owned(),
        }
    }

    /// The URL a profile's endpoint names.
    pub fn generate_url(&self) -> String {
        format!("{}{}", self.base_url(), self.shared.protocol.generate_path)
    }

    /// The loopback server a started endpoint answers through, or `None` for one answered in
    /// process.
    pub fn server(&self) -> Option<&TestServer> {
        self.server.as_ref()
    }

    /// Answer one request exactly as the server does, on the caller's thread: record it, hold its
    /// answer at its gate, and return the answer. `headers` are compared by lowercased name. For a
    /// test's in-memory transport; a started endpoint's server calls it for every request.
    pub fn answer(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> ProofAnswer {
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        reply(&self.shared, method, path, &headers, body)
    }

    /// The gate every answer to `POST /generate` passes once its request is recorded. A test shuts
    /// it to hold those answers until it opens it, and waits for [`Gate::reached`] to know a
    /// request has arrived and is held, so what it checks next happens while the job waits.
    pub fn generation(&self) -> &Gate {
        &self.shared.generation
    }

    /// The gate every answer to `GET /proof-palette.bin` passes; see [`Self::generation`].
    pub fn palette(&self) -> &Gate {
        &self.shared.palette
    }

    /// Hold every answer to `POST /generate` for at most `delay` after its request arrives, so the
    /// rendered smoke can watch a generation run; zero releases every held answer. A test holds an
    /// answer for as long as it needs at [`Self::generation`] instead.
    pub fn set_delay(&self, delay: Duration) {
        hold(&self.shared, &self.shared.generation, delay, |state| {
            &mut state.delay
        });
    }

    /// [`Self::set_delay`] for `GET /proof-palette.bin`, so a harness can watch an install while
    /// its download is still running.
    pub fn set_palette_delay(&self, delay: Duration) {
        hold(&self.shared, &self.shared.palette, delay, |state| {
            &mut state.palette_delay
        });
    }

    /// Answer the next authorized `POST /generate` with this status instead of a tint.
    pub fn fail_next(&self, status: u16) {
        self.shared.lock().fail_next = Some(status);
    }

    /// Serve a palette of the pinned length whose bytes do not match the pinned hash.
    pub fn serve_wrong_palette(&self, wrong: bool) {
        self.shared.lock().wrong_palette = wrong;
    }

    /// The requests answered so far, oldest first; at most the last 64.
    pub fn requests(&self) -> Vec<ProofRequest> {
        self.shared.lock().recorded.iter().cloned().collect()
    }
}

impl Drop for ProofEndpoint {
    /// Release every held answer; dropping the server then stops it.
    fn drop(&mut self) {
        self.shared.generation.open();
        self.shared.palette.open();
    }
}

impl std::fmt::Debug for ProofEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofEndpoint")
            .field("origin", &self.base_url())
            .finish_non_exhaustive()
    }
}

/// The 64 samples of a `sample-grid-8` body, when it is exactly one.
fn samples(protocol: &ProofProtocol, body: &[u8]) -> Option<Vec<[u8; 3]>> {
    if body.len() != protocol.grid_bytes {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    if object.len() != 4
        || object.get("data_class")? != "sample-grid-8"
        || object.get("width")? != 8
        || object.get("height")? != 8
    {
        return None;
    }
    let samples = object.get("samples")?.as_array()?;
    if samples.len() != protocol.grid_samples {
        return None;
    }
    samples
        .iter()
        .map(|sample| {
            let channels = sample.as_array().filter(|channels| channels.len() == 3)?;
            let code = |index: usize| u8::try_from(channels[index].as_u64()?).ok();
            Some([code(0)?, code(1)?, code(2)?])
        })
        .collect()
}

/// The tint the endpoint answers for a grid: per channel, 192 plus a quarter of the samples' mean.
fn tint(samples: &[[u8; 3]]) -> [u8; 3] {
    let mut rgb = [0; 3];
    for (channel, slot) in rgb.iter_mut().enumerate() {
        let sum: usize = samples
            .iter()
            .map(|sample| usize::from(sample[channel]))
            .sum();
        *slot = 192 + (sum / samples.len() / 4) as u8;
    }
    rgb
}

/// The status line of an answer.
fn status_line(status: u16) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    format!("{status} {reason}")
}

/// Answer one request, whose header names are lowercased.
fn reply(
    shared: &Shared,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> ProofAnswer {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    };
    let authorized = header("authorization") == Some(format!("Bearer {}", shared.api_key).as_str());
    let mut record = ProofRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers: headers
            .iter()
            .map(|(name, value)| {
                let value = if name == "authorization" {
                    "<redacted>".to_owned()
                } else {
                    value.clone()
                };
                (name.clone(), value)
            })
            .collect(),
        authorized,
        body_bytes: body.len(),
        samples: None,
        status: 200,
        rgb: None,
    };
    let protocol = &shared.protocol;
    let palette_path = path == protocol.palette_path;
    let generate_path = path == protocol.generate_path;
    let (status, content_type, answer) = match method {
        "GET" if palette_path => {
            let palette = if shared.lock().wrong_palette {
                protocol.wrong_palette.clone()
            } else {
                protocol.palette.clone()
            };
            (200, "application/octet-stream", palette)
        }
        "POST" if generate_path => {
            record.samples = samples(protocol, body);
            let failure = shared.lock().fail_next.take_if(|_| authorized);
            match (&record.samples, failure) {
                _ if !authorized => (
                    401,
                    "application/json",
                    br#"{"error":"unauthorized"}"#.to_vec(),
                ),
                (_, Some(status)) => (
                    status,
                    "application/json",
                    br#"{"error":"failed"}"#.to_vec(),
                ),
                (None, None) => (
                    400,
                    "application/json",
                    br#"{"error":"not a sample grid"}"#.to_vec(),
                ),
                (Some(samples), None) => {
                    let rgb = tint(samples);
                    record.rgb = Some(rgb);
                    (
                        200,
                        "application/json",
                        json!({"rgb": rgb}).to_string().into_bytes(),
                    )
                }
            }
        }
        _ if palette_path || generate_path => (
            405,
            "application/json",
            br#"{"error":"method not allowed"}"#.to_vec(),
        ),
        _ => (
            404,
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
        ),
    };
    record.status = status;
    // Which gate this answer passes, and the longest a configured delay holds it there.
    let arrived = Instant::now();
    let held = {
        let mut state = shared.lock();
        let held = if generate_path {
            Some((&shared.generation, state.delay))
        } else if palette_path {
            Some((&shared.palette, state.palette_delay))
        } else {
            None
        };
        if state.recorded.len() == MAX_RECORDED {
            state.recorded.pop_front();
        }
        state.recorded.push_back(record);
        held
    };
    if let Some((gate, delay)) = held {
        gate.pass_unless(|| delay.is_some_and(|delay| arrived.elapsed() >= delay));
    }
    ProofAnswer {
        status,
        content_type,
        body: answer,
    }
}

/// Hold every answer at `gate` for at most `delay`, or release every held one when it is zero.
fn hold(
    shared: &Shared,
    gate: &Gate,
    delay: Duration,
    configured: impl FnOnce(&mut State) -> &mut Option<Duration>,
) {
    let held = !delay.is_zero();
    *configured(&mut shared.lock()) = held.then_some(delay);
    if held {
        gate.shut();
    } else {
        gate.open();
    }
}
