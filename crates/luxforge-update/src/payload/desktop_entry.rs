//! Refreshing a person's desktop entry after an update. A user-level entry,
//! `~/.local/share/applications/luxforge.desktop`, whose `Exec` program lies inside the payload is
//! rewritten for the payload now installed: `Exec` and `TryExec` name its executable, and
//! `X-Luxforge-Version` and `X-Luxforge-Build` its build, so the desktop environment reads the
//! entry, and the icon it names, again. An entry that points anywhere else is left alone.
use super::BuildInfo;
use crate::error::{Error, Result};
use std::{
    ffi::OsStr,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

/// The entry's file name in the applications directory.
pub const DESKTOP_ENTRY_FILE: &str = "luxforge.desktop";
/// The largest entry read.
const MAX_ENTRY_BYTES: u64 = 64 * 1024;
const GROUP: &str = "[Desktop Entry]";
const VERSION_KEY: &str = "X-Luxforge-Version";
const BUILD_KEY: &str = "X-Luxforge-Build";

/// The user-level entry's path: `$XDG_DATA_HOME/applications/luxforge.desktop` when that is
/// absolute, otherwise `$HOME/.local/share/applications/luxforge.desktop`.
pub fn user_desktop_entry(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let data = match xdg_data_home
        .map(Path::new)
        .filter(|path| path.is_absolute())
    {
        Some(data) => data.to_owned(),
        None => Path::new(home.filter(|home| !home.is_empty())?).join(".local/share"),
    };
    Some(data.join("applications").join(DESKTOP_ENTRY_FILE))
}

/// Rewrite the entry at `entry` for `executable` and `build` when its `Exec` program is inside
/// `payload`. Answers whether it did: `false` for no entry, or one that points elsewhere. The new
/// entry replaces the old one with a rename, keeping its permissions.
pub fn refresh_desktop_entry(
    entry: &Path,
    payload: &Path,
    executable: &Path,
    build: &BuildInfo,
) -> Result<bool> {
    let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_ENTRY_BYTES {
        return Err(Error::Io(std::io::Error::other(format!(
            "{} is larger than {MAX_ENTRY_BYTES} bytes",
            entry.display()
        ))));
    }
    let text = fs::read_to_string(entry)?;
    let Some(refreshed) = rewrite(&text, payload, executable, build)? else {
        return Ok(false);
    };
    let mut temporary = entry.as_os_str().to_owned();
    temporary.push(format!(".luxforge-{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(refreshed.as_bytes())?;
        fs::set_permissions(&temporary, metadata.permissions())?;
        fs::rename(&temporary, entry)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written?;
    Ok(true)
}

/// The entry rewritten, or `None` when its `Exec` program is not inside `payload`.
fn rewrite(
    text: &str,
    payload: &Path,
    executable: &Path,
    build: &BuildInfo,
) -> Result<Option<String>> {
    let mut group = false;
    let mut points_here = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            group = line == GROUP;
        } else if group
            && let Some(value) = value(line, "Exec")
            && let Some(program) = program(value)
        {
            points_here = inside(Path::new(&program), payload);
        }
    }
    if !points_here {
        return Ok(None);
    }
    let program = executable
        .to_str()
        .filter(|path| !path.contains(['\\', '\n', '\r']))
        .ok_or_else(|| {
            Error::Io(std::io::Error::other(
                "the executable's path cannot be written into a desktop entry",
            ))
        })?;
    let mut out: Vec<String> = Vec::with_capacity(text.lines().count() + 2);
    let mut group: Option<usize> = None;
    let mut seen = (false, false);
    // The keys the group lacks, after its last non-blank line.
    let close = |out: &mut Vec<String>, start: usize, seen: (bool, bool)| {
        let at = (start..out.len())
            .rev()
            .find(|&index| !out[index].trim().is_empty())
            .map_or(out.len(), |index| index + 1);
        let mut missing = Vec::new();
        if !seen.0 {
            missing.push(format!("{VERSION_KEY}={}", build.version));
        }
        if !seen.1 {
            missing.push(format!("{BUILD_KEY}={}", build.build));
        }
        out.splice(at..at, missing);
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if let Some(start) = group.take() {
                close(&mut out, start, seen);
            }
            if trimmed == GROUP {
                group = Some(out.len() + 1);
            }
            out.push(line.to_owned());
        } else if group.is_none() {
            out.push(line.to_owned());
        } else if let Some(value) = value(trimmed, "Exec") {
            out.push(format!("Exec={}{}", quoted(program), arguments(value)));
        } else if value(trimmed, "TryExec").is_some() {
            out.push(format!("TryExec={program}"));
        } else if value(trimmed, VERSION_KEY).is_some() {
            seen.0 = true;
            out.push(format!("{VERSION_KEY}={}", build.version));
        } else if value(trimmed, BUILD_KEY).is_some() {
            seen.1 = true;
            out.push(format!("{BUILD_KEY}={}", build.build));
        } else {
            out.push(line.to_owned());
        }
    }
    if let Some(start) = group {
        close(&mut out, start, seen);
    }
    let mut out = out.join("\n");
    out.push('\n');
    Ok(Some(out))
}

/// The value of `key` on `line`, `key=value` with optional spaces around `=`.
fn value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?.trim_start();
    Some(rest.strip_prefix('=')?.trim_start())
}

/// The program an `Exec` value runs: its first argument, unquoted.
fn program(value: &str) -> Option<String> {
    let mut chars = value.chars();
    match chars.next()? {
        '"' => {
            let mut program = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => return Some(program),
                    '\\' => program.push(chars.next()?),
                    c => program.push(c),
                }
            }
            None
        }
        _ => value.split(' ').next().map(str::to_owned),
    }
}

/// Everything after an `Exec` value's first argument, with its leading space.
fn arguments(value: &str) -> &str {
    if let Some(rest) = value.strip_prefix('"') {
        let mut escaped = false;
        for (index, c) in rest.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => return &rest[index + 1..],
                _ => {}
            }
        }
        ""
    } else {
        value.find(' ').map_or("", |index| &value[index..])
    }
}

/// `program` as an `Exec` argument: bare when it holds no character the specification reserves,
/// otherwise double-quoted with `"`, `` ` `` and `$` escaped.
fn quoted(program: &str) -> String {
    let bare = program
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-+,".contains(&b));
    if bare {
        return program.to_owned();
    }
    let mut out = String::with_capacity(program.len() + 2);
    out.push('"');
    for c in program.chars() {
        if matches!(c, '"' | '`' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Whether `program` lies inside `payload`, comparing as written and as resolved.
fn inside(program: &Path, payload: &Path) -> bool {
    if program.starts_with(payload) {
        return true;
    }
    let resolved = |path: &Path| path.canonicalize().ok();
    match (
        resolved(program).or_else(|| program.parent().and_then(resolved)),
        resolved(payload),
    ) {
        (Some(program), Some(payload)) => program.starts_with(payload),
        _ => false,
    }
}
