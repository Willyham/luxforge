//! The tool modules end to end, one module per tool: the colour mixer, Presence and the vignette
//! against their independent references through the real render paths, the developer controls
//! proof, and presets; and the one field-patch conformance suite (`conformance/`) every field-patch
//! module passes, which `cargo xtask editor-acceptance` also runs in release; and the one committed
//! snapshot of what the built-in registry publishes (`descriptors`).

mod conformance;
mod controls;
mod descriptors;
mod field_patch;
mod mixer;
mod presence;
mod presets;
mod vignette;

// `luxforge-testkit`'s core-typed helpers, compiled into this binary from their one source: the
// core cannot name that crate, which depends on it, without building itself a second time for every
// test build. The binary names itself `luxforge_testkit`, so a helper reads the same here as in
// every other crate's tests and in xtask, which compiles the conformance suite as well.
extern crate self as luxforge_testkit;
#[path = "../../../luxforge-testkit/src/client.rs"]
pub mod client;
#[path = "../../../luxforge-testkit/src/fixtures.rs"]
pub mod fixtures;
