//! The host's only network path: endpoint classification, resolution checks, redirects, limits and
//! TLS. See `docs/design/module-capabilities.md#transport`.
//!
//! A request runs on its own `ureq` agent ([`agent`]) that connects directly to an address it
//! checked, over this module's own socket and TLS session; `ureq` writes the request and frames the
//! response on that connection, and this module keeps the redirects, bounds and the framing it
//! refuses. Proxy settings, including the `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` environment
//! variables, are deliberately ignored: a proxy would choose the address after the check and could
//! read or rewrite the request.
mod agent;
mod net;
pub mod policy;
#[cfg(test)]
mod tests;
mod tls;

pub use net::{Connect, Resolve, SystemConnector, SystemResolver};
pub use policy::{Endpoint, EndpointClass, address_allowed, parse_endpoint};
pub use rustls::pki_types::CertificateDer;
pub use tls::TlsTrust;

use crate::{Error, jobs::JobControl};
use agent::Pace;
use rustls::ClientConfig;
use std::{
    borrow::Cow,
    fmt,
    io::{self, Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};
use ureq::http::{self, Response, Version};
use url::{Position, Url};

/// A request follows at most this many redirects, whatever its policy asks for.
pub const MAX_REDIRECTS: u8 = 3;
/// Statuses that redirect a request. Other `3xx` statuses are returned as data.
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];
/// One header block holds at most this many fields.
const MAX_FIELDS: usize = 100;
/// The request head, including the caller's headers, is at most this many bytes.
const MAX_REQUEST_HEAD_BYTES: usize = 16 * 1024;
/// The caller supplies at most this many headers.
const MAX_CALLER_HEADERS: usize = 32;
/// Body bytes read and handed to the sink at a time.
const PIECE_BYTES: usize = 16 * 1024;
/// Headers only the host writes, or that would enable something the transport does not support.
const HOST_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "expect",
    "accept-encoding",
    "user-agent",
    "cookie",
    "proxy-authorization",
    "proxy-connection",
];

/// The methods the transport sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
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
    /// The job the request runs for. Its cancel stops the request with its `cancelled` error: the
    /// connection holds the job's cancel, which shuts the socket down, so a blocked read or write
    /// returns at once; a request whose job is already cancelled never resolves or connects.
    pub control: &'a Arc<JobControl>,
    /// Called after each piece of the final response body reaches the sink, with the bytes
    /// written so far and the declared length, if there is one.
    pub progress: &'a mut dyn FnMut(u64, Option<u64>),
}

/// The final response's head. Any status other than a redirect is returned as data, for the caller
/// to judge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    /// Header fields with lowercased names, at most 100 of them within 64 KiB, in the order
    /// received with a repeated name's values together.
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

/// Drops the job's clone of a connection's socket once its request is over, however it ends.
struct Held<'a>(&'a JobControl);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.release_connection();
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
            control,
            progress,
        } = options;
        let mut endpoint =
            parse_endpoint(request.endpoint.url.as_str(), &[request.endpoint.class])?;
        if [connect_timeout, read_timeout, total_timeout]
            .iter()
            .any(Duration::is_zero)
        {
            return Err(Error::validation("request timeouts must be positive"));
        }
        if redirects.max > MAX_REDIRECTS {
            return Err(Error::validation(format!(
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
                    .ok_or_else(|| Error::validation("a redirect origin is not a valid origin"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        check_headers(&request.headers)?;
        if request.method == Method::Get && !request.body.is_empty() {
            return Err(Error::validation("a GET request has no body"));
        }
        if request.body.len() as u64 > max_request_bytes {
            return Err(Error::resource_limit(format!(
                "the request body is larger than {max_request_bytes} bytes"
            )));
        }
        let deadline = Instant::now()
            .checked_add(total_timeout)
            .ok_or_else(|| Error::validation("the request timeout is too long"))?;

        let mut method = request.method;
        let mut body = request.body.as_slice();
        let mut headers = Cow::Borrowed(request.headers.as_slice());
        let mut followed = 0;
        loop {
            control.checkpoint()?;
            let prepared = prepare(method, &endpoint.url, &headers)?;
            let pace = Arc::new(Pace {
                name: endpoint.url.host_str().unwrap_or_default().to_owned(),
                control: control.clone(),
                idle: read_timeout,
                deadline,
            });
            let agent = agent::agent(
                &endpoint,
                &self.tls,
                &self.resolver,
                &self.connector,
                connect_timeout,
                &pace,
            );
            let _held = Held(control);
            let sent = match method {
                Method::Get => agent.run(prepared),
                Method::Post => agent.run(prepared.map(|()| body)),
            };
            let mut response = sent.map_err(|error| pace.error(error))?;
            let fields = check_head(&response, &pace.name)?;
            let status = response.status().as_u16();
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
            let received = read_body(&mut response, max_response_bytes, sink, progress, &pace)?;
            return Ok(TransportResponse {
                status,
                headers: fields,
                final_url: endpoint.url,
                received,
            });
        }
    }
}

fn is_token(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(byte))
}

/// Check the caller's headers. No refusal repeats a header's value.
fn check_headers(headers: &[(String, String)]) -> Result<(), Error> {
    if headers.len() > MAX_CALLER_HEADERS {
        return Err(Error::validation(format!(
            "a request has at most {MAX_CALLER_HEADERS} headers"
        )));
    }
    for (name, value) in headers {
        if !is_token(name.as_bytes()) {
            return Err(Error::validation(
                "a request header name is not a valid token",
            ));
        }
        if HOST_HEADERS
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            return Err(Error::validation(format!(
                "a request may not set the {name} header"
            )));
        }
        if value
            .bytes()
            .any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f)
        {
            return Err(Error::validation(format!(
                "the value of request header {name} contains a control character"
            )));
        }
    }
    Ok(())
}

/// The head of a request for `url` with the caller's checked `headers`, before anything connects.
/// Only the host writes `Host`, `User-Agent`, `Accept-Encoding: identity` and `Connection: close`;
/// the agent adds a `POST`'s `Content-Length`.
fn prepare(
    method: Method,
    url: &Url,
    headers: &[(String, String)],
) -> Result<http::Request<()>, Error> {
    let target = &url[Position::BeforePath..Position::AfterQuery];
    let mut host = url.host_str().unwrap_or_default().to_owned();
    if let Some(port) = url.port() {
        host = format!("{host}:{port}");
    }
    let fixed = [
        ("host", host.as_str()),
        (
            "user-agent",
            concat!("Luxforge/", env!("CARGO_PKG_VERSION")),
        ),
        ("accept-encoding", "identity"),
        ("connection", "close"),
    ];
    let fields = fixed
        .iter()
        .map(|(name, value)| name.len() + value.len())
        .chain(headers.iter().map(|(name, value)| name.len() + value.len()));
    if target.len() + fields.map(|bytes| bytes + 4).sum::<usize>() > MAX_REQUEST_HEAD_BYTES {
        return Err(Error::resource_limit(format!(
            "the request head is larger than {MAX_REQUEST_HEAD_BYTES} bytes"
        )));
    }
    let mut request = http::Request::builder()
        .method(match method {
            Method::Get => http::Method::GET,
            Method::Post => http::Method::POST,
        })
        .uri(url.as_str())
        .version(Version::HTTP_11);
    for (name, value) in fixed {
        request = request.header(name, value);
    }
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_bytes());
    }
    request
        .body(())
        .map_err(|_| Error::validation("the request cannot be sent: a header is not valid"))
}

fn too_many_fields(name: &str) -> Error {
    Error::resource_limit(format!(
        "the response from {name} has more than {MAX_FIELDS} header fields"
    ))
}

/// The final response's header fields, once its status, field count and framing pass. The framing
/// `ureq` would accept but the transport refuses rather than guess: compression, any transfer
/// coding but `chunked` on HTTP/1.1, both a transfer coding and a length, and lengths that are not
/// one plain number.
fn check_head<B>(response: &Response<B>, name: &str) -> Result<Vec<(String, String)>, Error> {
    let malformed = |what: &str| Error::file_access(format!("the response from {name} {what}"));
    let status = response.status().as_u16();
    if status == 101 {
        return Err(malformed("switched protocols"));
    }
    if status >= 600 {
        return Err(malformed("has an invalid status line"));
    }
    if response.headers().len() > MAX_FIELDS {
        return Err(too_many_fields(name));
    }
    let fields: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let values = |field: &'static str| {
        fields
            .iter()
            .filter(move |(name, _)| name == field)
            .map(|(_, value)| value.as_str())
    };
    if values("content-encoding").any(|coding| !coding.eq_ignore_ascii_case("identity")) {
        return Err(malformed("is compressed"));
    }
    if status == 204 || status == 304 {
        return Ok(fields);
    }
    let http10 = response.version() == Version::HTTP_10;
    let encodings: Vec<_> = values("transfer-encoding").collect();
    let lengths: Vec<_> = values("content-length").collect();
    match (encodings.as_slice(), lengths.as_slice()) {
        ([], []) => {}
        ([encoding], []) if encoding.eq_ignore_ascii_case("chunked") && !http10 => {}
        (_, []) => return Err(malformed("uses an unsupported transfer coding")),
        ([], [first, rest @ ..])
            if !first.is_empty()
                && first.bytes().all(|byte| byte.is_ascii_digit())
                && rest.iter().all(|other| other == first) => {}
        ([], _) => return Err(malformed("has an invalid Content-Length")),
        _ => return Err(malformed("has both Transfer-Encoding and Content-Length")),
    }
    Ok(fields)
}

/// Stream the final response's body into `sink`, refusing it once it would pass `limit` bytes:
/// before reading when its length is declared, and before writing the piece that crosses it
/// otherwise. `ureq` reads at most one byte past the limit. A cancel overrides the body's end,
/// because a shut-down plain socket reads as the end of a close-delimited body. Returns the number
/// of body bytes written.
fn read_body(
    response: &mut Response<ureq::Body>,
    limit: u64,
    sink: &mut dyn Write,
    progress: &mut dyn FnMut(u64, Option<u64>),
    pace: &Pace,
) -> Result<u64, Error> {
    let name = &pace.name;
    let too_large = || {
        Error::resource_limit(format!(
            "the response from {name} is larger than {limit} bytes"
        ))
    };
    let total = response.body().content_length();
    if total.is_some_and(|length| length > limit) {
        return Err(too_large());
    }
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(limit.saturating_add(1))
        .reader();
    let mut piece = vec![0; PIECE_BYTES];
    let mut received = 0;
    loop {
        let read = match reader.read(&mut piece) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(pace.error(ureq::Error::from(error))),
        };
        if read as u64 > limit - received {
            return Err(too_large());
        }
        sink.write_all(&piece[..read]).map_err(|error| {
            Error::file_access(format!(
                "cannot store the response from {name}: {}",
                error.kind()
            ))
        })?;
        received += read as u64;
        progress(received, total);
    }
    pace.control.checkpoint()?;
    Ok(received)
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
        return Err(Error::validation("redirect refused"));
    }
    if followed >= max {
        return Err(Error::validation(format!(
            "redirect refused: more than {max} redirects"
        )));
    }
    let location = fields
        .iter()
        .find(|(name, _)| name == "location")
        .ok_or_else(|| Error::validation("redirect refused: it has no Location"))?;
    let mut url = from
        .url
        .join(&location.1)
        .map_err(|_| Error::validation("redirect refused: its Location is not a valid URL"))?;
    url.set_fragment(None);
    let target = parse_endpoint(url.as_str(), &[from.class])
        .map_err(|error| Error::validation(format!("redirect refused: {}", error.detail)))?;
    if from.url.scheme() == "https" && target.url.scheme() != "https" {
        return Err(Error::validation("redirect refused: it leaves https"));
    }
    let origin = target.origin();
    if !origins.contains(&origin) {
        return Err(Error::validation(format!(
            "redirect refused: {origin} is not an allowed origin"
        )));
    }
    Ok(target)
}
