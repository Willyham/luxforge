//! A minimal HTTP/1.1 exchange over one connection: one request with `Connection: close`, then one
//! response whose head is bounded and whose body streams into the caller's sink under its limit.
//! There are no cookies, no compression, no keep-alive and no proxies.
use super::Method;
use crate::{Error, ErrorKind};
use std::{
    fmt::Write as _,
    io::{self, Read, Write},
    time::{Duration, Instant},
};
use url::{Position, Url};

/// The response head, including interim responses and trailers, is at most this many bytes.
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// One header block holds at most this many fields.
const MAX_FIELDS: usize = 100;
/// The request head, including the caller's headers, is at most this many bytes.
const MAX_REQUEST_HEAD_BYTES: usize = 16 * 1024;
/// The caller supplies at most this many headers.
const MAX_CALLER_HEADERS: usize = 32;
/// A chunk-size line, extensions included, is at most this many bytes.
const MAX_CHUNK_LINE: usize = 1024;
/// Bytes read from the socket at a time.
const BUFFER_BYTES: usize = 16 * 1024;
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

fn refused(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

pub(super) fn cancelled() -> Error {
    Error::new(ErrorKind::Cancelled, "the request was cancelled")
}

fn head_too_large() -> Error {
    Error::new(
        ErrorKind::ResourceLimit,
        format!("the request head is larger than {MAX_REQUEST_HEAD_BYTES} bytes"),
    )
}

/// Check the caller's headers. No refusal repeats a header's value.
pub(super) fn check_headers(headers: &[(String, String)]) -> Result<(), Error> {
    if headers.len() > MAX_CALLER_HEADERS {
        return Err(refused(format!(
            "a request has at most {MAX_CALLER_HEADERS} headers"
        )));
    }
    let mut bytes = 0;
    for (name, value) in headers {
        if !is_token(name.as_bytes()) {
            return Err(refused("a request header name is not a valid token"));
        }
        if HOST_HEADERS
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            return Err(refused(format!("a request may not set the {name} header")));
        }
        if value
            .bytes()
            .any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f)
        {
            return Err(refused(format!(
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

/// Serialize the request line and headers. The caller's headers were checked by `check_headers`;
/// only the host writes `Host`, `Content-Length` and the connection headers.
pub(super) fn request_head(
    method: Method,
    url: &Url,
    headers: &[(String, String)],
    body_len: usize,
) -> Result<Vec<u8>, Error> {
    let mut head = String::with_capacity(512);
    let _ = write!(
        head,
        "{} {} HTTP/1.1\r\nHost: {}",
        method.as_str(),
        &url[Position::BeforePath..Position::AfterQuery],
        url.host_str().unwrap_or_default()
    );
    if let Some(port) = url.port() {
        let _ = write!(head, ":{port}");
    }
    head.push_str(concat!(
        "\r\nUser-Agent: Lightwell/",
        env!("CARGO_PKG_VERSION"),
        "\r\nAccept-Encoding: identity\r\nConnection: close\r\n"
    ));
    if method == Method::Post {
        let _ = write!(head, "Content-Length: {body_len}\r\n");
    }
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    if head.len() > MAX_REQUEST_HEAD_BYTES {
        return Err(head_too_large());
    }
    Ok(head.into_bytes())
}

/// How a response body ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Framing {
    /// The status forbids a body.
    Empty,
    Length(u64),
    Chunked,
    /// The body ends when the server closes the connection.
    Close,
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
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
    /// Head bytes still allowed, shared by interim responses, the final head and trailers.
    head_budget: usize,
}

impl<'a> Exchange<'a> {
    pub(super) fn new(stream: Box<dyn Stream + 'a>, pace: Pace<'a>) -> Self {
        Self {
            stream,
            pace,
            progressed: Instant::now(),
            buffer: vec![0; BUFFER_BYTES].into_boxed_slice(),
            start: 0,
            end: 0,
            head_budget: MAX_HEAD_BYTES,
        }
    }

    fn malformed(&self, what: &str) -> Error {
        Error::new(
            ErrorKind::FileAccess,
            format!("the response from {} {what}", self.pace.name),
        )
    }

    fn ended(&self) -> Error {
        self.malformed("ended early")
    }

    fn failed(&self, error: &io::Error) -> Error {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            return self.ended();
        }
        Error::new(
            ErrorKind::FileAccess,
            format!("the request to {} failed: {error}", self.pace.name),
        )
    }

    /// Fail if the caller cancelled or a deadline passed; called before every read and write.
    fn wait(&self) -> Result<(), Error> {
        if (self.pace.cancel)() {
            return Err(cancelled());
        }
        let now = Instant::now();
        if now >= self.pace.deadline || now.duration_since(self.progressed) >= self.pace.idle {
            return Err(Error::new(
                ErrorKind::FileAccess,
                format!("the request to {} timed out", self.pace.name),
            ));
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

    /// Send the request head and body. Over TLS the handshake happens here.
    pub(super) fn send(&mut self, head: &[u8], body: &[u8]) -> Result<(), Error> {
        self.write_all(head)?;
        self.write_all(body)?;
        loop {
            self.wait()?;
            match self.stream.flush() {
                Ok(()) => return Ok(()),
                Err(error) if retry(&error) => {}
                Err(error) => return Err(self.failed(&error)),
            }
        }
    }

    /// Refill an empty buffer. `false` means the server closed the connection.
    fn fill(&mut self) -> Result<bool, Error> {
        loop {
            self.wait()?;
            match self.stream.read(&mut self.buffer) {
                Ok(0) => return Ok(false),
                Ok(read) => {
                    (self.start, self.end) = (0, read);
                    self.progressed = Instant::now();
                    return Ok(true);
                }
                Err(error) if retry(&error) => {}
                Err(error) => return Err(self.failed(&error)),
            }
        }
    }

    /// Read one CRLF-terminated line into `line` without its terminator. `false` means the line
    /// with its terminator is longer than `max`; the exchange cannot continue after that.
    fn line(&mut self, line: &mut Vec<u8>, max: usize) -> Result<bool, Error> {
        line.clear();
        loop {
            if self.start == self.end && !self.fill()? {
                return Err(self.ended());
            }
            let available = &self.buffer[self.start..self.end];
            let (taken, complete) = match available.iter().position(|&byte| byte == b'\n') {
                Some(at) => (at + 1, true),
                None => (available.len(), false),
            };
            if line.len() + taken > max {
                return Ok(false);
            }
            line.extend_from_slice(&available[..taken]);
            self.start += taken;
            if complete {
                line.pop();
                if line.pop() != Some(b'\r') {
                    return Err(self.malformed("has a line without CRLF"));
                }
                return Ok(true);
            }
        }
    }

    /// Read one header-block line against the shared head budget.
    fn head_line(&mut self, line: &mut Vec<u8>) -> Result<(), Error> {
        if !self.line(line, self.head_budget)? {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "the response head from {} is larger than {MAX_HEAD_BYTES} bytes",
                    self.pace.name
                ),
            ));
        }
        self.head_budget -= line.len() + 2;
        Ok(())
    }

    /// Read header fields up to the empty line that ends the block, with lowercased names.
    fn fields(&mut self, line: &mut Vec<u8>) -> Result<Vec<(String, String)>, Error> {
        let mut fields = Vec::new();
        loop {
            self.head_line(line)?;
            if line.is_empty() {
                return Ok(fields);
            }
            if fields.len() == MAX_FIELDS {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "the response from {} has more than {MAX_FIELDS} header fields",
                        self.pace.name
                    ),
                ));
            }
            let colon = line.iter().position(|&byte| byte == b':');
            let Some((name, value)) = colon.map(|at| (&line[..at], line[at + 1..].trim_ascii()))
            else {
                return Err(self.malformed("has an invalid header field"));
            };
            if !is_token(name) || value.iter().any(|&byte| byte == 0 || byte == b'\r') {
                return Err(self.malformed("has an invalid header field"));
            }
            fields.push((
                String::from_utf8_lossy(name).to_ascii_lowercase(),
                String::from_utf8_lossy(value).into_owned(),
            ));
        }
    }

    /// Read the status line and headers of the final response, skipping interim `1xx` responses.
    pub(super) fn read_head(&mut self) -> Result<(u16, Vec<(String, String)>), Error> {
        let mut line = Vec::new();
        loop {
            self.head_line(&mut line)?;
            let status = line
                .strip_prefix(b"HTTP/1.1 ")
                .or_else(|| line.strip_prefix(b"HTTP/1.0 "))
                .filter(|rest| {
                    rest.len() >= 3
                        && rest[..3].iter().all(u8::is_ascii_digit)
                        && rest.get(3).is_none_or(|&byte| byte == b' ')
                })
                .map(|rest| {
                    rest[..3]
                        .iter()
                        .fold(0_u16, |status, digit| status * 10 + u16::from(digit - b'0'))
                })
                .filter(|status| (100..600).contains(status))
                .ok_or_else(|| self.malformed("has an invalid status line"))?;
            let fields = self.fields(&mut line)?;
            match status {
                101 => return Err(self.malformed("switched protocols")),
                100..200 => continue,
                _ => return Ok((status, fields)),
            }
        }
    }

    /// Decide how the body of a final response ends, refusing ambiguous or unsupported framing
    /// rather than guessing.
    pub(super) fn framing(
        &self,
        status: u16,
        headers: &[(String, String)],
    ) -> Result<Framing, Error> {
        let values = |name: &'static str| {
            headers
                .iter()
                .filter(move |(field, _)| field == name)
                .map(|(_, value)| value.as_str())
        };
        if values("content-encoding").any(|coding| !coding.eq_ignore_ascii_case("identity")) {
            return Err(self.malformed("is compressed"));
        }
        if status == 204 || status == 304 {
            return Ok(Framing::Empty);
        }
        let encodings: Vec<_> = values("transfer-encoding").collect();
        let lengths: Vec<_> = values("content-length").collect();
        match (encodings.as_slice(), lengths.as_slice()) {
            ([], []) => Ok(Framing::Close),
            ([encoding], []) if encoding.eq_ignore_ascii_case("chunked") => Ok(Framing::Chunked),
            (_, []) => Err(self.malformed("uses an unsupported transfer coding")),
            ([], [first, rest @ ..]) => {
                let valid = !first.is_empty() && first.bytes().all(|byte| byte.is_ascii_digit());
                match first.parse() {
                    Ok(length) if valid && rest.iter().all(|other| other == first) => {
                        Ok(Framing::Length(length))
                    }
                    _ => Err(self.malformed("has an invalid Content-Length")),
                }
            }
            _ => Err(self.malformed("has both Transfer-Encoding and Content-Length")),
        }
    }

    /// The next buffered bytes, at most `most` of them; empty when the server closed.
    fn piece(&mut self, most: u64) -> Result<&[u8], Error> {
        if self.start == self.end && !self.fill()? {
            return Ok(&[]);
        }
        let take = (self.end - self.start).min(usize::try_from(most).unwrap_or(usize::MAX));
        let range = self.start..self.start + take;
        self.start += take;
        Ok(&self.buffer[range])
    }

    fn chunk_size(&mut self, line: &mut Vec<u8>) -> Result<u64, Error> {
        if !self.line(line, MAX_CHUNK_LINE)? {
            return Err(self.malformed("has an invalid chunk"));
        }
        let digits = line
            .iter()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count();
        let extension = line[digits..].trim_ascii_start();
        if digits == 0 || digits > 16 || !(extension.is_empty() || extension[0] == b';') {
            return Err(self.malformed("has an invalid chunk"));
        }
        Ok(line[..digits].iter().fold(0, |size, &digit| {
            size * 16 + u64::from(char::from(digit).to_digit(16).unwrap_or(0))
        }))
    }

    /// Stream the body into `sink`, refusing it once it would pass `limit` bytes: before reading
    /// when its length is declared, and before writing the piece that crosses it otherwise.
    /// Returns the number of body bytes written.
    pub(super) fn read_body(
        &mut self,
        framing: Framing,
        limit: u64,
        sink: &mut dyn Write,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<u64, Error> {
        let name = self.pace.name;
        let too_large = || {
            Error::new(
                ErrorKind::ResourceLimit,
                format!("the response from {name} is larger than {limit} bytes"),
            )
        };
        let mut deliver = |received: &mut u64, piece: &[u8], total: Option<u64>| {
            sink.write_all(piece).map_err(|error| {
                Error::new(
                    ErrorKind::FileAccess,
                    format!("cannot store the response from {name}: {}", error.kind()),
                )
            })?;
            *received += piece.len() as u64;
            progress(*received, total);
            Ok::<_, Error>(())
        };
        let mut received = 0;
        match framing {
            Framing::Empty => {}
            Framing::Length(length) => {
                if length > limit {
                    return Err(too_large());
                }
                while received < length {
                    let piece = self.piece(length - received)?;
                    if piece.is_empty() {
                        return Err(self.ended());
                    }
                    deliver(&mut received, piece, Some(length))?;
                }
            }
            Framing::Close => loop {
                let piece = self.piece(u64::MAX)?;
                if piece.is_empty() {
                    break;
                }
                if piece.len() as u64 > limit - received {
                    return Err(too_large());
                }
                deliver(&mut received, piece, None)?;
            },
            Framing::Chunked => {
                let mut line = Vec::new();
                loop {
                    let size = self.chunk_size(&mut line)?;
                    if size == 0 {
                        break;
                    }
                    if size > limit - received {
                        return Err(too_large());
                    }
                    let end = received + size;
                    while received < end {
                        let piece = self.piece(end - received)?;
                        if piece.is_empty() {
                            return Err(self.ended());
                        }
                        deliver(&mut received, piece, None)?;
                    }
                    if !self.line(&mut line, 2)? || !line.is_empty() {
                        return Err(self.malformed("has an invalid chunk"));
                    }
                }
                self.fields(&mut line)?;
            }
        }
        Ok(received)
    }
}
