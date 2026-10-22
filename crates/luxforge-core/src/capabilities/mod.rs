//! Host-owned module capabilities: typed settings and provider profiles, the secret store's
//! contract, scoped consent, endpoint parsing, the network transport's contract, the capability
//! worker, managed resources, the `local-runtime` resident set ([`runtime`]) and the host-planned
//! stage read ([`read`]). A module declares what it needs; only the host stores, downloads, loads,
//! contacts, reads or schedules anything. The host is given its secure store, its network transport
//! (`luxforge-net`'s, in the desktop and `luxforge-json`) and its inference runtime through
//! [`host::HostConfig`]. See
//! `docs/design/module-capabilities.md`.
pub(crate) mod analysis;
#[cfg(test)]
mod candidate_tests;
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
mod model_resource_tests;
pub(crate) mod permission_watch;
#[cfg(test)]
mod proof_tests;
pub mod read;
#[cfg(test)]
mod read_tests;
pub mod redact;
pub mod resources;
pub mod runtime;
#[cfg(test)]
mod runtime_tests;
pub mod secrets;
pub mod settings;
#[cfg(test)]
pub(crate) mod testing;
pub mod transport;
