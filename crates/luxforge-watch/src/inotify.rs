//! The Linux watcher's logic, apart from its system calls: parsing inotify's event buffer, and the
//! tree of watches it keeps (one per folder, since an inotify watch does not reach below its own
//! folder) with what each event means for the roots. It is plain code over a [`Kernel`] that adds
//! and removes watches, so it is compiled and tested on every platform; `linux.rs` gives it the
//! real calls.
#![forbid(unsafe_code)]
#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "Linux runs it; its tests run everywhere")
)]
use crate::{RescanReason, WatchEvent, changed};
use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
};

// The kernel's event bits (`linux/inotify.h`); `linux.rs` checks them against `rustix`'s at
// compile time.
pub(crate) const IN_ATTRIB: u32 = 0x0000_0004;
pub(crate) const IN_CLOSE_WRITE: u32 = 0x0000_0008;
pub(crate) const IN_MOVED_FROM: u32 = 0x0000_0040;
pub(crate) const IN_MOVED_TO: u32 = 0x0000_0080;
pub(crate) const IN_CREATE: u32 = 0x0000_0100;
pub(crate) const IN_DELETE: u32 = 0x0000_0200;
pub(crate) const IN_DELETE_SELF: u32 = 0x0000_0400;
pub(crate) const IN_MOVE_SELF: u32 = 0x0000_0800;
pub(crate) const IN_UNMOUNT: u32 = 0x0000_2000;
pub(crate) const IN_Q_OVERFLOW: u32 = 0x0000_4000;
pub(crate) const IN_IGNORED: u32 = 0x0000_8000;
pub(crate) const IN_ISDIR: u32 = 0x4000_0000;

/// What each folder's watch asks for: every change to an entry's name, a write finished (not every
/// write, which a large copy makes thousands of), a metadata change such as a new modification
/// time, and the folder itself moving, going or its volume unmounting.
pub(crate) const WATCHED: u32 = IN_ATTRIB
    | IN_CLOSE_WRITE
    | IN_MOVED_FROM
    | IN_MOVED_TO
    | IN_CREATE
    | IN_DELETE
    | IN_DELETE_SELF
    | IN_MOVE_SELF;

/// The fixed part of `struct inotify_event`: `wd`, `mask`, `cookie` and `len`.
const HEADER: usize = 16;

/// One event from inotify's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Event<'a> {
    pub wd: i32,
    pub mask: u32,
    /// The name of the entry inside the watched folder, when the event is about one.
    pub name: Option<&'a [u8]>,
}

/// The events in a buffer `read` from an inotify descriptor, in order: each a 16-byte header in
/// the machine's byte order and a name padded with NULs to `len` bytes. A truncated tail, which
/// the kernel never writes, ends the list.
pub(crate) fn parse(buffer: &[u8]) -> Vec<Event<'_>> {
    let mut events = Vec::new();
    let mut rest = buffer;
    while rest.len() >= HEADER {
        let word = |at: usize| u32::from_ne_bytes([rest[at], rest[at + 1], rest[at + 2], rest[at + 3]]);
        let (wd, mask, length) = (word(0) as i32, word(4), word(12) as usize);
        let Some(name) = rest.get(HEADER..HEADER + length) else {
            break;
        };
        let name = &name[..name.iter().position(|&byte| byte == 0).unwrap_or(length)];
        events.push(Event {
            wd,
            mask,
            name: (!name.is_empty()).then_some(name),
        });
        rest = &rest[HEADER + length..];
    }
    events
}

/// A name from the buffer as a path component.
fn component(name: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(name))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(name).into_owned())
    }
}

/// The system calls the tree makes.
pub(crate) trait Kernel {
    /// Watch `folder` for [`WATCHED`] and answer its watch descriptor, which is the one it
    /// already has when the folder is watched already.
    fn add(&mut self, folder: &Path) -> io::Result<i32>;
    /// Stop watching a descriptor. It may be gone already, when its folder was.
    fn remove(&mut self, wd: i32);
}

/// Every root and every folder watched under it.
#[derive(Default)]
pub(crate) struct Tree {
    roots: BTreeMap<u64, PathBuf>,
    /// Each descriptor's folder, per root: one folder is watched once however many roots it is
    /// under, and a descriptor is removed when no root needs it.
    watches: HashMap<i32, Vec<(u64, PathBuf)>>,
}

/// The events one root is sent: its id, its path and the events.
pub(crate) type Batch = (u64, PathBuf, Vec<WatchEvent>);

impl Tree {
    pub(crate) fn contains(&self, id: u64) -> bool {
        self.roots.contains_key(&id)
    }

    /// Watch `path` and every folder under it, before anything lists it. A limit reached or an
    /// error on the root refuses the root and leaves nothing watched for it; a folder under it that
    /// vanished or cannot be read is skipped, as the listing will skip it.
    pub(crate) fn add_root(&mut self, kernel: &mut impl Kernel, id: u64, path: &Path) -> io::Result<()> {
        if self.contains(id) {
            return Err(crate::already_watched(id));
        }
        if !path.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a folder", path.display()),
            ));
        }
        self.roots.insert(id, path.to_path_buf());
        if let Err(error) = self.watch(kernel, id, path) {
            self.remove_root(kernel, id);
            return Err(error);
        }
        Ok(())
    }

    /// Stop watching the root, and every folder no other root needs.
    pub(crate) fn remove_root(&mut self, kernel: &mut impl Kernel, id: u64) {
        if let Some(path) = self.roots.remove(&id) {
            self.unwatch(kernel, id, &path);
        }
    }

    /// Watch `top` and every folder under it for `root`. The first error that is not a folder under
    /// `top` vanishing or refusing to be read stops it; what was watched until then stays.
    fn watch(&mut self, kernel: &mut impl Kernel, root: u64, top: &Path) -> io::Result<()> {
        // A folder under `top` that vanished or cannot be read is skipped; `top` itself is not.
        let skipped = |error: &io::Error, folder: &Path| {
            folder != top
                && matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                )
        };
        let mut folders = vec![top.to_path_buf()];
        while let Some(folder) = folders.pop() {
            match kernel.add(&folder) {
                Ok(wd) => {
                    let entries = self.watches.entry(wd).or_default();
                    if !entries.iter().any(|(r, f)| *r == root && *f == folder) {
                        entries.push((root, folder.clone()));
                    }
                }
                Err(error) if skipped(&error, &folder) => continue,
                Err(error) if error.kind() == io::ErrorKind::StorageFull => {
                    return Err(io::Error::new(
                        io::ErrorKind::StorageFull,
                        format!(
                            "the inotify watch limit (fs.inotify.max_user_watches) is reached at {}",
                            folder.display()
                        ),
                    ));
                }
                Err(error) => return Err(error),
            }
            let entries = match std::fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(error) if skipped(&error, &folder) => continue,
                Err(error) => return Err(error),
            };
            for entry in entries.flatten() {
                // The type of the entry itself: a link to a folder is not followed.
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    folders.push(entry.path());
                }
            }
        }
        Ok(())
    }

    /// Stop watching `folder` and everything under it for `root`.
    fn unwatch(&mut self, kernel: &mut impl Kernel, root: u64, folder: &Path) {
        let mut unused = Vec::new();
        for (&wd, entries) in &mut self.watches {
            entries.retain(|(r, f)| !(*r == root && f.starts_with(folder)));
            if entries.is_empty() {
                unused.push(wd);
            }
        }
        for wd in unused {
            self.watches.remove(&wd);
            kernel.remove(wd);
        }
    }

    /// What `events` mean for each root, in root order: the paths that may have changed as
    /// `Changed` events, then the subtrees to list again and the folders no longer watched. A
    /// folder created or moved in is watched, with everything under it, before it is reported, so
    /// its listing misses nothing; one removed or moved out is no longer watched.
    pub(crate) fn handle(&mut self, kernel: &mut impl Kernel, events: &[Event<'_>]) -> Vec<Batch> {
        let mut changes: BTreeMap<u64, Vec<PathBuf>> = BTreeMap::new();
        let mut others: BTreeMap<u64, Vec<WatchEvent>> = BTreeMap::new();
        let mut gone = Vec::new();
        let mut new = Vec::new();
        for event in events {
            if event.mask & IN_Q_OVERFLOW != 0 {
                for (&root, path) in &self.roots {
                    others.entry(root).or_default().push(WatchEvent::Rescan {
                        root,
                        subtree: path.clone(),
                        reason: RescanReason::Overflow,
                    });
                }
                continue;
            }
            if event.mask & IN_IGNORED != 0 {
                self.watches.remove(&event.wd);
                continue;
            }
            let Some(entries) = self.watches.get(&event.wd) else {
                continue;
            };
            for (root, folder) in entries {
                if event.mask & (IN_DELETE_SELF | IN_MOVE_SELF | IN_UNMOUNT) != 0 {
                    // A folder under the root moving or going is reported by its parent's event.
                    if self.roots.get(root) == Some(folder) {
                        others.entry(*root).or_default().push(WatchEvent::Rescan {
                            root: *root,
                            subtree: folder.clone(),
                            reason: RescanReason::RootChanged,
                        });
                    }
                    continue;
                }
                let path = event
                    .name
                    .map_or_else(|| folder.clone(), |name| folder.join(component(name)));
                if event.mask & IN_ISDIR != 0 {
                    if event.mask & (IN_DELETE | IN_MOVED_FROM) != 0 {
                        gone.push((*root, path.clone()));
                    }
                    if event.mask & (IN_CREATE | IN_MOVED_TO) != 0 {
                        new.push((*root, path.clone()));
                    }
                }
                changes.entry(*root).or_default().push(path);
            }
        }
        for (root, folder) in gone {
            self.unwatch(kernel, root, &folder);
        }
        for (root, folder) in new {
            if !self.roots.contains_key(&root) {
                continue;
            }
            if let Err(error) = self.watch(kernel, root, &folder) {
                others.entry(root).or_default().push(WatchEvent::Unwatched {
                    root,
                    subtree: folder,
                    error: error.to_string(),
                });
            }
        }
        let mut batches = Vec::new();
        for (&root, path) in &self.roots {
            let mut events = changed(root, changes.remove(&root).unwrap_or_default(), None);
            events.extend(others.remove(&root).unwrap_or_default());
            if !events.is_empty() {
                batches.push((root, path.clone(), events));
            }
        }
        batches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Hands out descriptors, one per folder as the kernel does, up to a limit.
    #[derive(Default)]
    struct FakeKernel {
        next: i32,
        limit: Option<usize>,
        watched: BTreeMap<PathBuf, i32>,
    }

    impl Kernel for FakeKernel {
        fn add(&mut self, folder: &Path) -> io::Result<i32> {
            if let Some(&wd) = self.watched.get(folder) {
                return Ok(wd);
            }
            if !folder.is_dir() {
                return Err(io::ErrorKind::NotFound.into());
            }
            if self.limit.is_some_and(|limit| self.watched.len() >= limit) {
                return Err(io::ErrorKind::StorageFull.into());
            }
            self.next += 1;
            self.watched.insert(folder.to_path_buf(), self.next);
            Ok(self.next)
        }

        fn remove(&mut self, wd: i32) {
            self.watched.retain(|_, watched| *watched != wd);
        }
    }

    impl FakeKernel {
        fn wd(&self, folder: &Path) -> i32 {
            self.watched[folder]
        }

        fn folders(&self) -> BTreeSet<PathBuf> {
            self.watched.keys().cloned().collect()
        }
    }

    fn event(wd: i32, mask: u32, name: &str) -> (i32, u32, String) {
        (wd, mask, name.to_owned())
    }

    /// A buffer as the kernel writes it, each name padded with NULs to a multiple of 4.
    fn buffer(events: &[(i32, u32, String)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (wd, mask, name) in events {
            let length = if name.is_empty() {
                0
            } else {
                (name.len() + 1).next_multiple_of(4)
            };
            bytes.extend_from_slice(&wd.to_ne_bytes());
            bytes.extend_from_slice(&mask.to_ne_bytes());
            bytes.extend_from_slice(&0_u32.to_ne_bytes());
            bytes.extend_from_slice(&(length as u32).to_ne_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.resize(bytes.len() + length - name.len(), 0);
        }
        bytes
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = luxforge_testbase::paths::temp_dir(name);
        dir.canonicalize().unwrap()
    }

    #[test]
    fn the_buffer_parses_into_events_with_their_names() {
        let bytes = buffer(&[
            event(1, IN_CREATE, "DSC_0001.NEF"),
            event(2, IN_DELETE_SELF, ""),
            event(-1, IN_Q_OVERFLOW, ""),
        ]);
        let events = parse(&bytes);
        assert_eq!(
            events,
            [
                Event {
                    wd: 1,
                    mask: IN_CREATE,
                    name: Some(b"DSC_0001.NEF".as_slice())
                },
                Event {
                    wd: 2,
                    mask: IN_DELETE_SELF,
                    name: None
                },
                Event {
                    wd: -1,
                    mask: IN_Q_OVERFLOW,
                    name: None
                },
            ]
        );
        // A truncated tail ends the list.
        assert_eq!(parse(&bytes[..bytes.len() - 4]).len(), 2);
        assert_eq!(parse(&bytes[..20]).len(), 0);
    }

    #[test]
    fn a_root_is_watched_folder_by_folder_without_following_links() {
        let root = scratch("inotify-walk");
        let outside = scratch("inotify-outside");
        std::fs::create_dir_all(root.join("2024/09")).unwrap();
        std::fs::create_dir_all(root.join("2025")).unwrap();
        std::fs::write(root.join("2024/a.jpg"), b"a").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let (mut tree, mut kernel) = (Tree::default(), FakeKernel::default());
        tree.add_root(&mut kernel, 1, &root).unwrap();
        assert_eq!(
            kernel.folders(),
            BTreeSet::from([
                root.clone(),
                root.join("2024"),
                root.join("2024/09"),
                root.join("2025")
            ])
        );
        assert_eq!(
            tree.add_root(&mut kernel, 1, &root).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            tree.add_root(&mut kernel, 2, &root.join("2024/a.jpg"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotADirectory
        );
        tree.remove_root(&mut kernel, 1);
        assert!(kernel.folders().is_empty() && tree.watches.is_empty());
    }

    #[test]
    fn changes_are_reported_under_their_root_and_new_folders_are_watched_first() {
        let root = scratch("inotify-events");
        std::fs::create_dir_all(root.join("old/inner")).unwrap();
        let (mut tree, mut kernel) = (Tree::default(), FakeKernel::default());
        tree.add_root(&mut kernel, 7, &root).unwrap();
        let top = kernel.wd(&root);
        // A folder arrives with a folder already inside it, and a file is written.
        std::fs::create_dir_all(root.join("new/deeper")).unwrap();
        let bytes = buffer(&[
            event(top, IN_CREATE | IN_ISDIR, "new"),
            event(top, IN_CLOSE_WRITE, "a.jpg"),
            event(top, IN_CLOSE_WRITE, "a.jpg"),
        ]);
        let batches = tree.handle(&mut kernel, &parse(&bytes));
        assert_eq!(
            batches,
            [(
                7,
                root.clone(),
                vec![WatchEvent::Changed {
                    root: 7,
                    paths: vec![root.join("new"), root.join("a.jpg")],
                    cursor: None
                }]
            )]
        );
        assert!(kernel.folders().contains(&root.join("new/deeper")));
        // A folder moved within the root: its old watches go, its new place is watched.
        std::fs::rename(root.join("old"), root.join("moved")).unwrap();
        let bytes = buffer(&[
            event(top, IN_MOVED_FROM | IN_ISDIR, "old"),
            event(top, IN_MOVED_TO | IN_ISDIR, "moved"),
        ]);
        tree.handle(&mut kernel, &parse(&bytes));
        let folders = kernel.folders();
        assert!(!folders.contains(&root.join("old/inner")), "{folders:?}");
        assert!(folders.contains(&root.join("moved/inner")), "{folders:?}");
        // A file inside the moved folder is named under its new place.
        let inner = kernel.wd(&root.join("moved/inner"));
        let bytes = buffer(&[event(inner, IN_DELETE, "b.jpg")]);
        assert_eq!(
            tree.handle(&mut kernel, &parse(&bytes))[0].2,
            [WatchEvent::Changed {
                root: 7,
                paths: vec![root.join("moved/inner/b.jpg")],
                cursor: None
            }]
        );
    }

    #[test]
    fn an_overflow_rescans_every_root_and_the_root_going_is_a_root_change() {
        let first = scratch("inotify-first");
        let second = scratch("inotify-second");
        std::fs::create_dir(first.join("sub")).unwrap();
        let (mut tree, mut kernel) = (Tree::default(), FakeKernel::default());
        tree.add_root(&mut kernel, 1, &first).unwrap();
        tree.add_root(&mut kernel, 2, &second).unwrap();
        let rescans = tree.handle(&mut kernel, &parse(&buffer(&[event(-1, IN_Q_OVERFLOW, "")])));
        assert_eq!(
            rescans,
            [
                (
                    1,
                    first.clone(),
                    vec![WatchEvent::Rescan {
                        root: 1,
                        subtree: first.clone(),
                        reason: RescanReason::Overflow
                    }]
                ),
                (
                    2,
                    second.clone(),
                    vec![WatchEvent::Rescan {
                        root: 2,
                        subtree: second.clone(),
                        reason: RescanReason::Overflow
                    }]
                ),
            ]
        );
        // The root's own watch reports it moved; a folder under it moving says nothing itself.
        let bytes = buffer(&[
            event(kernel.wd(&first.join("sub")), IN_MOVE_SELF, ""),
            event(kernel.wd(&first), IN_MOVE_SELF, ""),
        ]);
        assert_eq!(
            tree.handle(&mut kernel, &parse(&bytes)),
            [(
                1,
                first.clone(),
                vec![WatchEvent::Rescan {
                    root: 1,
                    subtree: first.clone(),
                    reason: RescanReason::RootChanged
                }]
            )]
        );
        // The kernel dropping a watch forgets it; its later events are ignored.
        let wd = kernel.wd(&second);
        let bytes = buffer(&[event(wd, IN_IGNORED, ""), event(wd, IN_CREATE, "x")]);
        assert!(tree.handle(&mut kernel, &parse(&bytes)).is_empty());
    }

    #[test]
    fn nested_roots_share_a_watch_and_keep_it_until_neither_needs_it() {
        let outer = scratch("inotify-nested");
        let inner = outer.join("2024");
        std::fs::create_dir(&inner).unwrap();
        let (mut tree, mut kernel) = (Tree::default(), FakeKernel::default());
        tree.add_root(&mut kernel, 1, &outer).unwrap();
        tree.add_root(&mut kernel, 2, &inner).unwrap();
        let bytes = buffer(&[event(kernel.wd(&inner), IN_CREATE, "a.jpg")]);
        let roots: Vec<u64> = tree
            .handle(&mut kernel, &parse(&bytes))
            .into_iter()
            .map(|(root, ..)| root)
            .collect();
        assert_eq!(roots, [1, 2], "both roots hear of it");
        tree.remove_root(&mut kernel, 1);
        assert_eq!(kernel.folders(), BTreeSet::from([inner.clone()]));
        tree.remove_root(&mut kernel, 2);
        assert!(kernel.folders().is_empty());
    }

    #[test]
    fn the_watch_limit_is_reported_not_hidden() {
        let root = scratch("inotify-limit");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        let mut tree = Tree::default();
        let mut kernel = FakeKernel {
            limit: Some(2),
            ..FakeKernel::default()
        };
        // A root that does not fit is refused and leaves nothing watched.
        let error = tree.add_root(&mut kernel, 1, &root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert!(error.to_string().contains("max_user_watches"), "{error}");
        assert!(kernel.folders().is_empty() && !tree.contains(1));
        // A folder arriving past the limit is reported unwatched.
        kernel.limit = Some(3);
        tree.add_root(&mut kernel, 1, &root).unwrap();
        std::fs::create_dir(root.join("c")).unwrap();
        let bytes = buffer(&[event(kernel.wd(&root), IN_CREATE | IN_ISDIR, "c")]);
        let events = &tree.handle(&mut kernel, &parse(&bytes))[0].2;
        assert!(
            matches!(&events[1], WatchEvent::Unwatched { subtree, error, .. }
                if *subtree == root.join("c") && error.contains("max_user_watches")),
            "{events:?}"
        );
    }
}
