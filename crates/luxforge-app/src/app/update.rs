//! Automatic updates on the desktop: the offer in the workspace, Settings › General's Updates group
//! and the palette's Check for Updates, through the host's `update.*` commands and
//! `preferences.set` ([design](../../../../docs/design/auto-update.md#the-desktop)).
//!
//! The desktop decides nothing about updates. It reads `update.status` at launch, whenever the
//! event sync sees an `update.*` or a preference event, when the Settings sheet opens and after
//! the Updates group's switches are saved; it shows what that status offers. Every button sends
//! one command — Check now and Check for Updates `update.check`, Install and relaunch
//! `update.install`, Skip this version `update.skip`, the two switches `preferences.set` — except
//! Release notes and a notify-only installation's Download, which open the release page the
//! status names through the checked HTTPS opener, and Later, which only hides the offer until the
//! next check outcome. The calls are spelled once, in [`calls`].
use super::{
    Before, Editor,
    message::{Message, update::UpdateMessage},
    outcome::Outcome,
    tasks::{self, owner_task},
};
use crate::state::{
    preferences::PreferenceChange,
    update::{NoticeButton, Refusal, check_outcome_text, notice, reason_text},
};
use iced::Task;
use serde_json::{Value, json};

/// The desktop's `update.*` calls, the only place their names and bodies are spelled. Each blocks
/// its thread on the owner, so each runs in an [`owner_task`], never on the update loop.
pub(crate) mod calls {
    use crate::{
        app::tasks::{CallError, call, call_detailed, call_own, call_own_detailed, request},
        state::update::{UpdateRead, parse},
    };
    use luxforge_core::{ClientId, OwnerHandle};
    use serde_json::{Value, json};

    /// `update.status`.
    pub(crate) fn status(owner: &OwnerHandle, client: ClientId) -> Result<UpdateRead, String> {
        let (answer, _) = call(owner, client, "update.status", json!({}))?;
        parse(answer)
    }

    /// `update.check`: held by the owner until the check ends, so it runs on the blocking pool and
    /// answers the status the check left.
    pub(crate) fn check(owner: &OwnerHandle, client: ClientId) -> Result<UpdateRead, CallError> {
        let answer = call_detailed(owner, client, "update.check", json!({}))?;
        parse(answer).map_err(unreadable)
    }

    /// The body of an `update.install`.
    pub(crate) fn install_params(request_id: &str) -> Value {
        json!({"mutation": {"request_id": request_id, "actor": crate::app::tasks::ACTOR}})
    }

    /// `update.install`: the status it left and the request id its event carries.
    pub(crate) fn install(
        owner: &OwnerHandle,
        client: ClientId,
    ) -> Result<(UpdateRead, String), CallError> {
        let params = install_params(&request().request_id);
        let (mut answer, id) = call_own_detailed(owner, client, "update.install", params)?;
        Ok((parse(answer["status"].take()).map_err(unreadable)?, id))
    }

    /// The body of an `update.skip` of `version`.
    pub(crate) fn skip_params(version: &str) -> Value {
        json!({"version": version})
    }

    /// `update.skip`: the status it left and the request id its event carries.
    pub(crate) fn skip(
        owner: &OwnerHandle,
        client: ClientId,
        version: &str,
    ) -> Result<(UpdateRead, String), String> {
        let (mut answer, id) = call_own(owner, client, "update.skip", skip_params(version))?;
        Ok((parse(answer["status"].take())?, id))
    }

    fn unreadable(message: String) -> CallError {
        CallError {
            code: "internal".into(),
            message,
            data: None,
            job_id: None,
        }
    }
}

/// After every message: record the offer when it appears, changes or goes, which an evidence step
/// waiting for it settles on.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let shown = notice(&editor.updates).map(|notice| {
        (
            notice.title,
            notice.lines,
            notice.error,
            notice.buttons.iter().map(|button| button.name()).collect(),
        )
    });
    if shown != editor.updates.shown {
        let offer = shown.as_ref().map(|(title, lines, error, buttons)| {
            json!({"title": title, "lines": lines, "error": error, "buttons": buttons})
        });
        editor.event("update_offer", || json!({ "offer": offer }));
        editor.updates.shown = shown;
        editor.outcome(Outcome::UpdateOffer);
    }
    Task::none()
}

impl Editor {
    /// One update message.
    pub(super) fn update_message(&mut self, message: UpdateMessage) -> Task<Message> {
        match message {
            UpdateMessage::Read(answer) => {
                self.updates.reading.answered();
                match answer {
                    Ok(status) => {
                        self.updates.status = Some(status);
                        self.updates.read_error = None;
                    }
                    Err(reason) => {
                        self.event("update_read_failed", || json!({"reason": reason}));
                        self.updates.read_error = Some(reason);
                    }
                }
                self.outcome(Outcome::UpdateAnswered);
                return self.start_update_read();
            }
            UpdateMessage::Check => {
                if self.updates.checking {
                    return Task::none();
                }
                self.updates.checking = true;
                self.updates.check_error = None;
                self.status.text = crate::state::update::CHECKING.into();
                self.event("update_check", || json!({}));
                let (owner, client) = (self.owner.clone(), self.client);
                return owner_task(
                    move || calls::check(&owner, client),
                    |answer| Message::Update(UpdateMessage::Checked(answer)),
                );
            }
            UpdateMessage::Checked(answer) => {
                self.updates.checking = false;
                match answer {
                    Ok(status) => {
                        self.status.text = check_outcome_text(&status);
                        let outcome = status.last_check.map(|check| check.outcome);
                        self.event(
                            "update_checked",
                            || json!({"outcome": outcome, "checking": status.checking}),
                        );
                        self.updates.status = Some(status);
                    }
                    Err(error) => {
                        let reason = match error.data.as_ref().and_then(|data| data.get("reason")) {
                            Some(Value::String(reason)) if error.code == "not-ready" => {
                                reason_text(Some(reason))
                            }
                            _ => error.to_string(),
                        };
                        self.status.text = format!("Could not check for updates: {reason}");
                        self.event(
                            "update_check_refused",
                            || json!({"code": error.code, "reason": reason}),
                        );
                        self.updates.check_error = Some(reason);
                    }
                }
                self.outcome(Outcome::UpdateAnswered);
            }
            UpdateMessage::Notice(button) => return self.update_notice_pressed(button),
            UpdateMessage::Installed(answer) => {
                self.updates.installing = false;
                match answer {
                    Ok((status, request)) => {
                        self.read_back(request);
                        let version = status
                            .available
                            .as_ref()
                            .map(|available| available.version.clone());
                        self.event("update_install_requested", || json!({"version": version}));
                        if let Some(version) = version {
                            self.status.text = format!("Installing Luxforge {version}\u{2026}");
                        }
                        self.updates.status = Some(status);
                    }
                    Err(error) => {
                        let refusal =
                            Refusal::from_call(&error.code, &error.message, error.data.as_ref());
                        self.status.text = refusal.text();
                        self.event(
                            "update_install_refused",
                            || json!({"code": error.code, "data": error.data, "text": refusal.text()}),
                        );
                        let version = self
                            .updates
                            .offered()
                            .map(|available| available.version.clone())
                            .unwrap_or_default();
                        self.updates.refusal = Some((version, refusal));
                    }
                }
                self.outcome(Outcome::UpdateAnswered);
            }
            UpdateMessage::Skipped(answer) => {
                let version = self.updates.skipping.take();
                match answer {
                    Ok((status, request)) => {
                        self.read_back(request);
                        self.updates.failure = None;
                        self.event("update_skipped", || json!({"version": version}));
                        self.updates.status = Some(status);
                    }
                    Err(reason) => {
                        self.status.text = format!("Could not skip this version: {reason}");
                        self.event("update_skip_failed", || json!({"reason": reason}));
                        self.updates.failure = Some(self.status.text.clone());
                        // The skip may have landed before its answer was lost: read the truth
                        // back.
                        self.resync();
                        let read = self.read_updates();
                        self.outcome(Outcome::UpdateAnswered);
                        return read;
                    }
                }
                self.outcome(Outcome::UpdateAnswered);
            }
            UpdateMessage::Opened(result) => {
                match result {
                    Ok(()) => {
                        self.updates.failure = None;
                        self.status.text = "Opened the release page in your browser".into();
                    }
                    Err(reason) => {
                        self.status.text = reason.clone();
                        self.updates.failure = Some(reason);
                    }
                }
                self.outcome(Outcome::UpdateAnswered);
            }
            UpdateMessage::SetChecks(on) => {
                self.event("update_checks_set", || json!({"on": on}));
                return self.store_preferences(PreferenceChange {
                    update_checks: Some(on),
                    ..PreferenceChange::default()
                });
            }
            UpdateMessage::SetInstallAutomatically(on) => {
                // The switch is disabled while checks are off, so a gesture there sends nothing.
                if !self.workspace.settings.updates.install_enabled {
                    return Task::none();
                }
                self.event("update_install_automatically_set", || json!({"on": on}));
                return self.store_preferences(PreferenceChange {
                    update_install_automatically: Some(on),
                    ..PreferenceChange::default()
                });
            }
        }
        Task::none()
    }

    /// One of the offer's buttons, pressed while the offer is on screen.
    fn update_notice_pressed(&mut self, button: NoticeButton) -> Task<Message> {
        let Some(offer) = notice(&self.updates) else {
            return Task::none();
        };
        if !offer.buttons.contains(&button) {
            return Task::none();
        }
        let version = self
            .updates
            .offered()
            .map(|available| available.version.clone())
            .unwrap_or_default();
        self.event(
            "update_notice_pressed",
            || json!({"button": button.name(), "version": version}),
        );
        match button {
            NoticeButton::ReleaseNotes | NoticeButton::Download => {
                let Some(url) = offer.url else {
                    return Task::none();
                };
                self.updates.opened = Some(url.clone());
                // Tests and evidence runs record the page; only a person's run opens a browser.
                if cfg!(test) || self.evidence.is_some() {
                    self.status.text = "Release page recorded; no browser is opened here".into();
                    self.outcome(Outcome::UpdateAnswered);
                    return Task::none();
                }
                self.status.text = "Opening the release page in your browser\u{2026}".into();
                tasks::owner_work(move || crate::browser::open(&url))
                    .map(|result| Message::Update(UpdateMessage::Opened(result)))
            }
            NoticeButton::Install => {
                if offer.busy {
                    return Task::none();
                }
                self.updates.installing = true;
                self.updates.refusal = None;
                let (owner, client) = (self.owner.clone(), self.client);
                owner_task(
                    move || calls::install(&owner, client),
                    |answer| Message::Update(UpdateMessage::Installed(answer)),
                )
            }
            NoticeButton::Later => {
                self.updates.later = Some(
                    self.updates
                        .status
                        .as_ref()
                        .and_then(|status| status.last_check),
                );
                self.outcome(Outcome::UpdateAnswered);
                Task::none()
            }
            NoticeButton::Skip => {
                if offer.busy {
                    return Task::none();
                }
                self.updates.skipping = Some(version.clone());
                self.updates.refusal = None;
                let (owner, client) = (self.owner.clone(), self.client);
                owner_task(
                    move || calls::skip(&owner, client, &version),
                    |answer| Message::Update(UpdateMessage::Skipped(answer)),
                )
            }
        }
    }

    /// Read the update status again: at launch, for an `update.*` or preference event, when the
    /// Settings sheet opens and after the Updates group's switches are saved. One read is in
    /// flight; a request that arrives meanwhile reads once more after it.
    pub(crate) fn read_updates(&mut self) -> Task<Message> {
        self.updates.reading.offer(());
        self.start_update_read()
    }

    fn start_update_read(&mut self) -> Task<Message> {
        if self.updates.reading.start().is_none() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || calls::status(&owner, client),
            |answer| Message::Update(UpdateMessage::Read(answer)),
        )
    }

    /// What a captured frame records of updates: the status as last read, what is in flight, the
    /// offer as drawn and the Updates group as drawn.
    pub(crate) fn update_summary(&self) -> Value {
        let updates = &self.updates;
        let group = &self.workspace.settings.updates;
        json!({
            "status": updates.status,
            "read_error": updates.read_error,
            "checking": updates.checking,
            "check_error": updates.check_error,
            "later": updates.later_hides(),
            "installing": updates.installing,
            "refusal": updates.refusal.as_ref().map(|(version, refusal)| {
                json!({"version": version, "text": refusal.text()})
            }),
            "skipping": updates.skipping,
            "opened": updates.opened,
            "notice": self.workspace.update_notice.as_ref().map(|notice| json!({
                "title": notice.title,
                "lines": notice.lines,
                "progress": notice.progress,
                "error": notice.error,
                "buttons": notice.buttons.iter().map(|button| button.label()).collect::<Vec<_>>(),
                "busy": notice.busy,
                "url": notice.url,
            })),
            "group": self.settings.open.map(|_| json!({
                "version": group.version,
                "unavailable": group.unavailable,
                "loading": group.loading,
                "checks": group.checks,
                "install_automatically": group.install_automatically,
                "install_enabled": group.install_enabled,
                "saving": group.checks_saving || group.install_saving,
                "checking": group.checking,
                "last_check": group.last_check,
                "error": group.error,
            })),
        })
    }
}
