//! Omarchy's current theme, which the active theme follows while the person lets it
//! ([design](../../../../docs/design/ui-themes.md#following-omarchy)).
//!
//! Omarchy records the theme it last applied by its folder's slug: Omarchy 4 in
//! `~/.local/state/omarchy/current/theme.name`, Omarchy 3.3 to 3.8 in
//! `~/.config/omarchy/current/theme.name`. Omarchy 3.0 to 3.2 wrote no name; its
//! `~/.config/omarchy/current/theme` is a link to the theme's folder, whose name is the slug. They
//! are tried in that order and the first that answers is Omarchy's current theme. Only that one
//! small file, or that one link, is read; nothing of Omarchy's is written, and nothing watches it:
//! the desktop reads it at launch and whenever its window gains focus, off the update loop.
use luxforge_core::theme::MAX_THEME_FOLDER;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

/// Where Omarchy is looked for: the person's home directory, on Linux, outside an evidence run and
/// a hidden launch, so no automated launch reads the person's Omarchy state. `None` never looks.
pub(crate) fn home(evidence: bool, hidden: bool) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") || evidence || hidden {
        return None;
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Omarchy's current theme's slug under `home`, or `None` where Omarchy records none, or records
/// one that is not a folder name.
pub(crate) fn current_slug(home: &Path) -> Option<String> {
    let named = [
        ".local/state/omarchy/current/theme.name",
        ".config/omarchy/current/theme.name",
    ];
    if let Some(slug) = named.iter().find_map(|file| read_name(&home.join(file))) {
        return Some(slug);
    }
    let linked = std::fs::read_link(home.join(".config/omarchy/current/theme")).ok()?;
    slug(&linked.file_name()?.to_string_lossy())
}

/// The slug a `theme.name` holds: a regular file of at most a folder name's length, beside its line
/// ending.
fn read_name(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_THEME_FOLDER as u64 + 2)
        .read_to_string(&mut text)
        .ok()?;
    slug(&text)
}

/// `text` as a slug: trimmed, non-empty, at most [`MAX_THEME_FOLDER`] bytes, and a folder's own
/// name, as Omarchy's `theme-set` refuses any other.
fn slug(text: &str) -> Option<String> {
    let slug = text.trim();
    let folder_name = !slug.is_empty()
        && slug.len() <= MAX_THEME_FOLDER
        && !slug.starts_with('.')
        && !slug.contains(['/', '\\'])
        && !slug.chars().any(char::is_control);
    folder_name.then(|| slug.to_owned())
}
