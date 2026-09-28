//! [`HttpTransport`], the host's only network path: resolution checks, redirects, limits and TLS
//! behind the core's [`Transport`] trait. See `docs/design/module-capabilities.md#transport`.
//!
//! A request runs on its own `ureq` agent ([`agent`]) that connects directly to an address it
//! checked, over this module's own socket and TLS session; `ureq` writes the request and frames the
//! response on that connection, and this module keeps the redirects, bounds and the framing it
//! refuses. Proxy settings, including the `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` environment
//! variables, are deliberately ignored: a proxy would choose the address after the check and could
//! read or rewrite the request.
mod address;
mod agent;
mod connect;
#[cfg(test)]
mod tests;
mod tls;

pub use connect::{Connect, Resolve, SystemConnector, SystemResolver};
pub use tls::TlsTrust;

use agent::Pace;
use luxforge_core::{
    Error,
    capabilities::{
        endpoint::{Endpoint, parse_endpoint},
        transport::{
            MAX_REDIRECTS, Method, SendOptions, Transport, TransportRequest, TransportResponse,
        },
    },
    jobs::JobControl,
};
use rustls::ClientConfig;
use std::{
    borrow::Cow,
    fmt,
    io::{self, Read, Write},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};
use ureq::http::{self, Response, Version};
use url::{Position, Url};

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

/// The host's HTTP client for module capabilities, which the desktop and `luxforge-json` give the
/// catalog owner. It is shareable across worker threads, holds no connection between requests and
/// builds nothing until its first request: its TLS configuration, the platform verifier's
/// included, is built then, on the worker that sends it, never when the host starts.
pub struct HttpTransport {
    trust: TlsTrust,
    tls: Mutex<Option<Arc<ClientConfig>>>,
    resolver: Arc<dyn Resolve>,
    connector: Arc<dyn Connect>,
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport")
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

/// Drops the job's clone of a connection's socket once its request is over, however it ends.
struct Held<'a>(&'a JobControl);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.release_connection();
    }
}

impl HttpTransport {
    pub fn new(config: TransportConfig) -> Self {
        Self {
            trust: config.trust,
            tls: Mutex::new(None),
            resolver: config.resolver,
            connector: config.connector,
        }
    }

    /// A transport with the platform's trust store, the system resolver and direct connections.
    pub fn system() -> Self {
        Self::new(TransportConfig::default())
    }

    /// The TLS configuration every request shares, built by the first request that needs it. A
    /// failure to build it is that request's error, and the next request tries again.
    fn tls(&self) -> Result<Arc<ClientConfig>, Error> {
        let mut built = self.tls.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(config) = built.as_ref() {
            return Ok(config.clone());
        }
        let config = tls::client_config(&self.trust)?;
        *built = Some(config.clone());
        Ok(config)
    }
}

impl Transport for HttpTransport {
    /// Each connection resolves its host once and connects only to addresses of the endpoint's
    /// class; see [`Transport::send`] for the whole contract.
    fn send(
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
        let tls = self.tls()?;

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
                &tls,
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
