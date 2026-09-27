//! Export destinations: validation, the suggested name and a publish that never replaces.
//!
//! Contract (`docs/design/export.md#destination-rules` and step 5 of the behavior):
//! - `Destination::check(path)` accepts an absolute path with a `.jpg`/`.jpeg` extension (any case)
//!   whose parent is an existing directory and at which nothing exists (`symlink_metadata`, so a
//!   dangling symlink counts). Refusals: `validation` for the shape, `file_access` for an unreadable
//!   parent, `conflict` with `data: {"path": ...}` for anything already there.
//! - `suggest(directory, stem)` returns `<stem>-edited.jpg`, else `-edited-2.jpg` … within 64
//!   probes, else `None`.
//! - `Destination::stage()` creates `.<file name>.<uuid>.luxforge-export` exclusively in the same
//!   directory; `Staged` is a `Write` (buffered), and `Staged::publish()` flushes, syncs, publishes
//!   under the final name without replacement (hard link, else exclusive create and copy), removes
//!   the temporary file, syncs the directory and returns the byte length. Dropping a `Staged`
//!   without publishing removes the temporary file.

use crate::Error;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    path: PathBuf,
}

impl Destination {
    pub fn check(_path: &Path) -> Result<Self, Error> {
        unimplemented!("export destination check")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stage(&self) -> Result<Staged, Error> {
        unimplemented!("export staging")
    }
}

pub struct Staged {}

impl Staged {
    pub fn publish(self) -> Result<u64, Error> {
        unimplemented!("export publish")
    }
}

impl Write for Staged {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        unimplemented!("export staging")
    }

    fn flush(&mut self) -> io::Result<()> {
        unimplemented!("export staging")
    }
}

pub fn suggest(_directory: &Path, _stem: &str) -> Option<PathBuf> {
    unimplemented!("export suggested name")
}
