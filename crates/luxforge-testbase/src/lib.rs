//! The core-free base of the workspace's test support: the one [`Gate`] and the one
//! [`wait_until`] every test orders its steps by.
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
//!
//! This crate depends on no workspace crate, so the core's own unit tests, the widget crate's,
//! the desktop's and every other crate's can use it. `cargo xtask check-repository` refuses a
//! second gate or wait loop written in test code anywhere else: extend this crate instead.

mod gate;
mod wait;

pub use gate::Gate;
pub use wait::{HANG, try_wait_for, wait_for, wait_until};
