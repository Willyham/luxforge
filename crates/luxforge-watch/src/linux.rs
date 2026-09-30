//! Linux: inotify, one watch per folder (the tree in `inotify.rs`), and `/proc/self/mountinfo`,
//! which reports a mount or unmount as a priority event, both read by one thread blocking in
//! `poll` beside an eventfd that stops it. It blocks with no timeout unless an event is owed to a
//! full channel, when it wakes after [`RETRY`] to send it. inotify keeps no history, so every root
//! is listed when it is added ([`RescanReason::NoReplay`]) and a cursor is never given.
//!
//! Every call here is `rustix`'s safe API, so this module holds no `unsafe`.
#![forbid(unsafe_code)]
use crate::{
    RETRY, RescanReason, Resume, WatchEvent, WatchRoot,
    delivery::{Delivery, Sink},
    inotify::{self, Kernel, Tree},
    mounts::{Mount, parse_mountinfo},
};
use rustix::{
    event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll},
    fs::inotify as sys,
    io::Errno,
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    os::fd::OwnedFd,
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
};

/// How much one `read` of the inotify descriptor takes: a few hundred events.
const BUFFER: usize = 64 * 1024;

// The event bits `inotify.rs` spells out are the kernel's.
const _: () = {
    assert!(inotify::IN_ATTRIB == sys::ReadFlags::ATTRIB.bits());
    assert!(inotify::IN_CLOSE_WRITE == sys::ReadFlags::CLOSE_WRITE.bits());
    assert!(inotify::IN_MOVED_FROM == sys::ReadFlags::MOVED_FROM.bits());
    assert!(inotify::IN_MOVED_TO == sys::ReadFlags::MOVED_TO.bits());
    assert!(inotify::IN_CREATE == sys::ReadFlags::CREATE.bits());
    assert!(inotify::IN_DELETE == sys::ReadFlags::DELETE.bits());
    assert!(inotify::IN_DELETE_SELF == sys::ReadFlags::DELETE_SELF.bits());
    assert!(inotify::IN_MOVE_SELF == sys::ReadFlags::MOVE_SELF.bits());
    assert!(inotify::IN_UNMOUNT == sys::ReadFlags::UNMOUNT.bits());
    assert!(inotify::IN_Q_OVERFLOW == sys::ReadFlags::QUEUE_OVERFLOW.bits());
    assert!(inotify::IN_IGNORED == sys::ReadFlags::IGNORED.bits());
    assert!(inotify::IN_ISDIR == sys::ReadFlags::ISDIR.bits());
};

/// What the caller's thread and the watcher's thread share.
struct Shared {
    inotify: OwnedFd,
    /// Held while events are mapped and sent, so a root removed sends nothing after.
    tree: Mutex<Tree>,
    delivery: Delivery,
}

impl Shared {
    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// inotify's calls on the watcher's descriptor.
struct Calls<'a>(&'a OwnedFd);

impl Kernel for Calls<'_> {
    fn add(&mut self, folder: &Path) -> io::Result<i32> {
        let flags = sys::WatchFlags::from_bits_retain(inotify::WATCHED)
            | sys::WatchFlags::ONLYDIR
            | sys::WatchFlags::DONT_FOLLOW
            | sys::WatchFlags::EXCL_UNLINK;
        // The limit on watches is `ENOSPC`, which is `StorageFull`, as the tree expects.
        Ok(sys::add_watch(self.0, folder, flags)?)
    }

    fn remove(&mut self, wd: i32) {
        let _ = sys::remove_watch(self.0, wd);
    }
}

pub(crate) struct Watcher {
    shared: Arc<Shared>,
    stop: Arc<OwnedFd>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    pub(crate) fn start(sink: Sink) -> io::Result<Self> {
        let inotify = sys::init(sys::CreateFlags::CLOEXEC | sys::CreateFlags::NONBLOCK)?;
        let stop = Arc::new(eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?);
        // Without `/proc` no mount is reported; folders are still watched.
        let mut mountinfo = File::open("/proc/self/mountinfo").ok();
        let table = mountinfo
            .as_mut()
            .and_then(|file| read_table(file).ok())
            .unwrap_or_default();
        let shared = Arc::new(Shared {
            inotify,
            tree: Mutex::default(),
            delivery: Delivery::new(sink, table),
        });
        let thread = thread::Builder::new()
            .name("luxforge-watch".into())
            .spawn({
                let (shared, stop) = (shared.clone(), stop.clone());
                move || run(&shared, &stop, mountinfo)
            })?;
        Ok(Self {
            shared,
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn add_root(&mut self, root: WatchRoot) -> io::Result<()> {
        let mut tree = self.shared.tree();
        tree.add_root(&mut Calls(&self.shared.inotify), root.id, &root.path)?;
        // Every folder is watched now, so nothing the listing reads can change unreported.
        let first = vec![
            WatchEvent::Rescan {
                root: root.id,
                subtree: root.path.clone(),
                reason: RescanReason::NoReplay,
            },
            WatchEvent::CaughtUp {
                root: root.id,
                cursor: None,
            },
        ];
        self.shared.delivery.root(root.id, &root.path, first);
        Ok(())
    }

    pub(crate) fn remove_root(&mut self, id: u64) {
        let mut tree = self.shared.tree();
        tree.remove_root(&mut Calls(&self.shared.inotify), id);
        self.shared.delivery.forget(id);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = rustix::io::write(&*self.stop, &1_u64.to_ne_bytes());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn current_cursor(_path: &Path) -> Option<Resume> {
    None
}

/// The watcher's thread: wait for inotify, the mount table or the stop, and handle what is ready.
fn run(shared: &Shared, stop: &OwnedFd, mut mountinfo: Option<File>) {
    let mut buffer = vec![0_u8; BUFFER];
    let retry = Timespec {
        tv_sec: 0,
        tv_nsec: RETRY.as_nanos() as _,
    };
    loop {
        let timeout = shared.delivery.owed().then_some(&retry);
        let mut fds = vec![
            PollFd::new(&shared.inotify, PollFlags::IN),
            PollFd::new(stop, PollFlags::IN),
        ];
        if let Some(file) = &mountinfo {
            fds.push(PollFd::new(file, PollFlags::PRI));
        }
        match poll(&mut fds, timeout) {
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            // Nothing can be waited on any more; the watcher's owner finds out when it drops it.
            Err(_) => return,
        }
        let ready = |at: usize| fds.get(at).map_or(PollFlags::empty(), PollFd::revents);
        let (events, stopping, mounts) = (ready(0), ready(1), ready(2));
        drop(fds);
        if !stopping.is_empty() {
            return;
        }
        if events.contains(PollFlags::IN) {
            drain(shared, &mut buffer);
        }
        let remount =
            mounts.intersects(PollFlags::PRI | PollFlags::ERR) || shared.delivery.volumes_owed();
        if let Some(file) = mountinfo.as_mut().filter(|_| remount) {
            match read_table(file) {
                Ok(table) => {
                    shared.delivery.volumes(&table);
                }
                Err(_) => shared.delivery.owe_volumes(),
            }
        }
        shared.delivery.retry();
    }
}

/// Read every event inotify holds and send what it means.
fn drain(shared: &Shared, buffer: &mut [u8]) {
    loop {
        match rustix::io::read(&shared.inotify, &mut *buffer) {
            Ok(0) => return,
            Ok(read) => {
                let events = inotify::parse(&buffer[..read]);
                let mut tree = shared.tree();
                for (root, path, events) in tree.handle(&mut Calls(&shared.inotify), &events) {
                    shared.delivery.root(root, &path, events);
                }
            }
            Err(Errno::INTR) => {}
            // `EAGAIN`: nothing more for now.
            Err(_) => return,
        }
    }
}

/// The mount table from the start of `/proc/self/mountinfo`; reading it again also clears its
/// priority event.
fn read_table(file: &mut File) -> io::Result<Vec<Mount>> {
    file.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(parse_mountinfo(&text))
}
