//! Merges: the pure numerical library of each merge kind, and the job that composes it.
//!
//! - [`hdr`] is the HDR provider's numerical core: one module per stage, each owning its
//!   suitability validation, exposure or coordinate resolution and pixel equations
//!   (`docs/design/hdr-merge.md`). Nothing there reads a file, touches the catalog or schedules
//!   work.
//! - `job` is the merge lane's worker job, which composes those functions with the shared verified
//!   source development, bounded spooling and durable derived storage; the command service
//!   (`crate::api` `merge.*`) starts it and the owner publishes what it answers.
//! - `providers` is the registry of the kinds this build can merge
//!   ([`MergeKind`](crate::catalog_types::MergeKind)): `hdr` alone. The catalog records and
//!   browses panoramas too, but a panorama request is `not-ready` until its provider is registered.

pub mod hdr;
pub(crate) mod job;
pub(crate) mod providers;
