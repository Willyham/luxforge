//! [`ProofEndpoint`], the capability proof's fake provider. It is a developer test fixture, never
//! part of the editor: tests and the rendered smoke start one, point the proof module's resource and
//! a profile at it, and read back what it received. One thread on `127.0.0.1` answers one
//! connection at a time with bounded heads and bodies and stops when the value is dropped.
use super::{PROOF_GENERATE_PATH, PROOF_PALETTE, PROOF_PALETTE_PATH, palette_bytes};
use crate::{
    Error, ErrorKind,
    capabilities::data::{SAMPLE_GRID_BYTES, SAMPLE_GRID_SAMPLES},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// A request head is at most this many bytes.
const MAX_HEAD: usize = 16 * 1024;
/// A request body is at most this many bytes.
const MAX_BODY: usize = 64 * 1024;
/// The requests the endpoint remembers, newest last.
const MAX_RECORDED: usize = 64;
/// How long one connection may stall a read or a write.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

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
    stopped: bool,
    delay: Duration,
    palette_delay: Duration,
    fail_next: Option<u16>,
    wrong_palette: bool,
    recorded: VecDeque<ProofRequest>,
}

struct Shared {
    api_key: String,
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The fake provider the capability proof sends to: `GET /proof-palette.bin` serves the pinned
/// palette, and `POST /generate` checks `Authorization: Bearer <key>` (401 otherwise), validates a
/// `sample-grid-8` body (400 otherwise) and answers `{"rgb": [r, g, b]}` with each channel
/// `192 + mean / 4` of that channel's samples. Knobs let a test delay answers, fail the next
/// generation and serve a palette that does not match its pin.
pub struct ProofEndpoint {
    address: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl ProofEndpoint {
    /// Listen on a free loopback port and answer requests authorized with `api_key`.
    pub fn start(api_key: &str) -> Result<Self, Error> {
        let failed = |error: std::io::Error| {
            Error::new(
                ErrorKind::Startup,
                format!("cannot start the proof endpoint: {}", error.kind()),
            )
        };
        let listener = TcpListener::bind("127.0.0.1:0").map_err(failed)?;
        let address = listener.local_addr().map_err(failed)?;
        let shared = Arc::new(Shared {
            api_key: api_key.to_owned(),
            state: Mutex::new(State {
                stopped: false,
                delay: Duration::ZERO,
                palette_delay: Duration::ZERO,
                fail_next: None,
                wrong_palette: false,
                recorded: VecDeque::new(),
            }),
            wake: Condvar::new(),
        });
        let serving = shared.clone();
        let thread = thread::Builder::new()
            .name("lightwell-proof-endpoint".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if serving.lock().stopped {
                        break;
                    }
                    if let Ok(stream) = stream {
                        answer(&serving, stream);
                    }
                }
            })
            .map_err(failed)?;
        Ok(Self {
            address,
            shared,
            thread: Some(thread),
        })
    }

    /// `http://127.0.0.1:<port>`: the base the proof module's resource URL is built on.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The URL a profile's endpoint names.
    pub fn generate_url(&self) -> String {
        format!("{}{PROOF_GENERATE_PATH}", self.base_url())
    }

    /// Hold every answer to `POST /generate` this long after the request arrives. Shortening it,
    /// or setting zero, releases an answer already waiting.
    pub fn set_delay(&self, delay: Duration) {
        self.shared.lock().delay = delay;
        self.shared.wake.notify_all();
    }

    /// Hold every answer to `GET /proof-palette.bin` this long after the request arrives, so a
    /// harness can observe an install while its download is still running. Shortening it, or
    /// setting zero, releases an answer already waiting.
    pub fn set_palette_delay(&self, delay: Duration) {
        self.shared.lock().palette_delay = delay;
        self.shared.wake.notify_all();
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
    fn drop(&mut self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_all();
        // The accept blocks; one connection wakes it to see the flag.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for ProofEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofEndpoint")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

/// A parsed request.
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Read one request: a head of at most [`MAX_HEAD`] bytes and a body of its `Content-Length`, at
/// most [`MAX_BODY`]. `Err` carries the status to refuse it with.
fn read_request(stream: &mut TcpStream) -> Result<Request, u16> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    let end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buffer.len() > MAX_HEAD {
            return Err(431);
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(400),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    let head = std::str::from_utf8(&buffer[..end]).map_err(|_| 400u16)?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>().map_err(|_| 400u16))
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err(413);
    }
    let mut body = buffer[end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(400),
            Ok(read) => body.extend_from_slice(&chunk[..read]),
        }
    }
    body.truncate(length);
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

/// The 64 samples of a `sample-grid-8` body, when it is exactly one.
fn samples(body: &[u8]) -> Option<Vec<[u8; 3]>> {
    if body.len() != SAMPLE_GRID_BYTES {
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
    if samples.len() != SAMPLE_GRID_SAMPLES {
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

/// Answer one connection.
fn answer(shared: &Shared, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(status) => {
            respond(
                &mut stream,
                status,
                "application/json",
                br#"{"error":"bad request"}"#,
            );
            return;
        }
    };
    let authorized = request.headers.iter().any(|(name, value)| {
        name == "authorization" && *value == format!("Bearer {}", shared.api_key)
    });
    let mut record = ProofRequest {
        method: request.method.clone(),
        path: request.path.clone(),
        headers: request
            .headers
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
        body_bytes: request.body.len(),
        samples: None,
        status: 200,
        rgb: None,
    };
    let (status, content_type, body) = match (request.method.as_str(), request.path.as_str()) {
        ("GET", PROOF_PALETTE_PATH) => {
            let palette = if shared.lock().wrong_palette {
                palette_bytes([1.0, 1.0, 1.0]).to_vec()
            } else {
                PROOF_PALETTE.to_vec()
            };
            (200, "application/octet-stream", palette)
        }
        ("POST", PROOF_GENERATE_PATH) => {
            record.samples = samples(&request.body);
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
        (_, PROOF_PALETTE_PATH | PROOF_GENERATE_PATH) => (
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
    // Which configured delay holds this answer: the generation's, the palette's, or none.
    let delay: Option<fn(&State) -> Duration> = match record.path.as_str() {
        PROOF_GENERATE_PATH => Some(|state| state.delay),
        PROOF_PALETTE_PATH => Some(|state| state.palette_delay),
        _ => None,
    };
    {
        let mut state = shared.lock();
        if state.recorded.len() == MAX_RECORDED {
            state.recorded.pop_front();
        }
        state.recorded.push_back(record);
    }
    if let Some(delay) = delay {
        pause(shared, delay);
    }
    respond(&mut stream, status, content_type, &body);
}

/// Wait out the delay `configured` reads, returning early when it is shortened or the endpoint
/// stops.
fn pause(shared: &Shared, configured: fn(&State) -> Duration) {
    let arrived = Instant::now();
    let mut state = shared.lock();
    loop {
        let waited = arrived.elapsed();
        let delay = configured(&state);
        if state.stopped || waited >= delay {
            return;
        }
        let remaining = delay - waited;
        state = shared
            .wake
            .wait_timeout(state, remaining)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::data::sample_grid_body;

    /// One raw exchange with the endpoint.
    fn exchange(endpoint: &ProofEndpoint, request: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(endpoint.address).unwrap();
        stream.write_all(request).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let status = std::str::from_utf8(&response[9..12])
            .unwrap()
            .parse()
            .unwrap();
        let end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        (status, response[end + 4..].to_vec())
    }

    fn post(key: &str, body: &[u8]) -> Vec<u8> {
        let mut request = format!(
            "POST /generate HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(body);
        request
    }

    #[test]
    fn the_endpoint_serves_the_palette_checks_the_key_and_answers_a_tint_of_the_grid() {
        let endpoint = ProofEndpoint::start("secret-key").unwrap();
        assert!(endpoint.base_url().starts_with("http://127.0.0.1:"));
        assert_eq!(
            endpoint.generate_url(),
            format!("{}/generate", endpoint.base_url())
        );
        let (status, body) = exchange(
            &endpoint,
            b"GET /proof-palette.bin HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert_eq!(status, 200);
        assert_eq!(body, PROOF_PALETTE);
        endpoint.serve_wrong_palette(true);
        let (_, wrong) = exchange(
            &endpoint,
            b"GET /proof-palette.bin HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert_eq!(wrong.len(), PROOF_PALETTE.len());
        assert_ne!(wrong, PROOF_PALETTE);
        let grid: Vec<[u8; 3]> = (0..SAMPLE_GRID_SAMPLES)
            .map(|index| [index as u8, 100, 255])
            .collect();
        let body = sample_grid_body(&grid).unwrap();
        let (status, answer) = exchange(&endpoint, &post("secret-key", &body));
        assert_eq!(status, 200);
        // Means 31, 100 and 255, a quarter of each added to 192.
        assert_eq!(answer, br#"{"rgb":[199,217,255]}"#);
        let (status, _) = exchange(&endpoint, &post("wrong-key", &body));
        assert_eq!(status, 401);
        let (status, _) = exchange(&endpoint, &post("secret-key", b"{}"));
        assert_eq!(status, 400);
        endpoint.fail_next(500);
        assert_eq!(exchange(&endpoint, &post("secret-key", &body)).0, 500);
        assert_eq!(
            exchange(&endpoint, &post("secret-key", &body)).0,
            200,
            "once"
        );
        assert_eq!(
            exchange(&endpoint, b"GET /generate HTTP/1.1\r\nHost: x\r\n\r\n").0,
            405
        );
        assert_eq!(
            exchange(&endpoint, b"GET /other HTTP/1.1\r\nHost: x\r\n\r\n").0,
            404
        );
        let recorded = endpoint.requests();
        assert_eq!(recorded.len(), 9);
        let generated = &recorded[2];
        assert_eq!(generated.samples.as_deref(), Some(grid.as_slice()));
        assert_eq!(generated.rgb, Some([199, 217, 255]));
        assert_eq!(generated.header("authorization"), Some("<redacted>"));
        assert!(generated.authorized);
        assert!(!recorded[3].authorized);
        assert!(!format!("{recorded:?}").contains("secret-key"));
    }

    #[test]
    fn a_delayed_answer_is_released_early_and_dropping_the_endpoint_stops_it() {
        let endpoint = ProofEndpoint::start("key").unwrap();
        endpoint.set_delay(Duration::from_secs(30));
        let address = endpoint.address;
        let body = sample_grid_body(&[[10, 20, 30]; SAMPLE_GRID_SAMPLES]).unwrap();
        let waiting = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(&post("key", &body)).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let started = Instant::now();
        while endpoint.requests().is_empty() {
            assert!(started.elapsed() < Duration::from_secs(10));
            thread::sleep(Duration::from_millis(1));
        }
        endpoint.set_delay(Duration::ZERO);
        assert!(waiting.join().unwrap().starts_with("HTTP/1.1 200"));
        assert!(started.elapsed() < Duration::from_secs(10));
        endpoint.set_delay(Duration::from_secs(30));
        let stopped = Instant::now();
        drop(endpoint);
        assert!(stopped.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_palette_delay_holds_only_the_download_until_it_is_released() {
        let endpoint = ProofEndpoint::start("key").unwrap();
        endpoint.set_palette_delay(Duration::from_secs(30));
        // A generation is not held by the palette's delay.
        let body = sample_grid_body(&[[10, 20, 30]; SAMPLE_GRID_SAMPLES]).unwrap();
        let started = Instant::now();
        assert_eq!(exchange(&endpoint, &post("key", &body)).0, 200);
        assert!(started.elapsed() < Duration::from_secs(10));
        let address = endpoint.address;
        let waiting = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(b"GET /proof-palette.bin HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response
        });
        while endpoint.requests().len() < 2 {
            assert!(started.elapsed() < Duration::from_secs(10));
            thread::sleep(Duration::from_millis(1));
        }
        // The download has arrived and is held: nothing has been answered yet.
        thread::sleep(Duration::from_millis(50));
        assert!(!waiting.is_finished(), "the palette answer is held");
        endpoint.set_palette_delay(Duration::ZERO);
        let response = waiting.join().unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert!(response.ends_with(&PROOF_PALETTE));
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
