//! `luxforge-mcp` across its process boundary, against a real catalog owner serving a live session
//! from this test process: discovery and the legacy handshake, version refusal, the compact tools
//! with and without a session, edits in history under the agent's name, conflicts, held job waits,
//! pictures, drafts across calls, the queue's bound and cancellations, a lost connection, and no
//! or several sessions; the typed profiles and their list changes (`mcp_typed`); the `rmcp` SDK as
//! an independent client in both eras (`mcp_interop`); headless, over the `luxforge-json` child
//! built beside it (`headless`); and `config`'s printed client configuration (`config`). Every
//! message the server writes is held to the vendored schema of its era. Narrow a run with
//! `cargo test -p luxforge-cli --test mcp`, or `mcp_headless` for the headless tests.

mod config;
mod headless;
mod interop;
mod live;
mod process;
mod typed;
mod wire;
