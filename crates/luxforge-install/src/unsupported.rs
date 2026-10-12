//! The system calls off macOS, where no bundle is ever classified or moved: each answers that it
//! is unsupported, or nothing.
use std::{
    io,
    path::{Path, PathBuf},
};

/// The volume a path is on, as `statfs` and Disk Arbitration report it on macOS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub read_only: bool,
    pub device: String,
    pub mount_point: PathBuf,
    pub disk_image: bool,
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "moving into Applications is macOS only",
    )
}

pub(super) fn volume(_: &Path) -> io::Result<Volume> {
    Err(unsupported())
}

pub(super) fn writable(_: &Path) -> bool {
    false
}

pub(super) fn clear_quarantine(_: &Path) -> io::Result<bool> {
    Ok(false)
}

/// Whether `path` carries the quarantine attribute: never, off macOS.
pub fn quarantined(_: &Path) -> bool {
    false
}

pub(super) fn translocated(_: &Path) -> Option<Option<PathBuf>> {
    None
}

pub(super) fn trash(_: &Path) -> io::Result<()> {
    Err(unsupported())
}
