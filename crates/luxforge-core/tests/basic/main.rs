//! The Basic module end to end, one module per area: Exposure (the compiled unit against the
//! independent f64 corpus, mixed geometry and replacement order, placement before the geometry
//! tail), Temperature, Tint and the neutral picker, Tone, and Vibrance and Saturation. What Basic
//! shares with every field-patch module — discovery, neutral payloads, the field-patch rules,
//! drafts, no-ops, deduplication, resets, history, one layer per target, an unavailable provider
//! and reopen — is proved once by the field-patch conformance suite (`tests/modules/`).
//!
//! Numerical rule, from each design's numerical contract: a rendered code equals the f64
//! reference's code exactly, except where the reference's linear value sits within the contract's
//! band of the exact linear threshold between two codes, where one code of difference is permitted
//! because production evaluates in f32 ([`luxforge_testkit::fixtures::assert_code_near_threshold`]).
//! Identity stacks, byte sharing and history behaviour are exact with no tolerance at all.

mod colour;
mod exposure;
mod tone;
mod white_balance;

use luxforge_core::{BASIC_EFFECT, Layer, Mutation};
use luxforge_testkit::fixtures;
use serde_json::Value;

/// A global Basic layer holding `payload`.
fn basic_layer(payload: Value) -> Layer {
    fixtures::layer(BASIC_EFFECT, payload)
}

/// The mutation envelope of an in-process edit these tests make.
fn mutation(revision: u64, request: &str) -> Mutation {
    fixtures::mutation(revision, request, "basic-test")
}
