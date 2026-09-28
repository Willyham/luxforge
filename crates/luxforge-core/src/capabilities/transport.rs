//! The host's only network path, as the core sees it: one checked request at a time through a
//! [`Transport`] the host is given in its configuration. The core decides what is sent, to which
//! endpoint, under which bounds and for which job; the transport that resolves, connects, speaks
//! TLS and frames HTTP lives outside the core (`luxforge-net`), so the core links no network
//! stack. See `docs/design/module-capabilities.md#transport`.
use super::endpoint::Endpoint;
use crate::{Error, jobs::JobControl};
use std::{fmt, io::Write, sync::Arc, time::Duration};
use url::Url;

/// A request follows at most this many redirects, whatever its policy asks for.
pub const MAX_REDIRECTS: u8 = 3;

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
    /// Extra headers, such as `Authorization` or `Content-Type`. The transport writes `Host`,
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
    /// The job the request runs for. Its cancel stops the request with its `cancelled` error at
    /// once, however far the request has got; a request whose job is already cancelled sends
    /// nothing.
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

/// The host's HTTP client for module capabilities, shared by every download and task request. It
/// is called on worker lanes, never on the owner, and holds no connection between requests.
pub trait Transport: Send + Sync + fmt::Debug {
    /// Send `request` and stream the final response body into `sink`. Everything the request itself
    /// can be refused for is checked before connecting. The endpoint's host is resolved once and
    /// only addresses of its class are connected to. A redirect is followed only as
    /// `options.redirects` allows, with `GET`, no body, and no `Authorization` header once the
    /// origin changes. Errors are `validation` for a refused request, address or redirect,
    /// `resource-limit` for sizes, `read-error` for network, TLS and protocol failures, timeouts
    /// and a sink that refuses a write, and `cancelled`; no message contains a header value or a
    /// body.
    fn send(
        &self,
        request: &TransportRequest,
        options: SendOptions<'_>,
        sink: &mut dyn Write,
    ) -> Result<TransportResponse, Error>;
}

/// A transport that refuses every request with `not-ready: <reason>`: a host configured without a
/// network path.
#[derive(Clone, Debug)]
pub struct UnavailableTransport {
    reason: String,
}

impl UnavailableTransport {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl Transport for UnavailableTransport {
    fn send(
        &self,
        _: &TransportRequest,
        options: SendOptions<'_>,
        _: &mut dyn Write,
    ) -> Result<TransportResponse, Error> {
        options.control.checkpoint()?;
        Err(Error::not_ready(self.reason.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ErrorKind,
        capabilities::endpoint::{EndpointClass, parse_endpoint},
    };

    #[test]
    fn an_unavailable_transport_refuses_every_request_as_not_ready_after_its_cancel() {
        let transport = UnavailableTransport::new("no network transport is configured");
        let request = TransportRequest {
            method: Method::Get,
            endpoint: parse_endpoint("https://example.com/a", &[EndpointClass::Remote]).unwrap(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let send = |control: &Arc<JobControl>| {
            let mut sink = Vec::new();
            transport
                .send(
                    &request,
                    SendOptions {
                        max_request_bytes: 0,
                        max_response_bytes: 1,
                        connect_timeout: Duration::from_secs(1),
                        read_timeout: Duration::from_secs(1),
                        total_timeout: Duration::from_secs(1),
                        redirects: RedirectPolicy::default(),
                        control,
                        progress: &mut |_, _| {},
                    },
                    &mut sink,
                )
                .unwrap_err()
        };
        let refused = send(&JobControl::new());
        assert_eq!(
            (refused.kind, refused.detail.as_str()),
            (ErrorKind::NotReady, "no network transport is configured")
        );
        let control = JobControl::new();
        control.cancel("permission revoked");
        assert_eq!(send(&control).kind, ErrorKind::Cancelled);
    }
}
