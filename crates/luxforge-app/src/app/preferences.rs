//! The desktop's one preference writer: every `preferences.set` this desktop sends, whichever
//! control made the change, goes through [`Editor::store_preferences`]. One call is in flight; the
//! changes made meanwhile merge into one waiting change, the newer value of a field replacing the
//! older. A change shows at once, and the answer that lands confirms it or puts the stored value
//! back; a refusal says why in the status bar, and closing the window waits for the last write
//! ([design](../../../../docs/design/preferences.md#desktop-writes)).
//!
//! The preferences are read once at launch and again whenever another client's `preferences.set`
//! reaches the event sync, so what the desktop applies of them — today the mask overlay colour —
//! follows an agent's change at once.
use super::{
    Editor,
    message::{Message, preferences::PreferenceMessage, settings::SettingsMessage},
    outcome::Outcome,
    tasks::{call, call_own, owner_task},
};
use crate::state::preferences::{
    self as state, ANNOUNCED, GeneralPreference, GeneralValue, PreferenceChange, Preferences,
};
use iced::Task;
use luxforge_core::MaskOverlayColour;
use serde_json::json;
use std::path::{Path, PathBuf};

impl Editor {
    /// Store `change` through the writer. It shows at once wherever the preferences are read from
    /// [`Editor::preferences`], merged with any change already waiting, and is sent once nothing
    /// is in flight.
    pub(crate) fn store_preferences(&mut self, change: PreferenceChange) -> Task<Message> {
        self.preferences.offer(change);
        self.write_preferences()
    }

    /// Send the waiting change, once nothing is in flight.
    fn write_preferences(&mut self) -> Task<Message> {
        let Some(change) = self.preferences.start() else {
            return Task::none();
        };
        let params = change.params();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let (answer, request) = call_own(&owner, client, "preferences.set", params)?;
                Ok((state::parse(answer)?, request))
            },
            |result| Message::Preferences(PreferenceMessage::Saved(result)),
        )
    }

    /// One answer for the writer or for a read of the preferences.
    pub(super) fn preferences_update(&mut self, message: PreferenceMessage) -> Task<Message> {
        match message {
            PreferenceMessage::Saved(result) => self.preferences_saved(result),
            PreferenceMessage::Read(result) => {
                self.preferences.reading.answered();
                let applied = self.preferences_read(result);
                Task::batch([applied, self.start_preferences_read()])
            }
        }
    }

    /// The write in flight answered: adopt what it left, or say why it was refused and read back
    /// what is stored; then send the change waiting behind it, or close the window when it asked
    /// to close and this was the last.
    fn preferences_saved(
        &mut self,
        result: Result<(Preferences, String), String>,
    ) -> Task<Message> {
        let (answer, request) = match result {
            Ok((preferences, request)) => (Ok(preferences), Some(request)),
            Err(reason) => (Err(reason), None),
        };
        let refused = answer.as_ref().err().cloned();
        let change = self.preferences.answered(answer).unwrap_or_default();
        let mut read = Task::none();
        match refused {
            // Only a change the core announces has an event for the sync to skip; a remembered
            // field's request id would only crowd the bounded list of the desktop's own requests.
            None => {
                if let Some(request) = request.filter(|_| ANNOUNCED.iter().any(|f| change.sets(f)))
                {
                    self.read_back(request);
                }
            }
            Some(reason) => {
                // The write may have landed before its answer was lost: the poll reads its event
                // like another client's.
                self.resync();
                let fields = change.fields();
                self.status.text = format!("Could not save preferences: {reason}");
                self.event(
                    "preference_set_failed",
                    || json!({"fields": fields, "reason": reason}),
                );
                // The rows showed the change asked for; read back what is stored.
                read = self.read_preferences();
            }
        }
        if self.preferences.idle() {
            if self.preferences.closing {
                return self.close();
            }
            self.outcome(Outcome::PreferencesWritten);
        }
        Task::batch([read, self.write_preferences()])
    }

    /// Read the preferences again, for another client's change or to put back what a refused
    /// write showed. One read is in flight; a change that arrives meanwhile reads once more after
    /// it.
    pub(crate) fn read_preferences(&mut self) -> Task<Message> {
        self.preferences.reading.offer(());
        self.start_preferences_read()
    }

    fn start_preferences_read(&mut self) -> Task<Message> {
        if self.preferences.reading.start().is_none() {
            return Task::none();
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                call(&owner, client, "preferences.read", json!({}))
                    .and_then(|(answer, _)| state::parse(answer))
            },
            |result| Message::Preferences(PreferenceMessage::Read(result)),
        )
    }

    /// Take up a `preferences.read` answer. When the stored mask overlay colour changed — another
    /// client set it — this session draws the colour the desktop now shows.
    pub(crate) fn preferences_read(
        &mut self,
        answer: Result<Preferences, String>,
    ) -> Task<Message> {
        let colour = |editor: &Self| {
            editor
                .preferences
                .stored()
                .map(|preferences| preferences.mask_overlay_colour)
        };
        let before = colour(self);
        self.preferences.read(answer);
        if colour(self) == before {
            return Task::none();
        }
        match self.preferences.applied() {
            Some(preferences)
                if preferences.mask_overlay_colour
                    != self.session.workspace.mask_overlay_colour =>
            {
                self.set_mask_overlay(None, Some(preferences.mask_overlay_colour))
            }
            _ => Task::none(),
        }
    }

    /// The Masks panel's colour control and General's Mask overlay colour row: this session draws
    /// `colour` at once, through `workspace.set` as before, and the preference stores it for the
    /// next launch and every other client.
    pub(crate) fn choose_mask_overlay_colour(
        &mut self,
        colour: MaskOverlayColour,
    ) -> Task<Message> {
        let session = self.set_mask_overlay(None, Some(colour));
        let stored = self.store_preferences(PreferenceChange {
            mask_overlay_colour: Some(colour),
            ..PreferenceChange::default()
        });
        Task::batch([session, stored])
    }

    /// One gesture on a General row's control: the change it makes, stored through the writer,
    /// with whatever the row also does to this session.
    pub(crate) fn set_general(
        &mut self,
        row: GeneralPreference,
        value: GeneralValue,
    ) -> Task<Message> {
        if value == GeneralValue::ChooseFolder {
            return self.choose_catalog_folder();
        }
        let Some(change) = row.change(value) else {
            return Task::none();
        };
        match change.mask_overlay_colour {
            Some(colour) => self.choose_mask_overlay_colour(colour),
            None => self.store_preferences(change),
        }
    }

    /// The Catalog row's Choose Folder…: the native folder dialog, opened off the update loop as
    /// every file dialog is, starting in the open catalog's folder. An evidence run never opens
    /// one: its step names the folder, as the dialog's answer would.
    fn choose_catalog_folder(&mut self) -> Task<Message> {
        if self.view_state.picker_open || self.evidence.is_some() {
            return Task::none();
        }
        self.view_state.picker_open = true;
        let start = self
            .preferences
            .catalog
            .path
            .parent()
            .map(Path::to_path_buf);
        Task::perform(
            async move {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("Choose Catalog Folder");
                if let Some(start) = start {
                    dialog = dialog.set_directory(start);
                }
                dialog
                    .pick_folder()
                    .await
                    .map(|folder| folder.path().to_path_buf())
            },
            |folder| Message::Settings(SettingsMessage::CatalogFolder(folder)),
        )
    }

    /// The folder dialog answered: a chosen folder stores `<folder>/catalog.sqlite` for the next
    /// launch. Nothing moves the open catalog.
    pub(crate) fn catalog_folder_chosen(&mut self, folder: Option<PathBuf>) -> Task<Message> {
        self.view_state.picker_open = false;
        match folder {
            Some(folder) => {
                self.set_general(GeneralPreference::Catalog, GeneralValue::Folder(folder))
            }
            None => Task::none(),
        }
    }

    /// What a captured frame records of the preferences: what the desktop applies, which is the
    /// stored answer with the outstanding changes laid over it, what is stored, the changes in
    /// flight and waiting, and why the last read or write failed.
    pub(crate) fn preferences_summary(&self) -> serde_json::Value {
        json!({
            "applied": self.preferences.applied(),
            "stored": self.preferences.stored(),
            "writing": self.preferences.writing().map(PreferenceChange::params),
            "waiting": self.preferences.waiting().map(PreferenceChange::params),
            "error": self.preferences.error,
        })
    }
}
