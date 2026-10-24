//! The library side of the `luxforge-mcp` adapter ([design](../../../../docs/design/mcp.md)):
//! what the server binary reads from the live `schema.list` to offer and check its tools. It holds
//! no per-method knowledge; everything comes from the registry's own entries.
pub mod schema;
