//! Test support that speaks `luxforge-core` types, shared by the workspace's tests and by `xtask`,
//! kept out of every shipped crate: the in-process JSON client of the catalog owner ([`client`])
//! the field-patch conformance suite and xtask's acceptance chapters drive the owner with, a
//! `luxforge-json` process client ([`JsonProcess`]), the inputs tests build their checks from
//! ([`fixtures`]), and the capability proof's side of the exchange its fake provider serves
//! ([`proof_protocol`]). What needs no core type (the gate, the wait, the distribution, the loopback
//! test server, the proof endpoint and the fixture and scratch paths) is `luxforge-testbase`'s.
//!
//! Only `[dev-dependencies]` and `xtask` name this crate, so a build of `luxforge-app` never
//! compiles it. `luxforge-core` does not name it, since this crate depends on the core and a
//! dev-dependency on it would build the core a second time for every core test build: the core's
//! own unit tests use `luxforge-testbase`, and its integration tests compile [`client`] and
//! [`fixtures`] in from their one source through `#[path]` modules, naming the test binary itself
//! `luxforge_testkit` so these helpers and the conformance suite xtask shares read the same there.
pub mod client;
pub mod fixtures;
mod process;
mod proof;

pub use process::JsonProcess;
pub use proof::proof_protocol;
