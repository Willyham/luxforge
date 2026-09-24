//! Host-owned module capabilities: typed settings and provider profiles, secret storage, scoped
//! consent, the network and file transports, the capability worker and managed resources. A module
//! declares what it needs; only the host stores, opens, downloads, contacts or schedules anything.
//! See `docs/design/module-capabilities.md`.
mod atomic;
pub mod consent;
pub mod context;
pub mod data;
pub mod descriptor;
pub mod files;
pub mod grants;
pub mod host;
pub mod jobs;
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
