//! Evidence steps on automatic updates: the offer waited for, Check now pressed and the offer's
//! buttons pressed. Each gesture is the message its control sends; what the update port finds is
//! the launch's own, which an evidence launch scripts with `--update-fixture`
//! ([`crate::app::update_fixture`]).
use super::Settle;
use crate::{
    app::{
        Editor,
        message::{Message, update::UpdateMessage},
    },
    state::update::{NoticeButton, notice},
};
use iced::Task;
use luxforge_evidence::UpdateStep;

impl Editor {
    pub(super) fn update_step(&mut self, step: UpdateStep) -> Task<Message> {
        match step {
            UpdateStep::Offer => {
                if self.workspace_update_offered() {
                    self.capture_next_frame();
                } else {
                    self.await_step(Settle::UpdateOffer);
                }
                Task::none()
            }
            UpdateStep::Check => {
                if self.updates.checking {
                    return self.fail_step("a check this window asked for is already running");
                }
                self.await_step(Settle::Update);
                self.update(Message::Update(UpdateMessage::Check))
            }
            UpdateStep::Notice(button) => {
                let Some(button) = NoticeButton::parse(button.as_str()) else {
                    return self.fail_step("the update offer has no such button");
                };
                let shown = notice(&self.updates)
                    .is_some_and(|offer| offer.buttons.contains(&button) && !offer.busy);
                if !shown {
                    return self.fail_step(format!(
                        "the update offer does not show {} now",
                        button.label()
                    ));
                }
                self.await_step(Settle::Update);
                self.update(Message::Update(UpdateMessage::Notice(button)))
            }
        }
    }

    /// The update offer is on screen.
    pub(super) fn workspace_update_offered(&self) -> bool {
        notice(&self.updates).is_some()
    }
}
