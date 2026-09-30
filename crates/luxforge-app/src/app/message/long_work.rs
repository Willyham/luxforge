//! Long-running work (TASK-024).

/// Long-running work's gestures and answers, handled in `app/long_work.rs`. The task adds its
/// variants: what it reads of the activity board, and each job's progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LongWorkMessage {
    /// The status bar's busiest job was pressed: open the Performance section.
    OpenPerformance,
    /// Cancel from a Performance row or the progress sheet: `job.cancel` for this job.
    Cancel { job_id: String },
    /// The progress sheet's Continue in background: the view waits without the sheet.
    ContinueInBackground,
}
