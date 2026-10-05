//! Import Omarchy theme…: a chosen folder read as one Omarchy theme or a set of them, and each
//! theme imported through `theme.import` with its own report
//! ([design](../../../../docs/design/ui-themes.md#importing-a-folder)).
//!
//! A folder that holds a file the reader takes — `colors.toml`, `alacritty.toml` or `light.mode` —
//! is one theme. Otherwise each immediate subfolder that holds one is a theme of a set, and a
//! folder of more than [`MAX_SET`] subfolders is refused by name before any of them is looked
//! into. Only those three files are read, each a regular file of at most 64 KiB read through a
//! bounded reader, and `alacritty.toml` only where there is no `colors.toml`, since the reader
//! takes it only then; nothing else a theme ships is read or run. A file that cannot be read fails
//! its own theme with the reason, never the set. Everything here runs on a task, off the update
//! loop ([performance rule 12](../../../../docs/engineering/performance-rules.md#rules)).
//!
//! A theme is named after its folder by the core's own rule; a name the library already holds is a
//! `conflict`, listed as one, never renamed or replaced.
use super::{
    tasks::{call_own_detailed, request},
    themes::{list_now, read_theme_file},
};
use crate::state::themes::{FolderOutcome, FolderReport, FolderTheme, ThemeList};
use luxforge_core::{
    ErrorKind,
    theme::{
        built_in_themes,
        omarchy::{ALACRITTY_TOML, COLORS_TOML, FILES, LIGHT_MODE, theme_name},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The most subfolders a set may have.
pub(crate) const MAX_SET: usize = 256;

/// What a chosen folder is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Folder {
    /// The folder is one theme.
    Theme(PathBuf),
    /// Each of these subfolders is one theme, in name order.
    Set(Vec<PathBuf>),
}

impl Folder {
    pub(crate) fn themes(self) -> Vec<PathBuf> {
        match self {
            Self::Theme(folder) => vec![folder],
            Self::Set(folders) => folders,
        }
    }
}

/// A folder's own name, for the status line and as `theme.import`'s `folder`.
pub(crate) fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string())
}

/// `folder` holds, as a regular file, one of the files the reader takes.
fn holds_theme(folder: &Path) -> bool {
    FILES.iter().any(|file| folder.join(file).is_file())
}

/// Classify the chosen `folder`: one theme, or a set of the subfolders that hold one. A folder of
/// more than [`MAX_SET`] subfolders, or one where neither it nor any subfolder holds a theme, is
/// refused by name.
pub(crate) fn classify(folder: &Path) -> Result<Folder, String> {
    let name = folder_name(folder);
    if holds_theme(folder) {
        return Ok(Folder::Theme(folder.to_path_buf()));
    }
    let unreadable = |error: std::io::Error| {
        format!(
            "{}: cannot read the folder {name}: {error}",
            ErrorKind::FileAccess.code()
        )
    };
    let mut subfolders = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(unreadable)? {
        let path = entry.map_err(unreadable)?.path();
        if !path.is_dir() {
            continue;
        }
        if subfolders.len() == MAX_SET {
            return Err(format!(
                "{}: the folder {name} holds more than {MAX_SET} folders; a set of Omarchy \
                 themes is at most {MAX_SET}",
                ErrorKind::ResourceLimit.code()
            ));
        }
        subfolders.push(path);
    }
    subfolders.retain(|path| holds_theme(path));
    subfolders.sort();
    if subfolders.is_empty() {
        return Err(format!(
            "{}: the folder {name} is not an Omarchy theme: neither it nor any folder in it \
             holds {COLORS_TOML}, {ALACRITTY_TOML} or {LIGHT_MODE}",
            ErrorKind::UnsupportedInput.code()
        ));
    }
    Ok(Folder::Set(subfolders))
}

/// The text of the files the reader takes from one theme folder, by file name: `colors.toml`, or
/// `alacritty.toml` where there is none, and the `light.mode` marker. Each is a regular file read
/// within the bound; the first that cannot be read fails the theme with its reason.
pub(crate) fn read_files(theme: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut files = BTreeMap::new();
    for file in [COLORS_TOML, ALACRITTY_TOML, LIGHT_MODE] {
        if file == ALACRITTY_TOML && files.contains_key(COLORS_TOML) {
            continue;
        }
        let path = theme.join(file);
        // A file that is not regular, such as a pipe that would block the read, is not the
        // theme's; nor is one that is not there.
        if !path.is_file() {
            continue;
        }
        files.insert(file.to_owned(), read_theme_file(&path)?);
    }
    if files.is_empty() {
        return Err(format!(
            "{}: {} holds no {COLORS_TOML}, {ALACRITTY_TOML} or {LIGHT_MODE}",
            ErrorKind::UnsupportedInput.code(),
            folder_name(theme)
        ));
    }
    Ok(files)
}

/// `theme.import` for one Omarchy theme folder's files, named after the folder.
pub(crate) fn import_params(files: &BTreeMap<String, String>, folder: &str) -> Value {
    json!({"format": "omarchy", "files": files, "folder": folder, "mutation": request()})
}

/// Whether the name the core gives the theme in `folder` is a built-in theme's, ignoring case as
/// the library's names do: a conflict with it is "already built in", any other "already imported".
fn built_in(folder: &str) -> bool {
    let Some(name) = theme_name(folder) else {
        return false;
    };
    let name = name.to_lowercase();
    built_in_themes()
        .iter()
        .any(|theme| theme.name.to_lowercase() == name)
}

/// One Import Omarchy theme… and the listing after it.
#[derive(Clone, Debug)]
pub(crate) struct FolderImport {
    pub(crate) report: FolderReport,
    /// Each theme's `theme.import` answer — its record and report — or why it was not imported,
    /// in the report's order: what Copy in the status bar copies and an evidence step records.
    pub(crate) answers: Vec<Value>,
    /// The library after the last import, and the event sequence it was read at; or why it could
    /// not be listed, when the next listing will show it.
    pub(crate) list: Result<(ThemeList, u64), String>,
    /// The request ids of the imports that stored a theme, whose events the listing reflects.
    pub(crate) requests: Vec<String>,
}

/// Import every theme `folder` holds, one `theme.import` each, then list the library. Only a folder
/// that is neither a theme nor a set is refused as a whole.
pub(crate) fn import_folder_now(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    folder: &Path,
) -> Result<FolderImport, String> {
    let themes = classify(folder)?.themes();
    let mut report = FolderReport {
        chosen: folder_name(folder),
        themes: Vec::with_capacity(themes.len()),
    };
    let mut answers = Vec::with_capacity(themes.len());
    let mut requests = Vec::new();
    for theme in themes {
        let folder = folder_name(&theme);
        let imported = read_files(&theme)
            .map_err(|reason| (None, reason))
            .and_then(|files| {
                call_own_detailed(
                    owner,
                    client,
                    "theme.import",
                    import_params(&files, &folder),
                )
                .map_err(|error| (Some(error.code.clone()), error.to_string()))
            });
        let outcome = match imported {
            Ok((answer, request)) => {
                requests.push(request);
                let text = |name: &str| answer["theme"][name].as_str().unwrap_or_default();
                let outcome = FolderOutcome::Imported {
                    id: text("id").to_owned(),
                    name: text("name").to_owned(),
                };
                answers.push(json!({"folder": folder, "outcome": outcome.kind(),
                                    "theme": answer["theme"], "report": answer["report"]}));
                outcome
            }
            Err((code, reason)) => {
                let outcome = match (code.as_deref(), theme_name(&folder)) {
                    (Some(code), Some(name)) if code == ErrorKind::Conflict.code() => {
                        FolderOutcome::Conflict {
                            name,
                            built_in: built_in(&folder),
                        }
                    }
                    _ => FolderOutcome::Failed(reason.clone()),
                };
                answers
                    .push(json!({"folder": folder, "outcome": outcome.kind(), "reason": reason}));
                outcome
            }
        };
        report.themes.push(FolderTheme { folder, outcome });
    }
    Ok(FolderImport {
        report,
        answers,
        list: list_now(owner, client),
        requests,
    })
}

/// Where the folder dialog opens: on Linux, Omarchy's own theme folders, the person's installed
/// themes first, then Omarchy 4's built-in ones and Omarchy 3's, whichever exists first; elsewhere
/// wherever the system opens it.
pub(crate) fn picker_start() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    omarchy_folders(home.as_deref())
        .into_iter()
        .find(|folder| folder.is_dir())
}

/// Omarchy's theme folders, in the order the picker tries them.
pub(crate) fn omarchy_folders(home: Option<&Path>) -> Vec<PathBuf> {
    let mut folders = Vec::with_capacity(3);
    if let Some(home) = home {
        folders.push(home.join(".config/omarchy/themes"));
    }
    folders.push(PathBuf::from("/usr/share/omarchy/themes"));
    if let Some(home) = home {
        folders.push(home.join(".local/share/omarchy/themes"));
    }
    folders
}
