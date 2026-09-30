//! Disk Arbitration: a session on the watcher's queue that is called back when a disk appears or
//! disappears or its volume path is set or cleared, which is when a volume is mounted or
//! unmounted. Each call reads the mount table and tells the receiver how it differs from the one
//! it knows, so the burst of "appeared" for the disks already there when the session starts tells
//! nothing, and a missed or refused call is made good by the next.
//!
//! `objc2-disk-arbitration` does not declare the `DARegister…` callbacks, so the few functions
//! used are declared here, as `DiskArbitration/DiskArbitration.h` has them.
use super::Shared;
use dispatch2::DispatchQueue;
use objc2_core_foundation::{CFArray, CFRetained, CFString};
use std::{
    ffi::c_void,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    sync::Arc,
};

/// `DADiskAppearedCallback` and `DADiskDisappearedCallback`.
type DiskCallback = unsafe extern "C" fn(disk: *mut c_void, context: *mut c_void);
/// `DADiskDescriptionChangedCallback`.
type DescriptionCallback =
    unsafe extern "C" fn(disk: *mut c_void, keys: *const c_void, context: *mut c_void);

#[link(name = "DiskArbitration", kind = "framework")]
unsafe extern "C" {
    fn DASessionCreate(allocator: *const c_void) -> *mut c_void;
    fn DASessionSetDispatchQueue(session: *mut c_void, queue: Option<&DispatchQueue>);
    fn DARegisterDiskAppearedCallback(
        session: *mut c_void,
        filter: *const c_void,
        callback: DiskCallback,
        context: *mut c_void,
    );
    fn DARegisterDiskDisappearedCallback(
        session: *mut c_void,
        filter: *const c_void,
        callback: DiskCallback,
        context: *mut c_void,
    );
    fn DARegisterDiskDescriptionChangedCallback(
        session: *mut c_void,
        filter: *const c_void,
        watch: *const c_void,
        callback: DescriptionCallback,
        context: *mut c_void,
    );
    fn DAUnregisterCallback(session: *mut c_void, callback: *mut c_void, context: *mut c_void);
    /// The description key of a disk's mounted volume's path (a `CFURL`), a `CFString` constant.
    static kDADiskDescriptionVolumePathKey: &'static CFString;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
}

/// A Disk Arbitration session delivering on the watcher's queue.
pub(super) struct Session {
    session: NonNull<c_void>,
    /// The `Shared` the callbacks are given, as `Arc::into_raw` made it.
    context: *const Shared,
    /// The keys the description callback watches, kept for as long as it is registered.
    _keys: CFRetained<CFArray<CFString>>,
}

// SAFETY: a Disk Arbitration session is a CoreFoundation object whose functions may be called
// from any thread; the watcher that owns it calls them only through `&mut`, one thread at a time,
// and the context it points at is `Sync`.
unsafe impl Send for Session {}

/// A stopped session's context, dropped after the barrier that follows the stop.
pub(super) struct Retired(#[allow(dead_code, reason = "held only to be dropped")] Arc<Shared>);

impl Session {
    pub(super) fn start(shared: &Arc<Shared>) -> io::Result<Self> {
        // SAFETY: a null allocator is the default one; the call answers a new session or null.
        let session = NonNull::new(unsafe { DASessionCreate(std::ptr::null()) })
            .ok_or_else(|| io::Error::other("Disk Arbitration refused a session"))?;
        // SAFETY: the key is a constant CFString the framework defines for the process's life.
        let key: &CFString = unsafe { kDADiskDescriptionVolumePathKey };
        let keys = CFArray::from_objects(&[key]);
        let context = Arc::into_raw(shared.clone());
        let raw = context.cast_mut().cast::<c_void>();
        // SAFETY: the session is valid; a null filter matches every disk; each callback has the
        // signature Disk Arbitration calls; `keys` is a CFArray of CFStrings kept alive while the
        // callback is registered; the context stays valid until after the barrier that follows
        // `stop`; and the queue outlives the session.
        unsafe {
            DARegisterDiskAppearedCallback(session.as_ptr(), std::ptr::null(), appeared, raw);
            DARegisterDiskDisappearedCallback(session.as_ptr(), std::ptr::null(), disappeared, raw);
            DARegisterDiskDescriptionChangedCallback(
                session.as_ptr(),
                std::ptr::null(),
                (&raw const *keys).cast(),
                described,
                raw,
            );
            DASessionSetDispatchQueue(session.as_ptr(), Some(&shared.queue));
        }
        Ok(Self {
            session,
            context,
            _keys: keys,
        })
    }

    /// Unregister the callbacks, take the session off the queue and release it. A callback may
    /// still be running or queued until the watcher's next barrier, so the context it reads is
    /// handed back to be dropped after it.
    pub(super) fn stop(self) -> Retired {
        let raw = self.context.cast_mut().cast::<c_void>();
        // SAFETY: each callback was registered with this context on this session, which is
        // released exactly once, after it leaves the queue; the context came from
        // `Arc::into_raw` and is turned back into its `Arc` exactly once.
        unsafe {
            DAUnregisterCallback(self.session.as_ptr(), appeared as *mut c_void, raw);
            DAUnregisterCallback(self.session.as_ptr(), disappeared as *mut c_void, raw);
            DAUnregisterCallback(self.session.as_ptr(), described as *mut c_void, raw);
            DASessionSetDispatchQueue(self.session.as_ptr(), None);
            CFRelease(self.session.as_ptr().cast_const());
            Retired(Arc::from_raw(self.context))
        }
    }
}

/// Tell the receiver how the mount table changed.
///
/// # Safety
///
/// `context` is the `Shared` a live session was registered with.
unsafe fn changed(context: *mut c_void) {
    // SAFETY: the caller's contract; the `Shared` outlives every callback of the session.
    let shared = unsafe { &*context.cast::<Shared>().cast_const() };
    // A panic must not unwind into Disk Arbitration; the next change tells what this one missed.
    let _ = catch_unwind(AssertUnwindSafe(|| shared.volumes()));
}

unsafe extern "C" fn appeared(_disk: *mut c_void, context: *mut c_void) {
    // SAFETY: registered with the session's context.
    unsafe { changed(context) };
}

unsafe extern "C" fn disappeared(_disk: *mut c_void, context: *mut c_void) {
    // SAFETY: registered with the session's context.
    unsafe { changed(context) };
}

unsafe extern "C" fn described(_disk: *mut c_void, _keys: *const c_void, context: *mut c_void) {
    // SAFETY: registered with the session's context.
    unsafe { changed(context) };
}
