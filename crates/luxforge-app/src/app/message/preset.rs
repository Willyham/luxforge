//! The Presets section.
use crate::app::tasks::PresetChange;
use luxforge_core::PresetSummary;
use std::path::PathBuf;

/// Every change to the Presets section is one message, so a script drives the library through the
/// update function exactly as the section's buttons, fields and menus do. Applying a preset is not
/// one of them: a row's click is [`ActionMessage::Run`](super::action::ActionMessage::Run) with the
/// section's own action, the same path every other declared action takes.
#[derive(Clone, Debug)]
pub(crate) enum PresetMessage {
    /// `preset.list` answered, with the event sequence it was read at.
    Listed(Result<(Vec<PresetSummary>, u64), String>),
    /// Show or hide the create form the `+` button reveals.
    ToggleForm,
    /// The create form's name text.
    Name(String),
    /// The create form's group text.
    Group(String),
    /// One create-form checkbox, by its settings-group identity.
    Check { id: String, checked: bool },
    /// Capture the checked groups from the displayed entry and store them as a new preset.
    Create,
    /// Check or clear one declared analysis step, by identity, which recomputes its fields on
    /// every photo the preset is applied to.
    Analysis { id: String, checked: bool },
    /// Close the create form and forget what was typed.
    Cancel,
    /// `preset.capture`, `preset.create` and the listing after them answered.
    Created(Result<Box<PresetChange>, String>),
    /// Open the native file dialog for a preset file.
    Import,
    /// The dialog closed, with a chosen file or nothing.
    ImportPicked(Option<PathBuf>),
    /// `preset.import` and the listing after it answered, or the file was refused before either.
    Imported(Result<Box<PresetChange>, String>),
    /// Delete one library preset, by its identity.
    Delete(String),
    /// `preset.delete` and the listing after it answered.
    Deleted(Result<Box<PresetChange>, String>),
    /// Copy one imported preset's whole import report as JSON.
    CopyReport(String),
    /// `preset.read` answered with the report's text.
    ReportRead(Result<String, String>),
    /// Export one library preset through the native save dialog.
    Export(String),
    /// The export was written to the file of this name, or the dialog was cancelled.
    Exported(Result<Option<String>, String>),
}
