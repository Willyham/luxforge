//! The watcher on macOS, against the real FSEvents service and Disk Arbitration, in scratch
//! folders under the system's temporary directory. That directory is reached through the `/var`
//! link, so every test also checks that events are named under the root's path as given, not as
//! FSEvents spells it (`/private/var`, or relative to the data volume).
#![cfg(target_os = "macos")]
use luxforge_testbase::{paths::temp_dir, wait_until};
use luxforge_watch::{
    RescanReason, Resume, VolumeEvent, WatchEvent, WatchRoot, Watcher, current_cursor,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        mpsc::{Receiver, TryRecvError, sync_channel},
    },
};

/// What a test has received, and the channel it keeps reading.
struct Events<T> {
    receiver: Receiver<T>,
    seen: Vec<WatchEvent>,
}

impl<T: Into<WatchEvent>> Events<T> {
    fn new(receiver: Receiver<T>) -> Self {
        Self {
            receiver,
            seen: Vec::new(),
        }
    }

    /// Read until `done` holds for what was received about `root`, and answer it.
    fn until(
        &mut self,
        what: &str,
        root: u64,
        done: impl Fn(&[WatchEvent]) -> bool,
    ) -> Vec<WatchEvent> {
        wait_until(what, || {
            self.seen.extend(self.receiver.try_iter().map(Into::into));
            done(&about(&self.seen, root))
        });
        about(&self.seen, root)
    }

    /// Forget what was received so far.
    fn clear(&mut self) {
        self.seen.extend(self.receiver.try_iter().map(Into::into));
        self.seen.clear();
    }
}

fn about(events: &[WatchEvent], root: u64) -> Vec<WatchEvent> {
    events
        .iter()
        .filter(|event| event.root() == Some(root))
        .cloned()
        .collect()
}

/// Every path the events name as changed.
fn changed(events: &[WatchEvent]) -> BTreeSet<PathBuf> {
    events
        .iter()
        .flat_map(|event| match event {
            WatchEvent::Changed { paths, .. } => paths.clone(),
            _ => Vec::new(),
        })
        .collect()
}

fn caught_up(events: &[WatchEvent]) -> Option<Option<Resume>> {
    events.iter().find_map(|event| match event {
        WatchEvent::CaughtUp { cursor, .. } => Some(*cursor),
        _ => None,
    })
}

/// The cursor of the last event that carries one.
fn last_cursor(events: &[WatchEvent]) -> Option<Resume> {
    events.iter().rev().find_map(|event| match event {
        WatchEvent::Changed { cursor, .. } | WatchEvent::CaughtUp { cursor, .. } => *cursor,
        _ => None,
    })
}

fn root(id: u64, path: &Path, resume: Option<Resume>) -> WatchRoot {
    WatchRoot {
        id,
        path: path.to_path_buf(),
        resume,
    }
}

/// A scratch folder named as a person might name one, reached through `/var`.
fn folder(name: &str) -> PathBuf {
    let path = temp_dir(name).join("Fotos – Ålesund");
    fs::create_dir(&path).unwrap();
    assert!(path.starts_with("/var"), "{}", path.display());
    path
}

#[test]
fn changes_in_a_watched_folder_arrive_as_paths_under_the_root_as_given() {
    let dir = folder("watch-changes");
    fs::write(dir.join("b.jpg"), b"b").unwrap();
    let (sender, receiver) = sync_channel::<WatchEvent>(64);
    let mut events = Events::new(receiver);
    let mut watcher = Watcher::start(sender).unwrap();
    watcher.add_root(root(1, &dir, None)).unwrap();
    let first = events.until("the root caught up", 1, |seen| caught_up(seen).is_some());
    assert_eq!(
        first[0],
        WatchEvent::Rescan {
            root: 1,
            subtree: dir.clone(),
            reason: RescanReason::NoReplay
        },
        "without a cursor the root is listed first"
    );
    assert!(
        caught_up(&first).unwrap().is_some(),
        "the data volume keeps a history, so the root has a cursor"
    );
    events.clear();

    fs::write(dir.join("a.jpg"), b"a").unwrap();
    fs::write(dir.join("b.jpg"), b"b, longer").unwrap();
    fs::rename(dir.join("a.jpg"), dir.join("c.jpg")).unwrap();
    fs::remove_file(dir.join("b.jpg")).unwrap();
    fs::create_dir(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/Æøå.jpg"), b"d").unwrap();
    let expected: BTreeSet<PathBuf> = ["a.jpg", "b.jpg", "c.jpg", "sub", "sub/Æøå.jpg"]
        .iter()
        .map(|name| dir.join(name))
        .collect();
    let seen = events.until("every change", 1, |seen| {
        changed(seen).is_superset(&expected)
    });
    assert!(
        changed(&seen).iter().all(|path| path.starts_with(&dir)),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .all(|event| matches!(event, WatchEvent::Changed { .. })),
        "nothing but changes: {seen:?}"
    );
    assert!(last_cursor(&seen).is_some(), "live changes carry a cursor");

    // A folder above the root moving is a root change.
    let moved = dir.with_file_name("moved away");
    fs::rename(&dir, &moved).unwrap();
    events.until("the root change", 1, |seen| {
        seen.contains(&WatchEvent::Rescan {
            root: 1,
            subtree: dir.clone(),
            reason: RescanReason::RootChanged,
        })
    });
}

#[test]
fn a_watcher_started_again_replays_what_changed_while_none_ran() {
    let dir = folder("watch-replay");
    fs::write(dir.join("kept.jpg"), b"k").unwrap();
    fs::write(dir.join("renamed.jpg"), b"r").unwrap();
    let cursor = {
        let (sender, receiver) = sync_channel::<WatchEvent>(64);
        let mut events = Events::new(receiver);
        let mut watcher = Watcher::start(sender).unwrap();
        watcher.add_root(root(1, &dir, None)).unwrap();
        events.until("the root caught up", 1, |seen| caught_up(seen).is_some());
        fs::write(dir.join("first.jpg"), b"1").unwrap();
        let seen = events.until("the first change", 1, |seen| {
            changed(seen).contains(&dir.join("first.jpg"))
        });
        drop(watcher);
        // Dropping the watcher dropped its sender.
        events.seen.extend(events.receiver.try_iter());
        assert_eq!(events.receiver.try_recv(), Err(TryRecvError::Disconnected));
        last_cursor(&seen).expect("a cursor after the change")
    };

    // While no watcher runs: a file added, one changed, one renamed and one removed.
    fs::write(dir.join("added.jpg"), b"a").unwrap();
    fs::write(dir.join("kept.jpg"), b"k, changed").unwrap();
    fs::rename(dir.join("renamed.jpg"), dir.join("new name.jpg")).unwrap();
    fs::remove_file(dir.join("first.jpg")).unwrap();

    let (sender, receiver) = sync_channel::<WatchEvent>(64);
    let mut events = Events::new(receiver);
    let mut watcher = Watcher::start(sender).unwrap();
    watcher.add_root(root(1, &dir, Some(cursor))).unwrap();
    // Changes the service had not yet stored when the stream read its history arrive live, just
    // after the replay; either way each arrives once the root is watched again.
    let expected: BTreeSet<PathBuf> = [
        "added.jpg",
        "kept.jpg",
        "renamed.jpg",
        "new name.jpg",
        "first.jpg",
    ]
    .iter()
    .map(|name| dir.join(name))
    .collect();
    let seen = events.until("the replay", 1, |seen| {
        caught_up(seen).is_some() && changed(seen).is_superset(&expected)
    });
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, WatchEvent::Rescan { .. })),
        "a cursor from this history replays: {seen:?}"
    );
    let resumed = caught_up(&seen)
        .unwrap()
        .expect("a cursor at the end of the replay");
    assert_eq!(resumed.volume, cursor.volume);
    assert!(resumed.event_id >= cursor.event_id);
    let replay = seen
        .iter()
        .take_while(|event| !matches!(event, WatchEvent::CaughtUp { .. }));
    assert!(
        replay
            .clone()
            .all(|event| matches!(event, WatchEvent::Changed { cursor: None, .. })),
        "replayed paths carry no cursor: {seen:?}"
    );

    // A cursor from another history cannot replay.
    let other = folder("watch-foreign");
    let foreign = Resume {
        volume: [0xEE; 16],
        event_id: cursor.event_id,
    };
    watcher.add_root(root(2, &other, Some(foreign))).unwrap();
    let seen = events.until("the other root caught up", 2, |seen| {
        caught_up(seen).is_some()
    });
    assert_eq!(
        seen[0],
        WatchEvent::Rescan {
            root: 2,
            subtree: other,
            reason: RescanReason::NoReplay
        }
    );
}

#[test]
fn a_new_root_listed_after_reading_its_cursor_replays_what_changed_during_the_listing() {
    let dir = folder("watch-cursor");
    let cursor = current_cursor(&dir).expect("the data volume keeps a history");
    // What the first listing would have missed.
    fs::write(dir.join("during.jpg"), b"d").unwrap();
    let (sender, receiver) = sync_channel::<WatchEvent>(64);
    let mut events = Events::new(receiver);
    let mut watcher = Watcher::start(sender).unwrap();
    watcher.add_root(root(1, &dir, Some(cursor))).unwrap();
    // Replayed, or just after the replay when the service had not stored it yet.
    let seen = events.until("the replay", 1, |seen| {
        caught_up(seen).is_some() && changed(seen).contains(&dir.join("during.jpg"))
    });
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, WatchEvent::Rescan { .. }))
    );
}

/// Every event the watcher tried to send to [`Probe`]'s channel, delivered or refused, since the
/// conversion runs before each attempt.
static ATTEMPTS: Mutex<Vec<WatchEvent>> = Mutex::new(Vec::new());

/// A channel item that records each attempt to send it.
struct Probe(WatchEvent);

impl From<WatchEvent> for Probe {
    fn from(event: WatchEvent) -> Self {
        ATTEMPTS.lock().unwrap().push(event.clone());
        Self(event)
    }
}

impl From<Probe> for WatchEvent {
    fn from(probe: Probe) -> Self {
        probe.0
    }
}

fn attempted(root: u64, what: impl Fn(&WatchEvent) -> bool) -> bool {
    ATTEMPTS
        .lock()
        .unwrap()
        .iter()
        .any(|event| event.root() == Some(root) && what(event))
}

#[test]
fn a_full_channel_becomes_a_rescan_of_the_root_once_there_is_room() {
    let dir = folder("watch-overflow");
    let id = 90_001;
    // The channel starts full, and nothing reads it until the watcher has had a change refused.
    let (sender, receiver) = sync_channel::<Probe>(1);
    let filler = WatchEvent::Volume(VolumeEvent::Unmounted {
        mount_point: PathBuf::from("/nowhere"),
    });
    sender.send(Probe(filler)).unwrap();
    let mut events = Events::new(receiver);
    let mut watcher = Watcher::start(sender).unwrap();
    // The change replays, so it is the root's first event, and it is refused; the watcher, which
    // never waits on the channel, owes the root a rescan instead, and withholds what follows.
    let cursor = current_cursor(&dir);
    fs::write(dir.join("lost.jpg"), b"l").unwrap();
    watcher.add_root(root(id, &dir, cursor)).unwrap();
    wait_until("the change was refused", || {
        attempted(id, |event| matches!(event, WatchEvent::Changed { .. }))
    });
    // Once there is room, the retry sends the rescan, then the caught-up it was owed after it. A
    // channel of one can fill again between the two, which only owes the root another rescan.
    let seen = events.until("the rescan and the caught-up", id, |seen| {
        caught_up(seen).is_some()
    });
    let overflow = WatchEvent::Rescan {
        root: id,
        subtree: dir.clone(),
        reason: RescanReason::Overflow,
    };
    assert_eq!(seen[0], overflow, "{seen:?}");
    assert!(
        matches!(
            seen.last(),
            Some(WatchEvent::CaughtUp {
                cursor: Some(_),
                ..
            })
        ),
        "{seen:?}"
    );
    // Once it is paid, changes are sent as they come.
    fs::write(dir.join("next.jpg"), b"n").unwrap();
    let seen = events.until("the next change", id, |seen| {
        changed(seen).contains(&dir.join("next.jpg"))
    });
    // The refused change was covered by the rescan, never sent on its own.
    assert!(!changed(&seen).contains(&dir.join("lost.jpg")), "{seen:?}");
}

#[test]
fn a_removed_root_sends_nothing_more_and_a_dropped_watcher_closes_the_channel() {
    let (first, second) = (folder("watch-removed"), folder("watch-kept"));
    let (sender, receiver) = sync_channel::<WatchEvent>(64);
    let mut events = Events::new(receiver);
    let mut watcher = Watcher::start(sender).unwrap();
    watcher.add_root(root(1, &first, None)).unwrap();
    watcher.add_root(root(2, &second, None)).unwrap();
    assert_eq!(
        watcher.add_root(root(2, &first, None)).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    events.until("the first caught up", 1, |seen| caught_up(seen).is_some());
    events.until("the second caught up", 2, |seen| caught_up(seen).is_some());
    watcher.remove_root(1);
    watcher.remove_root(1);
    events.clear();
    fs::write(first.join("unseen.jpg"), b"u").unwrap();
    fs::write(second.join("seen.jpg"), b"s").unwrap();
    events.until("the kept root's change", 2, |seen| {
        changed(seen).contains(&second.join("seen.jpg"))
    });
    assert!(about(&events.seen, 1).is_empty(), "{:?}", events.seen);
    drop(watcher);
    events.seen.extend(events.receiver.try_iter());
    assert_eq!(events.receiver.try_recv(), Err(TryRecvError::Disconnected));
}

#[test]
fn a_file_is_not_a_root() {
    let dir = folder("watch-file");
    fs::write(dir.join("a.jpg"), b"a").unwrap();
    let (sender, _receiver) = sync_channel::<WatchEvent>(4);
    let mut watcher = Watcher::start(sender).unwrap();
    assert_eq!(
        watcher
            .add_root(root(1, &dir.join("a.jpg"), None))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotADirectory
    );
    assert_eq!(
        watcher
            .add_root(root(1, &dir.join("missing"), None))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
}

/// Run `hdiutil`, failing the test with its output when it fails.
fn hdiutil(args: &[&std::ffi::OsStr]) {
    let output = std::process::Command::new("hdiutil")
        .args(args)
        .output()
        .expect("hdiutil runs");
    assert!(
        output.status.success(),
        "hdiutil {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Detaches the image however the test ends.
struct Attached(PathBuf);

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil")
            .args(["detach".as_ref(), "-force".as_ref(), self.0.as_os_str()])
            .output();
    }
}

#[test]
#[ignore = "mounts a disk image with hdiutil"]
fn a_disk_image_mounted_and_unmounted_is_reported() {
    let dir = temp_dir("watch-image").canonicalize().unwrap();
    let image = dir.join("card.dmg");
    let mount = dir.join("mnt");
    fs::create_dir(&mount).unwrap();
    let name = format!("LFW{}", std::process::id());
    hdiutil(&[
        "create".as_ref(),
        "-size".as_ref(),
        "8m".as_ref(),
        "-fs".as_ref(),
        "HFS+".as_ref(),
        "-volname".as_ref(),
        name.as_ref(),
        image.as_os_str(),
    ]);
    let (sender, receiver) = sync_channel::<WatchEvent>(64);
    let mut events = Events::new(receiver);
    let _watcher = Watcher::start(sender).unwrap();
    let volumes = |seen: &[WatchEvent]| -> Vec<VolumeEvent> {
        seen.iter()
            .filter_map(|event| match event {
                WatchEvent::Volume(volume) => Some(volume.clone()),
                _ => None,
            })
            .collect()
    };
    let _attached = Attached(mount.clone());
    hdiutil(&[
        "attach".as_ref(),
        "-nobrowse".as_ref(),
        "-mountpoint".as_ref(),
        mount.as_os_str(),
        image.as_os_str(),
    ]);
    wait_until("the mount", || {
        events.seen.extend(events.receiver.try_iter());
        volumes(&events.seen).iter().any(
            |volume| matches!(volume, VolumeEvent::Mounted { mount: m } if m.mount_point == mount),
        )
    });
    let mounted = volumes(&events.seen)
        .into_iter()
        .find_map(|volume| match volume {
            VolumeEvent::Mounted { mount: m } if m.mount_point == mount => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(mounted.name.as_deref(), Some(name.as_str()), "{mounted:?}");
    assert!(
        mounted.removable && mounted.local && mounted.uuid.is_some(),
        "{mounted:?}"
    );
    hdiutil(&["detach".as_ref(), mount.as_os_str()]);
    wait_until("the unmount", || {
        events.seen.extend(events.receiver.try_iter());
        volumes(&events.seen).contains(&VolumeEvent::Unmounted {
            mount_point: mount.clone(),
        })
    });
}
