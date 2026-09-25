//! The mask test binaries still name the reference as `mod reference;`. The references themselves
//! live in the `lightwell-reference` crate, which cannot depend on the core; this re-export goes
//! when those binaries are merged into one and import the crate directly.
pub use lightwell_reference::*;
