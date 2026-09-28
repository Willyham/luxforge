//! End-to-end transport tests against loopback servers, a fake resolver and a connector that routes
//! chosen public addresses to those servers. Nothing here leaves the machine.
use super::*;
use luxforge_testbase::{Gate, HANG, wait_until};
use luxforge_testkit::{Options, Request, TestServer, send};
use rustls::{
    ServerConfig,
    pki_types::{PrivateKeyDer, pem::PemObject},
};
use std::{
    io,
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

const BOTH: &[EndpointClass] = &[EndpointClass::Remote, EndpointClass::Loopback];
/// A public address the fake resolver hands out and the test connector routes to a local server.
const PUBLIC: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(9, 9, 9, 9));

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
}

fn test_roots() -> TlsTrust {
    TlsTrust::Roots(vec![
        CertificateDer::from_pem_file(fixture("ca.pem")).unwrap(),
    ])
}

fn server_tls() -> Arc<ServerConfig> {
    let chain = vec![CertificateDer::from_pem_file(fixture("leaf.pem")).unwrap()];
    let key = PrivateKeyDer::from_pem_file(fixture("leaf.key")).unwrap();
    Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap(),
    )
}

/// A gate a test's server holds its answer at, shut until the test ends: dropping it opens the gate,
/// so no held answer outlives its test.
struct Stall(Arc<Gate>);

impl Stall {
    fn new() -> Self {
        let gate = Arc::new(Gate::new());
        gate.shut();
        Self(gate)
    }

    /// The gate, for a server's responder to pass.
    fn gate(&self) -> Arc<Gate> {
        self.0.clone()
    }
}

impl std::ops::Deref for Stall {
    type Target = Gate;
    fn deref(&self) -> &Gate {
        &self.0
    }
}

impl Drop for Stall {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// The requests a server read, each as it arrived.
fn requests(server: &TestServer) -> Vec<String> {
    server.requests().iter().map(Request::text).collect()
}

/// Answers from a list, one per call, repeating the last; counts calls.
struct FakeResolver {
    answers: Vec<Vec<IpAddr>>,
    calls: AtomicUsize,
}

impl FakeResolver {
    fn new(answers: Vec<Vec<IpAddr>>) -> Arc<Self> {
        Arc::new(Self {
            answers,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Resolve for FakeResolver {
    fn resolve(&self, _: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = &self.answers[call.min(self.answers.len() - 1)];
        Ok(answer
            .iter()
            .map(|&address| SocketAddr::new(address, port))
            .collect())
    }
}

/// Connects the listed addresses to local servers and refuses every other address, recording each
/// attempt.
#[derive(Default)]
struct Routes {
    routes: Vec<(SocketAddr, SocketAddr)>,
    attempts: Mutex<Vec<SocketAddr>>,
}

impl Routes {
    fn to(from: SocketAddr, to: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            routes: vec![(from, to)],
            ..Self::default()
        })
    }

    fn attempts(&self) -> Vec<SocketAddr> {
        self.attempts.lock().unwrap().clone()
    }
}

impl Connect for Routes {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        self.attempts.lock().unwrap().push(address);
        match self.routes.iter().find(|(from, _)| *from == address) {
            Some((_, to)) => TcpStream::connect_timeout(to, timeout),
            None => Err(io::ErrorKind::ConnectionRefused.into()),
        }
    }
}

fn transport(
    trust: TlsTrust,
    resolver: Arc<dyn Resolve>,
    connector: Arc<dyn Connect>,
) -> Transport {
    Transport::new(TransportConfig {
        trust,
        resolver,
        connector,
    })
    .unwrap()
}

/// A transport for loopback servers: the test roots and direct connections.
fn loopback() -> Transport {
    Transport::new(TransportConfig {
        trust: test_roots(),
        ..TransportConfig::default()
    })
    .unwrap()
}

/// A transport whose every lookup and connection attempt is observable and goes nowhere.
fn isolated() -> (Transport, Arc<FakeResolver>, Arc<Routes>) {
    let resolver = FakeResolver::new(vec![vec![PUBLIC]]);
    let routes = Arc::new(Routes::default());
    let transport = transport(test_roots(), resolver.clone(), routes.clone());
    (transport, resolver, routes)
}

fn request(method: Method, url: &str) -> TransportRequest {
    TransportRequest {
        method,
        endpoint: parse_endpoint(url, BOTH).unwrap(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

/// The plain-data parts of `SendOptions` a test varies.
#[derive(Clone)]
struct Plan {
    max_request_bytes: u64,
    max_response_bytes: u64,
    read_timeout: Duration,
    total_timeout: Duration,
    redirects: u8,
    origins: Vec<String>,
    /// Cancel the job before the request is sent.
    cancelled: bool,
    /// Told the body bytes received so far each time more arrive.
    received: Option<mpsc::Sender<u64>>,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            max_request_bytes: 1 << 20,
            max_response_bytes: 1 << 20,
            read_timeout: Duration::from_secs(5),
            total_timeout: Duration::from_secs(10),
            redirects: 0,
            origins: Vec::new(),
            cancelled: false,
            received: None,
        }
    }
}

struct Fetched {
    result: Result<TransportResponse, Error>,
    body: Vec<u8>,
    progress: Vec<(u64, Option<u64>)>,
}

impl Fetched {
    fn ok(&self) -> &TransportResponse {
        match &self.result {
            Ok(response) => response,
            Err(error) => panic!("request failed: {error}"),
        }
    }

    fn error(&self) -> &Error {
        self.result.as_ref().expect_err("request succeeded")
    }

    fn code(&self) -> &'static str {
        self.error().kind.code()
    }
}

fn fetch(transport: &Transport, request: &TransportRequest, plan: &Plan) -> Fetched {
    let control = JobControl::new();
    if plan.cancelled {
        control.cancel("the request was cancelled");
    }
    send_with(transport, request, plan, &control)
}

/// Send `request` for the job `control`, which the caller may cancel from another thread.
fn send_with(
    transport: &Transport,
    request: &TransportRequest,
    plan: &Plan,
    control: &Arc<JobControl>,
) -> Fetched {
    let mut seen = Vec::new();
    let mut progress = |received, total| {
        seen.push((received, total));
        if let Some(told) = &plan.received {
            let _ = told.send(received);
        }
    };
    let mut body = Vec::new();
    let result = transport.send(
        request,
        SendOptions {
            max_request_bytes: plan.max_request_bytes,
            max_response_bytes: plan.max_response_bytes,
            connect_timeout: Duration::from_secs(5),
            read_timeout: plan.read_timeout,
            total_timeout: plan.total_timeout,
            redirects: RedirectPolicy {
                max: plan.redirects,
                origins: &plan.origins,
            },
            control,
            progress: &mut progress,
        },
        &mut body,
    );
    Fetched {
        result,
        body,
        progress: seen,
    }
}

fn redirect_to(status: u16, location: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status} Moved\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
        .into_bytes()
}

#[test]
fn a_resolution_with_any_non_public_address_is_refused_before_connecting() {
    for answer in [
        vec![PUBLIC, "10.0.0.1".parse().unwrap()],
        vec!["127.0.0.1".parse().unwrap()],
        vec![PUBLIC, "::ffff:192.168.0.1".parse().unwrap()],
        vec!["fd00::1".parse().unwrap()],
    ] {
        let resolver = FakeResolver::new(vec![answer.clone()]);
        let routes = Arc::new(Routes::default());
        let transport = transport(test_roots(), resolver.clone(), routes.clone());
        let fetched = fetch(
            &transport,
            &request(Method::Get, "https://downloads.example/palette.bin"),
            &Plan::default(),
        );
        assert_eq!(fetched.code(), "validation", "{answer:?}");
        assert!(
            fetched
                .error()
                .detail
                .contains("which is not a remote address"),
            "{}",
            fetched.error()
        );
        assert_eq!(resolver.calls(), 1);
        assert!(routes.attempts().is_empty(), "{answer:?}");
    }
    let (transport, resolver, routes) = isolated();
    for literal in [
        "https://10.0.0.1/",
        "https://[::ffff:10.0.0.1]/",
        "https://169.254.169.254/",
    ] {
        let fetched = fetch(&transport, &request(Method::Get, literal), &Plan::default());
        assert_eq!(fetched.code(), "validation", "{literal}");
    }
    assert_eq!(resolver.calls(), 0, "an IP literal is never looked up");
    assert!(routes.attempts().is_empty());
}

#[test]
fn a_remote_https_download_resolves_once_and_cannot_be_rebound() {
    let server = TestServer::https(server_tls(), |_, out| {
        send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\npalette");
    })
    .unwrap();
    // The first lookup answers a public address; any later one would answer a private address.
    let resolver = FakeResolver::new(vec![vec![PUBLIC], vec!["10.0.0.1".parse().unwrap()]]);
    let public = SocketAddr::new(PUBLIC, 443);
    let routes = Routes::to(public, server.address());
    let transport = transport(test_roots(), resolver.clone(), routes.clone());
    let download = request(Method::Get, "https://downloads.example/models/palette.bin");

    let fetched = fetch(&transport, &download, &Plan::default());
    let response = fetched.ok();
    assert_eq!(response.status, 200);
    assert_eq!(fetched.body, b"palette");
    assert_eq!(
        response.final_url.as_str(),
        "https://downloads.example/models/palette.bin"
    );
    assert_eq!(resolver.calls(), 1, "one lookup per connection");
    assert_eq!(routes.attempts(), vec![public], "only the checked address");
    let seen = requests(&server);
    assert!(seen[0].starts_with("GET /models/palette.bin HTTP/1.1\r\nhost: downloads.example\r\n"));

    let again = fetch(&transport, &download, &Plan::default());
    assert_eq!(again.code(), "validation");
    assert_eq!(resolver.calls(), 2);
    assert_eq!(
        routes.attempts(),
        vec![public],
        "the rebound answer is never used"
    );
}

#[test]
fn plain_http_is_refused_for_anything_but_loopback() {
    let (transport, resolver, routes) = isolated();
    for (url, class) in [
        ("http://downloads.example/", EndpointClass::Remote),
        ("http://downloads.example/", EndpointClass::Loopback),
        ("https://downloads.example/#part", EndpointClass::Remote),
    ] {
        let forged = TransportRequest {
            method: Method::Get,
            endpoint: Endpoint {
                url: Url::parse(url).unwrap(),
                class,
            },
            headers: Vec::new(),
            body: Vec::new(),
        };
        assert_eq!(
            fetch(&transport, &forged, &Plan::default()).code(),
            "validation",
            "{url} as {class:?}"
        );
    }
    assert_eq!(resolver.calls(), 0);
    assert!(routes.attempts().is_empty());
}

#[test]
fn loopback_http_get_streams_a_content_length_body() {
    let server = TestServer::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Extra:  padded \r\nContent-Length: 11\r\n\r\nhello world"
            .to_vec(),
    ]).unwrap();
    let fetched = fetch(
        &loopback(),
        &request(Method::Get, &server.url("/v1/status?verbose=1")),
        &Plan::default(),
    );
    let response = fetched.ok();
    assert_eq!(response.status, 200);
    assert_eq!(fetched.body, b"hello world");
    assert_eq!(response.received, 11);
    assert_eq!(response.header("content-type"), Some("text/plain"));
    assert_eq!(response.header("X-EXTRA"), Some("padded"));
    assert_eq!(
        response.final_url.as_str(),
        server.url("/v1/status?verbose=1")
    );
    assert_eq!(fetched.progress.last(), Some(&(11, Some(11))));
    let seen = &requests(&server)[0];
    // Header names are case-insensitive; the client writes them lowercased.
    let expected = format!(
        "GET /v1/status?verbose=1 HTTP/1.1\r\nhost: {}\r\nuser-agent: Luxforge/{}\r\n\
         accept-encoding: identity\r\nconnection: close\r\n\r\n",
        server.address(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(seen, &expected);
}

#[test]
fn loopback_http_post_sends_a_host_framed_body_and_decodes_a_chunked_reply() {
    let server = TestServer::canned(vec![
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
          4;name=value\r\n{\"ok\r\n6\r\n\":true\r\n1\r\n}\r\n0\r\nX-Trailer: done\r\n\r\n"
            .to_vec(),
    ])
    .unwrap();
    let mut post = request(Method::Post, &server.url("/v1/run"));
    post.headers = vec![
        ("Content-Type".into(), "application/json".into()),
        ("Authorization".into(), "Bearer sentinel-token".into()),
    ];
    post.body = br#"{"grid":[1,2,3]}"#.to_vec();
    let debug = format!("{post:?}");
    assert!(
        !debug.contains("sentinel-token") && !debug.contains("grid"),
        "{debug}"
    );

    let fetched = fetch(&loopback(), &post, &Plan::default());
    assert_eq!(fetched.ok().status, 200);
    assert_eq!(fetched.body, br#"{"ok":true}"#);
    assert_eq!(fetched.progress.last(), Some(&(11, None)));
    let seen = &requests(&server)[0];
    assert!(seen.starts_with("POST /v1/run HTTP/1.1\r\n"), "{seen}");
    assert!(seen.contains("\r\ncontent-length: 16\r\n"), "{seen}");
    assert!(seen.contains("\r\nauthorization: Bearer sentinel-token\r\n"));
    assert!(seen.ends_with("\r\n\r\n{\"grid\":[1,2,3]}"));
}

#[test]
fn non_success_statuses_are_returned_as_data() {
    let server = TestServer::canned(vec![
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\nbusy".to_vec(),
        b"HTTP/1.1 304 Not Modified\r\nContent-Length: 99\r\n\r\n".to_vec(),
        b"HTTP/1.0 200 OK\r\n\r\nread to close".to_vec(),
    ])
    .unwrap();
    let transport = loopback();
    let busy = fetch(
        &transport,
        &request(Method::Get, &server.url("/")),
        &Plan::default(),
    );
    assert_eq!(
        (busy.ok().status, busy.body.as_slice()),
        (503, &b"busy"[..])
    );
    let unchanged = fetch(
        &transport,
        &request(Method::Get, &server.url("/")),
        &Plan::default(),
    );
    assert_eq!((unchanged.ok().status, unchanged.body.len()), (304, 0));
    let closed = fetch(
        &transport,
        &request(Method::Get, &server.url("/")),
        &Plan::default(),
    );
    assert_eq!(closed.body, b"read to close");
}

#[test]
fn caller_headers_cannot_inject_or_replace_host_headers() {
    let (transport, _, routes) = isolated();
    for (name, value) in [
        ("X-Note", "secret\r\nInjected: 1"),
        ("X-Note", "secret\nInjected: 1"),
        ("X-Note", "secret\0"),
        ("Bad Name", "x"),
        ("X-Note\r\nInjected", "x"),
        ("", "x"),
        ("Host", "elsewhere.example"),
        ("content-length", "0"),
        ("Transfer-Encoding", "chunked"),
        ("Connection", "keep-alive"),
        ("Accept-Encoding", "gzip"),
        ("Cookie", "a=b"),
    ] {
        let mut get = request(Method::Get, "http://127.0.0.1:9/");
        get.headers = vec![(name.into(), value.into())];
        let fetched = fetch(&transport, &get, &Plan::default());
        assert_eq!(fetched.code(), "validation", "{name:?}");
        assert!(!fetched.error().detail.contains("secret"), "{name:?}");
    }
    let mut get = request(Method::Get, "http://127.0.0.1:9/");
    get.body = b"body".to_vec();
    assert_eq!(
        fetch(&transport, &get, &Plan::default()).code(),
        "validation"
    );
    assert!(routes.attempts().is_empty(), "nothing was sent");
}

#[test]
fn a_request_body_over_its_limit_is_refused_before_connecting() {
    let (transport, _, routes) = isolated();
    let mut post = request(Method::Post, "http://127.0.0.1:9/");
    post.body = vec![b'x'; 11];
    let plan = Plan {
        max_request_bytes: 10,
        ..Plan::default()
    };
    assert_eq!(fetch(&transport, &post, &plan).code(), "resource-limit");
    // The head counts the host's own headers and the path as well as the caller's.
    for (header, path) in [(16 * 1024, 0), (15 * 1024, 1024)] {
        let mut get = request(
            Method::Get,
            &format!("http://127.0.0.1:9/{}", "p".repeat(path)),
        );
        get.headers = vec![("X-Large".into(), "v".repeat(header))];
        assert_eq!(
            fetch(&transport, &get, &Plan::default()).code(),
            "resource-limit"
        );
    }
    assert!(routes.attempts().is_empty());
}

#[test]
fn a_declared_length_over_the_limit_is_refused_before_reading_the_body() {
    let server = TestServer::canned(vec![
        [
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n".as_slice(),
            &[b'x'; 4096],
        ]
        .concat(),
    ])
    .unwrap();
    let plan = Plan {
        max_response_bytes: 1000,
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "resource-limit");
    assert!(fetched.body.is_empty() && fetched.progress.is_empty());
}

#[test]
fn a_streamed_body_crossing_the_limit_is_cut_off_before_the_crossing_piece() {
    let chunked = |chunks: usize| {
        let chunk = format!("190\r\n{}\r\n", "x".repeat(400));
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{}0\r\n\r\n",
            chunk.repeat(chunks)
        )
        .into_bytes()
    };
    let closed =
        |bytes: usize| format!("HTTP/1.1 200 OK\r\n\r\n{}", "y".repeat(bytes)).into_bytes();
    let sized = |bytes: usize| {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\n\r\n{}",
            "z".repeat(bytes)
        )
        .into_bytes()
    };
    let server = TestServer::canned(vec![
        chunked(3),
        closed(1500),
        chunked(2),
        closed(800),
        sized(800),
    ])
    .unwrap();
    let plan = Plan {
        max_response_bytes: 1000,
        ..Plan::default()
    };
    let transport = loopback();
    for _ in 0..2 {
        let fetched = fetch(&transport, &request(Method::Get, &server.url("/")), &plan);
        assert_eq!(fetched.code(), "resource-limit");
        assert!(fetched.body.len() <= 1000, "{}", fetched.body.len());
        assert!(
            fetched
                .progress
                .iter()
                .all(|&(received, _)| received <= 1000)
        );
    }
    // A body of exactly the limit is whole: the reader stops one byte past it, not at it.
    for expected in [800, 800, 800] {
        let plan = Plan {
            max_response_bytes: expected,
            ..Plan::default()
        };
        let fetched = fetch(&transport, &request(Method::Get, &server.url("/")), &plan);
        assert_eq!(fetched.ok().received, expected);
        assert_eq!(fetched.body.len() as u64, expected);
    }
}

#[test]
fn a_response_head_over_its_bounds_is_refused() {
    let long = format!(
        "HTTP/1.1 200 OK\r\nX-Long: {}\r\nContent-Length: 0\r\n\r\n",
        "v".repeat(70 * 1024)
    );
    let many = format!(
        "HTTP/1.1 200 OK\r\n{}Content-Length: 0\r\n\r\n",
        "X-Field: v\r\n".repeat(101)
    );
    let far_too_many = format!(
        "HTTP/1.1 200 OK\r\n{}Content-Length: 0\r\n\r\n",
        "X-Field: v\r\n".repeat(200)
    );
    let cases = [
        ("long", long),
        ("many", many),
        ("far too many", far_too_many),
    ];
    let server = TestServer::canned(
        cases
            .iter()
            .map(|(_, bytes)| bytes.clone().into_bytes())
            .collect(),
    )
    .unwrap();
    let transport = loopback();
    for (name, _) in cases {
        let fetched = fetch(
            &transport,
            &request(Method::Get, &server.url("/")),
            &Plan::default(),
        );
        assert_eq!(fetched.code(), "resource-limit", "{name}");
    }
}

#[test]
fn interim_responses_are_skipped_and_identical_lengths_accepted() {
    let server = TestServer::canned(vec![
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\n\
          HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\nabc"
            .to_vec(),
    ])
    .unwrap();
    let fetched = fetch(
        &loopback(),
        &request(Method::Get, &server.url("/")),
        &Plan::default(),
    );
    assert_eq!(fetched.ok().status, 200);
    assert_eq!(fetched.body, b"abc");
    assert_eq!(
        fetched.ok().header("link"),
        None,
        "interim fields are not the response's"
    );
}

/// Framing `ureq` reads as the server meant it: bare line feeds, trailers (read and discarded
/// line by line, each within the connection's input buffer, their total bounded by the deadlines),
/// and a last chunk after which the server closes without ending the trailers.
#[test]
fn bare_line_feeds_trailers_and_an_unfinished_trailer_section_are_read() {
    let cases = [
        b"HTTP/1.1 200 OK\nContent-Length: 2\n\nok".to_vec(),
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n{}\r\n",
            "X-Trailer: t\r\n".repeat(6000)
        )
        .into_bytes(),
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\nX-Trailer: {}\r\n\r\n",
            "t".repeat(70 * 1024)
        )
        .into_bytes(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n".to_vec(),
    ];
    let count = cases.len();
    let server = TestServer::canned(cases.to_vec()).unwrap();
    let transport = loopback();
    for case in 0..count {
        let fetched = fetch(
            &transport,
            &request(Method::Get, &server.url("/")),
            &Plan::default(),
        );
        assert_eq!(fetched.ok().status, 200, "case {case}");
        assert_eq!(fetched.body, b"ok", "case {case}");
    }
}

#[test]
fn ambiguous_malformed_or_encoded_responses_are_read_errors() {
    let cases: [(&str, &[u8]); 10] = [
        (
            "both",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
        ),
        ("coding", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nabc"),
        ("compressed", b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 3\r\n\r\nabc"),
        ("length", b"HTTP/1.1 200 OK\r\nContent-Length: 3, 3\r\n\r\nabc"),
        ("short", b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc"),
        ("status", b"HTTP/1.1 2x0 OK\r\nContent-Length: 0\r\n\r\n"),
        ("chunk", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabc\r\n0\r\n\r\n"),
        ("coding on HTTP/1.0", b"HTTP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"),
        ("switched", b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n"),
        ("status range", b"HTTP/1.1 600 Beyond\r\nContent-Length: 0\r\n\r\n"),
    ];
    let server =
        TestServer::canned(cases.iter().map(|(_, bytes)| bytes.to_vec()).collect()).unwrap();
    let transport = loopback();
    for (name, _) in cases {
        let fetched = fetch(
            &transport,
            &request(Method::Get, &server.url("/")),
            &Plan::default(),
        );
        assert_eq!(fetched.code(), "read-error", "{name}");
    }
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let refused = fetch(
        &transport,
        &request(Method::Get, &format!("http://{closed}/")),
        &Plan::default(),
    );
    assert_eq!(refused.code(), "read-error");
}

#[test]
fn a_silent_server_hits_the_read_deadline() {
    let stall = Stall::new();
    let gate = stall.gate();
    let server = TestServer::http(move |_, _| gate.pass()).unwrap();
    // The server stays silent until the test ends and the request's total outlasts the gate's
    // hang bound, so only the read timeout can end the request.
    let plan = Plan {
        read_timeout: Duration::from_millis(300),
        total_timeout: HANG * 2,
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "read-error");
    assert!(
        fetched.error().detail.contains("timed out"),
        "{}",
        fetched.error()
    );
}

#[test]
fn a_trickling_server_hits_the_total_deadline() {
    let (told, received) = mpsc::channel();
    let received = Mutex::new(received);
    // One byte at a time, each once the client has read the one before, so the body keeps
    // flowing until the client gives up, long before its length arrives.
    let server = TestServer::http(move |_, out| {
        send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n");
        let received = received.lock().unwrap();
        loop {
            send(out, b"x");
            if received.recv_timeout(HANG).is_err() {
                break;
            }
        }
    })
    .unwrap();
    // The read timeout outlasts the hang bound, so only the total deadline can end the request.
    let plan = Plan {
        read_timeout: HANG * 2,
        total_timeout: Duration::from_millis(500),
        received: Some(told),
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "read-error");
    assert!(fetched.error().detail.contains("timed out"));
    assert!(!fetched.body.is_empty(), "the body was flowing");
}

#[test]
fn cancelling_a_stalled_body_returns_cancelled_promptly() {
    let (server, stall) = stalling(
        false,
        b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789",
    );
    let fetched = cancel_while_stalled(server.url("/"), &stall, 10, "the request was cancelled");
    assert_eq!(fetched.code(), "cancelled");
    assert_eq!(fetched.body, b"0123456789");

    let (transport, resolver, routes) = isolated();
    let plan = Plan {
        cancelled: true,
        ..Plan::default()
    };
    let fetched = fetch(
        &transport,
        &request(Method::Get, "https://downloads.example/"),
        &plan,
    );
    assert_eq!(fetched.code(), "cancelled");
    assert_eq!(resolver.calls(), 0, "a cancelled request never resolves");
    assert!(routes.attempts().is_empty());
}

#[test]
fn connection_attempts_share_the_request_deadline() {
    /// Waits out every attempt's whole timeout and then fails, recording each timeout it was given.
    #[derive(Default)]
    struct Stalled(Mutex<Vec<Duration>>);
    impl Connect for Stalled {
        fn connect(&self, _: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
            self.0.lock().unwrap().push(timeout);
            let started = Instant::now();
            wait_until("the attempt's timeout to pass", || {
                started.elapsed() >= timeout
            });
            Err(io::ErrorKind::TimedOut.into())
        }
    }
    let stalled = Arc::new(Stalled::default());
    let resolver = FakeResolver::new(vec![vec![PUBLIC, "1.1.1.1".parse().unwrap()]]);
    let transport = transport(test_roots(), resolver, stalled.clone());
    let plan = Plan {
        total_timeout: Duration::from_millis(300),
        ..Plan::default()
    };
    let fetched = fetch(
        &transport,
        &request(Method::Get, "https://downloads.example/"),
        &plan,
    );
    assert_eq!(fetched.code(), "read-error");
    assert!(
        fetched.error().detail.contains("timed out"),
        "{}",
        fetched.error()
    );
    let attempts = stalled.0.lock().unwrap().clone();
    assert_eq!(
        attempts.len(),
        1,
        "the first attempt spent the whole budget"
    );
    assert!(attempts[0] <= Duration::from_millis(300), "{attempts:?}");
}

#[test]
fn localhost_is_resolved_by_the_system_resolver_to_loopback_addresses() {
    let server = TestServer::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nlocal".to_vec(),
    ])
    .unwrap();
    let url = format!("http://localhost:{}/", server.address().port());
    let fetched = fetch(&loopback(), &request(Method::Get, &url), &Plan::default());
    assert_eq!(fetched.body, b"local");
    assert!(requests(&server)[0].contains(&format!(
        "\r\nhost: localhost:{}\r\n",
        server.address().port()
    )));
}

#[test]
fn redirects_are_refused_unless_the_policy_allows_them() {
    let target = TestServer::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ])
    .unwrap();
    let origin = TestServer::canned(vec![redirect_to(302, &target.url("/next"))]).unwrap();
    let fetched = fetch(
        &loopback(),
        &request(Method::Get, &origin.url("/")),
        &Plan {
            origins: vec![target.origin()],
            ..Plan::default()
        },
    );
    assert_eq!(fetched.code(), "validation");
    assert_eq!(fetched.error().detail, "redirect refused");
    assert!(fetched.body.is_empty());
    assert!(requests(&target).is_empty());
}

#[test]
fn a_redirect_to_a_listed_origin_is_followed_with_get_and_without_authorization() {
    let target = TestServer::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".to_vec(),
    ])
    .unwrap();
    let origin = TestServer::canned(vec![redirect_to(307, &target.url("/next"))]).unwrap();
    let mut post = request(Method::Post, &origin.url("/start"));
    post.headers = vec![
        ("Authorization".into(), "Bearer sentinel-token".into()),
        ("X-Request".into(), "kept".into()),
    ];
    post.body = b"payload".to_vec();
    let plan = Plan {
        redirects: 1,
        origins: vec![target.origin()],
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &post, &plan);
    assert_eq!(fetched.body, b"done");
    assert_eq!(fetched.ok().final_url.as_str(), target.url("/next"));
    assert!(requests(&origin)[0].contains("\r\nauthorization: Bearer sentinel-token\r\n"));
    let followed = &requests(&target)[0];
    assert!(followed.starts_with("GET /next HTTP/1.1\r\n"), "{followed}");
    assert!(followed.contains("\r\nx-request: kept\r\n"), "{followed}");
    assert!(
        !followed.to_ascii_lowercase().contains("authorization"),
        "{followed}"
    );
    assert!(
        !followed.to_ascii_lowercase().contains("content-length") && !followed.contains("payload")
    );
}

#[test]
fn a_same_origin_redirect_keeps_authorization() {
    let server = TestServer::canned(vec![
        redirect_to(301, "/moved?to=here#ignored"),
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ])
    .unwrap();
    let mut get = request(Method::Get, &server.url("/"));
    get.headers = vec![("Authorization".into(), "Bearer sentinel-token".into())];
    let plan = Plan {
        redirects: 3,
        origins: vec![server.origin()],
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &get, &plan);
    assert_eq!(fetched.body, b"ok");
    let seen = requests(&server);
    assert!(
        seen[1].starts_with("GET /moved?to=here HTTP/1.1\r\n"),
        "{}",
        seen[1]
    );
    assert!(seen[1].contains("\r\nauthorization: Bearer sentinel-token\r\n"));
}

#[test]
fn redirects_to_unlisted_origins_other_classes_or_plain_http_are_refused() {
    let target = TestServer::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ])
    .unwrap();
    let unlisted = TestServer::canned(vec![redirect_to(302, &target.url("/"))]).unwrap();
    let remote = TestServer::canned(vec![redirect_to(302, "https://downloads.example/")]).unwrap();
    let secure = TestServer::https(server_tls(), {
        let location = target.url("/");
        move |_, out| send(out, &redirect_to(302, &location))
    })
    .unwrap();
    let transport = loopback();
    for (origin, listed, reason) in [
        (&unlisted, vec![], "is not an allowed origin"),
        (
            &remote,
            vec!["https://downloads.example".to_string()],
            "remote endpoints are not allowed",
        ),
        (&secure, vec![target.origin()], "leaves https"),
    ] {
        let plan = Plan {
            redirects: 3,
            origins: listed,
            ..Plan::default()
        };
        let fetched = fetch(&transport, &request(Method::Get, &origin.url("/")), &plan);
        assert_eq!(fetched.code(), "validation", "{reason}");
        assert!(
            fetched.error().detail.contains(reason),
            "{}",
            fetched.error()
        );
    }
    assert!(requests(&target).is_empty());
}

#[test]
fn the_redirect_count_is_capped() {
    let server = TestServer::canned(vec![redirect_to(302, "/again")]).unwrap();
    let plan = Plan {
        redirects: 2,
        origins: vec![server.origin()],
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "validation");
    assert!(fetched.error().detail.contains("more than 2 redirects"));
    assert_eq!(requests(&server).len(), 3);

    let (transport, _, routes) = isolated();
    let plan = Plan {
        redirects: MAX_REDIRECTS + 1,
        ..Plan::default()
    };
    let fetched = fetch(
        &transport,
        &request(Method::Get, "http://127.0.0.1:9/"),
        &plan,
    );
    assert_eq!(fetched.code(), "validation");
    assert!(routes.attempts().is_empty());
}

#[test]
fn loopback_https_works_with_the_test_roots_and_fails_with_platform_trust() {
    let server = TestServer::https(server_tls(), |_, out| {
        send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecure");
    })
    .unwrap();
    let get = request(Method::Get, &server.url("/"));
    let fetched = fetch(&loopback(), &get, &Plan::default());
    assert_eq!(fetched.body, b"secure");

    let platform = Transport::new(TransportConfig {
        trust: TlsTrust::Platform,
        ..TransportConfig::default()
    })
    .unwrap();
    let fetched = fetch(&platform, &get, &Plan::default());
    assert_eq!(fetched.code(), "read-error");
    assert!(
        fetched.error().detail.contains("certificate"),
        "{}",
        fetched.error()
    );
}

#[test]
fn a_tls_body_that_ends_without_the_close_signal_is_refused() {
    // Answers with a close-delimited body and then drops the connection without `close_notify`,
    // so the body's end cannot be told from a truncation.
    let server = TestServer::start(
        Options {
            tls: Some(server_tls()),
            truncate_tls: true,
            ..Options::default()
        },
        |_, out| send(out, b"HTTP/1.1 200 OK\r\n\r\npartial"),
    )
    .unwrap();
    let fetched = fetch(
        &loopback(),
        &request(Method::Get, &server.url("/")),
        &Plan::default(),
    );
    assert_eq!(fetched.code(), "read-error");
    assert!(
        fetched.error().detail.contains("ended early"),
        "{}",
        fetched.error()
    );
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let server = TestServer::https(server_tls(), |_, out| {
        send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    })
    .unwrap();
    let resolver = FakeResolver::new(vec![vec![PUBLIC]]);
    let routes = Routes::to(SocketAddr::new(PUBLIC, 443), server.address());
    let transport = transport(test_roots(), resolver, routes.clone());
    let fetched = fetch(
        &transport,
        &request(Method::Get, "https://mismatch.example/"),
        &Plan::default(),
    );
    assert_eq!(fetched.code(), "read-error");
    assert!(
        fetched.error().detail.contains("certificate"),
        "{}",
        fetched.error()
    );
    assert_eq!(
        routes.attempts().len(),
        1,
        "the connection was made and refused by TLS"
    );
}

/// Start a request to `url` on a worker, wait until the server is held at `stall` and the client
/// has received `body` bytes of the body, cancel its job with `reason` from this thread and return
/// its result. The request's timeouts outlast the hang bound and the server is held until the test
/// ends, so only the cancel can end the request; it must end it while the server still holds it.
fn cancel_while_stalled(url: String, stall: &Stall, body: u64, reason: &str) -> Fetched {
    let control = JobControl::new();
    let (told, received) = mpsc::channel();
    let worker = {
        let control = control.clone();
        thread::spawn(move || {
            let plan = Plan {
                read_timeout: HANG * 2,
                total_timeout: HANG * 2,
                received: Some(told),
                ..Plan::default()
            };
            send_with(&loopback(), &request(Method::Get, &url), &plan, &control)
        })
    };
    stall.wait_reached(1, "the server's stall");
    let mut arrived = 0;
    while arrived < body {
        arrived = received
            .recv_timeout(HANG)
            .expect("the body before the stall arrived");
    }
    control.cancel(reason);
    let fetched = worker.join().unwrap();
    assert!(
        stall.holding(),
        "the cancel ended the request while the server held it"
    );
    fetched
}

/// A server that answers `bytes` and then holds the connection open at the returned stall.
fn stalling(tls: bool, bytes: &'static [u8]) -> (TestServer, Stall) {
    let stall = Stall::new();
    let gate = stall.gate();
    let respond = move |_: &Request, out: &mut dyn Write| {
        send(out, bytes);
        gate.pass();
    };
    let server = if tls {
        TestServer::https(server_tls(), respond)
    } else {
        TestServer::http(respond)
    };
    (server.unwrap(), stall)
}

#[test]
fn cancelling_a_stalled_request_shuts_its_socket_down_promptly() {
    for (what, tls, bytes) in [
        (
            "TLS body",
            true,
            &b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789"[..],
        ),
        ("TLS head", true, b"HTTP/1.1 200 OK\r\n"),
        (
            "plain body",
            false,
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789",
        ),
        ("plain head", false, b"HTTP/1.1 200 OK\r\n"),
        // A shut-down plain socket reads as the end of a close-delimited body.
        (
            "close-delimited plain body",
            false,
            b"HTTP/1.1 200 OK\r\n\r\n0123456789",
        ),
    ] {
        let (server, stall) = stalling(tls, bytes);
        let fetched = cancel_while_stalled(server.url("/"), &stall, 0, "permission revoked");
        assert_eq!(fetched.code(), "cancelled", "{what}");
        assert_eq!(fetched.error().detail, "permission revoked", "{what}");
    }
}

#[test]
fn cancelling_a_stalled_tls_handshake_shuts_its_socket_down_promptly() {
    // Accepts the connection and never answers the client's hello.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stall = Stall::new();
    let gate = stall.gate();
    let holder = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        gate.pass();
        drop(socket);
    });
    let fetched = cancel_while_stalled(
        format!("https://{address}/"),
        &stall,
        0,
        "permission revoked",
    );
    assert_eq!(fetched.code(), "cancelled");
    drop(stall);
    holder.join().unwrap();
}

#[test]
fn a_stalled_tls_read_hits_the_idle_timeout() {
    for bytes in [
        &b"HTTP/1.1 200 OK\r\n"[..],
        b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789",
    ] {
        // Held silent until the test ends, with a total that outlasts the hang bound: only the
        // idle timeout can end the request.
        let (server, _stall) = stalling(true, bytes);
        let plan = Plan {
            read_timeout: Duration::from_millis(300),
            total_timeout: HANG * 2,
            ..Plan::default()
        };
        let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
        assert_eq!(fetched.code(), "read-error");
        assert!(
            fetched.error().detail.contains("timed out"),
            "{}",
            fetched.error()
        );
    }
}

#[test]
fn a_non_ascii_endpoint_host_is_refused_and_its_punycode_form_resolves() {
    let error = parse_endpoint("https://bücher.example/palette.bin", BOTH).unwrap_err();
    assert_eq!(error.kind.code(), "validation");
    assert!(
        error.detail.contains("international domain name"),
        "{error}"
    );
    /// Records the names it is asked for and answers a public address.
    #[derive(Default)]
    struct Names(Mutex<Vec<String>>);
    impl Resolve for Names {
        fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
            self.0.lock().unwrap().push(host.to_owned());
            Ok(vec![SocketAddr::new(PUBLIC, port)])
        }
    }
    let server = TestServer::https(server_tls(), |_, out| {
        send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    })
    .unwrap();
    let names = Arc::new(Names::default());
    let routes = Routes::to(SocketAddr::new(PUBLIC, 443), server.address());
    let transport = transport(test_roots(), names.clone(), routes.clone());
    let fetched = fetch(
        &transport,
        &request(Method::Get, "https://xn--bcher-kva.example/palette.bin"),
        &Plan::default(),
    );
    // The test certificate names neither host, so TLS refuses the connection the name reached.
    assert_eq!(fetched.code(), "read-error");
    assert!(
        fetched.error().detail.contains("certificate"),
        "{}",
        fetched.error()
    );
    assert_eq!(*names.0.lock().unwrap(), ["xn--bcher-kva.example"]);
    assert_eq!(routes.attempts().len(), 1);
}
