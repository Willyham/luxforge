//! Test support shared by the workspace's tests and by `xtask`, kept out of every shipped crate:
//! the one loopback HTTP test server, the capability proof's fake provider built on it, the
//! in-process JSON client of the catalog owner ([`client`]) the field-patch conformance suite and
//! xtask's acceptance chapters drive the owner with, a `luxforge-json` process client
//! ([`JsonProcess`]) and the inputs tests build their checks from ([`fixtures`]).
//!
//! Only `[dev-dependencies]` and `xtask` name this crate, so a build of `luxforge-app` never
//! compiles it. `luxforge-core`'s own unit tests reach it through a dev-dependency on a crate that
//! depends on `luxforge-core`, which Cargo allows, but a `luxforge-core` type is a different type
//! in those tests: the server and the proof endpoint therefore take and return none, and are what
//! the core's unit tests use, with the plain scratch and fixture paths of [`fixtures`]. The helpers
//! that speak core types serve the core's integration tests, other crates' tests and xtask, where
//! there is one `luxforge-core`.
pub mod client;
pub mod fixtures;
mod process;
mod proof;
mod server;

pub use process::JsonProcess;
pub use proof::{ProofEndpoint, ProofRequest};
pub use server::{Options, Request, TestServer, respond, send};
