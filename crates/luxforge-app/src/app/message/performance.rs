//! The state panel's Performance section.
use crate::app::tasks::PerformanceRead;
use luxforge_core::JobId;

/// The state panel's Performance section. Handled in `app/performance.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PerformanceMessage {
    /// Open or close the section and store its next-launch default through the desktop's
    /// preference writer.
    Toggle,
    Cancel(JobId),
    Cancelled {
        job_id: JobId,
        result: Result<serde_json::Value, String>,
    },
    /// One tick of the section's sampler. It exists only while the section is expanded and the
    /// state panel is shown, which is also when the timer that produces it exists.
    Tick,
    /// `resources.read` and `activity.list` answered, with the sampling epoch that asked, so a read
    /// that was in flight when the section stopped or restarted sampling is dropped.
    Sampled {
        epoch: u64,
        result: Result<Box<PerformanceRead>, String>,
    },
}
