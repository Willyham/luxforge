//! The Select introduction on the desktop
//! ([design](../../../../docs/design/select-introduction.md)).
//!
//! In a launch a person uses, the first time Select is shown while the `select_introduction_seen`
//! preference is off, the sheet is shown over Select once the workspace is usable and nothing that
//! delays it is open. Closing it in any way stores the preference through the desktop's one
//! preference writer, so it is not shown again; quitting with it open stores nothing. The title
//! bar's ? shows it again at any time without touching the preference. While it is open the
//! usage statistics notice's claim and the move into Applications question wait for it, and the
//! update offer is not drawn, so a first launch shows one thing at a time.
use super::{
    Before, Editor,
    message::{Message, select::SelectMessage, select_introduction::IntroductionMessage},
};
use crate::state::{
    preferences::PreferenceChange,
    select_introduction::{Closing, First},
};
use iced::Task;
use serde_json::{Value, json};

/// After every message: show the first showing once Select is shown, the workspace is usable — tool
/// discovery has answered, and an evidence run's opens have been captured — and nothing that
/// delays the sheet is open; or settle that it is not needed.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    if editor.introduction.first != First::Waiting
        || !editor.select_shown()
        || !editor.modules_ready
        || editor
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.step == 0)
        || editor.introduction_delayed()
    {
        return Task::none();
    }
    // An unreadable preferences file is no reason to withhold it: the person has not closed it
    // as far as this launch can tell.
    let seen = editor
        .preferences
        .applied()
        .is_some_and(|preferences| preferences.select_introduction_seen);
    if seen {
        editor.introduction.first = First::NotShown("seen");
        editor.event(
            "select_introduction_not_shown",
            || json!({"reason": "seen"}),
        );
        return Task::none();
    }
    editor.introduction.first = First::Shown;
    editor.select.state.menu = None;
    editor.event("select_introduction_shown", || json!({"reopened": false}));
    Task::none()
}

impl Editor {
    /// One Select introduction gesture.
    pub(super) fn introduction_update(&mut self, message: IntroductionMessage) -> Task<Message> {
        match message {
            IntroductionMessage::Reopen => {
                if self.introduction.open() || !self.select_shown() {
                    return Task::none();
                }
                self.introduction.reopened = true;
                self.select.state.menu = None;
                self.event("select_introduction_shown", || json!({"reopened": true}));
                Task::none()
            }
            IntroductionMessage::Close(how) => {
                if !self.introduction.open() {
                    return Task::none();
                }
                // The first showing, or a ? pressed before it came, is the one that stores the
                // preference; showing it again later changes nothing.
                let first = matches!(self.introduction.first, First::Waiting | First::Shown);
                if first {
                    self.introduction.first = First::Closed(how);
                }
                self.introduction.reopened = false;
                self.event(
                    "select_introduction_closed",
                    || json!({"how": how.name(), "stored": first}),
                );
                let store = if first {
                    self.store_preferences(PreferenceChange {
                        select_introduction_seen: Some(true),
                        ..PreferenceChange::default()
                    })
                } else {
                    Task::none()
                };
                let next = if how == Closing::AddFolder {
                    self.select_update(SelectMessage::AddFolder)
                } else {
                    Task::none()
                };
                Task::batch([store, next])
            }
        }
    }

    /// What delays the first showing: a native dialog, a module's consent notice or the Settings
    /// sheet, or one of Select's own sheets and confirmations.
    fn introduction_delayed(&self) -> bool {
        self.notice_delayed()
            || self.select.state.merge.sheet.is_some()
            || self.select.state.forget.is_some()
            || self.export.sheet.is_some()
            || self.develop.state.confirm.is_some()
            || self.view_state.copy_settings.confirm.is_some()
    }

    /// The startup notices wait while the Select introduction is up, or due now that Select is
    /// shown.
    pub(crate) fn introduction_holds_notices(&self) -> bool {
        self.introduction.holds_notices(self.select_shown())
    }

    /// What a captured frame records of the Select introduction: who it is shown to, where its
    /// first showing is and how it closed, whether the ? showed it again, and the sheet as drawn.
    pub(crate) fn introduction_summary(&self) -> Value {
        let introduction = &self.introduction;
        json!({
            "introducer": introduction.introducer.name(),
            "first": introduction.first.name(),
            "closed": match introduction.first {
                First::Closed(how) => Some(how.name()),
                _ => None,
            },
            "not_shown": match introduction.first {
                First::NotShown(reason) => Some(reason),
                _ => None,
            },
            "reopened": introduction.reopened,
            "sheet": self.workspace.select_introduction.as_ref().map(|sheet| json!({
                "title": sheet.title,
                "steps": sheet.steps.iter().map(|step| json!({
                    "name": step.name,
                    "keys": step.keys.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "buttons": sheet.buttons(),
                "stacked": sheet.stacked,
                "words": sheet.words(),
            })),
        })
    }
}
