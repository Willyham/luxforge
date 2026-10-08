//! A client of the desktop's live session, which `luxforge-ctl` drives
//! ([design](../../../../docs/design/live-cli.md)). [`LiveSession::discover`] reads the session file
//! beside a catalog the desktop has open, and [`LiveClient`] speaks the same bounded JSON-lines
//! protocol to its loopback address with its token, as an ordinary live client with edit authority.
//! Nothing here starts a catalog owner, writes a catalog or grants a permission: a catalog that is
//! not open is an error, never a fallback. [`command`] is the `luxforge-ctl` command line over them.
mod client;
pub mod command;
mod session;

pub use client::{CONNECT_TIMEOUT, LiveClient, REQUEST_LIMIT, RESPONSE_LIMIT};
pub use session::{LiveSession, SESSION_FILE_LIMIT, target_catalog};

use luxforge_core::ApiFailure;
use serde_json::Value;

/// The catalog named has no live-session file: the desktop does not have it open.
pub const NO_SESSION: &str = "no-session";
/// The live-session file names an address nothing answers at: its editor has quit.
pub const STALE_SESSION: &str = "stale-session";
/// The live-session file cannot be read as a loopback session.
pub const INVALID_SESSION: &str = "invalid-session";
/// The live-session file names a protocol other than this client's.
pub const UNSUPPORTED_PROTOCOL: &str = "unsupported-protocol";
/// The connection failed after a request was sent, before its answer arrived.
pub const CONNECTION_LOST: &str = "connection-lost";
/// A mutation was sent and the connection failed before its answer arrived: it may or may not
/// have been applied, and it is never sent again.
pub const OUTCOME_UNKNOWN: &str = "outcome-unknown";
/// The command line itself is wrong; nothing was sent.
pub const USAGE: &str = "usage";

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
mod endpoint;
#[cfg(test)]
mod live_session_client;
#[cfg(test)]
mod luxforge_ctl_commands;
