//! Host-owned module capabilities: typed settings and provider profiles, secret storage, scoped
//! consent, the network and file transports, the capability worker and managed resources. A module
//! declares what it needs; only the host stores, opens, downloads, contacts or schedules anything.
//! See `docs/design/module-capabilities.md`.
pub mod descriptor;
pub mod files;
pub mod redact;
pub mod secrets;
pub mod settings;
#[cfg(test)]
pub(crate) mod testing;
pub mod transport;
