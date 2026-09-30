//! One FSEvents stream per root, created relative to the root's device so it can replay that
//! volume's history from a cursor (FSEvents takes one path per such stream). The stream is made
//! with file-level events (`FileEvents`), delivered at once after a quiet spell and then at most
//! every [`LATENCY`] (`NoDefer`), a report when the root itself moves (`WatchRoot`), and every
//! event of the history chunk its cursor falls in (`FullHistory`), so a cursor never skips an event
//! stored out of order.
//!
//! FSEvents names paths relative to the device's root and without firmlinks (`Users/me/Pictures`
//! on the data volume, whose root is `/System/Volumes/Data`), so the root's own path is read the
//! same way (`F_GETPATH_NOFIRMLINK`) and each event is mapped back under the root's path as the
//! caller gave it.
use super::Shared;
use crate::{RescanReason, Resume, WatchEvent, WatchRoot, changed, mounts::c_field};
use objc2_core_foundation::{CFArray, CFString};
use objc2_core_services::{
    ConstFSEventStreamRef, FSEventStreamContext, FSEventStreamCreateRelativeToDevice,
    FSEventStreamInvalidate, FSEventStreamRelease, FSEventStreamSetDispatchQueue,
    FSEventStreamStart, FSEventStreamStop, FSEventsCopyUUIDForDevice, FSEventsGetCurrentEventId,
    FSEventStreamRef, kFSEventStreamCreateFlagFileEvents, kFSEventStreamCreateFlagFullHistory,
    kFSEventStreamCreateFlagNoDefer, kFSEventStreamCreateFlagWatchRoot,
    kFSEventStreamEventFlagEventIdsWrapped, kFSEventStreamEventFlagHistoryDone,
    kFSEventStreamEventFlagMustScanSubDirs, kFSEventStreamEventFlagRootChanged,
    kFSEventStreamEventIdSinceNow,
};
use std::{
    ffi::{CStr, OsStr, c_char, c_void},
    fs::File,
    io,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// How long FSEvents gathers events after the first of a burst before delivering them, in
/// seconds.
const LATENCY: f64 = 0.1;

const FLAGS: u32 = kFSEventStreamCreateFlagFileEvents
    | kFSEventStreamCreateFlagNoDefer
    | kFSEventStreamCreateFlagWatchRoot
    | kFSEventStreamCreateFlagFullHistory;

/// What a root's callback reads, alive from before its stream is created until after the barrier
/// that follows the stream's release.
struct Context {
    root: Root,
    /// Whether the replay has caught up, after which events carry a cursor. Only the queue's
    /// callbacks touch it, one at a time.
    caught_up: AtomicBool,
    shared: Arc<Shared>,
}

/// A root as its events are mapped.
struct Root {
    id: u64,
    /// The root's path as the caller gave it.
    path: PathBuf,
    /// The root's path relative to its device's root, as FSEvents names it.
    prefix: PathBuf,
    /// The device's history, when it keeps one.
    volume: Option<[u8; 16]>,
    /// The event ID the stream started after.
    since: u64,
}

/// A running stream.
pub(super) struct Stream {
    stream: FSEventStreamRef,
    context: Arc<Context>,
}

// SAFETY: an FSEvents stream is a reference-counted CoreServices object whose functions may be
// called from any thread. The watcher that owns the stream calls them only through `&mut`, so one
// thread at a time, and the callback reads only the context, which is `Sync`.
unsafe impl Send for Stream {}

/// A stopped stream's context, dropped after the barrier that follows the stop.
pub(super) struct Retired(#[allow(dead_code, reason = "held only to be dropped")] Arc<Context>);

/// The stream's handle, moved onto the queue to start it there.
struct Handle(FSEventStreamRef);

// SAFETY: as for `Stream`; the handle only carries the stream to the queue for
// `FSEventStreamStart`, while the creating thread waits.
unsafe impl Send for Handle {}

impl Stream {
    /// Create and start the stream for `root`. Before its first event the receiver is sent
    /// [`RescanReason::NoReplay`] when its cursor cannot replay, and `CaughtUp` at once when the
    /// volume keeps no history.
    pub(super) fn open(shared: &Arc<Shared>, root: WatchRoot) -> io::Result<Self> {
        let folder = File::open(&root.path)?;
        let metadata = folder.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a folder", root.path.display()),
            ));
        }
        let device = metadata.dev() as libc::dev_t;
        let (mount_point, full) = volume_path(&folder)?;
        let prefix = full
            .strip_prefix(&mount_point)
            .map_err(|_| {
                io::Error::other(format!(
                    "{} is not under its volume's mount point {}",
                    full.display(),
                    mount_point.display()
                ))
            })?
            .to_path_buf();
        let relative = prefix.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not UTF-8", full.display()),
            )
        })?;
        let paths = CFArray::from_retained_objects(&[CFString::from_str(relative)]);
        let volume = history(device);
        let replay = root.resume.filter(|resume| Some(resume.volume) == volume);
        let mut first = Vec::new();
        let since = match (replay, volume) {
            (Some(resume), _) => resume.event_id,
            (None, history) => {
                first.push(WatchEvent::Rescan {
                    root: root.id,
                    subtree: root.path.clone(),
                    reason: RescanReason::NoReplay,
                });
                if history.is_some() {
                    // SAFETY: takes nothing and reads the service's current event ID.
                    unsafe { FSEventsGetCurrentEventId() }
                } else {
                    first.push(WatchEvent::CaughtUp {
                        root: root.id,
                        cursor: None,
                    });
                    kFSEventStreamEventIdSinceNow
                }
            }
        };
        let context = Arc::new(Context {
            caught_up: AtomicBool::new(volume.is_none()),
            root: Root {
                id: root.id,
                path: root.path,
                prefix,
                volume,
                since,
            },
            shared: shared.clone(),
        });
        let mut stream_context = FSEventStreamContext {
            version: 0,
            info: Arc::as_ptr(&context).cast_mut().cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        // SAFETY: the callback has the signature FSEvents calls and only reads what it is given;
        // the context is copied by the call and its `info` stays valid for as long as the stream
        // can call back (see `stop`); the paths are one CFString in a CFArray.
        let stream = unsafe {
            FSEventStreamCreateRelativeToDevice(
                None,
                Some(callback),
                &raw mut stream_context,
                device,
                paths.as_opaque(),
                since,
                LATENCY,
                FLAGS,
            )
        };
        if stream.is_null() {
            return Err(io::Error::other(format!(
                "FSEvents refused a stream for {}",
                context.root.path.display()
            )));
        }
        // SAFETY: `stream` was just created and is scheduled on the watcher's serial queue, which
        // outlives it.
        unsafe { FSEventStreamSetDispatchQueue(stream, Some(&shared.queue)) };
        // Start it on the queue and send the first events in the same block, so they precede
        // anything the stream delivers, which the queue runs only after this block.
        let handle = Handle(stream);
        let (sender, first_context) = (shared.clone(), context.clone());
        let mut started = false;
        shared.queue.exec_sync(|| {
            let handle = handle;
            // SAFETY: the stream is valid and scheduled on this queue.
            started = unsafe { FSEventStreamStart(handle.0) };
            if started {
                sender.root(first_context.root.id, &first_context.root.path, first);
            }
        });
        if !started {
            // SAFETY: the stream never started; it is invalidated and released once, and nothing
            // can call back.
            unsafe {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
            }
            return Err(io::Error::other(format!(
                "FSEvents could not start a stream for {}",
                context.root.path.display()
            )));
        }
        Ok(Self { stream, context })
    }

    /// Stop, invalidate and release the stream. A callback may still be running or queued until
    /// the watcher's next barrier, so the context it reads is handed back to be dropped after it.
    pub(super) fn stop(self) -> Retired {
        // SAFETY: the stream is valid and started; after `FSEventStreamStop` it calls back no
        // more, and it is invalidated and released exactly once.
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
        }
        Retired(self.context)
    }
}

/// The mount point of the volume `folder` is on, and `folder`'s path without firmlinks.
fn volume_path(folder: &File) -> io::Result<(PathBuf, PathBuf)> {
    let descriptor = folder.as_raw_fd();
    // SAFETY: `statfs` is plain data, and all zeros is a valid value of it.
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is open for the call, and `stats` is a whole `statfs` to fill.
    if unsafe { libc::fstatfs(descriptor, &raw mut stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut path = [0 as c_char; libc::PATH_MAX as usize];
    // SAFETY: `F_GETPATH_NOFIRMLINK` writes at most `PATH_MAX` bytes, NUL-terminated, into the
    // buffer, which has that many.
    if unsafe { libc::fcntl(descriptor, libc::F_GETPATH_NOFIRMLINK, path.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        PathBuf::from(OsStr::from_bytes(c_field(&stats.f_mntonname))),
        PathBuf::from(OsStr::from_bytes(c_field(&path))),
    ))
}

/// The identifier of the device's FSEvents history, if it keeps one.
fn history(device: libc::dev_t) -> Option<[u8; 16]> {
    // SAFETY: takes a device number and returns a new CFUUID or null.
    let uuid = unsafe { FSEventsCopyUUIDForDevice(device) }?.uuid_bytes();
    Some([
        uuid.byte0,
        uuid.byte1,
        uuid.byte2,
        uuid.byte3,
        uuid.byte4,
        uuid.byte5,
        uuid.byte6,
        uuid.byte7,
        uuid.byte8,
        uuid.byte9,
        uuid.byte10,
        uuid.byte11,
        uuid.byte12,
        uuid.byte13,
        uuid.byte14,
        uuid.byte15,
    ])
}

pub(crate) fn current_cursor(path: &Path) -> Option<Resume> {
    let device = std::fs::metadata(path).ok()?.dev() as libc::dev_t;
    let volume = history(device)?;
    // SAFETY: takes nothing and reads the service's current event ID.
    let event_id = unsafe { FSEventsGetCurrentEventId() };
    Some(Resume { volume, event_id })
}

unsafe extern "C-unwind" fn callback(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: NonNull<c_void>,
    flags: NonNull<u32>,
    ids: NonNull<u64>,
) {
    // SAFETY: `info` is the `Context` the stream was created with, kept alive until after the
    // barrier that follows the stream's release, so for every callback.
    let context = unsafe { &*info.cast::<Context>().cast_const() };
    // SAFETY: without `UseCFTypes`, FSEvents passes `count` NUL-terminated C strings, `count`
    // flag words and `count` event IDs, valid for the callback.
    let (paths, flags, ids) = unsafe {
        (
            std::slice::from_raw_parts(paths.as_ptr().cast::<*const c_char>(), count),
            std::slice::from_raw_parts(flags.as_ptr(), count),
            std::slice::from_raw_parts(ids.as_ptr(), count),
        )
    };
    // A panic must not unwind into CoreServices; it loses this batch at worst.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let events: Vec<(&[u8], u32, u64)> = (0..count)
            .map(|at| {
                // SAFETY: each path is a NUL-terminated C string valid for the callback.
                let path = unsafe { CStr::from_ptr(paths[at]) };
                (path.to_bytes(), flags[at], ids[at])
            })
            .collect();
        let mut caught_up = context.caught_up.load(Ordering::SeqCst);
        let events = context.root.interpret(&mut caught_up, &events);
        context.caught_up.store(caught_up, Ordering::SeqCst);
        context
            .shared
            .root(context.root.id, &context.root.path, events);
    }));
}

impl Root {
    /// What one callback's events mean: its paths as one `Changed` (split past `MAX_PATHS`), the
    /// subtrees to list again, and `CaughtUp` where the replay ends. Paths replayed carry no
    /// cursor, since FSEvents does not replay in event-ID order; once caught up, a batch's cursor
    /// is its highest event ID.
    fn interpret(&self, caught_up: &mut bool, events: &[(&[u8], u32, u64)]) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        let mut paths = Vec::new();
        let mut latest: Option<u64> = None;
        let rescan = |subtree: PathBuf, reason| WatchEvent::Rescan {
            root: self.id,
            subtree,
            reason,
        };
        for &(raw, flags, id) in events {
            if flags & kFSEventStreamEventFlagHistoryDone != 0 {
                out.extend(changed(self.id, std::mem::take(&mut paths), None));
                // `FullHistory` replays the whole chunk the start falls in, so the replay can end
                // before where the stream started; the cursor never goes back past that.
                let at = if id != 0 { id } else { latest.unwrap_or(0) }.max(self.since);
                *caught_up = true;
                latest = None;
                out.push(WatchEvent::CaughtUp {
                    root: self.id,
                    cursor: self.cursor(at),
                });
                continue;
            }
            if flags & kFSEventStreamEventFlagRootChanged != 0 {
                out.push(rescan(self.path.clone(), RescanReason::RootChanged));
                continue;
            }
            if id != 0 {
                latest = latest.max(Some(id));
            }
            if flags & kFSEventStreamEventFlagEventIdsWrapped != 0 {
                out.push(rescan(self.path.clone(), RescanReason::Wrapped));
                continue;
            }
            let path = self.map(raw);
            if flags & kFSEventStreamEventFlagMustScanSubDirs != 0 {
                let subtree = path.unwrap_or_else(|| self.path.clone());
                out.push(rescan(subtree, RescanReason::Dropped));
                continue;
            }
            match path {
                Some(path) => paths.push(path),
                // Nothing outside the root is reported; a path that cannot be placed is looked
                // at by listing the whole root.
                None => out.push(rescan(self.path.clone(), RescanReason::Dropped)),
            }
        }
        let cursor = latest.filter(|_| *caught_up).and_then(|at| self.cursor(at));
        out.extend(changed(self.id, paths, cursor));
        out
    }

    fn cursor(&self, event_id: u64) -> Option<Resume> {
        self.volume.map(|volume| Resume { volume, event_id })
    }

    /// An event's path, relative to the device's root, under the root's path as the caller gave
    /// it; `None` when it is not under the root.
    fn map(&self, raw: &[u8]) -> Option<PathBuf> {
        let relative = Path::new(OsStr::from_bytes(raw.strip_prefix(b"/").unwrap_or(raw)));
        let rest = relative.strip_prefix(&self.prefix).ok()?;
        Some(if rest.as_os_str().is_empty() {
            self.path.clone()
        } else {
            self.path.join(rest)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_services::{
        kFSEventStreamEventFlagItemCreated, kFSEventStreamEventFlagItemIsFile,
        kFSEventStreamEventFlagItemRenamed, kFSEventStreamEventFlagUserDropped,
    };

    fn root() -> Root {
        Root {
            id: 3,
            path: PathBuf::from("/Users/me/Pictures"),
            prefix: PathBuf::from("Users/me/Pictures"),
            volume: Some([5; 16]),
            since: 100,
        }
    }

    fn cursor(event_id: u64) -> Option<Resume> {
        Some(Resume {
            volume: [5; 16],
            event_id,
        })
    }

    const FILE: u32 = kFSEventStreamEventFlagItemCreated | kFSEventStreamEventFlagItemIsFile;

    #[test]
    fn a_replay_carries_no_cursor_until_it_is_done_and_live_batches_carry_their_highest_id() {
        let root = root();
        let mut caught_up = false;
        let events = root.interpret(
            &mut caught_up,
            &[
                (b"Users/me/Pictures/b.jpg", FILE, 106),
                (b"Users/me/Pictures/a.jpg", kFSEventStreamEventFlagItemRenamed, 103),
                (b"Users/me/Pictures", kFSEventStreamEventFlagHistoryDone, 106),
                (b"Users/me/Pictures/sub/c.jpg", FILE, 110),
            ],
        );
        assert!(caught_up);
        assert_eq!(
            events,
            [
                WatchEvent::Changed {
                    root: 3,
                    paths: vec![
                        PathBuf::from("/Users/me/Pictures/b.jpg"),
                        PathBuf::from("/Users/me/Pictures/a.jpg")
                    ],
                    cursor: None
                },
                WatchEvent::CaughtUp {
                    root: 3,
                    cursor: cursor(106)
                },
                WatchEvent::Changed {
                    root: 3,
                    paths: vec![PathBuf::from("/Users/me/Pictures/sub/c.jpg")],
                    cursor: cursor(110)
                },
            ]
        );
        // Live and out of order: the cursor is the highest.
        let events = root.interpret(
            &mut caught_up,
            &[
                (b"Users/me/Pictures/d.jpg", FILE, 120),
                (b"Users/me/Pictures/e.jpg", FILE, 118),
            ],
        );
        assert!(
            matches!(&events[..], [WatchEvent::Changed { cursor: at, .. }] if *at == cursor(120)),
            "{events:?}"
        );
    }

    #[test]
    fn a_replay_that_ends_before_its_start_keeps_the_start_as_its_cursor() {
        let root = root();
        let mut caught_up = false;
        let events = root.interpret(
            &mut caught_up,
            &[
                (b"Users/me/Pictures/old.jpg", FILE, 97),
                (b"Users/me/Pictures", kFSEventStreamEventFlagHistoryDone, 97),
            ],
        );
        assert_eq!(
            events.last(),
            Some(&WatchEvent::CaughtUp {
                root: 3,
                cursor: cursor(100)
            })
        );
    }

    #[test]
    fn dropped_wrapped_and_moved_roots_are_rescans() {
        let root = root();
        let mut caught_up = true;
        let events = root.interpret(
            &mut caught_up,
            &[
                (
                    b"Users/me/Pictures/2024",
                    kFSEventStreamEventFlagMustScanSubDirs,
                    130,
                ),
                (
                    b"/",
                    kFSEventStreamEventFlagMustScanSubDirs | kFSEventStreamEventFlagUserDropped,
                    131,
                ),
                (b"Users/me/Pictures", kFSEventStreamEventFlagRootChanged, 0),
                (b"", kFSEventStreamEventFlagEventIdsWrapped, 1),
                (b"Users/someone-else/x.jpg", FILE, 2),
            ],
        );
        let rescan = |subtree: &str, reason| WatchEvent::Rescan {
            root: 3,
            subtree: PathBuf::from(subtree),
            reason,
        };
        assert_eq!(
            events,
            [
                rescan("/Users/me/Pictures/2024", RescanReason::Dropped),
                rescan("/Users/me/Pictures", RescanReason::Dropped),
                rescan("/Users/me/Pictures", RescanReason::RootChanged),
                rescan("/Users/me/Pictures", RescanReason::Wrapped),
                rescan("/Users/me/Pictures", RescanReason::Dropped),
            ]
        );
    }

    #[test]
    fn a_volume_root_and_a_volume_without_history_map_their_paths() {
        let card = Root {
            id: 1,
            path: PathBuf::from("/Volumes/CARD"),
            prefix: PathBuf::new(),
            volume: None,
            since: kFSEventStreamEventIdSinceNow,
        };
        let mut caught_up = true;
        assert_eq!(
            card.interpret(&mut caught_up, &[(b"DCIM/100NIKON/DSC_0001.NEF", FILE, 9)]),
            [WatchEvent::Changed {
                root: 1,
                paths: vec![PathBuf::from("/Volumes/CARD/DCIM/100NIKON/DSC_0001.NEF")],
                cursor: None
            }]
        );
        assert_eq!(card.map(b"/"), Some(PathBuf::from("/Volumes/CARD")));
    }
}
