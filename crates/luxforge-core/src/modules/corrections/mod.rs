//! Corrections: ordered Clone and Heal operations in one repair layer after the source
//! development (`docs/design/corrections.md`). The module owns the operations' payload, their
//! validation, no-op detection, labels and answers, and the automatic source search's query; the
//! host owns the stroke store, the revision envelope, drafts and rendering.
mod payload;
mod stroke;

pub(crate) use stroke::{CorrectionStroke, RepairCarrier};

/// The repair layer's effect.
pub const CORRECTIONS_EFFECT: &str = "luxforge.corrections.repair";
