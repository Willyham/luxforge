//! Long-running work.
use luxforge_core::activity::RecentActivity;
use serde_json::Value;

/// Long-running work's gestures and answers, handled in `app/long_work.rs`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LongWorkMessage {
    /// The status bar's busiest job was pressed: open the Performance section.
    OpenPerformance,
    /// Cancel from a Performance row or the progress sheet: `job.cancel` for this job.
    Cancel { job_id: String },
    /// The progress sheet's Continue in background: the view waits without the sheet.
    ContinueInBackground,
    /// The owner's activity board changed work this desktop follows: read it, now or once the
    /// throttle allows.
    Woken,
    /// A read is due: the throttle's delay after a wake has passed, or a job runs and the rules that
    /// follow its elapsed time — the display threshold, a stall — need a fresh read.
    Tick,
    /// `job.cancel` answered for this job.
    Cancelled {
        job_id: String,
        result: Result<(), String>,
    },
    /// `job.read` answered for a job that just ended, whose sentence the status bar says.
    Ended {
        job: Box<RecentActivity>,
        result: Result<Value, String>,
    },
}
