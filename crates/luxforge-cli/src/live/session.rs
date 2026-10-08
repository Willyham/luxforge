use super::{INVALID_SESSION, NO_SESSION, UNSUPPORTED_PROTOCOL, USAGE, failure};
use crate::Paths;
use luxforge_core::{
    ApiFailure, LocalSessionInfo, PROTOCOL, RunningSession, live_session_file, running_sessions,
};
use serde_json::{Value, json};
use std::{
    fmt,
    io::Read,
    net::{SocketAddr, SocketAddrV4},
    path::{Path, PathBuf},
};

/// The most a live-session file may hold; the desktop writes well under it.
pub const SESSION_FILE_LIMIT: usize = 4 * 1024;

/// The live session a client attaches to: `explicit`'s, read from the session file beside it, else
/// the one running desktop's, from the per-user registry of running sessions under `paths`
/// ([`Paths::live_sessions`]). No running session is [`NO_SESSION`]; several is a [`USAGE`]
/// failure listing them, since the client never guesses which one is meant. Finding creates
/// nothing, and only removes registry entries whose process has gone.
pub fn find_session(
    explicit: Option<&Path>,
    paths: Option<&Paths>,
) -> Result<LiveSession, ApiFailure> {
    if let Some(catalog) = explicit {
        return LiveSession::discover(catalog);
    }
    let Some(paths) = paths else {
        return Err(failure(
            USAGE,
            "no configuration directory to find a running Luxforge in; pass --catalog",
            None,
        ));
    };
    let registry = paths.live_sessions();
    let running = running_sessions(&registry).map_err(|error| {
        failure(
            INVALID_SESSION,
            error.detail,
            Some(json!({"registry": registry})),
        )
    })?;
    match running.as_slice() {
        [] => Err(failure(
            NO_SESSION,
            format!(
                "Luxforge is not running with a live session (none is registered in {}); open \
                 Luxforge, or pass --catalog for a desktop started with --data-root",
                registry.display()
            ),
            Some(json!({"registry": registry})),
        )),
        [running] => LiveSession::registered(running),
        several => {
            let sessions: Vec<Value> = several.iter().map(describe).collect();
            let catalogs: Vec<String> = several
                .iter()
                .map(|running| match &running.entry {
                    Ok(entry) => entry.catalog.display().to_string(),
                    Err(_) => running.file.display().to_string(),
                })
                .collect();
            Err(failure(
                USAGE,
                format!(
                    "{} Luxforge live sessions are running; name one with --catalog: {}",
                    several.len(),
                    catalogs.join(", ")
                ),
                Some(json!({"sessions": sessions})),
            ))
        }
    }
}

/// One running session as a failure listing several names it.
fn describe(running: &RunningSession) -> Value {
    match &running.entry {
        Ok(entry) => json!({"catalog": entry.catalog, "pid": entry.pid}),
        Err(problem) => json!({"entry": running.file, "problem": problem}),
    }
}

/// An open catalog's live session, read from the session file beside it.
pub struct LiveSession {
    /// The catalog whose session this is.
    pub catalog: PathBuf,
    /// The loopback address the desktop serves the session at.
    pub address: SocketAddrV4,
    pub(super) token: String,
}

impl fmt::Debug for LiveSession {
    /// The token stays out of every log and message.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveSession")
            .field("catalog", &self.catalog)
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl LiveSession {
    /// Read `catalog`'s live-session file: at most [`SESSION_FILE_LIMIT`] bytes, this client's
    /// protocol, an IPv4 loopback address and a token. Any other file is refused, and a missing one
    /// means the desktop does not have the catalog open.
    pub fn discover(catalog: &Path) -> Result<Self, ApiFailure> {
        let file = live_session_file(catalog);
        let data = || Some(json!({"catalog": catalog, "session_file": file}));
        let invalid = |message: String| failure(INVALID_SESSION, message, data());
        let mut bytes = Vec::new();
        match std::fs::File::open(&file) {
            Ok(opened) => opened
                .take(SESSION_FILE_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| invalid(format!("cannot read the live-session file: {error}")))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(failure(
                    NO_SESSION,
                    format!(
                        "Luxforge does not have the catalog {} open; open it in Luxforge, or leave \
                         out --catalog to use the running Luxforge's",
                        catalog.display()
                    ),
                    data(),
                ));
            }
            Err(error) => {
                return Err(invalid(format!(
                    "cannot read the live-session file: {error}"
                )));
            }
        };
        if bytes.len() > SESSION_FILE_LIMIT {
            return Err(invalid(format!(
                "the live-session file is larger than {SESSION_FILE_LIMIT} bytes"
            )));
        }
        let info: LocalSessionInfo = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("the live-session file is malformed: {error}")))?;
        if info.protocol != PROTOCOL {
            return Err(failure(
                UNSUPPORTED_PROTOCOL,
                format!(
                    "the live session speaks {}; this luxforge-ctl speaks {PROTOCOL}",
                    info.protocol
                ),
                data(),
            ));
        }
        let address = match info.address {
            SocketAddr::V4(address) if address.ip().is_loopback() => address,
            address => {
                return Err(invalid(format!(
                    "the live session's address {address} is not an IPv4 loopback address"
                )));
            }
        };
        if info.token.is_empty() {
            return Err(invalid("the live-session file has no token".into()));
        }
        Ok(Self {
            catalog: catalog.to_path_buf(),
            address,
            token: info.token,
        })
    }

    /// The session a registry entry names: its protocol this client's and its session file the
    /// one beside its catalog, then that file read as [`Self::discover`] reads it.
    fn registered(running: &RunningSession) -> Result<Self, ApiFailure> {
        let data = || Some(json!({"entry": running.file}));
        let entry = running.entry.as_ref().map_err(|problem| {
            failure(
                INVALID_SESSION,
                format!("the running Luxforge's registry entry cannot be read: {problem}"),
                data(),
            )
        })?;
        if entry.protocol != PROTOCOL {
            return Err(failure(
                UNSUPPORTED_PROTOCOL,
                format!(
                    "the running Luxforge speaks {}; this luxforge-ctl speaks {PROTOCOL}",
                    entry.protocol
                ),
                data(),
            ));
        }
        if entry.session_file != live_session_file(&entry.catalog) {
            return Err(failure(
                INVALID_SESSION,
                format!(
                    "the running Luxforge's registry entry names {} as the session file of {}",
                    entry.session_file.display(),
                    entry.catalog.display()
                ),
                data(),
            ));
        }
        Self::discover(&entry.catalog)
    }
}
