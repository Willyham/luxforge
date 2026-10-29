//! The one durable write every store outside the catalog uses: the module settings and grants
//! documents, a resource's `installed.json` and the artifact manifest (`capabilities::document`),
//! and the derived-artifact objects (`artifacts::store`). Bytes go to a temporary file that is
//! synced, then renamed over the target, and the directory is synced so the rename itself is
//! durable. A failure before rename leaves the previous file; a directory-sync failure after
//! rename leaves the replacement visible with durability unconfirmed. Errors distinguish these outcomes. Writers that share a file serialize on an OS advisory lock beside
//! it, and a read is bounded. [`flush`] is the one call in the core that
//! makes written bytes durable, for these writes and for the resource and export writers.
use crate::Error;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// A failed file operation, the one mapping of an io error every store and the source reader use:
/// a full disk or quota is `resource-limit: disk full`, anything else a `read-error` naming what
/// failed (`context`, normally the path) and the error's kind.
pub(crate) fn file_error(context: impl std::fmt::Display, kind: io::ErrorKind) -> Error {
    match kind {
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
            Error::resource_limit("disk full")
        }
        kind => Error::file_access(format!("{context}: {kind}")),
    }
}

/// Take the advisory lock `<dir>/<name>` that serializes writers, across processes as well as
/// threads, creating the directory and the lock file on first use. It is released when the
/// returned handle drops.
pub(crate) fn lock(dir: &Path, name: &str) -> Result<File, Error> {
    fs::create_dir_all(dir).map_err(|error| file_error(dir.display(), error.kind()))?;
    let path = dir.join(name);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| file_error(path.display(), error.kind()))?;
    file.lock()
        .map_err(|error| file_error(path.display(), error.kind()))?;
    Ok(file)
}

/// The whole file, or `None` when it does not exist. A file larger than `max_bytes` is refused with
/// `resource-limit` and left as it is.
pub(crate) fn read(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, Error> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_error(path.display(), error.kind())),
    };
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| file_error(path.display(), error.kind()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::resource_limit(format!(
            "{} is larger than {max_bytes} bytes; the file is kept unchanged",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// Whether durable writes reach the drive: false only in the core's own unit tests and in a test
/// build with the `test-skip-disk-flush` feature, which only `[dev-dependencies]` tables turn on.
/// Every other step of a durable write still runs there, and no test can observe a flush. The
/// catalog reads it for its flush settings (`editor::catalog::configure`).
pub(crate) const FLUSHES: bool = !cfg!(any(test, feature = "test-skip-disk-flush"));

/// Make what has been written to `file`, a file or a directory, durable: `sync_all`, which on macOS
/// is `F_FULLFSYNC`, a flush of the drive's own cache that the whole host queues for. Nothing when
/// [`FLUSHES`] is false.
pub(crate) fn flush(file: &File) -> io::Result<()> {
    if FLUSHES { file.sync_all() } else { Ok(()) }
}

/// Write `bytes` to the temporary file `path`, replacing one a crash left, and sync it. A failure
/// removes it.
pub(crate) fn stage(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let written = File::create(path).and_then(|mut file| {
        file.write_all(bytes)?;
        flush(&file)
    });
    if written.is_err() {
        let _ = fs::remove_file(path);
    }
    written
}

/// The result of a visible publication, retaining its value until required consequences run.
#[derive(Debug)]
pub(crate) struct Publication<T> {
    pub value: T,
    pub durability_error: Option<Error>,
}

impl<T> Publication<T> {
    pub(crate) fn confirmed(value: T) -> Self {
        Self {
            value,
            durability_error: None,
        }
    }

    pub(crate) fn map<U>(self, map: impl FnOnce(T) -> U) -> Publication<U> {
        Publication {
            value: map(self.value),
            durability_error: self.durability_error,
        }
    }

    pub(crate) fn finish(self) -> Result<T, Error> {
        match self.durability_error {
            Some(error) => Err(error),
            None => Ok(self.value),
        }
    }
}

/// A publication failure records whether the replacement is already visible.
#[derive(Debug)]
pub(crate) struct PublishError {
    pub published: bool,
    error: io::Error,
}

impl PublishError {
    pub(crate) fn into_error(self, path: &Path) -> Error {
        if self.published {
            durability_error(path, self.error.kind())
        } else {
            file_error(path.display(), self.error.kind())
        }
    }
}

pub(crate) fn durability_error(path: &Path, kind: io::ErrorKind) -> Error {
    let mut error = file_error(path.display(), kind);
    // Keep the ordinary code/cause mapping, including disk full, without letting its concise
    // resource-limit wording discard the publication phase that clients must display.
    error.detail = format!(
        "{}: the replacement is visible but durability is unconfirmed; {}",
        path.display(),
        error.detail
    );
    error.with_data(serde_json::json!({"published": true, "durable": false, "path": path}))
}

/// Rename the staged file `from` over `to` and sync `to`'s directory, so the rename is durable.
pub(crate) fn publish(from: &Path, to: &Path) -> Result<(), PublishError> {
    fs::rename(from, to).map_err(|error| PublishError {
        published: false,
        error,
    })?;
    match to.parent() {
        Some(dir) => sync_dir(dir).map_err(|error| PublishError {
            published: true,
            error,
        }),
        None => Ok(()),
    }
}

/// Replace `path` with `bytes` through `<path>.tmp` beside it: [`stage`], then [`publish`]. A
/// failure removes any remaining temporary file. Before rename the previous target is intact;
/// after rename a directory-sync error leaves the replacement visible.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<Publication<()>, Error> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    stage(&temporary, bytes).map_err(|error| file_error(temporary.display(), error.kind()))?;
    match publish(&temporary, path) {
        Ok(()) => Ok(Publication::confirmed(())),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            if error.published {
                Ok(Publication {
                    value: (),
                    durability_error: Some(error.into_error(path)),
                })
            } else {
                Err(error.into_error(path))
            }
        }
    }
}

/// Make a rename or removal in `dir` durable. Directories cannot be opened for syncing on Windows,
/// where a rename is durable once it returns.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(test)]
    if sync_faults::take(dir) {
        return Err(io::Error::other("injected directory sync failure"));
    }
    #[cfg(unix)]
    flush(&File::open(dir)?)?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
pub(crate) mod sync_faults {
    use std::{
        collections::HashSet,
        path::{Path, PathBuf},
        sync::{Mutex, OnceLock},
    };
    fn faults() -> &'static Mutex<HashSet<PathBuf>> {
        static FAULTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
        FAULTS.get_or_init(Default::default)
    }
    pub(crate) fn inject(dir: &Path) {
        faults().lock().unwrap().insert(dir.to_owned());
    }
    pub(super) fn take(dir: &Path) -> bool {
        faults().lock().unwrap().remove(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn published_disk_full_and_quota_errors_keep_the_visible_durability_message() {
        let path = Path::new("state.json");
        for kind in [io::ErrorKind::StorageFull, io::ErrorKind::QuotaExceeded] {
            let error = durability_error(path, kind);
            assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
            assert!(
                error
                    .detail
                    .contains("visible but durability is unconfirmed")
            );
            assert!(error.detail.contains("disk full"));
            assert_eq!(error.data.as_deref().unwrap()["published"], true);
            assert_eq!(error.data.as_deref().unwrap()["durable"], false);
            assert_eq!(error.data.as_deref().unwrap()["path"], "state.json");
        }
    }

    #[test]
    fn publication_failure_preserves_visible_replacement_and_reports_phase() {
        let dir =
            std::env::temp_dir().join(format!("luxforge-publication-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        fs::write(&path, b"old").unwrap();
        sync_faults::inject(&dir);
        let published = replace(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let error = published.finish().unwrap_err();
        assert_eq!(error.data.as_deref().unwrap()["published"], true);
        assert_eq!(error.data.as_deref().unwrap()["durable"], false);
        assert!(
            error
                .detail
                .contains("visible but durability is unconfirmed")
        );
        let error = publish(&dir.join("absent"), &path).unwrap_err();
        assert!(!error.published);
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_dir_all(dir).unwrap();
    }
}
