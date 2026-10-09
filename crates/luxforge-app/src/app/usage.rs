//! Usage statistics on the desktop: the one startup notice and the Settings sheet's Privacy tab,
//! through the host's `usage.*` commands
//! ([design](../../../../docs/design/usage-statistics.md#consent-and-first-launch)).
//!
//! The desktop decides nothing about consent. In an eligible launch, once the workspace is usable,
//! it asks `usage.prompt.claim` once; only an answer of `show_prompt: true` shows the notice, and
//! only once no native dialog, module consent notice or Settings sheet is open. Share statistics
//! and the Privacy switch write through `usage.set`, one write at a time; No thanks and Escape send
//! nothing, since the claim already recorded that the notice was shown. The switch is redrawn from
//! the state each answer reports, and another client's change is read again through the event
//! sync, as a preference is. The three calls are spelled once, in [`calls`].
use super::{
    Before, Editor,
    message::{Message, usage::UsageMessage},
    outcome::Outcome,
    tasks::owner_task,
};
use crate::state::usage::{Prompt, SaveFailure};
use iced::Task;
use serde_json::{Value, json};

/// The desktop's three `usage.*` calls, the only place their names and bodies are spelled. Each
/// blocks its thread on the owner, so each runs in an [`owner_task`], never on the update loop.
pub(crate) mod calls {
    use crate::{
        app::tasks::{call, request},
        state::usage::{UsageRead, parse},
    };
    use luxforge_core::{ClientId, OwnerHandle};
    use serde_json::{Value, json};

    /// `usage.prompt.claim`: whether this launch shows the notice, and the consent it leaves.
    pub(crate) fn claim(
        owner: &OwnerHandle,
        client: ClientId,
    ) -> Result<(bool, UsageRead), String> {
        let (mut answer, _) = call(
            owner,
            client,
            "usage.prompt.claim",
            json!({"mutation": request()}),
        )?;
        let show = answer["show_prompt"].as_bool().unwrap_or(false);
        Ok((show, parse(answer["state"].take())?))
    }

    /// `usage.read`.
    pub(crate) fn read(owner: &OwnerHandle, client: ClientId) -> Result<UsageRead, String> {
        let (answer, _) = call(owner, client, "usage.read", json!({}))?;
        parse(answer)
    }

    /// The body of the `usage.set` that asks for `enabled`: an opt-in names the disclosure the
    /// person was shown; an opt-out names none, which the host refuses.
    pub(crate) fn set_params(enabled: bool, request_id: &str) -> Value {
        let mut params = json!({
            "enabled": enabled,
            "mutation": {"request_id": request_id, "actor": crate::app::tasks::ACTOR},
        });
        if enabled {
            params["disclosure_version"] = json!(luxforge_core::usage::DISCLOSURE_VERSION);
        }
        params
    }

    /// `usage.set`: the consent it leaves, and the request id its event carries.
    pub(crate) fn set(
        owner: &OwnerHandle,
        client: ClientId,
        enabled: bool,
    ) -> Result<(UsageRead, String), String> {
        let request_id = request().request_id;
        let (mut answer, _) = call(owner, client, "usage.set", set_params(enabled, &request_id))?;
        Ok((parse(answer["state"].take())?, request_id))
    }
}

/// After every message: in an eligible launch, claim the notice once the workspace is usable —
/// tool discovery has answered, so the first frame and the panels are already up, and an evidence
/// run's opens, which are its startup, have all been captured — and show a claimed notice once
/// nothing that delays it is open.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    if !editor.usage.eligible {
        return Task::none();
    }
    let usable = editor.modules_ready
        && editor
            .evidence
            .as_ref()
            .is_none_or(|evidence| evidence.step > 0);
    match editor.usage.prompt {
        Prompt::Waiting if usable => {
            editor.usage.prompt = Prompt::Claiming;
            let (owner, client) = (editor.owner.clone(), editor.client);
            return owner_task(
                move || calls::claim(&owner, client),
                |answer| Message::Usage(UsageMessage::Claimed(answer)),
            );
        }
        Prompt::Pending if editor.usage.may_present(editor.notice_delayed()) => {
            editor.usage.prompt = Prompt::Shown;
            editor.event("usage_notice_shown", || json!({}));
            editor.outcome(Outcome::UsageNotice);
        }
        _ => {}
    }
    Task::none()
}

impl Editor {
    /// One usage statistics message.
    pub(super) fn usage_update(&mut self, message: UsageMessage) -> Task<Message> {
        if !self.usage.eligible {
            return Task::none();
        }
        match message {
            UsageMessage::Claimed(answer) => {
                match answer {
                    Ok((show, read)) => {
                        self.usage.read = Some(read);
                        self.usage.read_error = None;
                        // A choice made in Settings while the claim was out already answered it.
                        if self.usage.prompt == Prompt::Claiming {
                            self.usage.prompt = if show {
                                Prompt::Pending
                            } else {
                                Prompt::NotShown
                            };
                        }
                    }
                    Err(reason) => {
                        self.event("usage_claim_failed", || json!({"reason": reason}));
                        if self.usage.prompt == Prompt::Claiming {
                            self.usage.prompt = Prompt::Failed(reason);
                        }
                    }
                }
                let prompt = self.usage.prompt.name();
                self.event("usage_claimed", || json!({"prompt": prompt}));
                // A notice that is not shown is settled now; one that is waits to be drawn.
                if self.usage.prompt != Prompt::Pending {
                    self.outcome(Outcome::UsageNotice);
                }
            }
            UsageMessage::Share => {
                if self.usage.prompt != Prompt::Shown {
                    return Task::none();
                }
                self.usage.prompt = Prompt::Shared;
                self.event("usage_notice_answered", || json!({"answer": "share"}));
                return self.usage_update(UsageMessage::Set(true));
            }
            UsageMessage::Decline => {
                if self.usage.prompt != Prompt::Shown {
                    return Task::none();
                }
                // The claim already recorded the notice as shown and statistics stay off, so
                // refusing writes nothing.
                self.usage.prompt = Prompt::Declined;
                self.event("usage_notice_answered", || json!({"answer": "decline"}));
            }
            UsageMessage::Read(answer) => {
                self.usage.reading = false;
                match answer {
                    Ok(read) => {
                        self.usage.read = Some(read);
                        self.usage.read_error = None;
                    }
                    Err(reason) => self.usage.read_error = Some(reason),
                }
                self.outcome(Outcome::UsageAnswered);
            }
            UsageMessage::Set(enabled) => {
                if self.usage.writing.is_some() {
                    self.usage.waiting = Some(enabled);
                    return Task::none();
                }
                return self.write_usage(enabled);
            }
            UsageMessage::Saved(answer) => {
                let asked = self.usage.writing.take().unwrap_or_default();
                let mut read = Task::none();
                match answer {
                    Ok((state, request)) => {
                        let effective = state.effective;
                        self.usage.read = Some(state);
                        self.usage.read_error = None;
                        self.usage.failure = None;
                        self.read_back(request);
                        self.event(
                            "usage_set",
                            || json!({"enabled": asked, "effective": effective}),
                        );
                        // A choice made in Settings before the notice appeared answers it.
                        if matches!(self.usage.prompt, Prompt::Claiming | Prompt::Pending) {
                            self.usage.prompt = Prompt::Superseded;
                        }
                    }
                    Err(reason) => {
                        let failure = SaveFailure {
                            enabled: asked,
                            reason,
                        };
                        self.status.text = failure.text();
                        self.event(
                            "usage_set_failed",
                            || json!({"enabled": asked, "reason": failure.reason}),
                        );
                        self.usage.failure = Some(failure);
                        // The write may have landed before its answer was lost: read the truth
                        // back, and let the event sync read its event like another client's.
                        self.resync();
                        read = self.read_usage();
                    }
                }
                let next = match self.usage.waiting.take() {
                    Some(enabled) => self.write_usage(enabled),
                    None => {
                        self.outcome(Outcome::UsageAnswered);
                        Task::none()
                    }
                };
                return Task::batch([read, next]);
            }
            UsageMessage::Details(open) => self.usage.details_open = open,
        }
        Task::none()
    }

    /// Send one `usage.set`. An opt-out shows at once; an opt-in shows once it is saved.
    fn write_usage(&mut self, enabled: bool) -> Task<Message> {
        self.usage.writing = Some(enabled);
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || calls::set(&owner, client, enabled),
            |answer| Message::Usage(UsageMessage::Saved(answer)),
        )
    }

    /// Read the consent again, unless a read is already out: it answers for this request too. An
    /// ineligible launch shows no consent, so it reads none.
    pub(crate) fn read_usage(&mut self) -> Task<Message> {
        if !self.usage.eligible || self.usage.reading {
            return Task::none();
        }
        self.usage.reading = true;
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || calls::read(&owner, client),
            |answer| Message::Usage(UsageMessage::Read(answer)),
        )
    }

    /// What delays a claimed notice: a native dialog, a module's consent notice or the Settings
    /// sheet is open.
    pub(crate) fn notice_delayed(&self) -> bool {
        self.view_state.picker_open
            || self.capabilities.consent.is_some()
            || self.settings.open.is_some()
    }

    /// What a captured frame records of usage statistics: eligibility, where the notice is and
    /// what it drew, the consent as last read (its stored choice and effective state, never an
    /// identifier), the Privacy tab as drawn and the writes outstanding.
    pub(crate) fn usage_summary(&self) -> Value {
        let usage = &self.usage;
        let read = usage.read.as_ref();
        let privacy = self.workspace.settings.privacy.as_ref().map(|privacy| {
            json!({"switch": privacy.on, "saving": privacy.saving, "loading": privacy.loading,
                   "note": privacy.note, "error": privacy.error,
                   "details_open": privacy.details_open})
        });
        json!({
            "eligible": usage.eligible,
            "prompt": usage.prompt.name(),
            "claim_failure": match &usage.prompt {
                Prompt::Failed(reason) => Some(reason),
                _ => None,
            },
            "notice": self.workspace.usage_notice.as_ref().map(|notice| json!({
                "title": notice.title, "body": notice.body, "buttons": notice.actions,
            })),
            "stored": read.map(|read| json!({
                "prompt_shown": read.prompt_shown, "enabled": read.enabled,
                "disclosure_version": read.disclosure_version,
            })),
            "effective": read.map(|read| read.effective),
            "reason": read.and_then(|read| read.reason.clone()),
            "destination": read.and_then(|read| read.destination.clone()),
            "read_error": usage.read_error,
            "privacy": privacy,
            "writes_outstanding": usage.writes_outstanding(),
        })
    }
}
