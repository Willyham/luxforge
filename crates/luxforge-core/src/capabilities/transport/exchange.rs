//! One HTTP/1.1 exchange over one connection, framed by `ureq-proto`: it writes the request head,
//! parses the response head with `httparse` and decodes `Content-Length`, `chunked` and
//! close-delimited bodies. This module adds only what the transport's policy needs around it: the
//! connection's reads and writes, each bounded by a slice so a cancel and the deadlines are checked
//! between them; the bounds on the request head, the response head, trailers and the body; and the
//! framing it refuses rather than guesses. One request is sent with `Connection: close`; there are
//! no cookies, no compression, no keep-alive and no proxies.
use super::Method;
use crate::Error;
use std::{
    io::{self, Read, Write},
    time::{Duration, Instant},
};
use ureq_proto::{
    BodyMode,
    client::{
        Call, RecvResponseResult, SendRequestResult,
        state::{RecvBody, RecvResponse},
    },
    http::{self, Uri, Version},
};
use url::{Position, Url};

/// The response head, including interim responses, is at most this many bytes, and so are a
/// chunked body's trailers. It is also the connection's input buffer.
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// One header block holds at most this many fields.
const MAX_FIELDS: usize = 100;
/// The request head, including the caller's headers, is at most this many bytes.
const MAX_REQUEST_HEAD_BYTES: usize = 16 * 1024;
/// The caller supplies at most this many headers.
const MAX_CALLER_HEADERS: usize = 32;
/// Decoded body bytes handed to the sink at a time.
const PIECE_BYTES: usize = 16 * 1024;
/// How long one blocking read or write waits before cancellation and the deadlines are checked
/// again. It bounds how late a cancel is honoured on a stalled server; the wakes happen only while
/// a request is in flight. A timed-out socket operation consumes nothing on macOS and Linux and is
/// retried; Windows documents such a socket as indeterminate, which is not natively verified yet.
pub(super) const SLICE: Duration = Duration::from_millis(100);

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

fn is_token(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(byte))
}

fn head_too_large() -> Error {
    Error::resource_limit(format!(
        "the request head is larger than {MAX_REQUEST_HEAD_BYTES} bytes"
    ))
}

/// Check the caller's headers. No refusal repeats a header's value.
pub(super) fn check_headers(headers: &[(String, String)]) -> Result<(), Error> {
    if headers.len() > MAX_CALLER_HEADERS {
        return Err(Error::validation(format!(
            "a request has at most {MAX_CALLER_HEADERS} headers"
        )));
    }
    let mut bytes = 0;
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
        bytes += name.len() + value.len() + 4;
    }
    if bytes > MAX_REQUEST_HEAD_BYTES {
        return Err(head_too_large());
    }
    Ok(())
}

/// A request whose head is written and whose body is accounted for, ready to send.
pub(super) struct Prepared {
    head: Vec<u8>,
    call: Call<RecvResponse>,
}

/// Write the head of a request for `url` with the caller's checked `headers` and a body of
/// `body_len` bytes, before anything connects. Only the host writes `Host`, `User-Agent`,
/// `Accept-Encoding: identity`, `Connection: close` and a `POST`'s `Content-Length`.
pub(super) fn prepare(
    method: Method,
    url: &Url,
    headers: &[(String, String)],
    body_len: usize,
) -> Result<Prepared, Error> {
    let target: Uri = url[Position::BeforePath..Position::AfterQuery]
        .parse()
        .map_err(|_| Error::validation("the URL's path cannot be sent"))?;
    let mut host = url.host_str().unwrap_or_default().to_owned();
    if let Some(port) = url.port() {
        host = format!("{host}:{port}");
    }
    let mut request = http::Request::builder()
        .method(match method {
            Method::Get => http::Method::GET,
            Method::Post => http::Method::POST,
        })
        .uri(target)
        .version(Version::HTTP_11)
        .header("host", host)
        .header(
            "user-agent",
            concat!("Luxforge/", env!("CARGO_PKG_VERSION")),
        )
        .header("accept-encoding", "identity")
        .header("connection", "close");
    if method == Method::Post {
        request = request.header("content-length", body_len);
    }
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_bytes());
    }
    let unwritable = |error: ureq_proto::Error| {
        Error::validation(format!("the request cannot be sent: {error}"))
    };
    let request = request
        .body(())
        .map_err(|_| Error::validation("the request cannot be sent: a header is not valid"))?;
    let mut call = Call::new(request).map_err(unwritable)?.proceed();
    let mut head = vec![0; MAX_REQUEST_HEAD_BYTES];
    let written = match call.write(&mut head) {
        Ok(written) => written,
        Err(ureq_proto::Error::OutputOverflow) => return Err(head_too_large()),
        Err(error) => return Err(unwritable(error)),
    };
    if !call.can_proceed() {
        return Err(head_too_large());
    }
    head.truncate(written);
    let call = match call.proceed().map_err(unwritable)? {
        Some(SendRequestResult::RecvResponse(call)) => call,
        Some(SendRequestResult::SendBody(mut call)) => {
            // The body is written to the connection straight from the caller's buffer.
            call.consume_direct_write(body_len).map_err(unwritable)?;
            call.proceed()
                .ok_or_else(|| Error::validation("the request body does not match its length"))?
        }
        Some(SendRequestResult::Await100(_)) | None => {
            return Err(Error::validation("the request cannot be sent"));
        }
    };
    Ok(Prepared { head, call })
}

/// A final response's status and header fields, with its body still on the connection.
pub(super) struct Head {
    pub status: u16,
    /// Header fields with lowercased names, in the order received with a repeated name's values
    /// together.
    pub fields: Vec<(String, String)>,
    http10: bool,
    body: Option<Call<RecvBody>>,
}

/// A connected stream: plain TCP or TLS over TCP.
pub(super) trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// Whether a head holds a line feed that no carriage return precedes. `httparse` accepts one; the
/// transport does not.
fn bare_line_feed(head: &[u8]) -> bool {
    head.iter()
        .enumerate()
        .any(|(at, &byte)| byte == b'\n' && (at == 0 || head[at - 1] != b'\r'))
}

/// What bounds one exchange besides sizes: its deadlines and the caller's cancellation.
pub(super) struct Pace<'a> {
    /// The host, for messages.
    pub name: &'a str,
    /// The longest wait for any progress.
    pub idle: Duration,
    /// When the whole request, redirects included, must be complete.
    pub deadline: Instant,
    pub cancel: &'a dyn Fn() -> bool,
}

/// One request and its response over a connection whose socket times out every `SLICE`.
pub(super) struct Exchange<'a> {
    stream: Box<dyn Stream + 'a>,
    pace: Pace<'a>,
    progressed: Instant,
    /// Bytes read and not yet consumed are `buffer[start..end]`.
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
}

impl<'a> Exchange<'a> {
    pub(super) fn new(stream: Box<dyn Stream + 'a>, pace: Pace<'a>) -> Self {
        Self {
            stream,
            pace,
            progressed: Instant::now(),
            buffer: vec![0; MAX_HEAD_BYTES].into_boxed_slice(),
            start: 0,
            end: 0,
        }
    }

    fn malformed(&self, what: &str) -> Error {
        Error::file_access(format!("the response from {} {what}", self.pace.name))
    }

    /// A response `ureq-proto` could not frame. Its messages name the fault, never a value.
    fn unframed(&self, error: &ureq_proto::Error) -> Error {
        match error {
            ureq_proto::Error::HttpParseTooManyHeaders => self.too_many_fields(),
            error => self.malformed(&format!("is malformed: {error}")),
        }
    }

    fn too_many_fields(&self) -> Error {
        Error::resource_limit(format!(
            "the response from {} has more than {MAX_FIELDS} header fields",
            self.pace.name
        ))
    }

    fn head_too_large(&self, what: &str) -> Error {
        Error::resource_limit(format!(
            "the response {what} from {} is larger than {MAX_HEAD_BYTES} bytes",
            self.pace.name
        ))
    }

    fn ended(&self) -> Error {
        self.malformed("ended early")
    }

    fn failed(&self, error: &io::Error) -> Error {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            return self.ended();
        }
        Error::file_access(format!("the request to {} failed: {error}", self.pace.name))
    }

    /// Fail if the caller cancelled or a deadline passed; called before every read and write.
    fn wait(&self) -> Result<(), Error> {
        if (self.pace.cancel)() {
            return Err(Error::cancelled("the request was cancelled"));
        }
        let now = Instant::now();
        if now >= self.pace.deadline || now.duration_since(self.progressed) >= self.pace.idle {
            return Err(Error::file_access(format!(
                "the request to {} timed out",
                self.pace.name
            )));
        }
        Ok(())
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), Error> {
        while !bytes.is_empty() {
            self.wait()?;
            match self.stream.write(bytes) {
                Ok(0) => return Err(self.failed(&io::ErrorKind::WriteZero.into())),
                Ok(written) => {
                    bytes = &bytes[written..];
                    self.progressed = Instant::now();
                }
                Err(error) if retry(&error) => {}
                Err(error) => return Err(self.failed(&error)),
            }
        }
        Ok(())
    }

    fn pending(&self) -> usize {
        self.end - self.start
    }

    /// Read more bytes after the ones still buffered, which the buffer must have room for.
    /// `false` means the server closed the connection.
    fn fill(&mut self) -> Result<bool, Error> {
        self.buffer.copy_within(self.start..self.end, 0);
        (self.start, self.end) = (0, self.pending());
        loop {
            self.wait()?;
            match self.stream.read(&mut self.buffer[self.end..]) {
                Ok(0) => return Ok(false),
                Ok(read) => {
                    self.end += read;
                    self.progressed = Instant::now();
                    return Ok(true);
                }
                Err(error) if retry(&error) => {}
                Err(error) => return Err(self.failed(&error)),
            }
        }
    }

    /// Whether the last `fill` brought a line feed; `before` is what was pending until then.
    fn fresh_line(&self, before: usize) -> bool {
        self.buffer[self.start + before..self.end].contains(&b'\n')
    }

    /// Send the request head and body, then read the head of the final response, skipping interim
    /// `1xx` responses. Over TLS the handshake happens here.
    pub(super) fn send(&mut self, request: Prepared, body: &[u8]) -> Result<Head, Error> {
        let Prepared { head, mut call } = request;
        self.write_all(&head)?;
        self.write_all(body)?;
        loop {
            self.wait()?;
            match self.stream.flush() {
                Ok(()) => break,
                Err(error) if retry(&error) => {}
                Err(error) => return Err(self.failed(&error)),
            }
        }
        let mut budget = MAX_HEAD_BYTES;
        // A head is complete only once a line feed arrives, so nothing is parsed again before one.
        let mut parse = false;
        loop {
            if parse && self.start < self.end {
                let input = &self.buffer[self.start..self.end];
                let (used, response) = call
                    .try_response(input, false)
                    .map_err(|error| self.unframed(&error))?;
                if used > budget {
                    return Err(self.head_too_large("head"));
                }
                if bare_line_feed(&input[..used]) {
                    return Err(self.malformed("has a line without CRLF"));
                }
                budget -= used;
                self.start += used;
                if let Some(response) = response {
                    return self.head(call, &response);
                }
                if used > 0 {
                    continue;
                }
            }
            if self.pending() >= budget {
                return Err(self.head_too_large("head"));
            }
            let before = self.pending();
            if !self.fill()? {
                return Err(self.ended());
            }
            parse = self.fresh_line(before);
        }
    }

    fn head(&self, call: Call<RecvResponse>, response: &http::Response<()>) -> Result<Head, Error> {
        let status = response.status().as_u16();
        if status == 101 {
            return Err(self.malformed("switched protocols"));
        }
        if status >= 600 {
            return Err(self.malformed("has an invalid status line"));
        }
        if response.headers().len() > MAX_FIELDS {
            return Err(self.too_many_fields());
        }
        let fields = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = match call.proceed() {
            Some(RecvResponseResult::RecvBody(mut call)) => {
                // Each piece holds one chunk's data only, so the piece refused at the limit never
                // takes the end of an earlier chunk with it.
                call.stop_on_chunk_boundary(true);
                Some(call)
            }
            Some(RecvResponseResult::Redirect(_) | RecvResponseResult::Cleanup(_)) => None,
            None => return Err(self.malformed("has no complete head")),
        };
        Ok(Head {
            status,
            fields,
            http10: response.version() == Version::HTTP_10,
            body,
        })
    }

    /// Refuse ambiguous or unsupported framing rather than guess: compression, any transfer coding
    /// but `chunked` on HTTP/1.1, both a transfer coding and a length, and lengths that are not
    /// one plain number.
    fn check_framing(&self, head: &Head) -> Result<(), Error> {
        let values = |name: &'static str| {
            head.fields
                .iter()
                .filter(move |(field, _)| field == name)
                .map(|(_, value)| value.as_str())
        };
        if values("content-encoding").any(|coding| !coding.eq_ignore_ascii_case("identity")) {
            return Err(self.malformed("is compressed"));
        }
        if head.status == 204 || head.status == 304 {
            return Ok(());
        }
        let encodings: Vec<_> = values("transfer-encoding").collect();
        let lengths: Vec<_> = values("content-length").collect();
        match (encodings.as_slice(), lengths.as_slice()) {
            ([], []) => Ok(()),
            ([encoding], []) if encoding.eq_ignore_ascii_case("chunked") && !head.http10 => Ok(()),
            (_, []) => Err(self.malformed("uses an unsupported transfer coding")),
            ([], [first, rest @ ..])
                if !first.is_empty()
                    && first.bytes().all(|byte| byte.is_ascii_digit())
                    && rest.iter().all(|other| other == first) =>
            {
                Ok(())
            }
            ([], _) => Err(self.malformed("has an invalid Content-Length")),
            _ => Err(self.malformed("has both Transfer-Encoding and Content-Length")),
        }
    }

    /// Stream the body of a final response into `sink`, refusing it once it would pass `limit`
    /// bytes: before reading when its length is declared, and before writing the piece that crosses
    /// it otherwise. Returns the number of body bytes written.
    pub(super) fn read_body(
        &mut self,
        head: &mut Head,
        limit: u64,
        sink: &mut dyn Write,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<u64, Error> {
        self.check_framing(head)?;
        let Some(mut call) = head.body.take() else {
            return Ok(0);
        };
        let name = self.pace.name;
        let too_large = || {
            Error::resource_limit(format!(
                "the response from {name} is larger than {limit} bytes"
            ))
        };
        let mode = call.body_mode();
        let total = match mode {
            BodyMode::LengthDelimited(length) if length > limit => return Err(too_large()),
            BodyMode::LengthDelimited(length) => Some(length),
            _ => None,
        };
        let until_close = mode == BodyMode::CloseDelimited;
        let mut piece = vec![0; PIECE_BYTES];
        let mut received = 0;
        // Bytes consumed since the last body byte: chunk lines between chunks, then the trailers.
        let mut framing = 0;
        let mut parse = true;
        loop {
            if parse && self.start < self.end {
                let (used, produced) = call
                    .read(&self.buffer[self.start..self.end], &mut piece)
                    .map_err(|error| self.unframed(&error))?;
                self.start += used;
                if produced > 0 {
                    framing = 0;
                    if produced as u64 > limit - received {
                        return Err(too_large());
                    }
                    sink.write_all(&piece[..produced]).map_err(|error| {
                        Error::file_access(format!(
                            "cannot store the response from {name}: {}",
                            error.kind()
                        ))
                    })?;
                    received += produced as u64;
                    progress(received, total);
                } else {
                    framing += used;
                    if framing > MAX_HEAD_BYTES {
                        return Err(self.head_too_large("trailer"));
                    }
                }
                if !until_close && call.can_proceed() {
                    return Ok(received);
                }
                if used > 0 || produced > 0 {
                    continue;
                }
                // Without progress the reader waits for the end of a chunk line or a trailer.
                parse = false;
            }
            if self.pending() == self.buffer.len() {
                return Err(if call.is_ended_chunked() {
                    self.head_too_large("trailer")
                } else {
                    self.malformed("has an invalid chunk")
                });
            }
            let before = self.pending();
            if !self.fill()? {
                return if until_close {
                    Ok(received)
                } else {
                    Err(self.ended())
                };
            }
            parse = parse || self.fresh_line(before);
        }
    }
}
