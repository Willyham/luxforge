//! The question a release build launched by a person asks when it runs outside Applications on
//! macOS ([design](../../../../docs/design/auto-update.md#moving-into-applications)).
//!
//! Once the workspace is usable, and when the `update_ask_to_move` preference allows, the running
//! bundle is classified off the update loop. Outside `/Applications` and `~/Applications` — or
//! translocated, or on a disk image or read-only volume — the question appears as a non-modal
//! notice once no sheet, dialog or other startup notice is open: Move and relaunch, Not now, and
//! Don't ask again, which Not now stores. Move copies, verifies and places the app off the loop
//! (`luxforge-install`); a Luxforge already there is replaced only after a confirmation. A
//! finished move hands the relaunch to the quit path ([`super::relaunch`]) and closes the window
//! the ordinary way, so the catalog closes before the app starts from its new place.
//!
//! An evidence run given `--evidence-move-to-applications` shows the question wherever it runs so
//! it can be captured, and never moves anything.
use super::{
    Before, Editor,
    message::{
        Message,
        move_to_applications::{MoveDone, MoveFailure, MoveMessage},
    },
    tasks::{self, owner_task},
};
use crate::state::{
    move_to_applications::{Asker, Phase, cleanup_name},
    preferences::PreferenceChange,
    usage::Prompt,
};
use iced::Task;
use luxforge_core::{ClientId, OwnerHandle};
use luxforge_install::{MoveError, Placement, Roots};
use serde_json::{Value, json};

/// After every message: once the workspace is usable — tool discovery has answered, and an
/// evidence run's opens have been captured — classify where the app runs, and show a pending
/// question once nothing delays it.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let question = &editor.move_question;
    if question.asker == Asker::Never {
        return Task::none();
    }
    let usable = editor.modules_ready
        && editor
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.step > 0);
    match question.phase {
        Phase::Waiting if usable => {
            let allowed = editor
                .preferences
                .applied()
                .is_none_or(|preferences| preferences.update_ask_to_move);
            if question.asker == Asker::Person && !allowed {
                editor.move_question.phase = Phase::NotAsked("dont-ask");
                editor.event("move_question_not_asked", || json!({"reason": "dont-ask"}));
                return Task::none();
            }
            editor.move_question.phase = Phase::Classifying;
            return owner_task(classify_running, |placement| {
                Message::MoveToApplications(MoveMessage::Classified(placement))
            });
        }
        Phase::Pending if !editor.move_question_delayed() => {
            editor.move_question.phase = Phase::Shown;
            let summary = editor.move_summary();
            editor.event("move_question_shown", || summary);
        }
        _ => {}
    }
    Task::none()
}

/// Where the running bundle is, or `None` outside one. Blocking: the translocation, volume and
/// disk lookups run off the update loop.
fn classify_running() -> Option<Placement> {
    luxforge_install::running_bundle().map(|bundle| luxforge_install::classify(&bundle))
}

impl Editor {
    /// One move question gesture or answer.
    pub(super) fn move_update(&mut self, message: MoveMessage) -> Task<Message> {
        if self.move_question.asker == Asker::Never {
            return Task::none();
        }
        match message {
            MoveMessage::Classified(placement) => {
                if self.move_question.phase != Phase::Classifying {
                    return Task::none();
                }
                let location = placement.as_ref().map(|placement| placement.location);
                self.move_question.placement = placement;
                self.move_question.phase = match (self.move_question.asker, location) {
                    // An evidence run asks wherever it runs, so the question can be captured.
                    (Asker::Evidence, _) => Phase::Pending,
                    (_, None) => Phase::NotAsked("not-a-bundle"),
                    (_, Some(location)) if location.in_applications() => {
                        Phase::NotAsked("in-applications")
                    }
                    _ => Phase::Pending,
                };
                let summary = self.move_summary();
                self.event("move_question_classified", || summary);
            }
            MoveMessage::Move => {
                if matches!(self.move_question.phase, Phase::Shown | Phase::Failed(_)) {
                    self.event("move_question_answered", || json!({"answer": "move"}));
                    return self.start_move(false);
                }
            }
            MoveMessage::Replace => {
                if matches!(self.move_question.phase, Phase::Confirming(_)) {
                    self.event("move_question_answered", || json!({"answer": "replace"}));
                    return self.start_move(true);
                }
            }
            MoveMessage::Cancel => {
                if matches!(self.move_question.phase, Phase::Confirming(_)) {
                    self.event("move_question_answered", || json!({"answer": "cancel"}));
                    self.move_question.phase = Phase::Shown;
                }
            }
            MoveMessage::NotNow => {
                if matches!(
                    self.move_question.phase,
                    Phase::Shown | Phase::Failed(_) | Phase::Confirming(_)
                ) {
                    self.move_question.phase = Phase::Declined;
                    let dont_ask = self.move_question.dont_ask;
                    self.event(
                        "move_question_answered",
                        || json!({"answer": "not-now", "dont_ask": dont_ask}),
                    );
                    if dont_ask {
                        return self.store_preferences(PreferenceChange {
                            update_ask_to_move: Some(false),
                            ..PreferenceChange::default()
                        });
                    }
                }
            }
            MoveMessage::DontAsk(on) => self.move_question.dont_ask = on,
            MoveMessage::Answered(answer) => return self.move_answered(answer),
        }
        Task::none()
    }

    /// Start a move off the update loop. An evidence run never moves: it logs the press.
    fn start_move(&mut self, replace: bool) -> Task<Message> {
        if self.evidence.is_some() {
            self.event(
                "move_to_applications_skipped",
                || json!({"reason": "evidence", "replace": replace}),
            );
            return Task::none();
        }
        let Some(placement) = self.move_question.placement.clone() else {
            self.move_question.phase =
                Phase::Failed("Luxforge is not running from an app bundle".into());
            return Task::none();
        };
        self.move_question.phase = Phase::Moving { replace };
        let roots = self.move_question.roots.clone();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || perform(&owner, client, &placement, &roots, replace),
            |answer| Message::MoveToApplications(MoveMessage::Answered(answer)),
        )
    }

    /// The move answered: relaunch through the quit path, ask before replacing, or say why not.
    fn move_answered(&mut self, answer: Result<MoveDone, MoveFailure>) -> Task<Message> {
        if !matches!(self.move_question.phase, Phase::Moving { .. }) {
            return Task::none();
        }
        match answer {
            Ok(MoveDone::Moved(moved)) => {
                self.move_question.phase = Phase::Relaunching;
                self.event("move_to_applications_moved", || {
                    json!({"app": moved.app, "replaced": moved.replaced, "user": moved.user,
                           "cleanup": cleanup_name(&moved.cleanup),
                           "quarantine_cleared": moved.quarantine_cleared})
                });
                let relaunch = luxforge_install::Relaunch::moved(&moved);
                if !super::relaunch::relaunch(relaunch) {
                    self.event(
                        "move_to_applications_relaunch_busy",
                        || json!({"app": moved.app}),
                    );
                }
                // The ordinary quit path, asked for as the update port asks for it: the window
                // closes as its close button closes it, the catalog closes, then the relaunch runs.
                super::relaunch::request_quit();
            }
            Ok(MoveDone::Requested) => {
                // The update port carries the move out and runs the quit path itself.
                self.move_question.phase = Phase::Relaunching;
                self.event("move_to_applications_requested", || json!({}));
            }
            Err(MoveFailure::Existing(path)) => {
                self.event("move_to_applications_existing", || json!({"path": path}));
                self.move_question.phase = Phase::Confirming(path);
            }
            Err(MoveFailure::Failed { reason, detail }) => {
                self.event(
                    "move_to_applications_failed",
                    || json!({"reason": reason, "detail": detail}),
                );
                self.move_question.phase = Phase::Failed(detail);
            }
        }
        Task::none()
    }

    /// What holds a pending question back: a sheet, dialog or menu that delays the usage notice,
    /// the Select introduction while it is up or due, and the usage notice itself until it is
    /// answered, so two startup notices never stack. A move relaunches the app, so the usage
    /// notice, claimed once, comes first.
    pub(crate) fn move_question_delayed(&self) -> bool {
        self.notice_delayed()
            || self.introduction_holds_notices()
            || (self.usage.eligible
                && matches!(
                    self.usage.prompt,
                    Prompt::Waiting | Prompt::Claiming | Prompt::Pending | Prompt::Shown
                ))
    }

    /// What a captured frame records of the question: who may be asked, where it is, where the
    /// app runs and what a move would do with the original, and the notice as drawn.
    pub(crate) fn move_summary(&self) -> Value {
        let question = &self.move_question;
        let placement = question.placement.as_ref();
        json!({
            "asker": question.asker.name(),
            "phase": question.phase.name(),
            "not_asked": match question.phase {
                Phase::NotAsked(reason) => Some(reason),
                _ => None,
            },
            "failure": match &question.phase {
                Phase::Failed(reason) => Some(reason.as_str()),
                _ => None,
            },
            "location": question.location(),
            "bundle": placement.map(|placement| &placement.bundle),
            "original": placement.map(|placement| &placement.original),
            "cleanup": placement.map(|placement| cleanup_name(&placement.cleanup)),
            "dont_ask": question.dont_ask,
            "notice": self.workspace.move_notice.as_ref().map(|notice| json!({
                "title": notice.title,
                "body": notice.body,
                "buttons": notice.actions.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
                "dont_ask": notice.dont_ask,
            })),
        })
    }
}

/// Move the app, blocking: off the update loop. `update.move-to-applications` performs the same
/// move once the macOS update port can (design, "Moving into Applications"): while the port says
/// `can_move_to_applications` the question goes through the command, and otherwise it moves
/// through `luxforge-install` itself.
fn perform(
    owner: &OwnerHandle,
    client: ClientId,
    placement: &Placement,
    roots: &Roots,
    replace: bool,
) -> Result<MoveDone, MoveFailure> {
    let through_command = tasks::call(owner, client, "update.status", json!({}))
        .ok()
        .and_then(|(status, _)| status["can_move_to_applications"].as_bool())
        .unwrap_or(false);
    if through_command {
        let params = json!({
            "mutation": {"request_id": tasks::request().request_id, "actor": tasks::ACTOR},
            "replace": replace,
        });
        return match tasks::call_detailed(owner, client, "update.move-to-applications", params) {
            Ok(_) => Ok(MoveDone::Requested),
            Err(error) => {
                let data = error.data.clone().unwrap_or(Value::Null);
                match (data["reason"].as_str(), data["path"].as_str()) {
                    (Some("existing-app"), Some(path)) => Err(MoveFailure::Existing(path.into())),
                    (reason, _) => Err(MoveFailure::Failed {
                        reason: reason.unwrap_or(&error.code).to_owned(),
                        detail: error.message,
                    }),
                }
            }
        };
    }
    luxforge_install::move_to_applications(placement, roots, replace, &luxforge_install::System)
        .map(MoveDone::Moved)
        .map_err(|error| match error {
            MoveError::Existing(path) => MoveFailure::Existing(path),
            error => MoveFailure::Failed {
                reason: error.reason().to_owned(),
                detail: error.to_string(),
            },
        })
}
