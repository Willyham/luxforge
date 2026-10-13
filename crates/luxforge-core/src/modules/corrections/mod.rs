//! Corrections: ordered Clone and Heal operations in one repair layer after the source
//! development (`docs/design/corrections.md`). The module owns the operations' payload, their
//! validation, no-op detection, labels and answers, and the automatic source search's query; the
//! host owns the stroke store, the revision envelope, drafts and rendering.
mod actions;
mod geometry;
mod input;
mod labels;
mod module;
#[cfg(test)]
pub(crate) mod patch_testing;
#[cfg(test)]
mod patch_tests;
mod payload;
#[cfg(any(test, feature = "qualification"))]
pub mod qualification;
mod query;
mod search;
mod stroke;
mod table;
#[cfg(test)]
mod tests;

pub use geometry::{SourceFit, fit_correction_source};
pub(crate) use input::{selection_prefix, source_identity};
pub use input::{RepairInput, StaleReason};
pub(crate) use module::CorrectionsModule;
pub use query::StaleCorrection;
pub(crate) use query::{new_top_input, repair_stage, stale_corrections};
pub(crate) use stroke::{CorrectionStroke, RADIUS_MAX, RADIUS_MIN, RepairCarrier};

/// The repair layer's effect.
pub const CORRECTIONS_EFFECT: &str = "luxforge.corrections.repair";
