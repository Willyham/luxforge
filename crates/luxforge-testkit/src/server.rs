//! [`TestServer`], the one loopback HTTP server the workspace's tests start: the transport's
//! protocol and address tests, the capability lifecycle's resource downloads and the capability
//! proof's fake provider all answer through it. It listens on a free port of `127.0.0.1`, serves
//! plain HTTP or TLS, reads each request with bounded heads and bodies on its own thread, hands it
//! to the test's responder and stops when the value is dropped.
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// A request head is at most this many bytes; a longer one is answered `431`.
const MAX_HEAD: usize = 16 * 1024;
/// A request body is at most this many bytes; a longer one is answered `413`.
const MAX_BODY: usize = 64 * 1024;
/// The requests a server keeps, newest last.
const MAX_RECORDED: usize = 64;
/// How long one connection may stall a read or a write.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// One request a [`TestServer`] read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The connection's place in the order the server accepted connections, from zero.
    pub index: usize,
    pub method: String,
    pub path: String,
    /// Header fields with lowercased names and trimmed values, in the order they arrived.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The request as it arrived, head and body.
    raw: Vec<u8>,
}

impl Request {
    /// The first value of one header, by its lowercased name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }

    /// The request as it arrived, as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }
}

/// How a [`TestServer`] listens and what it keeps.
#[derive(Clone, Default)]
pub struct Options {
    /// Serve TLS with this configuration instead of plain HTTP. Each answer ends with the TLS
    /// close signal unless `truncate_tls` is set.
    pub tls: Option<Arc<ServerConfig>>,
    /// End each TLS connection without the close signal, so a close-delimited body cannot be told
    /// from a truncation.
    pub truncate_tls: bool,
    /// Keep no copy of the requests, for a server whose requests carry a credential.
    pub unrecorded: bool,
}

type Respond = dyn Fn(&Request, &mut dyn Write) + Send + Sync;

struct Shared {
    stopped: AtomicBool,
    hits: AtomicUsize,
    recorded: Mutex<VecDeque<Request>>,
}

impl Shared {
    fn recorded(&self) -> MutexGuard<'_, VecDeque<Request>> {
        self.recorded.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A loopback HTTP server that answers every request with the test's responder. Each connection is
/// answered on its own thread, so a responder that stalls one download never holds up another.
pub struct TestServer {
    address: SocketAddr,
    scheme: &'static str,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl TestServer {
    /// Plain HTTP.
    pub fn http(
        respond: impl Fn(&Request, &mut dyn Write) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::start(Options::default(), respond)
    }

    /// HTTPS with this server configuration.
    pub fn https(
        config: Arc<ServerConfig>,
        respond: impl Fn(&Request, &mut dyn Write) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::start(
            Options {
                tls: Some(config),
                ..Options::default()
            },
            respond,
        )
    }

    /// Plain HTTP that answers connection `n` with `responses[n]`, or the last one, byte for byte.
    pub fn canned(responses: Vec<Vec<u8>>) -> io::Result<Self> {
        let last = responses.len().saturating_sub(1);
        Self::http(move |request, out| send(out, &responses[request.index.min(last)]))
    }

    pub fn start(
        options: Options,
        respond: impl Fn(&Request, &mut dyn Write) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            stopped: AtomicBool::new(false),
            hits: AtomicUsize::new(0),
            recorded: Mutex::new(VecDeque::new()),
        });
        let scheme = if options.tls.is_some() {
            "https"
        } else {
            "http"
        };
        let respond: Arc<Respond> = Arc::new(respond);
        let serving = shared.clone();
        let accept = thread::Builder::new()
            .name("luxforge-test-server".into())
            .spawn(move || {
                for (index, socket) in listener.incoming().enumerate() {
                    if serving.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(socket) = socket else { continue };
                    let (shared, respond, options) =
                        (serving.clone(), respond.clone(), options.clone());
                    let _ = thread::Builder::new()
                        .name("luxforge-test-connection".into())
                        .spawn(move || connection(index, socket, &options, &shared, &*respond));
                }
            })?;
        Ok(Self {
            address,
            scheme,
            shared,
            accept: Some(accept),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// `http://127.0.0.1:<port>`, or `https://…` for a TLS server.
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.address)
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }

    /// The requests read so far, oldest first: at most the last 64, and none for a server started
    /// `unrecorded`.
    pub fn requests(&self) -> Vec<Request> {
        self.shared.recorded().iter().cloned().collect()
    }

    /// How many whole requests the server has read and handed to its responder.
    pub fn hits(&self) -> usize {
        self.shared.hits.load(Ordering::SeqCst)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        // The accept blocks; one connection wakes it to see the flag.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

impl std::fmt::Debug for TestServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestServer")
            .field("origin", &self.origin())
            .finish_non_exhaustive()
    }
}

/// Write `bytes` as they are, for a test that frames its own answer.
pub fn send(out: &mut dyn Write, bytes: &[u8]) {
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// Write one complete answer: `HTTP/1.1 <status>` (for example `200 OK`), its `Content-Length`,
/// `Connection: close`, the extra header lines in `headers` (each ending in `\r\n`) and the body.
pub fn respond(out: &mut dyn Write, status: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    );
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(body);
    let _ = out.flush();
}

/// Serve one connection: over TLS when the server is configured for it.
fn connection(
    index: usize,
    socket: TcpStream,
    options: &Options,
    shared: &Shared,
    respond: &Respond,
) {
    let _ = socket.set_read_timeout(Some(IO_TIMEOUT));
    let _ = socket.set_write_timeout(Some(IO_TIMEOUT));
    let Some(config) = &options.tls else {
        exchange(index, &mut { socket }, options, shared, respond);
        return;
    };
    let Ok(session) = ServerConnection::new(config.clone()) else {
        return;
    };
    let mut stream = StreamOwned::new(session, socket);
    exchange(index, &mut stream, options, shared, respond);
    if !options.truncate_tls {
        stream.conn.send_close_notify();
        let _ = stream.flush();
    }
}

/// Read one request and answer it: a request over the bounds is answered here, one that never
/// arrives whole is not answered at all.
fn exchange<S: Read + Write>(
    index: usize,
    stream: &mut S,
    options: &Options,
    shared: &Shared,
    respond: &Respond,
) {
    let request = match read_request(index, stream) {
        Ok(request) => request,
        Err(Some(status)) => {
            self::respond(stream, status, "", b"");
            return;
        }
        Err(None) => return,
    };
    shared.hits.fetch_add(1, Ordering::SeqCst);
    if !options.unrecorded {
        let mut recorded = shared.recorded();
        if recorded.len() == MAX_RECORDED {
            recorded.pop_front();
        }
        recorded.push_back(request.clone());
    }
    respond(&request, stream);
    let _ = stream.flush();
}

/// Read one request: a head of at most [`MAX_HEAD`] bytes and a body of its `Content-Length`, at
/// most [`MAX_BODY`]. `Err(Some(status))` is the status to refuse it with; `Err(None)` means the
/// connection ended or stalled first.
fn read_request(index: usize, stream: &mut impl Read) -> Result<Request, Option<&'static str>> {
    let bad = Some("400 Bad Request");
    let mut raw = Vec::with_capacity(1024);
    let mut chunk = [0; 4096];
    let too_large = Some("431 Request Header Fields Too Large");
    let end = loop {
        if let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if raw.len() > MAX_HEAD {
            return Err(too_large);
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(None),
            Ok(read) => raw.extend_from_slice(&chunk[..read]),
        }
    };
    if end > MAX_HEAD {
        return Err(too_large);
    }
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| bad)?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(bad)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>().map_err(|_| bad))
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err(Some("413 Payload Too Large"));
    }
    let body_start = end + 4;
    while raw.len() < body_start + length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(None),
            Ok(read) => raw.extend_from_slice(&chunk[..read]),
        }
    }
    raw.truncate(body_start + length);
    Ok(Request {
        index,
        method,
        path,
        headers,
        body: raw[body_start..].to_vec(),
        raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// One raw exchange: the status and the body.
    fn exchange(server: &TestServer, request: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(server.address()).unwrap();
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

    #[test]
    fn a_request_is_read_whole_recorded_and_answered_by_the_responder() {
        let server = TestServer::http(|request, out| {
            let body = format!("{} {} {}", request.method, request.path, request.body.len());
            respond(out, "200 OK", "X-Seen: yes\r\n", body.as_bytes());
        })
        .unwrap();
        assert!(server.origin().starts_with("http://127.0.0.1:"));
        assert_eq!(server.url("/a"), format!("{}/a", server.origin()));
        let (status, body) = exchange(
            &server,
            b"POST /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nX-Case:  Mixed \r\n\r\nhello",
        );
        assert_eq!(
            (status, body.as_slice()),
            (200, b"POST /upload 5".as_slice())
        );
        let seen = server.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].header("x-case"), Some("Mixed"));
        assert_eq!(seen[0].body, b"hello");
        assert!(
            seen[0]
                .text()
                .starts_with("POST /upload HTTP/1.1\r\nHost: x\r\n")
        );
        assert!(seen[0].text().ends_with("\r\n\r\nhello"));
        assert_eq!(server.hits(), 1);
    }

    #[test]
    fn canned_answers_follow_the_connection_order_and_bounds_are_refused() {
        let server = TestServer::canned(vec![
            b"HTTP/1.1 204 A\r\n\r\n".to_vec(),
            b"HTTP/1.1 205 B\r\n\r\n".to_vec(),
        ])
        .unwrap();
        let get = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(exchange(&server, get).0, 204);
        assert_eq!(exchange(&server, get).0, 205);
        assert_eq!(exchange(&server, get).0, 205, "the last one repeats");
        let long = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "v".repeat(MAX_HEAD));
        assert_eq!(exchange(&server, long.as_bytes()).0, 431);
        let large = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(exchange(&server, large.as_bytes()).0, 413);
        assert_eq!(
            exchange(&server, b"GET / HTTP/1.1\r\nno colon\r\n\r\n").0,
            400
        );
        assert_eq!(server.hits(), 3, "a refused request reaches no responder");
    }

    #[test]
    fn a_stalled_connection_holds_up_no_other_and_dropping_the_server_stops_it() {
        let server = TestServer::start(
            Options {
                unrecorded: true,
                ..Options::default()
            },
            |request, out| {
                if request.path == "/slow" {
                    thread::sleep(Duration::from_secs(2));
                }
                respond(out, "200 OK", "", b"");
            },
        )
        .unwrap();
        let address = server.address();
        let slow = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(b"GET /slow HTTP/1.1\r\n\r\n").unwrap();
            let mut answer = Vec::new();
            stream.read_to_end(&mut answer).unwrap();
        });
        thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        assert_eq!(exchange(&server, b"GET /fast HTTP/1.1\r\n\r\n").0, 200);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            server.requests().is_empty(),
            "an unrecorded server keeps nothing"
        );
        let stopping = Instant::now();
        drop(server);
        assert!(stopping.elapsed() < Duration::from_secs(1));
        assert!(
            TcpStream::connect(address).is_err(),
            "nothing listens any more"
        );
        slow.join().unwrap();
    }
}
