//! Evidence steps on long-running work (**catalog lane D**, TASK-024): a folder's first look
//! captured with its progress sheet, the sheet sent to the background, and the job cancelled from
//! its Performance row. Each gesture is the message its control sends, and each step is captured
//! once the model shows what it waits for, judged from the state the models are derived from, so
//! the frame after it draws exactly that.
use super::Settle;
use crate::app::{
    Editor,
    message::{Message, long_work::LongWorkMessage, select::SelectMessage},
};
use crate::state::{long_work, performance::LONG_JOB_MS};
use iced::Task;
use serde_json::json;
use std::path::PathBuf;

/// What a running long-work step still waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LongWorkWait {
    /// The view waiting on its first look shows the progress sheet, with the job's count.
    Sheet,
    /// The sheet is gone while the job runs on, in the status bar and as a Performance row that can
    /// be cancelled.
    Background { job_id: String },
    /// The job has ended on the board, which the Performance section's rows are drawn from too, and
    /// the view that waited on it has heard.
    Ended { job_id: String },
}

/// How far a wait has got.
enum Reached {
    Not,
    Yes,
    Failed(String),
}

impl Editor {
    /// Browse `path` as Browse a folder… does, and capture its first look's progress sheet.
    pub(super) fn first_look_step(&mut self, path: PathBuf) -> Task<Message> {
        if !self.select_shown() {
            return self.fail_step("Select is not shown");
        }
        let task = self.update(Message::Select(SelectMessage::FolderPicked(Some(path))));
        self.await_long_work(LongWorkWait::Sheet);
        task
    }

    /// Press the sheet's Continue in background.
    pub(super) fn background_step(&mut self) -> Task<Message> {
        let Some(sheet) = self.workspace.long_work.sheet.clone() else {
            return self.fail_step("no progress sheet is shown");
        };
        let task = self.update(Message::LongWork(LongWorkMessage::ContinueInBackground));
        self.await_long_work(LongWorkWait::Background {
            job_id: sheet.job_id,
        });
        task
    }

    /// Press Cancel on the Performance section's first row that can be stopped.
    pub(super) fn cancel_work_step(&mut self) -> Task<Message> {
        let Some((label, job_id)) = self.workspace.performance.jobs.iter().find_map(|row| {
            row.work
                .as_ref()
                .map(|work| (row.label.clone(), work.job_id.clone()))
        }) else {
            return self.fail_step("the Performance section lists no work to cancel");
        };
        self.note_step(json!({"cancel": {"label": label, "job_id": job_id}}));
        let task = self.update(Message::LongWork(LongWorkMessage::Cancel {
            job_id: job_id.clone(),
        }));
        self.await_long_work(LongWorkWait::Ended { job_id });
        task
    }

    fn await_long_work(&mut self, wait: LongWorkWait) {
        if let Some(evidence) = &mut self.evidence {
            evidence.long_work_wait = Some(wait);
        }
        self.await_step(Settle::LongWork);
        self.long_work_shown("step_sent");
    }

    /// Long-running work may show something new: settle a waiting step once it shows what the step
    /// waits for, or fail it once that can no longer happen.
    pub(super) fn long_work_shown(&mut self, by: &str) {
        let Some(wait) = self
            .evidence
            .as_ref()
            .filter(|evidence| evidence.awaiting == Some(Settle::LongWork))
            .and_then(|evidence| evidence.long_work_wait.clone())
        else {
            return;
        };
        let reached = self.long_work_reached(&wait);
        if matches!(reached, Reached::Not) {
            return;
        }
        if let Some(evidence) = &mut self.evidence {
            evidence.long_work_wait = None;
        }
        match reached {
            Reached::Failed(reason) => {
                let _ = self.fail_step(reason);
            }
            _ => self.settle_step(Settle::LongWork, by),
        }
    }

    fn long_work_reached(&self, wait: &LongWorkWait) -> Reached {
        let work = &self.long_work.state;
        let model = long_work::model(work, self.select_shown(), self.select.state.home.as_deref());
        // Long enough on the board to be the Performance section's row, which is drawn from the
        // same read as the status bar.
        let shown = |job_id: &str| {
            work.job(job_id)
                .is_some_and(|job| job.elapsed_ms >= LONG_JOB_MS)
        };
        match wait {
            LongWorkWait::Sheet => {
                if model
                    .sheet
                    .as_ref()
                    .is_some_and(|sheet| sheet.count.is_some())
                {
                    Reached::Yes
                } else if self.select.reading.is_none() {
                    Reached::Failed("the first look ended before its progress sheet showed".into())
                } else {
                    Reached::Not
                }
            }
            LongWorkWait::Background { job_id } => {
                if work.job(job_id).is_none() {
                    Reached::Failed("the job ended before it ran in the background".into())
                } else if model.sheet.is_none()
                    && model
                        .busiest
                        .as_ref()
                        .is_some_and(|busiest| &busiest.job_id == job_id)
                    && shown(job_id)
                {
                    Reached::Yes
                } else {
                    Reached::Not
                }
            }
            LongWorkWait::Ended { job_id } => {
                let ended = work.job(job_id).is_none()
                    && work.board.as_ref().is_some_and(|board| {
                        board
                            .recent
                            .iter()
                            .any(|job| job.entry.job_id.as_deref() == Some(job_id.as_str()))
                    });
                let heard = self.select.reading.is_none() && self.select_reads_quiet();
                if ended && heard {
                    Reached::Yes
                } else {
                    Reached::Not
                }
            }
        }
    }
}
