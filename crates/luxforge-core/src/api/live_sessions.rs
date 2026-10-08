//! The per-user registry of running live sessions, so a live client such as `luxforge-ctl` finds the
//! catalog a running desktop has open rather than the one its next launch would open. Each desktop
//! serving a live session keeps one entry in `<config>/live-sessions/`: `<pid>-<id>.json`, owner-only
//! like the session file, naming the protocol, its process, its catalog and that catalog's session
//! file, and beside it `<pid>-<id>.lock`, which it holds locked for as long as the entry is live.
//! The lock is what says the entry is live: one whose lock anyone else can take belongs to a
//! process that has gone, as after a crash, and a reader removes it. Nothing here grants anything:
//! the token stays in the session file, which a client still reads and checks.
use super::PROTOCOL;
use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// The registry's directory under the application's configuration directory.
pub const LIVE_SESSIONS_DIR: &str = "live-sessions";
/// The most an entry may hold; the desktop writes well under it.
const ENTRY_LIMIT: usize = 4 * 1024;
/// The most entries a reader looks at, so a directory filled by something else stays bounded.
const MAX_ENTRIES: usize = 256;
const ENTRY_EXTENSION: &str = "json";
const LOCK_EXTENSION: &str = "lock";

/// One running live session, as its desktop registers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveSessionEntry {
    /// The protocol the session speaks, [`PROTOCOL`] for this build.
    pub protocol: String,
    /// The process serving it.
    pub pid: u32,
    /// The absolute path of the catalog the session's owner has open.
    pub catalog: PathBuf,
    /// That catalog's session file ([`super::live_session_file`]).
    pub session_file: PathBuf,
}

/// A live entry of the registry: its file, and the entry or why it cannot be read. An entry whose
/// process is gone is never one.
#[derive(Debug)]
pub struct RunningSession {
    pub file: PathBuf,
    pub entry: Result<LiveSessionEntry, String>,
}

/// The entry a live session holds while it runs; dropping it removes the entry, then releases and
/// removes its lock.
#[derive(Debug)]
pub(super) struct RegistryEntry {
    entry: PathBuf,
    lock_path: PathBuf,
    lock: Option<File>,
}

impl RegistryEntry {
    /// Register `catalog`'s session, served from this process, in `dir`, creating the directory
    /// (owner-only on Unix) when it is missing. The lock is taken before the entry is written, so a
    /// reader never sees an entry without its live lock.
    pub(super) fn register(dir: &Path, catalog: &Path, session_file: &Path) -> Result<Self, Error> {
        let failed = |what: &str, error: std::io::Error| {
            Error::file_access(format!(
                "cannot {what} the live-session registry entry in {}: {error}",
                dir.display()
            ))
        };
        create_private_dir(dir).map_err(|error| failed("create", error))?;
        let pid = std::process::id();
        let name = format!("{pid}-{}", uuid::Uuid::new_v4().simple());
        let lock_path = dir.join(&name).with_extension(LOCK_EXTENSION);
        let lock = private_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|error| failed("create", error))?;
        lock.lock().map_err(|error| failed("lock", error))?;
        let mut registration = Self {
            entry: dir.join(name).with_extension(ENTRY_EXTENSION),
            lock_path,
            lock: Some(lock),
        };
        let entry = LiveSessionEntry {
            protocol: PROTOCOL.into(),
            pid,
            catalog: std::path::absolute(catalog).map_err(|error| failed("name", error))?,
            session_file: std::path::absolute(session_file)
                .map_err(|error| failed("name", error))?,
        };
        let bytes =
            serde_json::to_vec(&entry).map_err(|error| Error::internal(error.to_string()))?;
        if let Err(error) = write_private(&registration.entry, &bytes) {
            // Nothing was registered: the lock goes with the entry that never appeared.
            registration.entry = PathBuf::new();
            return Err(failed("write", error));
        }
        Ok(registration)
    }
}

impl Drop for RegistryEntry {
    fn drop(&mut self) {
        if !self.entry.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.entry);
        }
        // Closing the file releases its lock; Windows removes only a closed file.
        drop(self.lock.take());
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// The live sessions registered in `dir`, in file-name order: every entry whose lock its process
/// still holds. An entry whose lock can be taken, or has gone, belongs to a process that has gone,
/// and is removed with its lock. Anything else in the directory is left alone, a lock without an
/// entry included: a desktop creates its lock before it writes its entry, so such a lock may be one
/// about to be held. A missing directory is no sessions. At most [`MAX_ENTRIES`] files are looked
/// at.
pub fn running_sessions(dir: &Path) -> Result<Vec<RunningSession>, Error> {
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::file_access(format!(
                "cannot list the live sessions in {}: {error}",
                dir.display()
            )));
        }
    };
    let mut files: Vec<PathBuf> = listing
        .take(MAX_ENTRIES)
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    files.sort();
    let mut running = Vec::new();
    for file in &files {
        let extension = file.extension().and_then(|extension| extension.to_str());
        match extension {
            Some(ENTRY_EXTENSION) => {
                let lock = file.with_extension(LOCK_EXTENSION);
                if held(&lock) {
                    running.push(RunningSession {
                        file: file.clone(),
                        entry: read_entry(file),
                    });
                } else {
                    let _ = std::fs::remove_file(file);
                    let _ = std::fs::remove_file(lock);
                }
            }
            _ => {}
        }
    }
    Ok(running)
}

/// Whether a live process holds `lock`. A lock that is missing or that this process can take is
/// held by nobody; one that cannot be opened or tested is treated as held, so an entry is never
/// removed on a guess.
fn held(lock: &Path) -> bool {
    let file = match File::open(lock) {
        Ok(file) => file,
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    };
    match file.try_lock_shared() {
        Ok(()) => false,
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => true,
    }
}

fn read_entry(file: &Path) -> Result<LiveSessionEntry, String> {
    let mut bytes = Vec::new();
    File::open(file)
        .and_then(|opened| opened.take(ENTRY_LIMIT as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    if bytes.len() > ENTRY_LIMIT {
        return Err(format!(
            "{} is larger than {ENTRY_LIMIT} bytes",
            file.display()
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is malformed: {error}", file.display()))
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Write `bytes` to `path` as a whole: into a new owner-only file beside it (mode 0600 on Unix),
/// then renamed over it, so a reader sees either no file or the whole of it, never a partial one,
/// and the file has those permissions from its first byte. The live-session file and a registry
/// entry are written this way.
pub(super) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!("{name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    let written = private_options()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.flush()))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_testbase::paths::temp_dir;

    #[test]
    fn a_registered_session_is_running_until_its_registration_drops() {
        let dir = temp_dir("live-registry").join(LIVE_SESSIONS_DIR);
        assert!(running_sessions(&dir).unwrap().is_empty(), "no directory");
        let catalog = dir.parent().unwrap().join("catalog.sqlite");
        let session_file = super::super::live_session_file(&catalog);
        let registration = RegistryEntry::register(&dir, &catalog, &session_file).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&registration.entry), 0o600);
        }
        let running = running_sessions(&dir).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(
            running[0].entry.as_ref().unwrap(),
            &LiveSessionEntry {
                protocol: PROTOCOL.into(),
                pid: std::process::id(),
                catalog: catalog.clone(),
                session_file,
            }
        );
        drop(registration);
        assert!(running_sessions(&dir).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing left");
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_entry_whose_process_has_gone_is_removed_and_others_are_reported() {
        let dir = temp_dir("live-registry-stale").join(LIVE_SESSIONS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        // Left by a process that crashed: an entry and its lock, which nobody holds.
        let stale = dir.join("1-stale.json");
        std::fs::write(&stale, b"{}").unwrap();
        std::fs::write(dir.join("1-stale.lock"), b"").unwrap();
        // An entry without a lock, a lock without an entry, which may be about to be held, and a
        // file of no concern.
        std::fs::write(dir.join("2-unlocked.json"), b"{}").unwrap();
        std::fs::write(dir.join("3-starting.lock"), b"").unwrap();
        std::fs::write(dir.join("notes.txt"), b"kept").unwrap();
        // A live entry this build cannot read is still a running session, reported with why.
        let lock = File::create(dir.join("4-foreign.lock")).unwrap();
        lock.lock().unwrap();
        std::fs::write(dir.join("4-foreign.json"), b"{\"protocol\": 2}").unwrap();
        let running = running_sessions(&dir).unwrap();
        assert_eq!(running.len(), 1, "{running:?}");
        assert!(running[0].entry.as_ref().unwrap_err().contains("malformed"));
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "3-starting.lock",
                "4-foreign.json",
                "4-foreign.lock",
                "notes.txt"
            ]
        );
        drop(lock);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_private_write_replaces_the_whole_file_and_leaves_no_temporary() {
        let dir = temp_dir("private-write");
        let path = dir.join("session.json");
        write_private(&path, b"first").unwrap();
        write_private(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
