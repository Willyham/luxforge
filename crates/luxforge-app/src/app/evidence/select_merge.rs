//! Evidence steps on HDR merges in Select: a bracket header's Merge HDR pressed
//! through the header's own message once the grid is scrolled to it, captured once the merge's
//! pending row shows the job's first progress; then the merge's end, captured once the view read
//! again holds its result, selected. Each step is judged from the state the grid is derived from,
//! so the frame after it draws exactly that, and the job's record is noted beside it.
use super::Settle;
use crate::app::{
    Editor,
    message::{Message, select::SelectMessage, select_merge::MergeMessage},
};
use crate::state::select::{Block, MomentAction};
use iced::Task;
use serde_json::json;

/// What a running merge step still waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MergeWait {
    /// The pending row shows the job's progress.
    Running,
    /// The merge has ended ready and the view read again holds its result, selected.
    Ended,
}

/// How far a wait has got.
enum Reached {
    Not,
    Yes,
    Failed(String),
}

impl Editor {
    /// Press Merge HDR in the header of the bracket holding view position
    /// `position`, as the header does: the grid's moment number is its place among the grid's
    /// moments, and the action's its place among the header's actions.
    pub(super) fn merge_step(&mut self, position: u32) -> Task<Message> {
        if !self.select_shown() {
            return self.fail_step("Select is not shown");
        }
        let state = &self.select.state;
        let Some(summary) = &state.summary else {
            return self.fail_step("Select holds no view");
        };
        let found = state
            .content
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Moment { index, actions, .. } => Some((*index, actions)),
                _ => None,
            })
            .enumerate()
            .find(|(_, (index, _))| {
                summary
                    .groups
                    .moments
                    .get(*index as usize)
                    .is_some_and(|moment| {
                        (moment.start..moment.start + moment.len).contains(&position)
                    })
            })
            .map(|(number, (_, actions))| {
                (
                    number,
                    actions
                        .iter()
                        .position(|action| *action == MomentAction::Merge),
                )
            });
        let (number, action) = match found {
            Some((number, Some(action))) => (number as u32, action as u32),
            Some(_) => {
                return self.fail_step(format!(
                    "the moment holding position {position} offers no Merge HDR"
                ));
            }
            None => return self.fail_step(format!("no moment holds position {position}")),
        };
        // Scrolled to the bracket first, as a person scrolls to the header they press.
        let scroll =
            self.select
                .layout
                .reveal(position, self.select.scroll, self.select.viewport.height);
        let scrolled = self.update(Message::Select(SelectMessage::Scrolled(scroll)));
        let task = self.update(Message::Select(SelectMessage::Merge(
            MergeMessage::Header {
                moment: number,
                action,
            },
        )));
        self.await_merge(MergeWait::Running);
        Task::batch([scrolled, task])
    }

    /// Wait for the merge this desktop started to end and its result to be shown.
    pub(super) fn merged_step(&mut self) -> Task<Message> {
        if !self.select_shown() {
            return self.fail_step("Select is not shown");
        }
        self.await_merge(MergeWait::Ended);
        Task::none()
    }

    fn await_merge(&mut self, wait: MergeWait) {
        if let Some(evidence) = &mut self.evidence {
            evidence.merge_wait = Some(wait);
        }
        self.await_step(Settle::Merge);
        self.merge_shown("step_sent");
    }

    /// The merge's state may have changed: settle a waiting step once Select shows what it waits
    /// for, or fail it once that can no longer happen.
    pub(super) fn merge_shown(&mut self, by: &str) {
        let Some(wait) = self
            .evidence
            .as_ref()
            .filter(|evidence| evidence.awaiting == Some(Settle::Merge))
            .and_then(|evidence| evidence.merge_wait.clone())
        else {
            return;
        };
        let reached = self.merge_reached(&wait);
        if matches!(reached, Reached::Not) {
            return;
        }
        if let Some(evidence) = &mut self.evidence {
            evidence.merge_wait = None;
        }
        self.note_step(json!({
            "merge_record": self.select.merge_record,
            "merge_sent": self.select.merge_sent,
        }));
        self.note_step(self.owner_merges());
        match reached {
            Reached::Failed(reason) => {
                let _ = self.fail_step(reason);
            }
            _ => self.settle_step(Settle::Merge, by),
        }
    }

    fn merge_reached(&self, wait: &MergeWait) -> Reached {
        let merge = &self.select.state.merge;
        if let Some(failed) = merge
            .pending
            .as_ref()
            .and_then(|pending| pending.failed.as_ref())
        {
            return Reached::Failed(format!("the merge failed: {failed}"));
        }
        let ended = merge.starting.is_none() && merge.pending.is_none();
        let status = self
            .select
            .merge_record
            .as_ref()
            .and_then(|record| record["status"].as_str().map(str::to_owned));
        match wait {
            MergeWait::Running => {
                if merge.pending.as_ref().is_some_and(|pending| {
                    pending.message.is_some() && self.pending_block().is_some()
                }) {
                    Reached::Yes
                } else if ended {
                    Reached::Failed(format!(
                        "the merge did not run to show its progress: {}",
                        self.status.text
                    ))
                } else {
                    Reached::Not
                }
            }
            MergeWait::Ended => {
                if !ended {
                    return Reached::Not;
                }
                if status.as_deref() != Some("ready") {
                    return Reached::Failed(format!(
                        "the merge ended {}: {}",
                        status.as_deref().unwrap_or("without a record"),
                        self.status.text
                    ));
                }
                // Its cell's preview decoded too, so the frame draws the result's picture.
                if self.select.held_click.is_none()
                    && !self.select.state.loading
                    && self.select_quiet()
                {
                    Reached::Yes
                } else {
                    Reached::Not
                }
            }
        }
    }
}
