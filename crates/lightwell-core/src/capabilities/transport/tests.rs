//! End-to-end transport tests against loopback servers, a fake resolver and a connector that routes
//! chosen public addresses to those servers. Nothing here leaves the machine.
use super::{http::Stream, *};
use rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    pki_types::{PrivateKeyDer, pem::PemObject},
};
use std::{
    io::{self, Read},
    net::{IpAddr, SocketAddr, TcpListener},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
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

/// Read one request head and its `Content-Length` body, as text.
fn read_request(stream: &mut dyn Read) -> Option<String> {
    let mut data = Vec::new();
    let mut buffer = [0; 4096];
    let head_end = loop {
        if let Some(at) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut buffer).ok().filter(|&read| read > 0)?;
        data.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .map_or(0, |value| value.trim().parse().unwrap());
    while data.len() < head_end + length {
        let read = stream.read(&mut buffer).ok().filter(|&read| read > 0)?;
        data.extend_from_slice(&buffer[..read]);
    }
    Some(String::from_utf8_lossy(&data).into_owned())
}

/// A loopback server that records every request and answers connection `n` with `respond`.
struct Server {
    address: SocketAddr,
    scheme: &'static str,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(
        tls: Option<Arc<ServerConfig>>,
        respond: impl Fn(usize, &mut dyn Write) + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let scheme = if tls.is_some() { "https" } else { "http" };
        thread::spawn(move || {
            for (index, socket) in listener.incoming().enumerate() {
                let Ok(socket) = socket else { return };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let handle = |stream: &mut dyn Stream| {
                    if let Some(request) = read_request(stream) {
                        recorded.lock().unwrap().push(request);
                        respond(index, stream);
                    }
                };
                match &tls {
                    Some(config) => {
                        let session = ServerConnection::new(Arc::clone(config)).unwrap();
                        let mut stream = StreamOwned::new(session, socket);
                        handle(&mut stream);
                        stream.conn.send_close_notify();
                        let _ = stream.flush();
                    }
                    None => handle(&mut { socket }),
                }
            }
        });
        Self {
            address,
            scheme,
            requests,
        }
    }

    /// A plain server that answers connection `n` with `responses[n]`, or the last one.
    fn canned(responses: Vec<Vec<u8>>) -> Self {
        Self::start(None, move |index, out| {
            reply(out, &responses[index.min(responses.len() - 1)]);
        })
    }

    fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.address)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn reply(out: &mut dyn Write, bytes: &[u8]) {
    let _ = out.write_all(bytes);
    let _ = out.flush();
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
    cancel_after: Option<Duration>,
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
            cancel_after: None,
        }
    }
}

struct Fetched {
    result: Result<TransportResponse, Error>,
    body: Vec<u8>,
    progress: Vec<(u64, Option<u64>)>,
    elapsed: Duration,
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
    let started = Instant::now();
    let cancel = || {
        plan.cancel_after
            .is_some_and(|after| started.elapsed() >= after)
    };
    let mut seen = Vec::new();
    let mut progress = |received, total| seen.push((received, total));
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
            cancel: &cancel,
            progress: &mut progress,
        },
        &mut body,
    );
    Fetched {
        result,
        body,
        progress: seen,
        elapsed: started.elapsed(),
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
    let server = Server::start(Some(server_tls()), |_, out| {
        reply(out, b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\npalette");
    });
    // The first lookup answers a public address; any later one would answer a private address.
    let resolver = FakeResolver::new(vec![vec![PUBLIC], vec!["10.0.0.1".parse().unwrap()]]);
    let public = SocketAddr::new(PUBLIC, 443);
    let routes = Routes::to(public, server.address);
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
    let seen = server.requests();
    assert!(seen[0].starts_with("GET /models/palette.bin HTTP/1.1\r\nHost: downloads.example\r\n"));

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
    let server = Server::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Extra:  padded \r\nContent-Length: 11\r\n\r\nhello world"
            .to_vec(),
    ]);
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
    let seen = &server.requests()[0];
    let expected = format!(
        "GET /v1/status?verbose=1 HTTP/1.1\r\nHost: {}\r\nUser-Agent: Lightwell/{}\r\n\
         Accept-Encoding: identity\r\nConnection: close\r\n\r\n",
        server.address,
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(seen, &expected);
}

#[test]
fn loopback_http_post_sends_a_host_framed_body_and_decodes_a_chunked_reply() {
    let server = Server::canned(vec![
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
          4;name=value\r\n{\"ok\r\n6\r\n\":true\r\n1\r\n}\r\n0\r\nX-Trailer: done\r\n\r\n"
            .to_vec(),
    ]);
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
    let seen = &server.requests()[0];
    assert!(seen.starts_with("POST /v1/run HTTP/1.1\r\n"), "{seen}");
    assert!(seen.contains("\r\nContent-Length: 16\r\n"));
    assert!(seen.contains("\r\nAuthorization: Bearer sentinel-token\r\n"));
    assert!(seen.ends_with("\r\n\r\n{\"grid\":[1,2,3]}"));
}

#[test]
fn non_success_statuses_are_returned_as_data() {
    let server = Server::canned(vec![
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\nbusy".to_vec(),
        b"HTTP/1.1 304 Not Modified\r\nContent-Length: 99\r\n\r\n".to_vec(),
        b"HTTP/1.0 200 OK\r\n\r\nread to close".to_vec(),
    ]);
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
    assert!(routes.attempts().is_empty());
}

#[test]
fn a_declared_length_over_the_limit_is_refused_before_reading_the_body() {
    let server = Server::canned(vec![
        [
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n".as_slice(),
            &[b'x'; 4096],
        ]
        .concat(),
    ]);
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
    let chunk = format!("190\r\n{}\r\n", "x".repeat(400));
    let chunked = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{}0\r\n\r\n",
        chunk.repeat(3)
    );
    let closed = format!("HTTP/1.1 200 OK\r\n\r\n{}", "y".repeat(1500));
    let server = Server::canned(vec![chunked.into_bytes(), closed.into_bytes()]);
    let plan = Plan {
        max_response_bytes: 1000,
        ..Plan::default()
    };
    let transport = loopback();
    let fetched = fetch(&transport, &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "resource-limit");
    assert_eq!(fetched.body.len(), 800, "two whole chunks, not the third");
    let fetched = fetch(&transport, &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "resource-limit");
    assert!(fetched.body.len() <= 1000);
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
    let server = Server::canned(vec![long.into_bytes(), many.into_bytes()]);
    let transport = loopback();
    for _ in 0..2 {
        let fetched = fetch(
            &transport,
            &request(Method::Get, &server.url("/")),
            &Plan::default(),
        );
        assert_eq!(fetched.code(), "resource-limit");
    }
}

#[test]
fn ambiguous_malformed_or_encoded_responses_are_read_errors() {
    let cases: [(&str, &[u8]); 8] = [
        (
            "both",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
        ),
        ("coding", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nabc"),
        ("compressed", b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 3\r\n\r\nabc"),
        ("length", b"HTTP/1.1 200 OK\r\nContent-Length: 3, 3\r\n\r\nabc"),
        ("short", b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc"),
        ("status", b"HTTP/1.1 2x0 OK\r\nContent-Length: 0\r\n\r\n"),
        ("bare line feed", b"HTTP/1.1 200 OK\nContent-Length: 0\r\n\r\n"),
        ("chunk", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabc\r\n0\r\n\r\n"),
    ];
    let server = Server::canned(cases.iter().map(|(_, bytes)| bytes.to_vec()).collect());
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
    let server = Server::start(None, |_, _| thread::sleep(Duration::from_secs(3)));
    let plan = Plan {
        read_timeout: Duration::from_millis(300),
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "read-error");
    assert!(
        fetched.error().detail.contains("timed out"),
        "{}",
        fetched.error()
    );
    assert!(
        fetched.elapsed < Duration::from_millis(1500),
        "{:?}",
        fetched.elapsed
    );
}

#[test]
fn a_trickling_server_hits_the_total_deadline() {
    let server = Server::start(None, |_, out| {
        reply(out, b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n");
        for _ in 0..100 {
            reply(out, b"x");
            thread::sleep(Duration::from_millis(50));
        }
    });
    let plan = Plan {
        read_timeout: Duration::from_secs(1),
        total_timeout: Duration::from_millis(500),
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "read-error");
    assert!(fetched.error().detail.contains("timed out"));
    assert!(!fetched.body.is_empty(), "the body was flowing");
    assert!(
        fetched.elapsed < Duration::from_millis(1500),
        "{:?}",
        fetched.elapsed
    );
}

#[test]
fn cancelling_a_stalled_body_returns_cancelled_promptly() {
    let server = Server::start(None, |_, out| {
        reply(
            out,
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789",
        );
        thread::sleep(Duration::from_secs(5));
    });
    let cancel_after = Duration::from_millis(300);
    let plan = Plan {
        cancel_after: Some(cancel_after),
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "cancelled");
    assert_eq!(fetched.body, b"0123456789");
    assert!(
        fetched.elapsed < cancel_after + Duration::from_millis(500),
        "{:?}",
        fetched.elapsed
    );

    let (transport, resolver, routes) = isolated();
    let plan = Plan {
        cancel_after: Some(Duration::ZERO),
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
            thread::sleep(timeout);
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
    assert!(
        fetched.elapsed < Duration::from_millis(1000),
        "{:?}",
        fetched.elapsed
    );
}

#[test]
fn localhost_is_resolved_by_the_system_resolver_to_loopback_addresses() {
    let server = Server::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nlocal".to_vec(),
    ]);
    let url = format!("http://localhost:{}/", server.address.port());
    let fetched = fetch(&loopback(), &request(Method::Get, &url), &Plan::default());
    assert_eq!(fetched.body, b"local");
    assert!(server.requests()[0].contains(&format!(
        "\r\nHost: localhost:{}\r\n",
        server.address.port()
    )));
}

#[test]
fn redirects_are_refused_unless_the_policy_allows_them() {
    let target = Server::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ]);
    let origin = Server::canned(vec![redirect_to(302, &target.url("/next"))]);
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
    assert!(target.requests().is_empty());
}

#[test]
fn a_redirect_to_a_listed_origin_is_followed_with_get_and_without_authorization() {
    let target = Server::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".to_vec(),
    ]);
    let origin = Server::canned(vec![redirect_to(307, &target.url("/next"))]);
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
    assert!(origin.requests()[0].contains("Authorization: Bearer sentinel-token"));
    let followed = &target.requests()[0];
    assert!(followed.starts_with("GET /next HTTP/1.1\r\n"), "{followed}");
    assert!(followed.contains("\r\nX-Request: kept\r\n"));
    assert!(
        !followed.to_ascii_lowercase().contains("authorization"),
        "{followed}"
    );
    assert!(!followed.contains("Content-Length") && !followed.contains("payload"));
}

#[test]
fn a_same_origin_redirect_keeps_authorization() {
    let server = Server::canned(vec![
        redirect_to(301, "/moved?to=here#ignored"),
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ]);
    let mut get = request(Method::Get, &server.url("/"));
    get.headers = vec![("Authorization".into(), "Bearer sentinel-token".into())];
    let plan = Plan {
        redirects: 3,
        origins: vec![server.origin()],
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &get, &plan);
    assert_eq!(fetched.body, b"ok");
    let seen = server.requests();
    assert!(
        seen[1].starts_with("GET /moved?to=here HTTP/1.1\r\n"),
        "{}",
        seen[1]
    );
    assert!(seen[1].contains("Authorization: Bearer sentinel-token"));
}

#[test]
fn redirects_to_unlisted_origins_other_classes_or_plain_http_are_refused() {
    let target = Server::canned(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ]);
    let unlisted = Server::canned(vec![redirect_to(302, &target.url("/"))]);
    let remote = Server::canned(vec![redirect_to(302, "https://downloads.example/")]);
    let secure = Server::start(Some(server_tls()), {
        let location = target.url("/");
        move |_, out| reply(out, &redirect_to(302, &location))
    });
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
    assert!(target.requests().is_empty());
}

#[test]
fn the_redirect_count_is_capped() {
    let server = Server::canned(vec![redirect_to(302, "/again")]);
    let plan = Plan {
        redirects: 2,
        origins: vec![server.origin()],
        ..Plan::default()
    };
    let fetched = fetch(&loopback(), &request(Method::Get, &server.url("/")), &plan);
    assert_eq!(fetched.code(), "validation");
    assert!(fetched.error().detail.contains("more than 2 redirects"));
    assert_eq!(server.requests().len(), 3);

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
    let server = Server::start(Some(server_tls()), |_, out| {
        reply(out, b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecure");
    });
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
fn a_certificate_for_another_name_is_refused() {
    let server = Server::start(Some(server_tls()), |_, out| {
        reply(out, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    });
    let resolver = FakeResolver::new(vec![vec![PUBLIC]]);
    let routes = Routes::to(SocketAddr::new(PUBLIC, 443), server.address);
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
