//! The host's only network path: endpoint classification, resolution checks, redirects, limits and
//! TLS. See `docs/design/module-capabilities.md#transport`.
//!
//! A request connects directly to an address it checked. Proxy settings, including the
//! `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` environment variables, are deliberately ignored: a
//! proxy would choose the address after the check and could read or rewrite the request.
mod http;
mod net;
pub mod policy;
#[cfg(test)]
mod tests;
mod tls;

pub use net::{Connect, Resolve, SystemConnector, SystemResolver};
pub use policy::{Endpoint, EndpointClass, address_allowed, parse_endpoint};
pub use rustls::pki_types::CertificateDer;
pub use tls::TlsTrust;

use crate::{Error, ErrorKind};
use rustls::ClientConfig;
use std::{
    borrow::Cow,
    fmt,
    io::Write,
    net::TcpStream,
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;

/// A request follows at most this many redirects, whatever its policy asks for.
pub const MAX_REDIRECTS: u8 = 3;
/// Statuses that redirect a request. Other `3xx` statuses are returned as data.
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

fn refused(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// The methods the transport sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One request. Its `Debug` shows the origin, header names and body length, never a header value
/// or the body.
#[derive(Clone)]
pub struct TransportRequest {
    pub method: Method,
    pub endpoint: Endpoint,
    /// Extra headers, such as `Authorization` or `Content-Type`. The host writes `Host`,
    /// `User-Agent`, `Accept-Encoding: identity`, `Connection: close` and `Content-Length` itself
    /// and refuses them here, together with cookies and the other connection-level headers.
    pub headers: Vec<(String, String)>,
    /// The body of a `POST`; a `GET` has none.
    pub body: Vec<u8>,
}

impl fmt::Debug for TransportRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportRequest")
            .field("method", &self.method)
            .field("origin", &self.endpoint.origin())
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Which redirects a request may follow. The default follows none.
#[derive(Clone, Copy, Debug, Default)]
pub struct RedirectPolicy<'a> {
    /// How many redirects may be followed, at most `MAX_REDIRECTS`.
    pub max: u8,
    /// The origins (`scheme://host[:port]`) a redirect may lead to. A redirect must also keep the
    /// endpoint's class and may not leave `https`.
    pub origins: &'a [String],
}

/// The bounds and hooks of one `send`, borrowed from the caller for its duration.
pub struct SendOptions<'a> {
    /// The body is refused before connecting if it is larger.
    pub max_request_bytes: u64,
    /// The response body is refused from its declared length, or cut off where it crosses this.
    pub max_response_bytes: u64,
    /// The longest wait for one connection attempt.
    pub connect_timeout: Duration,
    /// The longest wait for any progress on a connection.
    pub read_timeout: Duration,
    /// The whole request, resolution and redirects included.
    pub total_timeout: Duration,
    pub redirects: RedirectPolicy<'a>,
    /// Checked before each connection and every read and write; `true` stops the request with
    /// `cancelled`.
    pub cancel: &'a dyn Fn() -> bool,
    /// Called after each piece of the final response body reaches the sink, with the bytes
    /// written so far and the declared length, if there is one.
    pub progress: &'a mut dyn FnMut(u64, Option<u64>),
}

/// The final response's head. Any status other than a redirect is returned as data, for the caller
/// to judge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    /// Header fields with lowercased names, at most 100 of them within 64 KiB.
    pub headers: Vec<(String, String)>,
    /// The URL that answered, after any redirects.
    pub final_url: Url,
    /// Body bytes written to the sink.
    pub received: u64,
}

impl TransportResponse {
    /// The first value of the header `name`, compared without regard to case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What a transport trusts and how it reaches the network. Tests replace each part.
#[derive(Clone)]
pub struct TransportConfig {
    pub trust: TlsTrust,
    pub resolver: Arc<dyn Resolve>,
    pub connector: Arc<dyn Connect>,
}

impl Default for TransportConfig {
    /// The platform's trust store, the system resolver and direct connections.
    fn default() -> Self {
        Self {
            trust: TlsTrust::Platform,
            resolver: Arc::new(SystemResolver),
            connector: Arc::new(SystemConnector),
        }
    }
}

/// The host's HTTP client for module capabilities. It is shareable across worker threads and
/// holds no connection between requests.
pub struct Transport {
    tls: Arc<ClientConfig>,
    resolver: Arc<dyn Resolve>,
    connector: Arc<dyn Connect>,
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transport").finish_non_exhaustive()
    }
}

impl Transport {
    pub fn new(config: TransportConfig) -> Result<Self, Error> {
        Ok(Self {
            tls: tls::client_config(&config.trust)?,
            resolver: config.resolver,
            connector: config.connector,
        })
    }

    /// A transport with the platform's trust store, the system resolver and direct connections.
    pub fn system() -> Result<Self, Error> {
        Self::new(TransportConfig::default())
    }

    /// Send `request` and stream the final response body into `sink`. Everything the request itself
    /// can be refused for is checked before connecting. Each connection resolves its host once and
    /// connects only to addresses of the endpoint's class. A redirect is followed only as
    /// `options.redirects` allows, with `GET`, no body, and no `Authorization` header once the
    /// origin changes. Errors are `validation` for a refused request, address or redirect,
    /// `resource-limit` for sizes, `read-error` for network, TLS and protocol failures and
    /// timeouts, and `cancelled`; no message contains a header value or a body.
    pub fn send(
        &self,
        request: &TransportRequest,
        options: SendOptions<'_>,
        sink: &mut dyn Write,
    ) -> Result<TransportResponse, Error> {
        let SendOptions {
            max_request_bytes,
            max_response_bytes,
            connect_timeout,
            read_timeout,
            total_timeout,
            redirects,
            cancel,
            progress,
        } = options;
        let mut endpoint =
            parse_endpoint(request.endpoint.url.as_str(), &[request.endpoint.class])?;
        if [connect_timeout, read_timeout, total_timeout]
            .iter()
            .any(Duration::is_zero)
        {
            return Err(refused("request timeouts must be positive"));
        }
        if redirects.max > MAX_REDIRECTS {
            return Err(refused(format!(
                "a request follows at most {MAX_REDIRECTS} redirects"
            )));
        }
        let origins = redirects
            .origins
            .iter()
            .map(|origin| {
                Url::parse(origin)
                    .ok()
                    .map(|url| url.origin())
                    .filter(url::Origin::is_tuple)
                    .map(|origin| origin.ascii_serialization())
                    .ok_or_else(|| refused("a redirect origin is not a valid origin"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        http::check_headers(&request.headers)?;
        if request.method == Method::Get && !request.body.is_empty() {
            return Err(refused("a GET request has no body"));
        }
        if request.body.len() as u64 > max_request_bytes {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!("the request body is larger than {max_request_bytes} bytes"),
            ));
        }
        let deadline = Instant::now()
            .checked_add(total_timeout)
            .ok_or_else(|| refused("the request timeout is too long"))?;

        let mut method = request.method;
        let mut body = request.body.as_slice();
        let mut headers = Cow::Borrowed(request.headers.as_slice());
        let mut followed = 0;
        loop {
            if cancel() {
                return Err(http::cancelled());
            }
            let head = http::request_head(method, &endpoint.url, &headers, body.len())?;
            let server_name = match endpoint.url.scheme() {
                "https" => Some(tls::server_name(&endpoint)?),
                _ => None,
            };
            let name = endpoint.url.host_str().unwrap_or_default().to_owned();
            let socket = net::connect(
                &endpoint,
                &*self.resolver,
                &*self.connector,
                connect_timeout,
                deadline,
            )?;
            slice(&socket, &name)?;
            let stream: Box<dyn http::Stream> = match server_name {
                Some(server_name) => Box::new(tls::wrap(&self.tls, server_name, socket)?),
                None => Box::new(socket),
            };
            let pace = http::Pace {
                name: &name,
                idle: read_timeout,
                deadline,
                cancel,
            };
            let mut exchange = http::Exchange::new(stream, pace);
            exchange.send(&head, body)?;
            let (status, fields) = exchange.read_head()?;
            if REDIRECT_STATUSES.contains(&status) {
                let target = redirect(&endpoint, &fields, redirects.max, followed, &origins)?;
                if target.origin() != endpoint.origin() {
                    headers
                        .to_mut()
                        .retain(|(name, _)| !name.eq_ignore_ascii_case("authorization"));
                }
                method = Method::Get;
                body = &[];
                endpoint = target;
                followed += 1;
                continue;
            }
            let framing = exchange.framing(status, &fields)?;
            let received = exchange.read_body(framing, max_response_bytes, sink, progress)?;
            return Ok(TransportResponse {
                status,
                headers: fields,
                final_url: endpoint.url,
                received,
            });
        }
    }
}

/// Make every blocking read and write on `socket` return after `http::SLICE`, so the exchange can
/// check cancellation and its deadlines while a server is silent.
fn slice(socket: &TcpStream, name: &str) -> Result<(), Error> {
    socket
        .set_nodelay(true)
        .and_then(|()| socket.set_read_timeout(Some(http::SLICE)))
        .and_then(|()| socket.set_write_timeout(Some(http::SLICE)))
        .map_err(|error| {
            Error::new(
                ErrorKind::FileAccess,
                format!(
                    "cannot configure the connection to {name}: {}",
                    error.kind()
                ),
            )
        })
}

/// The endpoint a redirect leads to, if the policy allows following it: within the count, with a
/// valid `Location` of the same class, not leaving `https`, and to an allowed origin. The address
/// checks happen again when the redirect connects.
fn redirect(
    from: &Endpoint,
    fields: &[(String, String)],
    max: u8,
    followed: u8,
    origins: &[String],
) -> Result<Endpoint, Error> {
    if max == 0 {
        return Err(refused("redirect refused"));
    }
    if followed >= max {
        return Err(refused(format!(
            "redirect refused: more than {max} redirects"
        )));
    }
    let location = fields
        .iter()
        .find(|(name, _)| name == "location")
        .ok_or_else(|| refused("redirect refused: it has no Location"))?;
    let mut url = from
        .url
        .join(&location.1)
        .map_err(|_| refused("redirect refused: its Location is not a valid URL"))?;
    url.set_fragment(None);
    let target = parse_endpoint(url.as_str(), &[from.class])
        .map_err(|error| refused(format!("redirect refused: {}", error.detail)))?;
    if from.url.scheme() == "https" && target.url.scheme() != "https" {
        return Err(refused("redirect refused: it leaves https"));
    }
    let origin = target.origin();
    if !origins.contains(&origin) {
        return Err(refused(format!(
            "redirect refused: {origin} is not an allowed origin"
        )));
    }
    Ok(target)
}
