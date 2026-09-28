//! One request's `ureq` agent, built from the transport's own parts: a resolver that resolves the
//! endpoint's host once and answers only addresses of its class, a connector that connects only to
//! those addresses and hands the job a clone of the socket so its cancel shuts the socket down, and
//! TLS on the transport's own rustls configuration. `ureq` writes the request and frames the
//! response; it has no proxy, no pool and no redirect following, and returns every status as data.
//!
//! Two parts of `ureq` 3.4.2 are not used. Its `TcpTransport` is not exported, so the connector
//! supplies [`SocketTransport`]. Its `RustlsConnector` ends a close-delimited body at an
//! `UnexpectedEof`, which is how rustls reports a connection closed without the server's close
//! signal, so it would take a truncated body for a whole one; [`TlsTransport`] reports that as an
//! error instead. `ureq`'s timeouts are per phase, and a zero remaining timeout reads as one
//! second, so the socket applies the transport's own deadlines: the idle wait for any progress and
//! the whole request's deadline, both checked with the job's cancel before every read and write.
use super::{
    connect::{self, Connect, Resolve},
    tls,
};
use luxforge_core::{Error, capabilities::endpoint::Endpoint, jobs::JobControl};
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use std::{
    fmt,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::{Duration, Instant},
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

/// The response head, interim responses each counted on their own, is at most this many bytes.
pub(super) const MAX_HEAD_BYTES: usize = 64 * 1024;

/// What bounds one connection besides sizes: the job whose cancel stops it and its deadlines.
pub(super) struct Pace {
    /// The host, for messages.
    pub name: String,
    pub control: Arc<JobControl>,
    /// The longest wait for any progress.
    pub idle: Duration,
    /// When the whole request, redirects included, must be complete.
    pub deadline: Instant,
}

impl Pace {
    fn timed_out(&self) -> Error {
        Error::file_access(format!("the request to {} timed out", self.name))
    }

    /// Fail if the job was cancelled or the deadline passed, or else the longest one read or write
    /// may wait.
    fn wait(&self) -> Result<Duration, ureq::Error> {
        self.control.checkpoint().map_err(refused)?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(refused(self.timed_out()));
        }
        Ok(self.idle.min(remaining))
    }

    /// A socket error: a timeout is the transport's own, and a reset is a failure, never the end
    /// of a close-delimited body, which is how `ureq` would take it.
    fn failed(&self, error: io::Error) -> ureq::Error {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => refused(self.timed_out()),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => refused(
                Error::file_access(format!("the connection to {} was reset", self.name)),
            ),
            _ => ureq::Error::Io(error),
        }
    }

    /// Map an agent failure back onto the transport's error kinds: a cancel wins over whatever the
    /// shut-down socket reported, and a refusal of the transport's own comes back as itself.
    /// Messages name the fault, never a header value or the body.
    pub(super) fn error(&self, error: ureq::Error) -> Error {
        if self.control.is_cancelled() {
            return self.control.cancelled_error();
        }
        let name = &self.name;
        match error {
            ureq::Error::Other(inner) => match inner.downcast::<Error>() {
                Ok(error) => *error,
                Err(other) => Error::file_access(format!("the request to {name} failed: {other}")),
            },
            ureq::Error::Timeout(_) => self.timed_out(),
            ureq::Error::LargeResponseHeader(..) => Error::resource_limit(format!(
                "the response head from {name} is larger than {MAX_HEAD_BYTES} bytes"
            )),
            ureq::Error::Protocol(ureq_proto::Error::HttpParseTooManyHeaders) => {
                super::too_many_fields(name)
            }
            ureq::Error::Protocol(error) => {
                Error::file_access(format!("the response from {name} is malformed: {error}"))
            }
            ureq::Error::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                Error::file_access(format!("the response from {name} ended early"))
            }
            ureq::Error::Io(error) => {
                Error::file_access(format!("the request to {name} failed: {error}"))
            }
            other => Error::file_access(format!("the request to {name} failed: {other}")),
        }
    }
}

fn refused(error: Error) -> ureq::Error {
    ureq::Error::Other(Box::new(error))
}

/// Resolves the endpoint's host exactly once through the transport's `Resolve` and answers only
/// if every address belongs to the endpoint's class. An IP literal is checked without a lookup.
struct PolicyResolver {
    endpoint: Endpoint,
    resolve: Arc<dyn Resolve>,
    pace: Arc<Pace>,
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
        self.pace.control.checkpoint().map_err(refused)?;
        // The agent follows no redirects, so it only ever asks for the endpoint's own host.
        if uri.host() != self.endpoint.url.host_str() {
            return Err(refused(Error::validation("the request left its endpoint")));
        }
        let addresses = connect::resolve(&self.endpoint, &*self.resolve).map_err(refused)?;
        let mut answer = self.empty();
        for address in addresses {
            if answer.try_push(address).is_err() {
                break;
            }
        }
        Ok(answer)
    }
}

/// Connects through the transport's `Connect` to the addresses the resolver answered, checking
/// each again against the class, and hands the job a clone of the socket for its cancel.
struct PolicyConnector {
    endpoint: Endpoint,
    connect: Arc<dyn Connect>,
    timeout: Duration,
    pace: Arc<Pace>,
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
        let pace = &self.pace;
        pace.control.checkpoint().map_err(refused)?;
        let addresses: Vec<SocketAddr> = details.addrs.iter().copied().collect();
        let stream = connect::connect(
            &addresses,
            self.endpoint.class,
            &pace.name,
            &*self.connect,
            self.timeout,
            pace.deadline,
        )
        .map_err(refused)?;
        let held = stream
            .set_nodelay(true)
            .and_then(|()| pace.control.hold_connection(&stream))
            .map_err(|error| {
                refused(Error::file_access(format!(
                    "cannot configure the connection to {}: {}",
                    pace.name,
                    error.kind()
                )))
            })?;
        if !held {
            return Err(refused(pace.control.cancelled_error()));
        }
        Ok(Some(SocketTransport {
            stream,
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
            pace: pace.clone(),
        }))
    }
}

/// A plain socket as `ureq` transport, under the transport's own deadlines and cancel.
struct SocketTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
    pace: Arc<Pace>,
}

impl fmt::Debug for SocketTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketTransport").finish_non_exhaustive()
    }
}

impl Transport for SocketTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, _: NextTimeout) -> Result<(), ureq::Error> {
        let mut output = &self.buffers.output()[..amount];
        while !output.is_empty() {
            let wait = self.pace.wait()?;
            self.stream.set_write_timeout(Some(wait))?;
            match self.stream.write(output) {
                Ok(0) => return Err(ureq::Error::Io(io::ErrorKind::WriteZero.into())),
                Ok(written) => output = &output[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(self.pace.failed(error)),
            }
        }
        Ok(())
    }

    fn await_input(&mut self, _: NextTimeout) -> Result<bool, ureq::Error> {
        let wait = self.pace.wait()?;
        self.stream.set_read_timeout(Some(wait))?;
        let input = self.buffers.input_append_buf();
        let amount = loop {
            match self.stream.read(input) {
                Ok(amount) => break amount,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(self.pace.failed(error)),
            }
        };
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    /// Never reused: the agent keeps no idle connection.
    fn is_open(&mut self) -> bool {
        false
    }
}

/// TLS over the policy connector's socket with the transport's own rustls configuration, shared by
/// every request's agent.
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
        let pace = socket.pace.clone();
        let name = tls::server_name(&self.endpoint).map_err(refused)?;
        let mut session = ClientConnection::new(self.config.clone(), name)
            .map_err(|error| refused(Error::file_access(format!("cannot start TLS: {error}"))))?;
        let mut adapter = TransportAdapter::new(socket);
        session.complete_io(&mut adapter)?;
        Ok(Some(Either::B(TlsTransport {
            stream: StreamOwned::new(session, adapter),
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
            pace,
        })))
    }
}

struct TlsTransport {
    stream: StreamOwned<ClientConnection, TransportAdapter<SocketTransport>>,
    buffers: LazyBuffers,
    pace: Arc<Pace>,
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

    fn transmit_output(&mut self, amount: usize, _: NextTimeout) -> Result<(), ureq::Error> {
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output)?;
        self.stream.flush()?;
        Ok(())
    }

    fn await_input(&mut self, _: NextTimeout) -> Result<bool, ureq::Error> {
        let input = self.buffers.input_append_buf();
        let amount = match self.stream.read(input) {
            Ok(amount) => amount,
            // rustls's word for a connection closed without `close_notify`, which `ureq` would
            // take for the end of a close-delimited body.
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(refused(Error::file_access(format!(
                    "the response from {} ended early, without the server's TLS close signal",
                    self.pace.name
                ))));
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

/// One request's agent for `endpoint`: the policy resolver, connector and TLS, and every agent
/// behaviour the transport does not want switched off. The request supplies `Host`, `User-Agent`,
/// `Accept-Encoding` and `Connection` itself, so the agent adds only a `POST`'s `Content-Length`.
pub(super) fn agent(
    endpoint: &Endpoint,
    tls: &Arc<ClientConfig>,
    resolve: &Arc<dyn Resolve>,
    connect: &Arc<dyn Connect>,
    connect_timeout: Duration,
    pace: &Arc<Pace>,
) -> Agent {
    let config = Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .max_response_header_size(MAX_HEAD_BYTES)
        .accept("")
        .build();
    let connector = PolicyConnector {
        endpoint: endpoint.clone(),
        connect: connect.clone(),
        timeout: connect_timeout,
        pace: pace.clone(),
    }
    .chain(PolicyTls {
        config: tls.clone(),
        endpoint: endpoint.clone(),
    });
    let resolver = PolicyResolver {
        endpoint: endpoint.clone(),
        resolve: resolve.clone(),
        pace: pace.clone(),
    };
    Agent::with_parts(config, connector, resolver)
}
