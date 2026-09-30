//! Long-running work's pieces of the screen, from `state/long_work.rs`'s model: the status bar's
//! busiest job and the in-view progress sheet. Both status bars and Select's centre call these.
//!
//! **Seam.** The long-running-work task settles their look and what they say; the widgets are
//! `luxforge_ui::status_job` and `progress_sheet` (the components board's long-running work).
use crate::{
    app::message::{Message, long_work::LongWorkMessage},
    state::long_work::LongWorkModel,
};
use iced::Element;
use luxforge_ui::{
    Icon, ProgressSheetModel, StatusJobModel, WorkProgress, progress_sheet, status_job,
};

/// The status bar's busiest job, which opens the Performance section; none while nothing runs.
pub(crate) fn busiest(model: &LongWorkModel) -> Option<Element<'_, Message>> {
    let job = model.busiest.as_ref()?;
    Some(status_job(
        &StatusJobModel {
            label: job.label.clone(),
            fraction: job.fraction,
            jobs: job.jobs as usize,
        },
        Message::LongWork(LongWorkMessage::OpenPerformance),
    ))
}

/// The progress sheet of a Select view with nothing to show yet; none otherwise.
pub(crate) fn sheet(model: &LongWorkModel) -> Option<Element<'_, Message>> {
    let sheet = model.sheet.as_ref()?;
    Some(progress_sheet(
        &ProgressSheetModel {
            icon: if sheet.card {
                Icon::Drive
            } else {
                Icon::Folder
            },
            title: sheet.title.clone(),
            note: sheet.note.clone(),
            progress: WorkProgress {
                count: sheet.count.clone(),
                fraction: sheet.fraction,
            },
            estimate: sheet.estimate.clone(),
        },
        Message::LongWork(LongWorkMessage::ContinueInBackground),
        Message::LongWork(LongWorkMessage::Cancel {
            job_id: sheet.job_id.clone(),
        }),
    ))
}
