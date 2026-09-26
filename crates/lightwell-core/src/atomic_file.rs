//! The one durable write every store outside the catalog uses: the module settings and grants
//! documents and a resource's `installed.json` (`capabilities::document`), and the derived-artifact
//! objects and manifest (`artifacts::store`). Bytes go to a temporary file that is synced, then
//! renamed over the target, and the directory is synced so the rename itself is durable; a failure
//! at any point leaves the previous file. Writers that share a file serialize on an OS advisory
//! lock beside it, and a read is bounded.
use crate::Error;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// A failed file operation: a full disk or quota is `resource-limit: disk full`, anything else a
/// `read-error` naming the path.
pub(crate) fn file_error(path: &Path, error: io::Error) -> Error {
    match error.kind() {
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
            Error::resource_limit("disk full")
        }
        kind => Error::file_access(format!("{}: {kind}", path.display())),
    }
}

/// Take the advisory lock `<dir>/<name>` that serializes writers, across processes as well as
/// threads, creating the directory and the lock file on first use. It is released when the
/// returned handle drops.
pub(crate) fn lock(dir: &Path, name: &str) -> Result<File, Error> {
    fs::create_dir_all(dir).map_err(|error| file_error(dir, error))?;
    let path = dir.join(name);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| file_error(&path, error))?;
    file.lock().map_err(|error| file_error(&path, error))?;
    Ok(file)
}

/// The whole file, or `None` when it does not exist. A file larger than `max_bytes` is refused with
/// `resource-limit` and left as it is.
pub(crate) fn read(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, Error> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_error(path, error)),
    };
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| file_error(path, error))?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::resource_limit(format!(
            "{} is larger than {max_bytes} bytes; the file is kept unchanged",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// Write `bytes` to the temporary file `path`, replacing one a crash left, and sync it. A failure
/// removes it.
pub(crate) fn stage(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let written = File::create(path).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if written.is_err() {
        let _ = fs::remove_file(path);
    }
    written
}

/// Rename the staged file `from` over `to` and sync `to`'s directory, so the rename is durable.
pub(crate) fn publish(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)?;
    match to.parent() {
        Some(dir) => sync_dir(dir),
        None => Ok(()),
    }
}

/// Replace `path` with `bytes` through `<path>.tmp` beside it: [`stage`], then [`publish`]. A
/// failure removes the temporary file and leaves the previous one.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    stage(&temporary, bytes).map_err(|error| file_error(&temporary, error))?;
    publish(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        file_error(path, error)
    })
}

/// Make a rename or removal in `dir` durable. Directories cannot be opened for syncing on Windows,
/// where a rename is durable once it returns.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}
