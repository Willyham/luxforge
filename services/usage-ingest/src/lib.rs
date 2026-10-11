//! The Luxforge usage statistics ingress: a small, stateless validating service.
//!
//! It accepts `POST /v1/usage/batches` (see `docs/design/usage-statistics-wire.md`), refuses
//! anything that is not exactly a current batch, normalises an accepted one into NDJSON rows and
//! forwards them once, through a bounded in-memory queue, to a private collector. It keeps no
//! account, secret, disk spool or network address, and logs only aggregate counters.

pub mod config;
pub mod diag;
pub mod forward;
pub mod limits;
pub mod registry;
pub mod rows;
pub mod server;
pub mod wire;
