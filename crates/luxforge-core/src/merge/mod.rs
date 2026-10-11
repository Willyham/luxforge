//! Merges: the pure numerical library of each merge kind, and the job that composes it.
//!
//! - [`hdr`] is the HDR provider's numerical core: one module per stage, each owning its
//!   suitability validation, exposure or coordinate resolution and pixel equations
//!   (`docs/design/hdr-merge.md`). Nothing there reads a file, touches the catalog or schedules
//!   work.
//! - `job` is the merge lane's worker job, which composes those functions with the shared verified
//!   source development, bounded spooling and durable derived storage; the command service
//!   (`crate::api` `merge.*`) starts it and the owner publishes what it answers.
//!
//! Only `hdr` is a merge kind.

pub mod hdr;
pub(crate) mod job;
