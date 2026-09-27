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
use crate::atomic_file::{file_error, sync_dir};
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    path: PathBuf,
}

impl Destination {
    pub fn check(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(Error::validation(
                "export destination must be an absolute path",
            ));
        }
        if path.file_name().is_none() {
            return Err(Error::validation("export destination has no file name"));
        }
        let has_jpeg_extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            });
        if !has_jpeg_extension {
            return Err(Error::validation(
                "export destination must end in .jpg or .jpeg",
            ));
        }
        // The shape is valid; anything from here on touches the file system.
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| Error::validation("export destination has no parent directory"))?;
        let parent_is_directory = fs::metadata(parent)
            .map_err(|error| file_error(parent, error))?
            .is_dir();
        if !parent_is_directory {
            return Err(Error::file_access(format!(
                "{} is not a directory",
                parent.display()
            )));
        }
        match fs::symlink_metadata(path) {
            Ok(_) => Err(Error::conflict("export destination already exists")
                .with_data(json!({"path": path}))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self {
                path: path.to_path_buf(),
            }),
            Err(error) => Err(file_error(path, error)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stage(&self) -> Result<Staged, Error> {
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| Error::validation("export destination has no parent directory"))?;
        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| Error::validation("export destination has no file name"))?;
        let temp_path = parent.join(format!(
            ".{}.{}.luxforge-export",
            file_name.to_string_lossy(),
            uuid::Uuid::new_v4().simple()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| file_error(&temp_path, error))?;
        Ok(Staged {
            file: BufWriter::new(file),
            temp_path,
            final_path: self.path.clone(),
            published: false,
        })
    }
}

/// A temporary file exclusive to this export, beside its final destination. Dropping it without
/// calling [`Staged::publish`] removes the temporary file.
pub struct Staged {
    file: BufWriter<File>,
    temp_path: PathBuf,
    final_path: PathBuf,
    published: bool,
}

impl Staged {
    /// Flushes and syncs the temporary file, then publishes it under the final name without ever
    /// replacing an existing entry: a hard link, or, when the file system refuses one (for example
    /// an exFAT volume), an exclusive create of the final name and a copy of the temporary file's
    /// bytes into it. Either way the temporary file is then removed and the destination directory
    /// synced. Consumes `self` so an already-published `Staged` is never dropped as unpublished.
    pub fn publish(mut self) -> Result<u64, Error> {
        self.file
            .flush()
            .map_err(|error| file_error(&self.temp_path, error))?;
        let file = self.file.get_ref();
        file.sync_all()
            .map_err(|error| file_error(&self.temp_path, error))?;
        let bytes = file
            .metadata()
            .map_err(|error| file_error(&self.temp_path, error))?
            .len();

        match fs::hard_link(&self.temp_path, &self.final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(Error::conflict("export destination already exists")
                    .with_data(json!({"path": &self.final_path})));
            }
            Err(_unsupported) => {
                // The file system may not support hard links (e.g. exFAT): fall back to an
                // exclusive create of the final name and a copy of the temporary file's bytes.
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&self.final_path)
                {
                    Ok(mut final_file) => {
                        let copied = File::open(&self.temp_path)
                            .and_then(|mut temp_file| io::copy(&mut temp_file, &mut final_file))
                            .and_then(|_| final_file.sync_all());
                        if let Err(error) = copied {
                            let _ = fs::remove_file(&self.final_path);
                            return Err(file_error(&self.final_path, error));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        return Err(Error::conflict("export destination already exists")
                            .with_data(json!({"path": &self.final_path})));
                    }
                    Err(error) => return Err(file_error(&self.final_path, error)),
                }
            }
        }

        fs::remove_file(&self.temp_path).map_err(|error| file_error(&self.temp_path, error))?;
        if let Some(directory) = self.final_path.parent() {
            sync_dir(directory).map_err(|error| file_error(directory, error))?;
        }
        self.published = true;
        Ok(bytes)
    }
}

impl Write for Staged {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}

/// `<stem>-edited.jpg` in `directory`, else `-edited-2.jpg`, `-edited-3.jpg` and so on, probing at
/// most 64 names; `None` once all 64 are taken. `stem` is used as-is: it comes from an existing
/// file name, so nothing further needs sanitizing.
pub fn suggest(directory: &Path, stem: &str) -> Option<PathBuf> {
    const MAX_PROBES: u32 = 64;
    for n in 1..=MAX_PROBES {
        let candidate = if n == 1 {
            directory.join(format!("{stem}-edited.jpg"))
        } else {
            directory.join(format!("{stem}-edited-{n}.jpg"))
        };
        if fs::symlink_metadata(&candidate).is_err() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_testkit::fixtures::temp_dir;

    fn scratch() -> PathBuf {
        temp_dir("export-publish")
    }

    #[test]
    fn accepts_a_valid_destination() {
        let dir = scratch();
        let path = dir.join("photo-edited.jpg");
        let destination = Destination::check(&path).expect("valid destination");
        assert_eq!(destination.path(), path);
    }

    #[test]
    fn refuses_a_relative_path() {
        let error = Destination::check(Path::new("relative.jpg")).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
    }

    #[test]
    fn refuses_a_path_with_no_file_name() {
        let dir = scratch();
        let error = Destination::check(&dir.join("..")).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
    }

    #[test]
    fn refuses_a_path_with_the_wrong_extension() {
        let dir = scratch();
        let error = Destination::check(&dir.join("photo.png")).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
    }

    #[test]
    fn refuses_a_path_with_no_extension() {
        let dir = scratch();
        let error = Destination::check(&dir.join("photo")).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Validation);
    }

    #[test]
    fn accepts_uppercase_extension() {
        let dir = scratch();
        Destination::check(&dir.join("photo.JPG")).expect("uppercase extension accepted");
        Destination::check(&dir.join("photo.JPEG")).expect("uppercase extension accepted");
    }

    #[test]
    fn refuses_a_missing_parent_directory() {
        let dir = scratch();
        let path = dir.join("missing-parent").join("photo-edited.jpg");
        let error = Destination::check(&path).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::FileAccess);
    }

    #[test]
    fn refuses_a_parent_that_is_a_file() {
        let dir = scratch();
        let parent_as_file = dir.join("not-a-directory");
        fs::write(&parent_as_file, b"x").unwrap();
        let path = parent_as_file.join("photo-edited.jpg");
        let error = Destination::check(&path).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::FileAccess);
    }

    #[test]
    fn refuses_an_existing_file() {
        let dir = scratch();
        let path = dir.join("photo-edited.jpg");
        fs::write(&path, b"already here").unwrap();
        let error = Destination::check(&path).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Conflict);
        assert_eq!(error.data.unwrap()["path"], json!(path));
    }

    #[test]
    fn refuses_an_existing_directory() {
        let dir = scratch();
        let path = dir.join("photo-edited.jpg");
        fs::create_dir(&path).unwrap();
        let error = Destination::check(&path).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Conflict);
    }

    #[test]
    fn refuses_a_dangling_symlink() {
        let dir = scratch();
        let path = dir.join("photo-edited.jpg");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("does-not-exist"), &path).unwrap();
        #[cfg(not(unix))]
        {
            // No portable symlink API without extra privileges; skip elsewhere.
            return;
        }
        let error = Destination::check(&path).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Conflict);
    }

    #[test]
    #[cfg(unix)]
    fn refuses_a_symlink_to_a_file() {
        let dir = scratch();
        let target = dir.join("original.jpg");
        fs::write(&target, b"original").unwrap();
        let path = dir.join("photo-edited.jpg");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let error = Destination::check(&path).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Conflict);
    }

    #[test]
    fn refuses_a_hard_link_to_a_file() {
        let dir = scratch();
        let original = dir.join("original.jpg");
        fs::write(&original, b"original").unwrap();
        let path = dir.join("photo-edited.jpg");
        fs::hard_link(&original, &path).unwrap();
        let error = Destination::check(&path).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Conflict);
    }

    #[test]
    fn stage_and_publish_leaves_exactly_the_final_file() {
        let dir = scratch();
        let path = dir.join("photo-edited.jpg");
        let destination = Destination::check(&path).expect("valid destination");
        let mut staged = destination.stage().expect("staged");
        staged.write_all(b"hello jpeg bytes").unwrap();
        let bytes = staged.publish().expect("published");
        assert_eq!(bytes, 16);

        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "no temp file left behind: {entries:?}");
        assert_eq!(fs::read(&path).unwrap(), b"hello jpeg bytes");
    }

    #[test]
    fn a_race_at_publish_time_is_refused_and_leaves_the_winner_untouched() {
        let dir = scratch();
        let path = dir.join("photo-edited.jpg");
        let destination = Destination::check(&path).expect("valid destination");
        let staged = destination.stage().expect("staged");

        // Something else creates the destination after `check` but before `publish`.
        fs::write(&path, b"someone else got here first").unwrap();

        let error = staged.publish().unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Conflict);
        assert_eq!(
            fs::read(&path).unwrap(),
            b"someone else got here first",
            "the winner's bytes are untouched"
        );
        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "no temp file left behind: {entries:?}");
    }

    #[test]
    fn dropping_an_unpublished_staged_file_removes_its_temp() {
        let dir = scratch();
        let path = dir.join("photo-edited.jpg");
        let destination = Destination::check(&path).expect("valid destination");
        {
            let mut staged = destination.stage().expect("staged");
            staged.write_all(b"never published").unwrap();
        }
        let entries: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert!(entries.is_empty(), "temp file removed on drop");
    }

    #[test]
    fn suggest_counts_up_past_taken_names() {
        let dir = scratch();
        fs::write(dir.join("photo-edited.jpg"), b"x").unwrap();
        fs::write(dir.join("photo-edited-2.jpg"), b"x").unwrap();
        let suggested = suggest(&dir, "photo").expect("a free name");
        assert_eq!(suggested, dir.join("photo-edited-3.jpg"));
    }

    #[test]
    fn suggest_returns_none_after_64_taken_names() {
        let dir = scratch();
        fs::write(dir.join("photo-edited.jpg"), b"x").unwrap();
        for n in 2..=64 {
            fs::write(dir.join(format!("photo-edited-{n}.jpg")), b"x").unwrap();
        }
        assert_eq!(suggest(&dir, "photo"), None);
    }
}
