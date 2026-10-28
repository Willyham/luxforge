//! The authoring tooling, native builds only: [`build`] a module crate into its component,
//! [`Runner`] a component on the module engine, hold a module's native reference to its
//! component's with the [`harness`], and the [`fixtures`] both run over.
//!
//! [`Runner`] runs a component through `luxforge-wasm`, the engine the host renders with: its
//! Wasmtime configuration (relaxed SIMD and threads off, NaN canonicalization on, no imports), its
//! memory limit and its deadline.
mod build;
pub mod fixtures;
pub mod harness;
mod runner;

pub use build::{WASM_TARGET, build_component, build_core_module, componentize, toolchain_channel};
pub use runner::{Instance, Runner, entries};

/// What a host-side step failed with, said for a person: what it was doing and why.
pub type Error = String;
