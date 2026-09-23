//! Linux: `/proc/self/stat` for CPU time and `/proc/self/status` for resident memory. GPU time and
//! GPU allocations have no process-level source that every driver publishes, so they are
//! unavailable until one is chosen.
use crate::{Gpu, Memory, MemoryKind, Unavailable, procfs};

pub(crate) struct State {
    /// `sysconf(_SC_CLK_TCK)`, the unit of `/proc/self/stat` times, or why it could not be read.
    ticks_per_second: Result<u64, Unavailable>,
}

impl State {
    pub(crate) fn new() -> Self {
        // SAFETY: sysconf takes an integer name and returns a long; it touches no memory of ours.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            ticks_per_second: u64::try_from(ticks)
                .ok()
                .filter(|&ticks| ticks > 0)
                .ok_or(Unavailable("sysconf(_SC_CLK_TCK) gave no clock rate")),
        }
    }

    pub(crate) fn enable_gpu_allocations(&mut self) {}

    pub(crate) fn cpu_and_memory(&mut self) -> (Result<u64, Unavailable>, Memory) {
        let cpu = self.ticks_per_second.and_then(|per_second| {
            let stat = std::fs::read_to_string("/proc/self/stat")
                .map_err(|_| Unavailable("/proc/self/stat could not be read"))?;
            let ticks = procfs::stat_ticks(&stat)
                .ok_or(Unavailable("/proc/self/stat has no utime and stime"))?;
            Ok(procfs::ticks_ns(ticks, per_second))
        });
        let memory = match std::fs::read_to_string("/proc/self/status") {
            Ok(status) => {
                let resident = procfs::status_bytes(&status, "VmRSS")
                    .ok_or(Unavailable("/proc/self/status has no VmRSS"));
                Memory {
                    kind: MemoryKind::Resident,
                    bytes: resident,
                    peak_bytes: procfs::status_bytes(&status, "VmHWM")
                        .ok_or(Unavailable("/proc/self/status has no VmHWM")),
                    resident_bytes: resident,
                }
            }
            Err(_) => {
                let unreadable = Err(Unavailable("/proc/self/status could not be read"));
                Memory {
                    kind: MemoryKind::Resident,
                    bytes: unreadable,
                    peak_bytes: unreadable,
                    resident_bytes: unreadable,
                }
            }
        };
        (cpu, memory)
    }

    pub(crate) fn gpu(&mut self) -> Gpu {
        Gpu {
            time_ns: Err(Unavailable("GPU time is not reported on Linux yet")),
            allocated_bytes: Err(Unavailable("GPU allocations are not reported on Linux yet")),
            unified_memory: None,
        }
    }
}
