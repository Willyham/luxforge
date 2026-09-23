//! What the operating system accounts to this process: CPU time over all threads, memory in the
//! measure the platform's own monitor shows, and, where the platform reports them, GPU time and GPU
//! allocations.
//!
//! The platform code is FFI, so it lives in this leaf crate, the second one allowed `unsafe` after
//! `lightwell-raw`. Every `unsafe` block sits beside a `SAFETY:` comment and nothing unsafe crosses
//! the public API. A counter the platform cannot give is an [`Unavailable`] with a reason a person
//! can read, never a zero, so a monitor can tell "nothing used" from "nothing known".
//!
//! Every time and byte counter here is cumulative or a current level taken at the call; a rate
//! comes from two reads and the caller's own monotonic clock.
use serde::Serialize;
use std::fmt;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod procfs;
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

/// Why a counter has no value, in words for a person: the platform does not report it, the call
/// that reads it failed, or the process has not got the thing it measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unavailable(pub &'static str);

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Which measure [`Memory::bytes`] is. Each platform reports the one its own monitor shows, so the
/// figure can be compared with what the owner sees there; they are not interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// macOS physical footprint: Activity Monitor's Memory column. On unified memory it includes
    /// the process's GPU allocations.
    Footprint,
    /// Linux resident set size.
    Resident,
    /// Windows private bytes: memory committed to this process alone, which Task Manager lists as
    /// Commit size.
    Private,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Footprint => "footprint",
            Self::Resident => "resident",
            Self::Private => "private",
        }
    }
}

/// The process's memory at the moment of the read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    pub kind: MemoryKind,
    /// The current level in [`Self::kind`]'s measure.
    pub bytes: Result<u64, Unavailable>,
    /// The highest level the platform has recorded for this process. On macOS and Linux it is the
    /// peak of the same measure as `bytes`; on Windows it is the peak working set, which is
    /// resident memory, not private bytes.
    pub peak_bytes: Result<u64, Unavailable>,
    /// Pages of this process resident in physical memory now.
    pub resident_bytes: Result<u64, Unavailable>,
}

/// The process's GPU use, where the platform reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gpu {
    /// GPU time spent on this process's work since it started, in nanoseconds. It never decreases.
    pub time_ns: Result<u64, Unavailable>,
    /// Bytes the process's GPU device holds allocated now.
    pub allocated_bytes: Result<u64, Unavailable>,
    /// Whether the GPU shares system memory with the CPU, so its allocations are part of the
    /// memory figure; `None` when no device was read.
    pub unified_memory: Option<bool>,
}

/// One read of every counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counters {
    /// User plus system CPU time of every thread of this process since it started, in nanoseconds.
    pub cpu_time_ns: Result<u64, Unavailable>,
    /// How many threads can run at once, the ceiling a CPU rate is read against: a process can
    /// spend at most this many nanoseconds of CPU time per nanosecond of wall time.
    pub logical_cpus: u32,
    pub memory: Memory,
    pub gpu: Gpu,
}

/// Reads [`Counters`] for this process. It keeps what makes the next read cheaper, such as the
/// GPU driver entries that belong to this process, so one sampler should serve every read.
///
/// GPU allocations are read from the system's default GPU device, and asking for that device
/// creates one in a process that has none. So they are read only after
/// [`Self::enable_gpu_allocations`], which a process that presents through the GPU calls; before
/// that they are unavailable with the reason.
pub struct Sampler {
    logical_cpus: u32,
    platform: platform::State,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            // Read once: on Linux it consults the affinity mask and cgroup quota files, which is
            // more than a monitor should repeat every second, and neither changes under a running
            // editor in practice.
            logical_cpus: std::thread::available_parallelism()
                .map_or(1, |count| u32::try_from(count.get()).unwrap_or(u32::MAX)),
            platform: platform::State::new(),
        }
    }

    /// Read GPU allocations from now on. The device is opened at the next read, not here, so a
    /// declaration costs nothing until someone asks. Idempotent.
    pub fn enable_gpu_allocations(&mut self) {
        self.platform.enable_gpu_allocations();
    }

    pub fn read(&mut self) -> Counters {
        let (cpu_time_ns, memory) = self.platform.cpu_and_memory();
        Counters {
            cpu_time_ns,
            logical_cpus: self.logical_cpus,
            memory,
            gpu: self.platform.gpu(),
        }
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

/// A process-wide sampler lives behind a mutex that any thread may lock, which needs `Send`. The
/// platform state holds only integers and IOKit port names, apart from the macOS Metal device,
/// whose `Send` is argued where it is declared.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Sampler>();
};
