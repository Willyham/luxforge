//! A client of the desktop's live session, which `luxforge-ctl` drives
//! ([design](../../../../docs/design/live-cli.md)). [`find_session`] finds the running desktop's
//! session through the per-user registry of running sessions, or reads the session file beside a
//! catalog it is given, and [`LiveClient`] speaks the same bounded JSON-lines protocol to its
//! loopback address with its token, as an ordinary live client with edit authority, after a
//! bounded handshake that proves the address is that session. Nothing here starts a catalog owner,
//! writes a catalog or grants a permission: a catalog that is not open is an error, never a
//! fallback. [`command`] is the `luxforge-ctl` command line over them.
mod client;
pub mod command;
mod session;

pub use client::{CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT, LiveClient, RESPONSE_LIMIT};
pub use session::{LiveSession, SESSION_FILE_LIMIT, find_session};

use luxforge_core::ApiFailure;
use serde_json::Value;

/// No live session to attach to: the catalog named has no session file, or no running desktop
/// registered one.
pub const NO_SESSION: &str = "no-session";
/// The live-session file names an address that does not answer as that session: its editor has
/// quit, or another program now holds the address.
pub const STALE_SESSION: &str = "stale-session";
/// The live-session file, or the registry entry naming it, cannot be read as a loopback session.
pub const INVALID_SESSION: &str = "invalid-session";
/// The live session speaks a protocol other than this client's.
pub const UNSUPPORTED_PROTOCOL: &str = "unsupported-protocol";
/// The connection failed after a request was sent, before its answer arrived.
pub const CONNECTION_LOST: &str = "connection-lost";
/// A mutation was sent and the connection failed before its answer arrived: it may or may not
/// have been applied, and it is never sent again.
pub const OUTCOME_UNKNOWN: &str = "outcome-unknown";
/// The command line, or a batch request line, is wrong; nothing was sent for it.
pub const USAGE: &str = "usage";
/// An answer the client cannot read as this protocol's: malformed, or for another request.
pub const PROTOCOL_ERROR: &str = "protocol";
/// A failure of the client itself, such as a request it could not encode.
pub const INTERNAL: &str = "internal";
/// The warning code for a job a call started without `--wait` that belongs to the clients that
/// requested it, and so stops, if it is still running, when this client's connection closes.
pub const JOB_RELEASED: &str = "job-released";

/// The exit status of a run that failed in a way not listed below, the API's own failures
/// included.
pub const EXIT_FAILURE: i32 = 1;
/// The exit status of a command line, or batch request, that cannot be read.
pub const EXIT_USAGE: i32 = 2;
/// The exit status of a run that found no live session it could speak to: [`NO_SESSION`],
/// [`STALE_SESSION`], [`INVALID_SESSION`] or [`UNSUPPORTED_PROTOCOL`].
pub const EXIT_SESSION: i32 = 3;
/// The exit status of a mutation whose outcome is unknown ([`OUTCOME_UNKNOWN`]).
pub const EXIT_OUTCOME_UNKNOWN: i32 = 4;

/// The exit status a failure with `code` ends a run with.
pub fn exit_status(code: &str) -> i32 {
    match code {
        USAGE => EXIT_USAGE,
        NO_SESSION | STALE_SESSION | INVALID_SESSION | UNSUPPORTED_PROTOCOL => EXIT_SESSION,
        OUTCOME_UNKNOWN => EXIT_OUTCOME_UNKNOWN,
        _ => EXIT_FAILURE,
    }
}

/// A failure of the client's own, in the shape the API answers its failures with, so a caller
/// reads both the same way.
pub(crate) fn failure(code: &str, message: impl Into<String>, data: Option<Value>) -> ApiFailure {
    ApiFailure {
        code: code.into(),
        message: message.into(),
        job_id: None,
        data,
    }
}

#[cfg(test)]
mod tests;
