//! Fixtures shared by the workspace's tests and by `xtask`, kept out of every shipped crate: the one
//! loopback HTTP test server, the capability proof's fake provider built on it, and the JSON
//! client of the catalog owner ([`client`]) the field-patch conformance suite and xtask's
//! acceptance chapters drive the owner with.
//!
//! Only `[dev-dependencies]` and `xtask` name this crate, so a build of `lightwell-app` never
//! compiles it. `lightwell-core`'s own unit tests reach it through a dev-dependency on a crate that
//! depends on `lightwell-core`, which Cargo allows, but a `lightwell-core` type is a different type
//! in those tests: the server and the proof endpoint therefore take and return none, and are what
//! the core's unit tests use. The helpers that speak core types ([`client`]) serve the core's
//! integration tests, other crates' tests and xtask, where there is one `lightwell-core`.
pub mod client;
mod proof;
mod server;

pub use proof::{ProofEndpoint, ProofRequest};
pub use server::{Options, Request, TestServer, respond, send};
