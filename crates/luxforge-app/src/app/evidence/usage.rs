//! Evidence steps on usage statistics: the startup notice's claim waited for, its buttons pressed,
//! and the Privacy tab's switch and What is sent section. Each gesture is the message its control
//! sends. Only a launch with explicit usage statistics test eligibility shows either surface, so a
//! step in any other launch is refused and recorded.
use super::Settle;
use crate::{
    app::{
        Editor,
        message::{Message, usage::UsageMessage},
    },
    state::{settings::SettingsTab, usage::Prompt},
};
use iced::Task;
use luxforge_evidence::{NoticeAnswer, UsageStep};

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
        }
    }
}
