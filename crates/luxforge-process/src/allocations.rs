//! A counting wrapper around the system allocator, for a test binary that holds a code path to an
//! allocation budget.
//!
//! The workspace forbids `unsafe` everywhere but the FFI crates, and implementing [`GlobalAlloc`] is
//! `unsafe`, so the wrapper lives here and a test installs it with `#[global_allocator]`, which is
//! not. It counts every allocation, zeroed allocation and reallocation of every thread in the
//! process; a test that reads it keeps the process otherwise idle, in a binary of its own, so the
//! difference between two reads is the work in between. Only test builds turn the feature on.
//!
//! Beside the count it keeps the bytes live now, by the size each allocation's layout asked for
//! (its capacity, not what was written into it), and their high-water mark since the last
//! [`reset_peak`]: process-wide, and for the calling thread alone ([`thread_live_bytes`]), which
//! counts what this thread allocated less what this thread freed, so a test can measure work done
//! on its own thread beside a server another thread runs. A reallocation counts its old and new
//! sizes as live at once, as a moving one is. Thread stacks and memory the system maps for itself
//! are not allocations and are never counted.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicI64, AtomicU64, Ordering},
};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

thread_local! {
    static THREAD_LIVE: Cell<i64> = const { Cell::new(0) };
    static THREAD_PEAK: Cell<i64> = const { Cell::new(0) };
}

/// The system allocator, counting. Install it with
/// `#[global_allocator] static ALLOCATOR: Counting = Counting;`.
pub struct Counting;

/// How many allocations, zeroed allocations and reallocations every thread has made since the
/// process started, if [`Counting`] is the global allocator; zero otherwise.
pub fn allocations() -> u64 {
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// The bytes every thread's live allocations asked for, if [`Counting`] is the global allocator.
pub fn live_bytes() -> i64 {
    LIVE.load(Ordering::Relaxed)
}

/// The highest [`live_bytes`] since the last [`reset_peak`] (or the process's start).
pub fn peak_bytes() -> i64 {
    PEAK.load(Ordering::Relaxed)
}

/// Start new high-water marks at the bytes live now, process-wide and for the calling thread.
pub fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    let _ = THREAD_LIVE.try_with(|live| THREAD_PEAK.try_with(|peak| peak.set(live.get())));
}

/// The bytes the calling thread has allocated less those it has freed: negative once it has freed
/// more of what other threads allocated than it allocated itself.
pub fn thread_live_bytes() -> i64 {
    THREAD_LIVE.try_with(Cell::get).unwrap_or(0)
}

/// The highest [`thread_live_bytes`] since this thread's last [`reset_peak`].
pub fn thread_peak_bytes() -> i64 {
    THREAD_PEAK.try_with(Cell::get).unwrap_or(0)
}

fn grow(bytes: usize) {
    let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
    // `try_with`: a thread being torn down has no counters left, and its allocations still go on.
    let _ = THREAD_LIVE.try_with(|thread| {
        let now = thread.get() + bytes;
        thread.set(now);
        let _ = THREAD_PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

fn shrink(bytes: usize) {
    let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
    let _ = THREAD_LIVE.try_with(|thread| thread.set(thread.get() - bytes));
}

// SAFETY: every method forwards its arguments unchanged to the system allocator, which upholds
// `GlobalAlloc`'s contract; counting touches only atomics and const-initialized thread-local cells
// without destructors, and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller's layout is passed through as `GlobalAlloc::alloc` requires.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as `alloc`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `ptr` was allocated by this allocator, which is the system's, with `layout`.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            grow(new_size);
            shrink(layout.size());
        }
        moved
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrink(layout.size());
        // SAFETY: `ptr` was allocated by this allocator, which is the system's, with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}
