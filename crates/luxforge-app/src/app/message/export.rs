//! Export.
use crate::{
    app::export::ExportChoice,
    app::tasks::CallError,
    state::export_sheet::{CardChoice, Field, Fit, NamingTarget, SheetPlan},
};
use luxforge_core::{Chroma, ExportFormat, ExportSettings, MetadataLevel, tiles::Depth};
use serde_json::Value;
use std::path::PathBuf;

/// Exporting: the Export menu, the Export sheet, Export again and Export with preset, and the
/// owner's answers. Handled in `app/export.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ExportMessage {
    /// The title bar's Export button: open its menu, and list the presets it offers.
    Menu,
    /// Open the Export sheet over the displayed entry (`Cmd+E`, Export…, the palette), sending
    /// `reference: true` when `reference` asks for the reference renderer's export.
    OpenSheet { reference: bool },
    /// One gesture on the open sheet.
    Sheet(SheetMessage),
    /// The sheet's plan for `settings` answered.
    Planned {
        settings: Box<ExportSettings>,
        result: Result<SheetPlan, String>,
    },
    /// Export again (`Shift+Cmd+E`): the last export's settings, at once.
    Again,
    /// Export with preset: that preset's own settings, at once.
    WithPreset(String),
    /// The plan answered for an export and `export.write` was sent: the choice it was sent with
    /// and what it answered, or why nothing was sent.
    Sent {
        choice: Option<Box<ExportChoice>>,
        result: Result<Value, CallError>,
    },
    /// Export anyway: send the export held for its stale patches again, acknowledging exactly them.
    Anyway,
    /// Cancel the export held for its stale patches.
    Dismiss,
    /// What the export's reader read of the job it names: its first record, a record that differs
    /// from the one sent before it, the job's end, or a read that failed. The reader sends
    /// nothing for a read that finds the job as the one before it did.
    Read {
        job_id: String,
        result: Result<Value, String>,
    },
}

/// One control of the Export sheet.
#[derive(Clone, Debug)]
pub(crate) enum SheetMessage {
    /// A preset card pressed: its settings.
    Preset(String),
    /// A preset card right-clicked, opening its menu; `None` closes it.
    CardMenu(Option<String>),
    /// A choice of a preset card's menu.
    Card(String, CardChoice),
    Format(ExportFormat),
    Fit(Fit),
    Text(Field, String),
    Quality(u8),
    Optimise(bool),
    Chroma(Chroma),
    Limit(bool),
    BitDepth(Depth),
    Metadata(MetadataLevel),
    /// Sharpen's medium, or `None` for no output sharpening.
    Sharpen(Option<luxforge_core::SharpenMedium>),
    /// Sharpen's amount.
    SharpenAmount(luxforge_core::SharpenAmount),
    /// Set print size, or remove it.
    Density(bool),
    /// Original's folder.
    OriginalFolder,
    /// Chosen folder, or Choose…: the native folder dialog, unless a folder was chosen before and
    /// `choose` is false.
    ChosenFolder {
        choose: bool,
    },
    /// The native folder dialog answered, or was dismissed.
    FolderChosen(Option<PathBuf>),
    /// Start naming a preset: Save as preset…, `+`, Rename… or Duplicate.
    Name(NamingTarget),
    NameText(String),
    NameSubmit,
    NameCancel,
    /// The footer's Update "Name"…: the selected preset of the person's own, with the current
    /// settings.
    Update,
    /// Cancel, Escape, the close button or a press on the scrim: close without exporting.
    Close,
    /// Export: write with the sheet's settings and close it.
    Export,
}
