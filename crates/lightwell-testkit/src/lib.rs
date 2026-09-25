//! Fixtures shared by the workspace's tests and by `xtask`, kept out of every shipped crate: the one
//! loopback HTTP test server, and the capability proof's fake provider built on it.
//!
//! Only `[dev-dependencies]` and `xtask` name this crate, so a build of `lightwell-app` never
//! compiles it. `lightwell-core`'s own unit tests reach it through a dev-dependency on a crate that
//! depends on `lightwell-core`, which Cargo allows; that is why nothing here takes or returns a
//! `lightwell-core` type, which would be a different type in those tests. Constants such as the
//! proof palette are plain values and mean the same on both sides.
mod proof;
mod server;

pub use proof::{ProofEndpoint, ProofRequest};
pub use server::{Options, Request, TestServer, respond, send};
