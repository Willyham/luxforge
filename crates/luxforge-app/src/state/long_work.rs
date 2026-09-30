//! Long-running work as the desktop shows it ([catalog design](../../../../docs/design/catalog.md#long-running-work),
//! P18, TASK-024): the status bar's busiest job with a small bar and the number of jobs, which opens
//! the Performance section, and the in-view progress sheet of a Select view with nothing to show
//! yet. The Performance section's job rows, with progress, estimate and Cancel, are
//! `state/performance.rs`'s. Like every view model it names no framework type.
//!
//! **Seam.** This file, `app/long_work.rs` and `app/message/long_work.rs` are long-running work's own
//! modules. [`derive`] runs after every message into `Workspace::long_work`; both status bars (the
//! Develop workspace's `view/status_bar.rs` and Select's `view/select.rs`) draw
//! [`LongWorkModel::busiest`] when there is one, and Select's centre draws
//! [`LongWorkModel::sheet`] over a view that has nothing to show yet. They draw nothing yet: the
//! task derives them from the activity board it reads.
use super::Inputs;

/// What long-running work shows now.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LongWorkModel {
    /// The busiest job, for the status bar; none while nothing runs.
    pub(crate) busiest: Option<BusiestJob>,
    /// The in-view progress sheet: only for a Select view with nothing to show yet, such as the
    /// first look at a card or folder before its headers are read.
    pub(crate) sheet: Option<ProgressSheet>,
}

/// The status bar's job: what the busiest job is doing, how far it has got when its total is known,
/// and how many jobs run.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BusiestJob {
    pub(crate) label: String,
    /// `0.0..=1.0`, only for work that knows its total; none draws no fill and says "working".
    pub(crate) fraction: Option<f32>,
    pub(crate) jobs: u32,
}

/// The in-view progress sheet: the job it follows, its title and note, how far it has got and
/// the estimate once one is truthful.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ProgressSheet {
    pub(crate) job_id: String,
    /// A card is read (the drive glyph) rather than a folder.
    pub(crate) card: bool,
    pub(crate) title: String,
    pub(crate) note: String,
    /// "312 of 612 files", or none while the extent is unknown.
    pub(crate) count: Option<String>,
    pub(crate) fraction: Option<f32>,
    /// "about 4 s left", once the rate is steady.
    pub(crate) estimate: Option<String>,
}

/// Long-running work's model, derived from the inputs after every message.
pub(crate) fn derive(_: &Inputs<'_>) -> LongWorkModel {
    LongWorkModel::default()
}
