//! `luxforge-ctl` across its process boundary, against a real catalog owner serving a live session
//! from this test process, as the desktop serves one: schema and state, edits recorded in history,
//! a conflict with a competing commit, and jobs followed to their end or left running. Narrow a run
//! with `cargo test -p luxforge-cli live_session_process`.

mod live_session_process;
