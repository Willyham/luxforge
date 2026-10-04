//! A counting wrapper around the system allocator, for a test binary that holds a code path to an
//! allocation budget.
//!
//! The workspace forbids `unsafe` everywhere but the FFI crates, and implementing [`GlobalAlloc`] is
//! `unsafe`, so the wrapper lives here and a test installs it with `#[global_allocator]`, which is
//! not. It counts every allocation, zeroed allocation and reallocation of every thread in the
//! process; a test that reads it keeps the process otherwise idle, in a binary of its own, so the
//! difference between two reads is the work in between. Only test builds turn the feature on.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicU64, Ordering},
};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

/// The system allocator, counting. Install it with
/// `#[global_allocator] static ALLOCATOR: Counting = Counting;`.
pub struct Counting;

/// How many allocations, zeroed allocations and reallocations every thread has made since the
/// process started, if [`Counting`] is the global allocator; zero otherwise.
pub fn allocations() -> u64 {
    ALLOCATIONS.load(Ordering::Relaxed)
}

// SAFETY: every method forwards its arguments unchanged to the system allocator, which upholds
// `GlobalAlloc`'s contract; counting touches only an atomic and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller's layout is passed through as `GlobalAlloc::alloc` requires.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `ptr` was allocated by this allocator, which is the system's, with `layout`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by this allocator, which is the system's, with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}
