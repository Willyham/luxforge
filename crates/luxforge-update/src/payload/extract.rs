//! Extracting a verified release archive into `Luxforge.new/`. The archive's signature has already
//! been checked, so these refusals are defence in depth against a signed archive that is wrong:
//!
//! - every entry lies under the archive's one `Luxforge/` directory, and no path is absolute or has
//!   a `..` component;
//! - a symbolic link's target is relative and descends only (no `..`, never absolute), so whatever
//!   resolves through any chain of the archive's links stays inside the payload;
//! - a hard link names an earlier regular file of the archive;
//! - devices, FIFOs, sparse files and every other special entry are refused;
//! - no path is written twice, and the entries and their bytes stay under [`Limits`].
//!
//! Files keep their permission bits without set-id or sticky bits, and with group and other write
//! cleared and owner read and write added; directories keep theirs with owner access added, applied
//! once everything under them is written.
use super::{ARCHIVE_ROOT, fetch::Cancel};
use crate::error::{Error, Result};
use flate2::read::GzDecoder;
use std::{
    fs,
    io::{self, BufReader, Read as _},
    path::{Component, Path, PathBuf},
};
use tar::EntryType;

/// The archive's read buffer.
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// What an extraction may write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The most file bytes, summed over every regular file.
    pub(crate) bytes: u64,
    /// The most entries.
    pub(crate) entries: u64,
}

impl Limits {
    /// A release payload is about a hundred megabytes in a few thousand files.
    pub(crate) const DEFAULT: Limits = Limits {
        bytes: 4 * 1024 * 1024 * 1024,
        entries: 100_000,
    };
}

fn unsafe_entry(why: impl Into<String>) -> Error {
    Error::UnsafeArchive(why.into())
}

/// Extract the `.tar.gz` at `archive` into `destination`, which must not exist yet.
pub(crate) fn extract(
    archive: &Path,
    destination: &Path,
    cancel: &Cancel,
    limits: Limits,
) -> Result<()> {
    fs::create_dir(destination)?;
    let reader = BufReader::with_capacity(READ_BUFFER_BYTES, fs::File::open(archive)?);
    let mut tar = tar::Archive::new(GzDecoder::new(reader));
    let mut bytes = 0u64;
    let mut entries = 0u64;
    let mut directories = Vec::new();
    for entry in tar.entries()? {
        cancel.check()?;
        let mut entry = entry?;
        entries += 1;
        if entries > limits.entries {
            return Err(unsafe_entry(format!(
                "the archive has more than {} entries",
                limits.entries
            )));
        }
        let raw = entry.path()?.into_owned();
        let Some(relative) = inside(&raw)? else {
            // The `Luxforge/` directory itself.
            continue;
        };
        let target = destination.join(&relative);
        let kind = entry.header().entry_type();
        let mode = entry.header().mode()? & 0o777;
        match kind {
            EntryType::Directory => {
                match target.symlink_metadata() {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(_) => return Err(duplicate(&raw)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        fs::create_dir_all(&target)?;
                    }
                    Err(error) => return Err(error.into()),
                }
                directories.push((target, mode));
            }
            EntryType::Regular | EntryType::Continuous => {
                let size = entry.size();
                bytes = bytes.saturating_add(size);
                if bytes > limits.bytes {
                    return Err(unsafe_entry(format!(
                        "the archive holds more than {} bytes",
                        limits.bytes
                    )));
                }
                parent(&target)?;
                let mut file = create(&target, &raw)?;
                let copied = io::copy(&mut (&mut entry).take(size), &mut file)?;
                if copied != size {
                    return Err(unsafe_entry(format!(
                        "{} ends after {copied} of its {size} bytes",
                        raw.display()
                    )));
                }
                drop(file);
                set_mode(&target, (mode & 0o755) | 0o600)?;
            }
            EntryType::Symlink => {
                let link = entry
                    .link_name()?
                    .ok_or_else(|| unsafe_entry(format!("{} links nowhere", raw.display())))?
                    .into_owned();
                descending(&link).map_err(|why| {
                    unsafe_entry(format!(
                        "{} links to {}, which {why}",
                        raw.display(),
                        link.display()
                    ))
                })?;
                parent(&target)?;
                symlink(&link, &target, &raw)?;
            }
            EntryType::Link => {
                let link = entry
                    .link_name()?
                    .ok_or_else(|| unsafe_entry(format!("{} links nowhere", raw.display())))?
                    .into_owned();
                let source = inside(&link)?.map(|source| destination.join(source));
                let regular = source
                    .as_ref()
                    .and_then(|source| source.symlink_metadata().ok())
                    .is_some_and(|metadata| metadata.is_file());
                let Some(source) = source.filter(|_| regular) else {
                    return Err(unsafe_entry(format!(
                        "{} is a hard link to {}, which is not an earlier file of the payload",
                        raw.display(),
                        link.display()
                    )));
                };
                parent(&target)?;
                if target.symlink_metadata().is_ok() {
                    return Err(duplicate(&raw));
                }
                fs::hard_link(source, &target)?;
            }
            other => {
                return Err(unsafe_entry(format!(
                    "{} is a {other:?} entry, which a payload never holds",
                    raw.display()
                )));
            }
        }
    }
    // Deepest first, so a directory made read-only does not stop its parent's mode being set.
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, mode) in directories {
        set_mode(&path, (mode & 0o755) | 0o700)?;
    }
    Ok(())
}

/// The entry's path below `Luxforge/`; `None` for `Luxforge/` itself. Refuses a path outside it,
/// an absolute path and any `..`.
fn inside(path: &Path) -> Result<Option<PathBuf>> {
    let mut components = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir));
    match components.next() {
        Some(Component::Normal(root)) if root == ARCHIVE_ROOT => {}
        _ => {
            return Err(unsafe_entry(format!(
                "{} is not inside {ARCHIVE_ROOT}/",
                path.display()
            )));
        }
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(name) => relative.push(name),
            _ => {
                return Err(unsafe_entry(format!(
                    "{} is absolute or climbs with ..",
                    path.display()
                )));
            }
        }
    }
    Ok((!relative.as_os_str().is_empty()).then_some(relative))
}

/// Whether a symbolic link's target only descends from the link's own directory.
fn descending(link: &Path) -> std::result::Result<(), &'static str> {
    if link.as_os_str().is_empty() {
        return Err("is empty");
    }
    for component in link.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err("climbs with .."),
            Component::RootDir | Component::Prefix(_) => return Err("is absolute"),
        }
    }
    Ok(())
}

fn duplicate(raw: &Path) -> Error {
    unsafe_entry(format!("{} is in the archive twice", raw.display()))
}

/// Create the directories above `path`.
fn parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// A new file at `path`, refusing one that exists, a dangling link included.
fn create(path: &Path, raw: &Path) -> Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => duplicate(raw),
            _ => error.into(),
        })
}

#[cfg(unix)]
fn symlink(link: &Path, target: &Path, raw: &Path) -> Result<()> {
    std::os::unix::fs::symlink(link, target).map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => duplicate(raw),
        _ => error.into(),
    })
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path, raw: &Path) -> Result<()> {
    Err(unsafe_entry(format!(
        "{} is a symbolic link, which this platform's payload never holds",
        raw.display()
    )))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(fs::set_permissions(path, fs::Permissions::from_mode(mode))?)
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<()> {
    Ok(())
}
