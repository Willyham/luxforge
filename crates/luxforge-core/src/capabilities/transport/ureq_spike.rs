//! Spike: `ureq`'s own agent behind the transport's address policy, cancelled by shutting its
//! socket down. Test-only; the production transport is unchanged. See
//! `docs/design/module-capabilities.md#transport` for what it established.
//!
//! One agent per request, built with `Agent::with_parts` from a policy resolver (resolves the host
//! once through the transport's `Resolve`, requires every address to pass `address_allowed` for the
//! endpoint's class) and a policy connector (connects through the transport's `Connect` to checked
//! addresses only and registers a clone of the socket with an owned cancel handle), chained into a
//! rustls connector on the transport's own client configuration. No proxy, no pool, no redirect
//! following, statuses returned as data.
use super::{
    Connect, EndpointClass, Resolve, address_allowed, parse_endpoint, policy::Endpoint, tls,
};
use crate::Error;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use std::{
    fmt,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};
use ureq::{
    Agent,
    config::Config,
    http::Uri,
    unversioned::{
        resolver::{ResolvedSocketAddrs, Resolver},
        transport::{
            Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
            TransportAdapter,
        },
    },
};
use url::Host;

/// An owned cancel handle: the connector registers each connection's cloned socket, and `cancel`
/// shuts that socket down from any thread, so a read or write blocked on it returns at once. In
/// the transport this would live on the job's `Arc`-shared `JobControl`.
#[derive(Default)]
struct SocketCancel(Mutex<CancelState>);

#[derive(Default)]
struct CancelState {
    cancelled: bool,
    socket: Option<TcpStream>,
}

impl SocketCancel {
    fn cancel(&self) {
        let mut state = self.0.lock().unwrap();
        state.cancelled = true;
        if let Some(socket) = &state.socket {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.0.lock().unwrap().cancelled
    }

    /// Keep a clone of `socket` for `cancel`; `false` if the request is already cancelled. The
    /// check and the registration share the lock, so a cancel cannot fall between them.
    fn register(&self, socket: &TcpStream) -> io::Result<bool> {
        let mut state = self.0.lock().unwrap();
        if state.cancelled {
            return Ok(false);
        }
        state.socket = Some(socket.try_clone()?);
        Ok(true)
    }

    /// Drop the clone once the request is over.
    fn release(&self) {
        self.0.lock().unwrap().socket = None;
    }
}

fn refused(error: Error) -> ureq::Error {
    ureq::Error::Other(Box::new(error))
}

fn cancelled() -> Error {
    Error::cancelled("the request was cancelled")
}

/// Resolves the endpoint's host exactly once through the transport's `Resolve` and answers only
/// if every address belongs to the endpoint's class. An IP literal is checked without a lookup.
struct PolicyResolver {
    endpoint: Endpoint,
    resolve: Arc<dyn Resolve>,
    cancel: Arc<SocketCancel>,
}

impl fmt::Debug for PolicyResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolicyResolver").finish_non_exhaustive()
    }
}

impl Resolver for PolicyResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _: &Config,
        _: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        if self.cancel.is_cancelled() {
            return Err(refused(cancelled()));
        }
        let url = &self.endpoint.url;
        // The agent follows no redirects, so it only ever asks for the endpoint's own host.
        let name = url.host_str().unwrap_or_default();
        if uri.host() != Some(name) {
            return Err(refused(Error::validation(format!(
                "{} is not the endpoint's host",
                uri.host().unwrap_or_default()
            ))));
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| refused(Error::validation("URL has no port")))?;
        let addresses = match url.host() {
            Some(Host::Domain(domain)) => self.resolve.resolve(domain, port).map_err(|error| {
                refused(Error::file_access(format!(
                    "cannot resolve {name}: {}",
                    error.kind()
                )))
            })?,
            Some(Host::Ipv4(address)) => vec![SocketAddr::new(address.into(), port)],
            Some(Host::Ipv6(address)) => vec![SocketAddr::new(address.into(), port)],
            None => return Err(refused(Error::validation("URL has no host"))),
        };
        if addresses.is_empty() {
            return Err(refused(Error::file_access(format!(
                "{name} did not resolve to any address"
            ))));
        }
        let class = self.endpoint.class;
        if let Some(address) = addresses
            .iter()
            .map(SocketAddr::ip)
            .find(|&address| !address_allowed(address, class))
        {
            return Err(refused(Error::validation(match url.host() {
                Some(Host::Domain(_)) => format!(
                    "{name} resolved to {address}, which is not a {} address",
                    class.label()
                ),
                _ => format!("{address} is not a {} address", class.label()),
            })));
        }
        let mut answer = self.empty();
        for address in addresses {
            if answer.try_push(address).is_err() {
                break;
            }
        }
        Ok(answer)
    }
}

/// A plain socket as `ureq` transport. `ureq` 3.4.2 does not export its own `TcpTransport` (it is
/// public in a private module), so a connector that opens its own socket supplies this.
struct SocketTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
}

impl fmt::Debug for SocketTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketTransport").finish_non_exhaustive()
    }
}

/// A socket timeout as `ureq`'s timeout for the phase it expired in.
fn timed(error: io::Error, timeout: NextTimeout) -> ureq::Error {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => ureq::Error::Timeout(timeout.reason),
        _ => ureq::Error::Io(error),
    }
}

impl Transport for SocketTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.stream
            .set_write_timeout(timeout.not_zero().map(|after| *after))?;
        let output = &self.buffers.output()[..amount];
        self.stream
            .write_all(output)
            .map_err(|error| timed(error, timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream
            .set_read_timeout(timeout.not_zero().map(|after| *after))?;
        let input = self.buffers.input_append_buf();
        let amount = self
            .stream
            .read(input)
            .map_err(|error| timed(error, timeout))?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    /// Never reused: the agent keeps no idle connection.
    fn is_open(&mut self) -> bool {
        false
    }
}

/// Opens a direct connection through the transport's `Connect` to an address the resolver
/// answered, checking it again against the class, and registers the socket for cancellation.
struct PolicyConnector {
    class: EndpointClass,
    connect: Arc<dyn Connect>,
    cancel: Arc<SocketCancel>,
}

impl fmt::Debug for PolicyConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolicyConnector").finish_non_exhaustive()
    }
}

impl Connector for PolicyConnector {
    type Out = SocketTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _: Option<()>,
    ) -> Result<Option<SocketTransport>, ureq::Error> {
        let timeout = details
            .timeout
            .not_zero()
            .map_or(Duration::from_secs(30), |timeout| *timeout);
        let mut last = io::ErrorKind::NotConnected;
        for &address in details.addrs.iter() {
            if !address_allowed(address.ip(), self.class) {
                return Err(refused(Error::validation(format!(
                    "{} is not a {} address",
                    address.ip(),
                    self.class.label()
                ))));
            }
            if self.cancel.is_cancelled() {
                return Err(refused(cancelled()));
            }
            match self.connect.connect(address, timeout) {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    if !self.cancel.register(&stream)? {
                        return Err(refused(cancelled()));
                    }
                    let buffers = LazyBuffers::new(
                        details.config.input_buffer_size(),
                        details.config.output_buffer_size(),
                    );
                    return Ok(Some(SocketTransport { stream, buffers }));
                }
                Err(error) => last = error.kind(),
            }
        }
        Err(ureq::Error::Io(last.into()))
    }
}

/// TLS over the policy connector's socket with the transport's own rustls configuration (ring,
/// TLS 1.3 and 1.2, ALPN `http/1.1`, the platform verifier or exact roots), built once per
/// transport and shared by every request's agent. `ureq`'s own `RustlsConnector` is not used: `ureq`
/// ends a close-delimited body at an `UnexpectedEof`, which is how rustls reports a connection that
/// ended without the server's close signal, so a truncated body would be accepted as whole.
struct PolicyTls {
    config: Arc<ClientConfig>,
    endpoint: Endpoint,
}

impl fmt::Debug for PolicyTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolicyTls").finish_non_exhaustive()
    }
}

impl Connector<SocketTransport> for PolicyTls {
    type Out = Either<SocketTransport, TlsTransport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<SocketTransport>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let Some(socket) = chained else {
            return Ok(None);
        };
        if !details.needs_tls() {
            return Ok(Some(Either::A(socket)));
        }
        let name = tls::server_name(&self.endpoint).map_err(refused)?;
        let mut session = ClientConnection::new(self.config.clone(), name)
            .map_err(|error| refused(Error::file_access(format!("cannot start TLS: {error}"))))?;
        let mut adapter = TransportAdapter::new(socket.boxed());
        adapter.set_timeout(details.timeout);
        session.complete_io(&mut adapter)?;
        Ok(Some(Either::B(TlsTransport {
            stream: StreamOwned::new(session, adapter),
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
        })))
    }
}

struct TlsTransport {
    stream: StreamOwned<ClientConnection, TransportAdapter>,
    buffers: LazyBuffers,
}

impl fmt::Debug for TlsTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsTransport").finish_non_exhaustive()
    }
}

impl Transport for TlsTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.stream.get_mut().set_timeout(timeout);
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output)?;
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream.get_mut().set_timeout(timeout);
        let input = self.buffers.input_append_buf();
        let amount = match self.stream.read(input) {
            Ok(amount) => amount,
            // rustls's word for a connection closed without `close_notify`, which `ureq` would
            // take for the end of a close-delimited body.
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(ureq::Error::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the TLS connection ended early, without the server's close signal",
                )));
            }
            Err(error) => return Err(error.into()),
        };
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        false
    }

    fn is_tls(&self) -> bool {
        true
    }
}

/// The per-transport parts a request's agent is built from.
struct Parts {
    tls: Arc<ClientConfig>,
    resolve: Arc<dyn Resolve>,
    connect: Arc<dyn Connect>,
}

/// One request's agent: the policy resolver, connector and TLS for `endpoint`, and every agent
/// behaviour the transport does not want switched off.
fn agent(parts: &Parts, endpoint: &Endpoint, cancel: &Arc<SocketCancel>, total: Duration) -> Agent {
    let config = Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .max_response_header_size(64 * 1024)
        .accept_encoding("identity")
        .user_agent("luxforge")
        .timeout_global(Some(total))
        .build();
    let connector = PolicyConnector {
        class: endpoint.class,
        connect: parts.connect.clone(),
        cancel: cancel.clone(),
    }
    .chain(PolicyTls {
        config: parts.tls.clone(),
        endpoint: endpoint.clone(),
    });
    let resolver = PolicyResolver {
        endpoint: endpoint.clone(),
        resolve: parts.resolve.clone(),
        cancel: cancel.clone(),
    };
    Agent::with_parts(config, connector, resolver)
}

/// What a spiked `GET` answered.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Vec<u8>,
}

/// Map an agent failure back onto the transport's error kinds: a cancel wins over whatever the
/// shut-down socket reported, a policy refusal comes back as itself, and everything else is a
/// `read-error`.
fn failed(error: ureq::Error, cancel: &SocketCancel) -> Error {
    if cancel.is_cancelled() {
        return cancelled();
    }
    match error {
        ureq::Error::Other(inner) => match inner.downcast::<Error>() {
            Ok(error) => *error,
            Err(other) => Error::file_access(format!("request failed: {other}")),
        },
        ureq::Error::Timeout(_) => Error::file_access("the request timed out"),
        other => Error::file_access(format!("request failed: {other}")),
    }
}

/// `GET` `url` as `class` on a fresh agent, reading the whole body.
fn get(
    parts: &Parts,
    url: &str,
    class: EndpointClass,
    cancel: &Arc<SocketCancel>,
    total: Duration,
) -> Result<Answer, Error> {
    let endpoint = parse_endpoint(url, &[class])?;
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let agent = agent(parts, &endpoint, cancel, total);
    let result = agent
        .get(endpoint.url.as_str())
        .call()
        .and_then(|mut response| {
            let mut body = Vec::new();
            response
                .body_mut()
                .with_config()
                .limit(1 << 20)
                .reader()
                .read_to_end(&mut body)?;
            Ok(Answer {
                status: response.status().as_u16(),
                body,
            })
        });
    cancel.release();
    // A shut-down plain socket reads as the end of a close-delimited body, so a cancel also
    // overrides a success.
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    result.map_err(|error| failed(error, cancel))
}

#[cfg(test)]
mod tests {
    //! The address-policy and TLS-roots tests of `transport/tests.rs`, ported onto the spiked agent,
    //! and the cancellation proof. Loopback servers only.
    use super::*;
    use crate::capabilities::transport::{
        CertificateDer, SystemConnector, SystemResolver, TlsTrust,
    };
    use luxforge_testkit::{Options, TestServer, send};
    use rustls::{
        ServerConfig,
        pki_types::{PrivateKeyDer, pem::PemObject},
    };
    use std::{
        net::{IpAddr, TcpListener},
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Instant,
    };

    const PUBLIC: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(9, 9, 9, 9));
    const TOTAL: Duration = Duration::from_secs(30);

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

    fn parts(trust: TlsTrust, resolve: Arc<dyn Resolve>, connect: Arc<dyn Connect>) -> Parts {
        Parts {
            tls: tls::client_config(&trust).unwrap(),
            resolve,
            connect,
        }
    }

    fn loopback() -> Parts {
        parts(
            test_roots(),
            Arc::new(SystemResolver),
            Arc::new(SystemConnector),
        )
    }

    fn fetch(parts: &Parts, url: &str, class: EndpointClass) -> Result<Answer, Error> {
        get(parts, url, class, &Arc::default(), TOTAL)
    }

    #[test]
    fn spike_a_resolution_with_any_non_public_address_is_refused_before_connecting() {
        for answer in [
            vec![PUBLIC, "10.0.0.1".parse().unwrap()],
            vec!["127.0.0.1".parse().unwrap()],
            vec![PUBLIC, "::ffff:192.168.0.1".parse().unwrap()],
            vec!["fd00::1".parse().unwrap()],
        ] {
            let resolver = FakeResolver::new(vec![answer.clone()]);
            let routes = Arc::new(Routes::default());
            let parts = parts(test_roots(), resolver.clone(), routes.clone());
            let error = fetch(
                &parts,
                "https://downloads.example/palette.bin",
                EndpointClass::Remote,
            )
            .unwrap_err();
            assert_eq!(error.kind.code(), "validation", "{answer:?}");
            assert!(
                error.detail.contains("which is not a remote address"),
                "{error}"
            );
            assert_eq!(resolver.calls(), 1);
            assert!(routes.attempts().is_empty(), "{answer:?}");
        }
        let resolver = FakeResolver::new(vec![vec![PUBLIC]]);
        let routes = Arc::new(Routes::default());
        let parts = parts(test_roots(), resolver.clone(), routes.clone());
        for literal in [
            "https://10.0.0.1/",
            "https://[::ffff:10.0.0.1]/",
            "https://169.254.169.254/",
        ] {
            let error = fetch(&parts, literal, EndpointClass::Remote).unwrap_err();
            assert_eq!(error.kind.code(), "validation", "{literal}");
        }
        assert!(routes.attempts().is_empty());
        // An allowed literal is connected to as written: the one attempt is refused by the routes.
        let error = fetch(&parts, "https://[::1]:8443/", EndpointClass::Loopback).unwrap_err();
        assert_eq!(error.kind.code(), "read-error", "{error}");
        assert_eq!(routes.attempts(), vec!["[::1]:8443".parse().unwrap()]);
        assert_eq!(resolver.calls(), 0, "an IP literal is never looked up");
    }

    #[test]
    fn spike_a_remote_https_download_resolves_once_and_cannot_be_rebound() {
        let server = TestServer::https(server_tls(), |_, out| {
            send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\npalette");
        })
        .unwrap();
        let resolver = FakeResolver::new(vec![vec![PUBLIC], vec!["10.0.0.1".parse().unwrap()]]);
        let public = SocketAddr::new(PUBLIC, 443);
        let routes = Routes::to(public, server.address());
        let parts = parts(test_roots(), resolver.clone(), routes.clone());
        let url = "https://downloads.example/models/palette.bin";

        let answer = fetch(&parts, url, EndpointClass::Remote).unwrap();
        assert_eq!(answer.status, 200);
        assert_eq!(answer.body, b"palette");
        assert_eq!(resolver.calls(), 1, "one lookup per connection");
        assert_eq!(routes.attempts(), vec![public], "only the checked address");
        // `ureq` writes `host` last and adds `accept: */*`; the name is what matters here.
        let seen = server.requests()[0].clone();
        assert_eq!(
            (seen.method.as_str(), seen.path.as_str()),
            ("GET", "/models/palette.bin")
        );
        assert_eq!(seen.header("host"), Some("downloads.example"));

        let again = fetch(&parts, url, EndpointClass::Remote).unwrap_err();
        assert_eq!(again.kind.code(), "validation");
        assert_eq!(resolver.calls(), 2);
        assert_eq!(
            routes.attempts(),
            vec![public],
            "the rebound answer is never used"
        );
    }

    #[test]
    fn spike_plain_http_is_refused_for_anything_but_loopback() {
        let resolver = FakeResolver::new(vec![vec![PUBLIC]]);
        let routes = Arc::new(Routes::default());
        let parts = parts(test_roots(), resolver.clone(), routes.clone());
        for (url, class) in [
            ("http://downloads.example/", EndpointClass::Remote),
            ("http://downloads.example/", EndpointClass::Loopback),
            ("https://downloads.example/#part", EndpointClass::Remote),
        ] {
            let error = fetch(&parts, url, class).unwrap_err();
            assert_eq!(error.kind.code(), "validation", "{url} as {class:?}");
        }
        assert_eq!(resolver.calls(), 0);
        assert!(routes.attempts().is_empty());
    }

    #[test]
    fn spike_localhost_is_resolved_by_the_system_resolver_to_loopback_addresses() {
        let server = TestServer::canned(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nlocal".to_vec(),
        ])
        .unwrap();
        let url = format!("http://localhost:{}/", server.address().port());
        let answer = fetch(&loopback(), &url, EndpointClass::Loopback).unwrap();
        assert_eq!(answer.body, b"local");
        assert!(server.requests()[0].text().contains(&format!(
            "\r\nhost: localhost:{}\r\n",
            server.address().port()
        )));
    }

    #[test]
    fn spike_statuses_and_redirects_are_returned_as_data() {
        let server = TestServer::canned(vec![
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/next\r\nContent-Length: 0\r\n\r\n"
                .to_vec(),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\ngone".to_vec(),
        ])
        .unwrap();
        let redirect = fetch(&loopback(), &server.url("/"), EndpointClass::Loopback).unwrap();
        assert_eq!(redirect.status, 302, "the agent follows no redirect");
        let missing = fetch(&loopback(), &server.url("/"), EndpointClass::Loopback).unwrap();
        assert_eq!(
            (missing.status, missing.body.as_slice()),
            (404, &b"gone"[..])
        );
        assert_eq!(server.hits(), 2);
    }

    #[test]
    fn spike_loopback_https_works_with_the_test_roots_and_fails_with_platform_trust() {
        let server = TestServer::https(server_tls(), |_, out| {
            send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecure");
        })
        .unwrap();
        let url = server.url("/");
        let answer = fetch(&loopback(), &url, EndpointClass::Loopback).unwrap();
        assert_eq!(answer.body, b"secure");

        let platform = parts(
            TlsTrust::Platform,
            Arc::new(SystemResolver),
            Arc::new(SystemConnector),
        );
        let error = fetch(&platform, &url, EndpointClass::Loopback).unwrap_err();
        assert_eq!(error.kind.code(), "read-error");
        assert!(error.detail.contains("certificate"), "{error}");
    }

    #[test]
    fn spike_a_tls_body_that_ends_without_the_close_signal_is_refused() {
        let server = TestServer::start(
            Options {
                tls: Some(server_tls()),
                truncate_tls: true,
                ..Options::default()
            },
            |_, out| send(out, b"HTTP/1.1 200 OK\r\n\r\npartial"),
        )
        .unwrap();
        let error = fetch(&loopback(), &server.url("/"), EndpointClass::Loopback).unwrap_err();
        assert_eq!(error.kind.code(), "read-error");
        assert!(error.detail.contains("ended early"), "{error}");
    }

    #[test]
    fn spike_a_certificate_for_another_name_is_refused() {
        let server = TestServer::https(server_tls(), |_, out| {
            send(out, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        })
        .unwrap();
        let resolver = FakeResolver::new(vec![vec![PUBLIC]]);
        let routes = Routes::to(SocketAddr::new(PUBLIC, 443), server.address());
        let parts = parts(test_roots(), resolver, routes.clone());
        let error = fetch(&parts, "https://mismatch.example/", EndpointClass::Remote).unwrap_err();
        assert_eq!(error.kind.code(), "read-error");
        assert!(error.detail.contains("certificate"), "{error}");
        assert_eq!(
            routes.attempts().len(),
            1,
            "the connection was made and refused by TLS"
        );
    }

    /// Start `get` on a worker, wait until `stalled` says the server has gone quiet and the client
    /// is blocked reading, cancel from this thread and return how long the worker took to return
    /// after the cancel, with its result.
    fn cancel_while_stalled(
        parts: Parts,
        url: String,
        stalled: mpsc::Receiver<()>,
    ) -> (Result<Answer, Error>, Duration) {
        let cancel = Arc::new(SocketCancel::default());
        let worker = {
            let cancel = cancel.clone();
            thread::spawn(move || {
                let result = get(&parts, &url, EndpointClass::Loopback, &cancel, TOTAL);
                (result, Instant::now())
            })
        };
        stalled
            .recv_timeout(Duration::from_secs(10))
            .expect("the server stalled");
        // Long enough for the client to be parked in its read; far shorter than any timeout.
        thread::sleep(Duration::from_millis(200));
        let cancelled_at = Instant::now();
        cancel.cancel();
        let (result, returned_at) = worker.join().unwrap();
        (result, returned_at.saturating_duration_since(cancelled_at))
    }

    /// The bound a cancel must meet: well inside the 100 ms slice the transport polls on today,
    /// and nowhere near the 30 s the agent's own timeout would wait.
    const PROMPT: Duration = Duration::from_millis(50);

    #[test]
    fn spike_cancelling_a_stalled_tls_body_shuts_the_socket_down_promptly() {
        let (tx, stalled) = mpsc::sync_channel(1);
        let server = TestServer::https(server_tls(), move |_, out| {
            send(
                out,
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789",
            );
            let _ = tx.send(());
            thread::sleep(Duration::from_secs(5));
        })
        .unwrap();
        let (result, latency) = cancel_while_stalled(loopback(), server.url("/"), stalled);
        eprintln!("stalled TLS body: returned {latency:?} after the cancel");
        assert_eq!(result.unwrap_err().kind.code(), "cancelled");
        assert!(latency < PROMPT, "{latency:?}");
    }

    #[test]
    fn spike_cancelling_a_stalled_tls_head_shuts_the_socket_down_promptly() {
        let (tx, stalled) = mpsc::sync_channel(1);
        let server = TestServer::https(server_tls(), move |_, out| {
            send(out, b"HTTP/1.1 200 OK\r\n");
            let _ = tx.send(());
            thread::sleep(Duration::from_secs(5));
        })
        .unwrap();
        let (result, latency) = cancel_while_stalled(loopback(), server.url("/"), stalled);
        eprintln!("stalled TLS head: returned {latency:?} after the cancel");
        assert_eq!(result.unwrap_err().kind.code(), "cancelled");
        assert!(latency < PROMPT, "{latency:?}");
    }

    #[test]
    fn spike_cancelling_a_stalled_tls_handshake_shuts_the_socket_down_promptly() {
        // Accepts the connection and never answers the client's hello.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, stalled) = mpsc::sync_channel(1);
        let holder = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let _ = tx.send(());
            thread::sleep(Duration::from_secs(5));
            drop(socket);
        });
        let (result, latency) =
            cancel_while_stalled(loopback(), format!("https://{address}/"), stalled);
        eprintln!("stalled TLS handshake: returned {latency:?} after the cancel");
        assert_eq!(result.unwrap_err().kind.code(), "cancelled");
        assert!(latency < PROMPT, "{latency:?}");
        drop(holder);
    }

    #[test]
    fn spike_cancelling_a_stalled_plain_body_shuts_the_socket_down_promptly() {
        let (tx, stalled) = mpsc::sync_channel(1);
        let server = TestServer::http(move |_, out| {
            send(
                out,
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789",
            );
            let _ = tx.send(());
            thread::sleep(Duration::from_secs(5));
        })
        .unwrap();
        let (result, latency) = cancel_while_stalled(loopback(), server.url("/"), stalled);
        eprintln!("stalled plain body: returned {latency:?} after the cancel");
        assert_eq!(result.unwrap_err().kind.code(), "cancelled");
        assert!(latency < PROMPT, "{latency:?}");
    }

    #[test]
    fn spike_cancelling_a_stalled_close_delimited_body_is_not_taken_for_its_end() {
        // A shut-down plain socket reads as end of stream, which ends a close-delimited body.
        let (tx, stalled) = mpsc::sync_channel(1);
        let server = TestServer::http(move |_, out| {
            send(out, b"HTTP/1.1 200 OK\r\n\r\n0123456789");
            let _ = tx.send(());
            thread::sleep(Duration::from_secs(5));
        })
        .unwrap();
        let (result, latency) = cancel_while_stalled(loopback(), server.url("/"), stalled);
        assert_eq!(result.unwrap_err().kind.code(), "cancelled");
        assert!(latency < PROMPT, "{latency:?}");
    }

    #[test]
    fn spike_a_request_cancelled_before_it_starts_never_resolves_or_connects() {
        let resolver = FakeResolver::new(vec![vec![PUBLIC]]);
        let routes = Arc::new(Routes::default());
        let parts = parts(test_roots(), resolver.clone(), routes.clone());
        let cancel = Arc::new(SocketCancel::default());
        cancel.cancel();
        let error = get(
            &parts,
            "https://downloads.example/",
            EndpointClass::Remote,
            &cancel,
            TOTAL,
        )
        .unwrap_err();
        assert_eq!(error.kind.code(), "cancelled");
        assert_eq!(resolver.calls(), 0);
        assert!(routes.attempts().is_empty());
    }
}
