//! Platforms the watcher has no notifications for: it cannot start, and no cursor is kept.
#![forbid(unsafe_code)]
use crate::{Resume, WatchRoot, delivery::Sink};
use std::{io, path::Path};

pub(crate) struct Watcher;

impl Watcher {
    pub(crate) fn start(_sink: Sink) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "folders are not watched on this platform",
        ))
    }

    pub(crate) fn add_root(&mut self, _root: WatchRoot) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub(crate) fn remove_root(&mut self, _id: u64) {}
}

pub(crate) fn current_cursor(_path: &Path) -> Option<Resume> {
    None
}
