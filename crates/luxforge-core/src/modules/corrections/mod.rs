//! Corrections: ordered Clone and Heal operations in one repair layer after the source
//! development (`docs/design/corrections.md`). The module owns the operations' payload, their
//! validation, no-op detection, labels and answers, and the automatic source search's query; the
//! host owns the stroke store, the revision envelope, drafts and rendering.
mod actions;
mod geometry;
mod labels;
mod module;
mod payload;
mod query;
mod search;
mod stroke;
mod table;
#[cfg(test)]
mod tests;

pub(crate) use module::CorrectionsModule;
pub(crate) use stroke::RepairCarrier;

/// The repair layer's effect.
pub const CORRECTIONS_EFFECT: &str = "luxforge.corrections.repair";
