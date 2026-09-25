//! Small files the host owns outside every catalog — the settings and grants documents and a
//! resource's `installed.json` — share one discipline: writers serialize on an OS advisory lock
//! beside the file, a read is bounded, and a write goes to a synced temporary file that is renamed
//! over the old one, so a failure at any point leaves the previous file.
use crate::{Error, ErrorKind};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub(super) fn file_error(path: &Path, error: io::Error) -> Error {
    Error::new(
        ErrorKind::FileAccess,
        format!("{}: {}", path.display(), error.kind()),
    )
}

/// Take the advisory lock `<dir>/<name>` that serializes writers, across processes as well as
/// threads, creating the directory and the lock file on first use. It is released when the
/// returned handle drops.
pub(super) fn lock(dir: &Path, name: &str) -> Result<File, Error> {
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
pub(super) fn read(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, Error> {
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
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "{} is larger than {max_bytes} bytes; the file is kept unchanged",
                path.display()
            ),
        ));
    }
    Ok(Some(bytes))
}

/// Replace `<dir>/<name>` with `bytes`: write `<dir>/<temporary>`, sync it, rename it over the old
/// file and sync the directory, so the rename is durable. A failure removes the temporary file and
/// leaves the previous one.
pub(super) fn replace(dir: &Path, name: &str, temporary: &str, bytes: &[u8]) -> Result<(), Error> {
    try_replace(dir, name, temporary, bytes).map_err(|(path, error)| file_error(&path, error))
}

/// [`replace`], keeping the I/O error and the path it concerns, for a caller that reports a full
/// disk differently from other failures.
pub(super) fn try_replace(
    dir: &Path,
    name: &str,
    temporary: &str,
    bytes: &[u8],
) -> Result<(), (PathBuf, io::Error)> {
    let temporary = dir.join(temporary);
    let written = File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, dir.join(name)));
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err((temporary, error));
    }
    #[cfg(unix)]
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| (dir.to_path_buf(), error))?;
    Ok(())
}

/// Make a rename or removal in `dir` durable. Directories cannot be opened for syncing on Windows,
/// where a rename is durable once it returns.
pub(super) fn sync_dir(dir: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| file_error(dir, error))?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}
