//! The core-free base of the workspace's test support: the one [`Gate`] and the one
//! [`wait_until`] every test orders its steps by, the one [`Distribution`] every timing figure is
//! computed with, the one loopback [`TestServer`] (TLS only with the `tls` feature, which only the
//! transport's own tests in `luxforge-net` ask for), the capability proof's fake provider
//! [`ProofEndpoint`] built on it, the one rule for a host that forbids loopback listening
//! ([`loopback_forbidden`]), and the repository fixtures and unique scratch paths of
//! [`paths`].
//!
//! A test must not depend on how loaded the host is. It shares no mutable state with another test,
//! it orders its steps by a gate or a channel, never by sleeping for long enough, and a deadline
//! in it only bounds a hang ([`HANG`]); it never asserts how fast something happened outside the
//! timing tier. So:
//!
//! - To hold work where a test wants it — a render, a job, an answer from a test server — the work
//!   passes a [`Gate`] the test shut, and the test waits for it to be [`Gate::reached`] before it
//!   acts, then opens it.
//! - To wait for something a test cannot be signalled about, such as a job status read through
//!   the API, it polls through [`wait_until`] or [`wait_for`], which fail naming what never
//!   happened once [`HANG`] has passed.
//! - To report how long something took — in an ignored timing test or in one of `xtask`'s timing
//!   tools — it collects the samples into a [`Distribution`], whose nearest-rank p50 and p95 are
//!   the only percentile definition in the workspace.
//!
//! This crate depends on no workspace crate, so the core's own unit tests, the widget crate's,
//! the desktop's and every other crate's can use it, and a core that names it as a
//! dev-dependency is built once for its tests. `cargo xtask check-repository` refuses a second
//! gate, wait loop or percentile written anywhere else: extend this crate instead. The helpers that
//! speak core types are `luxforge-testkit`'s.

mod distribution;
mod gate;
mod loopback;
pub mod paths;
mod proof;
mod server;
mod wait;

pub use distribution::Distribution;
pub use gate::Gate;
pub use loopback::{NO_LOOPBACK, loopback_forbidden};
pub use proof::{ProofAnswer, ProofEndpoint, ProofProtocol, ProofRequest};
pub use server::{Options, Request, TestServer, respond, send};
pub use wait::{HANG, try_wait_for, wait_for, wait_until};
