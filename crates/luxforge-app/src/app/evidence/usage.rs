//! Evidence steps on usage statistics: the startup notice's claim waited for, its buttons pressed,
//! and the Privacy tab's switch and What is sent section. Each gesture is the message its control
//! sends. Only a launch with explicit usage statistics test eligibility shows either surface, so a
//! step in any other launch is refused and recorded.
//!
//! Two steps follow delivery in such a launch: `advance` moves the statistics worker's evidence
//! clock forward ([`usage_supervisor::advance`]), and `await` reads `usage.read` again each
//! evidence tick until its conditions hold, which is how a script waits for an outside client
//! (`luxforge-ctl` on the live session) or the receiver.
use super::Settle;
use crate::app::usage_supervisor;
use crate::{
    app::{
        Editor,
        message::{Message, usage::UsageMessage},
    },
    state::{settings::SettingsTab, usage::Prompt},
};
use iced::Task;
use luxforge_evidence::{MAX_USAGE_AWAIT_MS, NoticeAnswer, UsageAwait, UsageStep};
use serde_json::json;
use std::time::{Duration, Instant};

/// A running usage `await` step.
#[derive(Debug)]
pub(crate) struct UsageAwaiting {
    wait: UsageAwait,
    deadline: Instant,
    /// The open photograph's revision when the step began, for `edited`.
    revision: Option<u64>,
}

impl Editor {
    pub(super) fn usage_step(&mut self, step: UsageStep) -> Task<Message> {
        if !self.usage.eligible {
            return self.fail_step(
                "this launch shows no usage statistics: it was started without test eligibility",
            );
        }
        match step {
            UsageStep::Notice => {
                if matches!(
                    self.usage.prompt,
                    Prompt::Waiting | Prompt::Claiming | Prompt::Pending
                ) {
                    self.await_step(Settle::UsageNotice);
                } else {
                    self.capture_next_frame();
                }
                Task::none()
            }
            UsageStep::Answer(answer) => {
                if self.workspace.usage_notice.is_none() {
                    return self.fail_step("the usage statistics notice is not on screen");
                }
                match answer {
                    NoticeAnswer::Share => {
                        self.await_step(Settle::Usage);
                        self.update(Message::Usage(UsageMessage::Share))
                    }
                    NoticeAnswer::Decline => {
                        self.capture_next_frame();
                        self.update(Message::Usage(UsageMessage::Decline))
                    }
                }
            }
            UsageStep::Share(on) => {
                if self.settings.open != Some(SettingsTab::Privacy) {
                    return self.fail_step("the Settings sheet is not open at Privacy");
                }
                self.await_step(Settle::Usage);
                self.update(Message::Usage(UsageMessage::Set(on)))
            }
            UsageStep::Details(open) => {
                if self.settings.open != Some(SettingsTab::Privacy) {
                    return self.fail_step("the Settings sheet is not open at Privacy");
                }
                self.capture_next_frame();
                self.update(Message::Usage(UsageMessage::Details(open)))
            }
            UsageStep::Advance(seconds) => {
                if !usage_supervisor::advance(Duration::from_secs(seconds)) {
                    return self.fail_step(
                        "this launch has no statistics evidence clock: it was started without a \
                         test receiver",
                    );
                }
                self.event("usage_clock_advanced", || json!({"seconds": seconds}));
                self.capture_next_frame();
                Task::none()
            }
            UsageStep::Await(wait) => {
                let timeout = wait.timeout_ms.unwrap_or(MAX_USAGE_AWAIT_MS);
                let revision = self.document.state.as_ref().map(|state| state.revision);
                if let Some(evidence) = &mut self.evidence {
                    evidence.usage_await = Some(UsageAwaiting {
                        wait,
                        deadline: Instant::now() + Duration::from_millis(timeout),
                        revision,
                    });
                }
                self.usage_await_tick()
            }
        }
    }

    /// The evidence tick: check a running `await` step, reading `usage.read` again when its
    /// conditions need it and no read is out.
    pub(in crate::app) fn usage_await_tick(&mut self) -> Task<Message> {
        let Some(awaiting) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.usage_await.as_ref())
        else {
            return Task::none();
        };
        if Instant::now() >= awaiting.deadline {
            let wait = awaiting.wait.clone();
            self.end_usage_await();
            return self.fail_step(format!("usage await timed out waiting for {wait:?}"));
        }
        let edited = !awaiting.wait.edited
            || self
                .document
                .state
                .as_ref()
                .is_some_and(|state| Some(state.revision) > awaiting.revision);
        if !edited {
            return Task::none();
        }
        let reads = awaiting.wait.attempts.is_some()
            || awaiting.wait.accepted.is_some()
            || awaiting.wait.effective.is_some();
        if !reads {
            self.usage_await_met();
            return Task::none();
        }
        self.read_usage()
    }

    /// A `usage.read` answered: a running `await` step whose conditions now hold captures.
    pub(crate) fn usage_await_answered(&mut self) {
        let Some(awaiting) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.usage_await.as_ref())
        else {
            return;
        };
        let Some(read) = &self.usage.read else {
            return;
        };
        let wait = &awaiting.wait;
        let counts = read.diagnostics.clone().unwrap_or_default();
        let edited = !wait.edited
            || self
                .document
                .state
                .as_ref()
                .is_some_and(|state| Some(state.revision) > awaiting.revision);
        if edited
            && wait
                .attempts
                .is_none_or(|attempts| counts.attempts >= attempts)
            && wait
                .accepted
                .is_none_or(|accepted| counts.accepted >= accepted)
            && wait
                .effective
                .is_none_or(|effective| read.effective == effective)
        {
            self.usage_await_met();
        }
    }

    fn usage_await_met(&mut self) {
        let diagnostics = self
            .usage
            .read
            .as_ref()
            .and_then(|read| read.diagnostics.clone());
        self.end_usage_await();
        self.note_step(json!({"usage_diagnostics": diagnostics}));
        self.capture_next_frame();
    }

    fn end_usage_await(&mut self) {
        if let Some(evidence) = &mut self.evidence {
            evidence.usage_await = None;
        }
    }
}
