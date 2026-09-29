//! Host-owned module capabilities: typed settings and provider profiles, the secret store's
//! contract, scoped consent, endpoint parsing, the network transport's contract, the capability
//! worker and managed resources. A module declares what it needs; only the host stores, downloads,
//! contacts or schedules anything. The host is given its secure store and its network transport
//! (`luxforge-net`'s, in the desktop and `luxforge-json`) through [`host::HostConfig`]. See
//! `docs/design/module-capabilities.md`.
pub mod consent;
pub(crate) mod context;
pub mod data;
pub mod descriptor;
pub(crate) mod document;
pub mod endpoint;
pub mod grants;
pub mod host;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod proof_tests;
pub mod redact;
pub mod resources;
pub mod secrets;
pub mod settings;
#[cfg(test)]
pub(crate) mod testing;
pub mod transport;
