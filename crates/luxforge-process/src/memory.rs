//! The process's memory against the system's: returning the allocator's freed pages
//! ([`relieve_allocator`]) and the system's memory-pressure notifications
//! ([`watch_memory_pressure`]).
//!
//! **Freed pages.** After a burst of large allocations, freed blocks are kept by the allocator
//! dirty, and they still count toward the process's footprint. On macOS
//! `malloc_zone_pressure_relief(NULL, 0)` returns every zone's free pages to the system and answers
//! how many bytes it returned; with glibc, `malloc_trim(0)` does the same for every arena and
//! answers only whether it returned any. Both walk the allocator's free lists under its locks, so
//! a caller runs them off the interface and catalog owner threads, and only after heavy work has
//! ended. Elsewhere there is nothing as cheap to call, and nothing is done.
//!
//! **Memory pressure.** On macOS a dispatch memory-pressure source reports the system's warning
//! and critical levels; the handler runs on a utility-class global queue and should only post a
//! signal. Other platforms report none.
use crate::Unavailable;

/// What one [`relieve_allocator`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relief {
    /// The allocator returned this many bytes (macOS).
    Returned(u64),
    /// The allocator ran and says whether it returned any memory, not how much (glibc).
    Trimmed(bool),
}

/// Return the allocator's freed pages to the system. Blocks for as long as the allocator walks its
/// free lists, which can be milliseconds with a large heap: call it from a background thread.
pub fn relieve_allocator() -> Result<Relief, Unavailable> {
    #[cfg(target_os = "macos")]
    {
        Ok(crate::macos::relieve_allocator())
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` takes a padding size and touches only the allocator's own state,
        // under its own locks; any value is valid.
        let returned = unsafe { libc::malloc_trim(0) };
        Ok(Relief::Trimmed(returned != 0))
    }
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    {
        Err(Unavailable(
            "returning freed allocator pages is not supported on this platform",
        ))
    }
}

/// The level the system reports in a memory-pressure notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryPressure {
    /// Memory is getting short: release what can be rebuilt.
    Warning,
    /// Memory is critically short.
    Critical,
}

/// Call `handler` on each memory-pressure warning or critical notification, on a system queue,
/// for the rest of the process. One watch per process: a second call answers unavailable. The
/// handler should only post a signal.
pub fn watch_memory_pressure(
    handler: impl Fn(MemoryPressure) + Send + Sync + 'static,
) -> Result<(), Unavailable> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::watch_memory_pressure(Box::new(handler))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = handler;
        Err(Unavailable(
            "memory pressure is not reported on this platform yet",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Freed pages are returned where the platform can, after a large block was freed, and the
    /// call says what it did; elsewhere it says why not.
    #[test]
    fn relief_runs_after_a_large_block_is_freed() {
        let block = vec![1u8; 64 << 20];
        std::hint::black_box(&block);
        drop(block);
        let relief = relieve_allocator();
        #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
        assert!(relief.is_ok(), "{relief:?}");
        #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
        assert!(relief.is_err());
    }

    /// The system's memory pressure is watched once per process.
    #[test]
    fn memory_pressure_is_watched_once() {
        let first = watch_memory_pressure(|_| {});
        #[cfg(target_os = "macos")]
        {
            assert_eq!(first, Ok(()));
            assert!(watch_memory_pressure(|_| {}).is_err(), "a second watch");
        }
        #[cfg(not(target_os = "macos"))]
        assert!(first.is_err());
    }
}
