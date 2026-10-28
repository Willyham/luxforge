//! The authoring tooling, native builds only: [`build`] a module crate into its component,
//! [`Runner`] a component through Wasmtime, hold a module's native reference to its component's
//! with the [`harness`], and the [`fixtures`] both run over.
//!
//! TASK-006's `luxforge-wasm` owns the engine the host renders with. [`Runner`] configures its own
//! Wasmtime engine the same way (relaxed SIMD and threads off, NaN canonicalization on, no imports,
//! bounded memory) until it can use that crate's.
mod build;
pub mod fixtures;
pub mod harness;
mod runner;

pub use build::{WASM_TARGET, build_component, build_core_module, componentize, toolchain_channel};
pub use runner::{Instance, Runner};

/// What a host-side step failed with, said for a person: what it was doing and why.
pub type Error = String;
