//! The `luxforge-mcp` adapter ([design](../../../../docs/design/mcp.md)): a Model Context Protocol
//! server on standard input and output that operates the running desktop's live session, or with
//! `--headless` a `luxforge-json` owner of its own. It holds no per-method knowledge; everything
//! it offers comes from the registry's own `schema.list`.
//!
//! - [`schema`] generates each method's input schema and request check from `schema.list`.
//! - [`protocol`] reads JSON-RPC lines and answers in the modern (2026-07-28) or legacy
//!   (2025-11-25, 2025-06-18) era.
//! - `serve` runs the reader thread and the one worker over a bounded queue.
//! - `adapter` answers discovery, the legacy handshake, subscriptions and the tool list, and
//!   hands tool calls to `tools`, whose profile chooses the tools offered.
//! - `owner` attaches to a catalog owner lazily and again after a lost connection, through a
//!   `Target` and `Connection` pair the live session implements.
//! - `headless` is the other pair: the adapter's own `luxforge-json` child, for `--headless`.
//! - `images` attaches the pictures the registry's markers name.
//! - [`command`] is the command line.
mod adapter;
pub mod command;
mod headless;
mod images;
mod owner;
pub mod protocol;
pub mod schema;
mod serve;
mod tools;
