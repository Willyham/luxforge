//! The theme library and the Settings sheet's Appearance tab.
use crate::app::themes::{ReadTheme, ThemeChange};
use crate::state::themes::ThemeList;
use std::path::PathBuf;

/// One Appearance-tab gesture, or an owner answer about the themes. Handled in `app/themes.rs`, so
/// the tab's rows, its menus, the palette's theme entries and an evidence script share one path.
#[derive(Clone, Debug)]
pub(crate) enum ThemeMessage {
    /// `theme.list` answered, with the event sequence it was read at.
    Listed(Result<(ThemeList, u64), String>),
    /// Choose a theme, by its id, through the desktop's preference writer: a row's click or a
    /// palette entry.
    Choose(String),
    /// `theme.read` answered for the theme the preferences chose, to be drawn: the theme, or why it
    /// cannot be shown.
    Read {
        id: String,
        result: Result<Box<ReadTheme>, String>,
    },
    /// Open the native file dialog for a Luxforge theme document.
    Import,
    /// The dialog closed, with a chosen file or nothing.
    ImportPicked(Option<PathBuf>),
    /// `theme.import` and the listing after it answered, or the file was refused before either.
    Imported(Result<Box<ThemeChange>, String>),
    /// Export one theme through the native save dialog, as a Luxforge theme document.
    Export(String),
    /// The export was written to the file of this name, or the dialog was cancelled.
    Exported(Result<Option<String>, String>),
    /// Copy one theme's whole import report as JSON.
    CopyReport(String),
    /// `theme.read` answered with the report's text.
    ReportRead(Result<String, String>),
    /// Delete one stored theme, by its id.
    Delete(String),
    /// `theme.delete` and the listing after it answered.
    Deleted(Result<Box<ThemeChange>, String>),
}
