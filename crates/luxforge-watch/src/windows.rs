//! Windows: each root's folder is read by one overlapped `ReadDirectoryChangesW` over its whole
//! subtree, completing on one I/O completion port, and the configuration manager's volume
//! notifications (`CM_Register_Notification` for the volume interface, called on the system's
//! thread pool) only post to the same port. One thread blocks on the port with no timeout, except
//! while an event is owed to a full channel (it wakes after [`RETRY`]) or for a few seconds after a
//! volume arrives or leaves: a drive letter is assigned after its volume arrives, so the table is
//! read again [`SETTLE_READS`] times, [`SETTLE`] apart. Windows keeps no history here, so every root
//! is listed when it is added ([`RescanReason::NoReplay`]).
//!
//! This module is compiled and checked on every change; the record parsing it relies on is tested
//! on every platform (`notify_information.rs`). It has not yet been run on Windows.
use crate::{
    RETRY, RescanReason, Resume, WatchEvent, WatchRoot, already_watched, changed,
    delivery::{Delivery, Sink},
    mounts,
    notify_information::{parse, path},
};
use std::{
    collections::HashMap,
    ffi::c_void,
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::Duration,
};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::{
        CM_NOTIFY_ACTION, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER,
        CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification,
        CM_Unregister_Notification, CR_SUCCESS, HCMNOTIFICATION,
    },
    Foundation::{
        CloseHandle, ERROR_NOTIFY_ENUM_DIR, ERROR_SUCCESS, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
        FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
    },
    System::{
        IO::{
            CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
            PostQueuedCompletionStatus,
        },
        Ioctl::GUID_DEVINTERFACE_VOLUME,
    },
};

/// Wait with no timeout (`INFINITE`, which `windows-sys` puts behind its threading feature).
const INFINITE: u32 = u32::MAX;

/// The completion keys: the watcher stopping, a volume arriving or leaving, a folder read.
const STOP: usize = 0;
const VOLUMES: usize = 1;
const FOLDER: usize = 2;

/// Each read's buffer: the most a read over the network may take.
const BUFFER: usize = 64 * 1024;

/// How long after a volume notification the drive letters are read again, and how many times.
const SETTLE: Duration = Duration::from_millis(500);
const SETTLE_READS: u32 = 4;

/// What a read reports: names, a write's size and time, attributes and creation.
const CHANGES: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_ATTRIBUTES
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_CREATION;

/// A handle, closed when dropped.
struct Owned(HANDLE);

// SAFETY: a kernel handle is valid on every thread of the process; it is closed once, by its one
// owner.
unsafe impl Send for Owned {}
// SAFETY: as above; the calls made through a shared handle (posting to the port, cancelling a
// read) are thread-safe.
unsafe impl Sync for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned here alone.
        unsafe { CloseHandle(self.0) };
    }
}

/// One root's read. It is boxed and never moved while the system may write it.
#[repr(C)]
struct Read {
    /// First, so the `OVERLAPPED` pointer the port answers with is this read's address.
    overlapped: OVERLAPPED,
    /// `u64`s so the records are aligned as the system requires.
    buffer: Vec<u64>,
    folder: Owned,
    root: u64,
    path: PathBuf,
}

// SAFETY: the `OVERLAPPED` and the buffer are written by the system only while a read is in
// flight, and touched otherwise only by the thread holding the reads' lock.
unsafe impl Send for Read {}

impl Read {
    /// Start the next read.
    fn issue(&mut self) -> io::Result<()> {
        self.overlapped = OVERLAPPED::default();
        // SAFETY: the folder is open for listing and overlapped; the buffer holds `BUFFER` bytes
        // and it and the `OVERLAPPED` stay in place, unmoved in their box, until the port answers
        // for this read.
        let issued = unsafe {
            ReadDirectoryChangesW(
                self.folder.0,
                self.buffer.as_mut_ptr().cast(),
                BUFFER as u32,
                1,
                CHANGES,
                std::ptr::null_mut(),
                &raw mut self.overlapped,
                None,
            )
        };
        if issued == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The records the system wrote.
    fn filled(&self, bytes: usize) -> &[u8] {
        // SAFETY: the read completed, so the system no longer writes the buffer, and `bytes` is
        // clamped to its length; a `u64` buffer is readable as bytes.
        unsafe { std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), bytes.min(BUFFER)) }
    }
}

/// The reads, by their `OVERLAPPED`'s address.
#[derive(Default)]
struct Reads {
    live: HashMap<usize, Box<Read>>,
    /// Cancelled reads, kept until the port answers for them.
    cancelled: HashMap<usize, Box<Read>>,
    /// Each root's read, or `None` once its folder could no longer be read.
    roots: HashMap<u64, Option<usize>>,
    stopping: bool,
}

struct Shared {
    port: Owned,
    reads: Mutex<Reads>,
    delivery: Delivery,
}

impl Shared {
    fn reads(&self) -> MutexGuard<'_, Reads> {
        self.reads.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn volumes(&self) {
        match mounts() {
            Ok(table) => {
                self.delivery.volumes(&table);
            }
            Err(_) => self.delivery.owe_volumes(),
        }
    }
}

/// The volume notification's registration.
struct Notification(HCMNOTIFICATION);

// SAFETY: the registration is a handle the configuration manager accepts from any thread.
unsafe impl Send for Notification {}

impl Drop for Notification {
    fn drop(&mut self) {
        // SAFETY: registered once and unregistered once, never from its own callback; this waits
        // for callbacks running, after which the port it posts to may close.
        unsafe { CM_Unregister_Notification(self.0) };
    }
}

pub(crate) struct Watcher {
    shared: Arc<Shared>,
    notification: Option<Notification>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    pub(crate) fn start(sink: Sink) -> io::Result<Self> {
        // SAFETY: a new port, associated with no file yet, for one thread.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        let shared = Arc::new(Shared {
            port: Owned(port),
            reads: Mutex::default(),
            delivery: Delivery::new(sink, mounts().unwrap_or_default()),
        });
        let notification = register(&shared.port)?;
        let thread = thread::Builder::new()
            .name("luxforge-watch".into())
            .spawn({
                let shared = shared.clone();
                move || run(&shared)
            })?;
        Ok(Self {
            shared,
            notification: Some(notification),
            thread: Some(thread),
        })
    }

    pub(crate) fn add_root(&mut self, root: WatchRoot) -> io::Result<()> {
        if !std::fs::metadata(&root.path)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a folder", root.path.display()),
            ));
        }
        let mut reads = self.shared.reads();
        if reads.roots.contains_key(&root.id) {
            return Err(already_watched(root.id));
        }
        let mut read = Box::new(Read {
            overlapped: OVERLAPPED::default(),
            buffer: vec![0; BUFFER / 8],
            folder: open(&root.path)?,
            root: root.id,
            path: root.path.clone(),
        });
        // SAFETY: both handles are open; the folder's completions go to the port with its key.
        if unsafe { CreateIoCompletionPort(read.folder.0, self.shared.port.0, FOLDER, 0) }.is_null()
        {
            return Err(io::Error::last_os_error());
        }
        read.issue()?;
        let key = (&raw const *read) as usize;
        reads.live.insert(key, read);
        reads.roots.insert(root.id, Some(key));
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
        let mut reads = self.shared.reads();
        if let Some(Some(key)) = reads.roots.remove(&id) {
            cancel(&mut reads, key);
        }
        self.shared.delivery.forget(id);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Nothing posts to the port once this returns.
        drop(self.notification.take());
        {
            let mut reads = self.shared.reads();
            let keys: Vec<usize> = reads.live.keys().copied().collect();
            for key in keys {
                cancel(&mut reads, key);
            }
            reads.stopping = true;
        }
        // SAFETY: the port is open; the packet only wakes the thread.
        unsafe { PostQueuedCompletionStatus(self.shared.port.0, 0, STOP, std::ptr::null()) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn current_cursor(_path: &Path) -> Option<Resume> {
    None
}

/// Open `folder` for overlapped change reads, sharing it fully so nothing else is kept from it.
fn open(folder: &Path) -> io::Result<Owned> {
    let wide: Vec<u16> = folder.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path; no security attributes or template.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Owned(handle))
}

/// Cancel a live read; it is freed when the port answers for it.
fn cancel(reads: &mut Reads, key: usize) {
    if let Some(read) = reads.live.remove(&key) {
        // SAFETY: the folder is open and the read is in flight on it; if it completed already,
        // the call fails harmlessly and its completion is still to come.
        unsafe { CancelIoEx(read.folder.0, &raw const read.overlapped) };
        reads.cancelled.insert(key, read);
    }
}

/// Register for volumes arriving and leaving; the callback posts to `port`.
fn register(port: &Owned) -> io::Result<Notification> {
    let mut filter = CM_NOTIFY_FILTER {
        cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
        FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        ..CM_NOTIFY_FILTER::default()
    };
    filter.u.DeviceInterface.ClassGuid = GUID_DEVINTERFACE_VOLUME;
    let mut handle: HCMNOTIFICATION = std::ptr::null_mut();
    // SAFETY: the filter is complete; the context is the port's handle, open until after the
    // registration is dropped; the callback has the signature the configuration manager calls.
    let status = unsafe {
        CM_Register_Notification(
            &raw const filter,
            port.0.cast_const(),
            Some(volume_changed),
            &raw mut handle,
        )
    };
    if status != CR_SUCCESS {
        return Err(io::Error::other(format!(
            "the configuration manager refused volume notifications ({status})"
        )));
    }
    Ok(Notification(handle))
}

unsafe extern "system" fn volume_changed(
    _notification: HCMNOTIFICATION,
    context: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    // SAFETY: the context is the port's handle, open for as long as the registration.
    unsafe { PostQueuedCompletionStatus(context.cast_mut(), 0, VOLUMES, std::ptr::null()) };
    ERROR_SUCCESS
}

/// The watcher's thread.
fn run(shared: &Shared) {
    let mut settle = 0;
    loop {
        let timeout = if shared.delivery.owed() {
            RETRY.as_millis() as u32
        } else if settle > 0 {
            SETTLE.as_millis() as u32
        } else {
            INFINITE
        };
        let (mut bytes, mut key) = (0_u32, 0_usize);
        let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
        // SAFETY: the port is open until this thread is joined; the outputs are plain values.
        let dequeued = unsafe {
            GetQueuedCompletionStatus(
                shared.port.0,
                &raw mut bytes,
                &raw mut key,
                &raw mut overlapped,
                timeout,
            )
        } != 0;
        // SAFETY: reads the calling thread's last error, set by the call above when it failed.
        let error = (!dequeued).then(|| unsafe { GetLastError() });
        if !overlapped.is_null() {
            completed(shared, overlapped as usize, bytes as usize, error);
        } else if error == Some(WAIT_TIMEOUT) {
            if settle > 0 {
                settle -= 1;
                shared.volumes();
            }
        } else if error.is_some() {
            // The port itself failed: nothing more can be waited on.
            return;
        } else if key == VOLUMES {
            settle = SETTLE_READS;
            shared.volumes();
        }
        if shared.delivery.volumes_owed() {
            shared.volumes();
        }
        shared.delivery.retry();
        let reads = shared.reads();
        if reads.stopping && reads.live.is_empty() && reads.cancelled.is_empty() {
            return;
        }
    }
}

/// The port answered for the read at `key`: send what it read and start the next, or free it if
/// it was cancelled.
fn completed(shared: &Shared, key: usize, bytes: usize, error: Option<u32>) {
    let mut reads = shared.reads();
    if reads.cancelled.remove(&key).is_some() {
        return;
    }
    let Some(mut read) = reads.live.remove(&key) else {
        return;
    };
    let (root, folder) = (read.root, read.path.clone());
    let rescan = |reason| WatchEvent::Rescan {
        root,
        subtree: folder.clone(),
        reason,
    };
    let mut events = Vec::new();
    let failed = match error {
        // No records: more changed than the buffer holds.
        None if bytes == 0 => {
            events.push(rescan(RescanReason::Overflow));
            None
        }
        None => {
            let paths = parse(read.filled(bytes))
                .iter()
                .map(|record| path(&folder, record))
                .collect();
            events.extend(changed(root, paths, None));
            None
        }
        Some(ERROR_NOTIFY_ENUM_DIR) => {
            events.push(rescan(RescanReason::Overflow));
            None
        }
        // The folder was removed, or its volume or share went away.
        Some(code) => Some(io::Error::from_raw_os_error(code as i32)),
    };
    let failed = failed.or_else(|| read.issue().err());
    match failed {
        None => {
            reads.live.insert(key, read);
        }
        Some(error) => {
            events.push(rescan(RescanReason::RootChanged));
            events.push(WatchEvent::Unwatched {
                root,
                subtree: folder.clone(),
                error: error.to_string(),
            });
            reads.roots.insert(root, None);
        }
    }
    shared.delivery.root(root, &folder, events);
}
