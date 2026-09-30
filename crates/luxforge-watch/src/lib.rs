//! The platform's change notifications for the folders the catalog's index watches and for the
//! volumes that come and go, and the mount table ([`mounts`]). The index lane sleeps on a blocking
//! receive and wakes only when one of these arrives, so nothing polls while nothing changes
//! (`docs/design/catalog.md`, "Keeping up with the disk"; performance rule 8).
//!
//! A [`Watcher`] sends [`WatchEvent`]s into the caller's bounded channel:
//!
//! - **What changed.** [`WatchEvent::Changed`] names paths under a root, batched as the platform
//!   delivers them. Each is a hint to look again, never a verdict: platforms coalesce what happened
//!   to a path, a rename arrives as its old and its new path, unlinked, and a folder moved in
//!   arrives as the folder alone. So the caller stats each path: a missing one is gone, with
//!   everything under it; a file is read by signature; a folder the index did not know is listed
//!   with everything under it, and one it knew needs only its own row.
//! - **What to list again.** [`WatchEvent::Rescan`] names a subtree whose changes were not all
//!   reported, and why ([`RescanReason`]); the caller reconciles it by signature. After
//!   [`RescanReason::RootChanged`] the watch may no longer follow the root (it moved, was removed
//!   or its volume went away): the caller checks the path and adds the root again.
//! - **Caught up.** [`WatchEvent::CaughtUp`] ends the replay of what changed while no watcher ran
//!   (macOS); elsewhere it follows [`RescanReason::NoReplay`] at once.
//! - **Volumes.** [`WatchEvent::Volume`] reports a file system mounted or unmounted since the
//!   watcher started, with what the mount table knows of it. What was mounted before is
//!   [`mounts`].
//! - **Unwatched.** [`WatchEvent::Unwatched`] says that changes under a subtree are no longer
//!   reported, and why (on Linux, the inotify watch limit).
//!
//! # Cursors
//!
//! On macOS, FSEvents keeps each volume's history, so a root can resume from a [`Resume`]: the
//! volume's history identifier and an event ID. Events carry the cursor after them once the
//! replay has caught up. The caller persists a cursor only after it has durably applied the event
//! it came with and every event before it, and passes it back in [`WatchRoot::resume`] on the next
//! start. A cursor from another history (the volume's history was discarded, or it is another
//! volume) cannot replay: the root gets [`RescanReason::NoReplay`], as it does on Linux and Windows,
//! which keep no history, and whenever no cursor is given.
//!
//! A new root is either added first and listed on the [`RescanReason::NoReplay`] that follows, on
//! every platform; or, on macOS, listed after reading [`current_cursor`] and added with that
//! cursor, so what changed during the listing replays.
//!
//! # Delivery and memory
//!
//! Platform callbacks only copy paths and try to send; they never wait on the channel. When it is
//! full, the root's events are withheld and replaced by one [`RescanReason::Overflow`] rescan of
//! the whole root (and the [`WatchEvent::CaughtUp`] it was owed, after it), and the mount table's
//! difference is sent again from the table then current. What is owed is retried every
//! [`RETRY`] until it is delivered: the one timer, armed only while something is owed. A
//! `Changed` holds at most [`MAX_PATHS`] paths; a larger batch is split, with its cursor on the
//! last part. So what the watcher holds is bounded by the channel's capacity.
//!
//! # Threads
//!
//! - **macOS**: none of its own. Each root's FSEvents stream and the Disk Arbitration session
//!   deliver on one serial dispatch queue.
//! - **Linux**: one thread, blocking in `poll` on the inotify descriptor, `/proc/self/mountinfo`
//!   (which reports a mount or unmount as a priority event) and an eventfd that stops it.
//! - **Windows**: one thread, blocking on an I/O completion port that each root's overlapped
//!   `ReadDirectoryChangesW` and the configuration manager's volume notifications complete on.
//!
//! Dropping the [`Watcher`] stops every stream and notification, waits for any callback running,
//! joins its thread and drops the sender, so the receiver then sees the channel closed.
//!
//! The platform code is FFI (except on Linux, through `rustix`), so it lives in this leaf crate.
//! Every `unsafe` block sits beside a `SAFETY:` comment, only the modules that call the platform
//! hold one, and nothing unsafe crosses the public API.
pub use mounts::{Mount, mounts};
use std::{
    io,
    path::{Path, PathBuf},
    sync::mpsc::{SyncSender, TrySendError},
    time::Duration,
};

mod delivery;
mod inotify;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod mounts;
mod notify_information;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use unsupported as platform;
#[cfg(windows)]
use windows as platform;

/// The most paths one [`WatchEvent::Changed`] holds.
pub const MAX_PATHS: usize = 4096;

/// How long the watcher waits before it tries again to send what a full channel refused.
pub const RETRY: Duration = Duration::from_millis(50);

/// Where a root's replay resumes: the volume's FSEvents history and the last event applied. Only
/// macOS has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resume {
    /// The identifier of the volume's event history (`FSEventsCopyUUIDForDevice`), which changes
    /// when the history is discarded.
    pub volume: [u8; 16],
    /// The last event ID applied.
    pub event_id: u64,
}

/// A folder to watch, with everything under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchRoot {
    /// The caller's name for the root, which every event about it carries.
    pub id: u64,
    /// An absolute path to a folder. Events name paths under it as given here.
    pub path: PathBuf,
    /// Where to resume from, if the caller kept a cursor; see [`Resume`].
    pub resume: Option<Resume>,
}

/// Why a subtree must be listed again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RescanReason {
    /// More changed than could be delivered: the channel was full, or the kernel's own queue
    /// overflowed (inotify's `IN_Q_OVERFLOW`, a `ReadDirectoryChangesW` buffer overflow).
    Overflow,
    /// The platform coalesced or dropped events under the subtree (FSEvents' `MustScanSubDirs`).
    Dropped,
    /// The platform's event IDs wrapped: every stored cursor is void.
    Wrapped,
    /// The root itself, or a folder above it, moved, was removed, or its volume went away.
    RootChanged,
    /// Nothing could be replayed: the platform keeps no history, no cursor was given, or the
    /// cursor belongs to another history.
    NoReplay,
}

/// A file system mounted or unmounted after the watcher started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeEvent {
    /// Mounted, as the mount table then lists it.
    Mounted { mount: Mount },
    /// No longer mounted at `mount_point`.
    Unmounted { mount_point: PathBuf },
}

/// What a [`Watcher`] reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// These paths under the root may have changed; see the crate documentation.
    Changed {
        root: u64,
        paths: Vec<PathBuf>,
        /// Where to resume after applying this and every earlier event, once the replay has
        /// caught up (macOS only).
        cursor: Option<Resume>,
    },
    /// Everything under `subtree` must be listed again.
    Rescan {
        root: u64,
        subtree: PathBuf,
        reason: RescanReason,
    },
    /// The replay of what changed while no watcher ran is over; what follows is live.
    CaughtUp {
        root: u64,
        cursor: Option<Resume>,
    },
    /// Changes under `subtree` are no longer reported, for the reason given.
    Unwatched {
        root: u64,
        subtree: PathBuf,
        error: String,
    },
    Volume(VolumeEvent),
}

impl WatchEvent {
    /// The root the event is about, if it is about one.
    pub fn root(&self) -> Option<u64> {
        match self {
            Self::Changed { root, .. }
            | Self::Rescan { root, .. }
            | Self::CaughtUp { root, .. }
            | Self::Unwatched { root, .. } => Some(*root),
            Self::Volume(_) => None,
        }
    }
}

/// Watches roots and volumes; see the crate documentation.
pub struct Watcher {
    platform: platform::Watcher,
}

impl Watcher {
    /// Start watching volumes, with no root yet. Events go to `events` as whatever the caller's
    /// channel carries, so a lane can receive them with its other work.
    pub fn start<T>(events: SyncSender<T>) -> io::Result<Self>
    where
        T: From<WatchEvent> + Send + 'static,
    {
        let sink: delivery::Sink = Box::new(move |event| match events.try_send(T::from(event)) {
            Ok(()) => delivery::Sent::Delivered,
            Err(TrySendError::Full(_)) => delivery::Sent::Full,
            Err(TrySendError::Disconnected(_)) => delivery::Sent::Closed,
        });
        Ok(Self {
            platform: platform::Watcher::start(sink)?,
        })
    }

    /// Watch a folder and everything under it. Its first events are its replay and
    /// [`WatchEvent::CaughtUp`], or [`RescanReason::NoReplay`] and then `CaughtUp`. An `id`
    /// already watched, a relative path and a path that is not a folder are refused.
    pub fn add_root(&mut self, root: WatchRoot) -> io::Result<()> {
        if !root.path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not an absolute path", root.path.display()),
            ));
        }
        self.platform.add_root(root)
    }

    /// Stop watching the root. Nothing more is sent about it once this returns, though events
    /// sent before may still wait in the channel. An unknown `id` is ignored.
    pub fn remove_root(&mut self, id: u64) {
        self.platform.remove_root(id);
    }
}

/// A lane may start its watcher on one thread and keep it on another.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Watcher>();
};

/// Where a new root under `path` would resume from if it were added now: read it before the
/// root's first listing and add the root with it, so what changes during the listing replays.
/// `None` where the platform keeps no history (Linux, Windows) or the volume has none.
pub fn current_cursor(path: &Path) -> Option<Resume> {
    platform::current_cursor(path)
}

/// The error for an `id` already watched.
fn already_watched(id: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("root {id} is already watched"),
    )
}

/// `paths` without repeats, in the order first named, as one [`WatchEvent::Changed`] or, past
/// [`MAX_PATHS`], several, with `cursor` on the last.
fn changed(root: u64, paths: Vec<PathBuf>, cursor: Option<Resume>) -> Vec<WatchEvent> {
    let mut seen = std::collections::HashSet::with_capacity(paths.len());
    let paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect();
    if paths.is_empty() {
        return Vec::new();
    }
    let parts = paths.len().div_ceil(MAX_PATHS);
    let mut events = Vec::with_capacity(parts);
    let mut paths = paths.into_iter();
    for part in 0..parts {
        events.push(WatchEvent::Changed {
            root,
            paths: paths.by_ref().take(MAX_PATHS).collect(),
            cursor: (part + 1 == parts).then_some(cursor).flatten(),
        });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_names_each_path_once_and_splits_with_its_cursor_on_the_last_part() {
        let cursor = Some(Resume {
            volume: [7; 16],
            event_id: 42,
        });
        let a = PathBuf::from("/r/a");
        let b = PathBuf::from("/r/b");
        assert_eq!(
            changed(1, vec![a.clone(), b.clone(), a.clone()], cursor),
            [WatchEvent::Changed {
                root: 1,
                paths: vec![a, b],
                cursor
            }]
        );
        assert!(changed(1, Vec::new(), cursor).is_empty());
        let many: Vec<PathBuf> = (0..MAX_PATHS * 2 + 1)
            .map(|n| PathBuf::from(format!("/r/{n}")))
            .collect();
        let parts = changed(1, many, cursor);
        let sizes: Vec<(usize, Option<Resume>)> = parts
            .iter()
            .map(|event| match event {
                WatchEvent::Changed { paths, cursor, .. } => (paths.len(), *cursor),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            sizes,
            [(MAX_PATHS, None), (MAX_PATHS, None), (1, cursor)],
            "the cursor waits for the last part"
        );
    }

    #[test]
    fn a_relative_root_is_refused() {
        let (events, _receiver) = std::sync::mpsc::sync_channel::<WatchEvent>(4);
        let Ok(mut watcher) = Watcher::start(events) else {
            return;
        };
        let error = watcher
            .add_root(WatchRoot {
                id: 1,
                path: PathBuf::from("photos"),
                resume: None,
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
