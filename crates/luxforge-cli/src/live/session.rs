use super::{INVALID_SESSION, NO_SESSION, UNSUPPORTED_PROTOCOL, USAGE, failure};
use crate::{CatalogSelection, Paths};
use luxforge_core::{
    ApiFailure, LocalSessionInfo, PROTOCOL, live_session_file, preferences::LaunchPreferences,
};
use serde_json::json;
use std::{
    fmt,
    io::Read,
    net::{SocketAddr, SocketAddrV4},
    path::{Path, PathBuf},
};

/// The most a live-session file may hold; the desktop writes well under it.
pub const SESSION_FILE_LIMIT: usize = 4 * 1024;

/// The catalog a live client attaches to: `explicit`, else the catalog the desktop's ordinary
/// launch opens with the configuration under `paths` ([`CatalogSelection`]). Reading the stored
/// location creates nothing.
pub fn target_catalog(
    explicit: Option<PathBuf>,
    paths: Option<&Paths>,
) -> Result<PathBuf, ApiFailure> {
    let stored = match (&explicit, paths) {
        (None, Some(paths)) => LaunchPreferences::read(Some(paths.config.clone())).catalog,
        _ => None,
    };
    CatalogSelection::select(explicit, paths.map(Paths::default_catalog), stored)
        .map(|selection| selection.path)
        .ok_or_else(|| {
            failure(
                USAGE,
                "no configuration directory to find the desktop's catalog in; pass --catalog",
                None,
            )
        })
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
                        "Luxforge does not have the catalog {} open; open it in Luxforge or pass \
                         --catalog",
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
}
