//! The delivery policy one statistics batch is sent under
//! ([design](../../../../docs/design/usage-statistics.md#delivery-policy)): the fixed schedule and
//! bounds, the request the transport is handed, and how each outcome of the one attempt a batch
//! gets changes when the next batch may go. The [supervisor](super::supervisor) applies it.
//!
//! Every batch is attempted once and then discarded, whatever happened. `200`, `202` and `204` are
//! accepted. A network, TLS or timeout failure, an oversized response, `408` and `5xx` hold fresh
//! data back for 20, 40, 80, 160 and then 360 minutes, plus up to 10% jitter; a success resets
//! that. `429` honours a valid `Retry-After` in seconds, clamped to 10 minutes to 24 hours, and
//! otherwise takes the cooldown. Any `3xx` (a refused redirect included), any other status and a
//! request the transport refuses suspend sending until the next launch, without touching consent.
use super::{
    DESTINATION,
    eligibility::Eligibility,
    registry::{MAX_BODY_BYTES, MAX_EVENTS},
};
use crate::{
    Error, ErrorKind,
    capabilities::{
        endpoint::{EndpointClass, parse_endpoint},
        transport::{Method, TransportRequest, TransportResponse},
    },
};
use std::time::Duration;

/// A fresh window is due this long after its first event.
pub const FLUSH_AFTER: Duration = Duration::from_secs(10 * 60);
/// At most this much jitter is added to a window's due time.
pub const FLUSH_JITTER: Duration = Duration::from_secs(60);
/// Two attempts are at least this far apart, and the first is at least this long after the session
/// began: a flood fills windows rather than sending back to back, and no request is made at once at
/// startup or opt-in.
pub const ATTEMPT_INTERVAL: Duration = Duration::from_secs(60);
/// A window whose oldest event is this old is discarded unsent.
pub const WINDOW_AGE: Duration = Duration::from_secs(30 * 60);
/// A window holding this many events is due at once (plus jitter).
pub const WINDOW_EVENTS: usize = MAX_EVENTS;
/// The cooldown after the first, second, ... consecutive failure; the last repeats.
pub const COOLDOWNS_MINUTES: [u64; 5] = [20, 40, 80, 160, 360];
/// A cooldown is lengthened by up to this fraction, in percent.
pub const COOLDOWN_JITTER_PERCENT: u64 = 10;
/// `Retry-After` is clamped to this range.
pub const RETRY_AFTER_MIN: Duration = Duration::from_secs(10 * 60);
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(24 * 60 * 60);
/// The largest request body.
pub const MAX_REQUEST_BYTES: u64 = MAX_BODY_BYTES as u64;
/// The largest response body read, which is then ignored.
pub const MAX_RESPONSE_BYTES: u64 = 1024;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(2);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(5);
/// A `Retry-After` longer than this many characters is ignored as oversized.
const MAX_RETRY_AFTER_CHARS: usize = 10;

/// Why sending is held back for a while.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cooldown {
    /// A network, TLS or timeout failure, an oversized response, `408`, `5xx`, or `429` without a
    /// usable `Retry-After`.
    Failure,
    /// `429` with a valid `Retry-After`.
    RateLimited,
}

/// Why sending is suspended until the next launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suspension {
    /// A `3xx` answer, including a redirect the transport refused to follow.
    Redirect,
    /// A `4xx` other than `408` and `429`, or any status the policy does not know.
    Rejected,
    /// The transport refused the request or the receiver's address.
    Refused,
    /// The host has no network transport.
    Unavailable,
    /// The build carries no release to report, so no batch can be encoded.
    Build,
    /// No device profile can be formed in this consent generation, so nothing is collected in it.
    Device,
}

/// The delivery state `usage.read` reports beside the counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delivery {
    #[default]
    Ready,
    Cooldown(Cooldown),
    Suspended(Suspension),
}

impl Delivery {
    pub fn state(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Cooldown(_) => "cooldown",
            Self::Suspended(_) => "suspended",
        }
    }

    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Cooldown(Cooldown::Failure) => Some("failure"),
            Self::Cooldown(Cooldown::RateLimited) => Some("rate-limited"),
            Self::Suspended(Suspension::Redirect) => Some("redirect"),
            Self::Suspended(Suspension::Rejected) => Some("rejected"),
            Self::Suspended(Suspension::Refused) => Some("refused"),
            Self::Suspended(Suspension::Unavailable) => Some("unavailable"),
            Self::Suspended(Suspension::Build) => Some("build"),
            Self::Suspended(Suspension::Device) => Some("device"),
        }
    }
}

/// What one attempt's outcome means for the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    /// Hold fresh data back for the next step of the cooldown.
    Failed,
    /// Hold fresh data back for exactly this long.
    RetryAfter(Duration),
    Suspend(Suspension),
    /// The request was cancelled (an opt-out or quit): nothing changes.
    Cancelled,
}

/// Judge one attempt's outcome.
pub fn verdict(outcome: &Result<TransportResponse, Error>) -> Verdict {
    match outcome {
        Ok(response) => match response.status {
            200 | 202 | 204 => Verdict::Accepted,
            408 | 500..=599 => Verdict::Failed,
            429 => retry_after(response).map_or(Verdict::Failed, Verdict::RetryAfter),
            300..=399 => Verdict::Suspend(Suspension::Redirect),
            _ => Verdict::Suspend(Suspension::Rejected),
        },
        Err(error) => match error.kind {
            ErrorKind::Cancelled => Verdict::Cancelled,
            // The transport answers a redirect it may not follow with this refusal.
            ErrorKind::Validation if error.detail.starts_with("redirect refused") => {
                Verdict::Suspend(Suspension::Redirect)
            }
            ErrorKind::Validation => Verdict::Suspend(Suspension::Refused),
            ErrorKind::NotReady => Verdict::Suspend(Suspension::Unavailable),
            // Network, TLS, protocol and timeout failures, and a response over its limit.
            _ => Verdict::Failed,
        },
    }
}

/// A `429`'s `Retry-After` as a number of seconds, clamped; `None` when it is absent, malformed
/// (an HTTP date included) or oversized.
pub fn retry_after(response: &TransportResponse) -> Option<Duration> {
    let value = response.header("retry-after")?.trim();
    if value.is_empty()
        || value.len() > MAX_RETRY_AFTER_CHARS
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let seconds: u64 = value.parse().ok()?;
    Some(Duration::from_secs(seconds).clamp(RETRY_AFTER_MIN, RETRY_AFTER_MAX))
}

/// The cooldown after `failures` consecutive failures (at least one), before jitter.
pub fn cooldown(failures: u32) -> Duration {
    let step = (failures.max(1) - 1) as usize;
    Duration::from_secs(COOLDOWNS_MINUTES[step.min(COOLDOWNS_MINUTES.len() - 1)] * 60)
}

/// The one request every batch of this launch is sent as, without its body: `POST` to the
/// production destination or the test receiver, `Content-Type: application/json` and nothing else.
/// No query string, cookie, authorization or ID is in the URL or a header. `None` for an
/// ineligible launch, which sends nothing.
pub fn request(eligibility: &Eligibility) -> Result<Option<TransportRequest>, Error> {
    let endpoint = match eligibility {
        Eligibility::Production { .. } => parse_endpoint(DESTINATION, &[EndpointClass::Remote])?,
        Eligibility::Test(receiver) => parse_endpoint(receiver.url(), &[EndpointClass::Loopback])?,
        Eligibility::Ineligible(_) => return Ok(None),
    };
    Ok(Some(TransportRequest {
        method: Method::Post,
        endpoint,
        headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
        body: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{LaunchMode, TestReceiver};
    use url::Url;

    fn answer(status: u16, headers: &[(&str, &str)]) -> Result<TransportResponse, Error> {
        Ok(TransportResponse {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            final_url: Url::parse("http://127.0.0.1:9/v1/usage/batches").unwrap(),
            received: 0,
        })
    }

    #[test]
    fn usage_every_response_class_has_its_verdict() {
        for status in [200, 202, 204] {
            assert_eq!(verdict(&answer(status, &[])), Verdict::Accepted, "{status}");
        }
        for status in [408, 500, 502, 503, 504, 599] {
            assert_eq!(verdict(&answer(status, &[])), Verdict::Failed, "{status}");
        }
        for status in [300, 301, 302, 304, 307, 308] {
            assert_eq!(
                verdict(&answer(status, &[])),
                Verdict::Suspend(Suspension::Redirect),
                "{status}"
            );
        }
        for status in [201, 203, 206, 400, 401, 403, 404, 405, 410, 413, 415, 422] {
            assert_eq!(
                verdict(&answer(status, &[])),
                Verdict::Suspend(Suspension::Rejected),
                "{status}"
            );
        }
        let failed = |error: Error| verdict(&Err(error));
        assert_eq!(
            failed(Error::validation("redirect refused")),
            Verdict::Suspend(Suspension::Redirect)
        );
        assert_eq!(
            failed(Error::validation("redirect refused: more than 0 redirects")),
            Verdict::Suspend(Suspension::Redirect)
        );
        assert_eq!(
            failed(Error::validation("the address 10.0.0.1 is not public")),
            Verdict::Suspend(Suspension::Refused)
        );
        assert_eq!(
            failed(Error::not_ready("no network transport is configured")),
            Verdict::Suspend(Suspension::Unavailable)
        );
        assert_eq!(failed(Error::cancelled("off")), Verdict::Cancelled);
        for error in [
            Error::file_access("connection refused"),
            Error::file_access("the request to api.luxforge.app timed out"),
            Error::file_access("TLS handshake failed"),
            Error::resource_limit("the response from 127.0.0.1 is larger than 1024 bytes"),
        ] {
            assert_eq!(failed(error), Verdict::Failed);
        }
    }

    #[test]
    fn usage_retry_after_is_clamped_and_malformed_values_take_the_cooldown() {
        let after = |value: &str| verdict(&answer(429, &[("retry-after", value)]));
        assert_eq!(
            after("1"),
            Verdict::RetryAfter(RETRY_AFTER_MIN),
            "clamped up to 10 minutes"
        );
        assert_eq!(
            after("3600"),
            Verdict::RetryAfter(Duration::from_secs(3600))
        );
        assert_eq!(
            after(" 900 "),
            Verdict::RetryAfter(Duration::from_secs(900))
        );
        assert_eq!(
            after("999999999"),
            Verdict::RetryAfter(RETRY_AFTER_MAX),
            "clamped down to 24 hours"
        );
        for malformed in [
            "",
            "-5",
            "1.5",
            "soon",
            "Wed, 21 Oct 2026 07:28:00 GMT",
            "12345678901",
        ] {
            assert_eq!(after(malformed), Verdict::Failed, "{malformed:?}");
        }
        assert_eq!(verdict(&answer(429, &[])), Verdict::Failed);
    }

    #[test]
    fn usage_the_cooldown_doubles_to_six_hours_and_stays_there() {
        let minutes: Vec<u64> = (1..=8).map(|n| cooldown(n).as_secs() / 60).collect();
        assert_eq!(minutes, [20, 40, 80, 160, 360, 360, 360, 360]);
    }

    #[test]
    fn usage_the_request_is_a_bare_json_post_to_the_one_destination() {
        let test = request(&Eligibility::test(
            TestReceiver::loopback("http://127.0.0.1:4100/v1/usage/batches").unwrap(),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(test.method, Method::Post);
        assert_eq!(
            test.endpoint.url.as_str(),
            "http://127.0.0.1:4100/v1/usage/batches"
        );
        assert_eq!(test.endpoint.class, EndpointClass::Loopback);
        assert_eq!(
            test.headers,
            [("Content-Type".to_owned(), "application/json".to_owned())]
        );
        assert!(test.body.is_empty());
        let production = request(&Eligibility::Production {
            release: "0.0.0+9d4138ca1f2e".to_owned(),
        })
        .unwrap()
        .unwrap();
        assert_eq!(production.endpoint.url.as_str(), DESTINATION);
        assert_eq!(production.endpoint.class, EndpointClass::Remote);
        assert_eq!(production.endpoint.url.query(), None);
        assert!(
            request(&Eligibility::production(LaunchMode::Interactive))
                .unwrap()
                .is_none(),
            "with the activation switch off nothing is sent"
        );
    }

    #[test]
    fn usage_delivery_states_read_as_fixed_words() {
        assert_eq!(
            (Delivery::Ready.state(), Delivery::Ready.reason()),
            ("ready", None)
        );
        assert_eq!(
            Delivery::Cooldown(Cooldown::RateLimited).reason(),
            Some("rate-limited")
        );
        assert_eq!(
            Delivery::Suspended(Suspension::Redirect).state(),
            "suspended"
        );
    }
}
