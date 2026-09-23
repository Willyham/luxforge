//! Any other platform: the crate compiles and says why it has nothing, so a port starts from a
//! working editor rather than a build failure.
use crate::{Gpu, Memory, MemoryKind, Unavailable};

pub(crate) struct State;

const UNSUPPORTED: Unavailable =
    Unavailable("process counters are not implemented on this platform");

impl State {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn enable_gpu_allocations(&mut self) {}

    pub(crate) fn cpu_and_memory(&mut self) -> (Result<u64, Unavailable>, Memory) {
        (
            Err(UNSUPPORTED),
            Memory {
                kind: MemoryKind::Resident,
                bytes: Err(UNSUPPORTED),
                peak_bytes: Err(UNSUPPORTED),
                resident_bytes: Err(UNSUPPORTED),
            },
        )
    }

    pub(crate) fn gpu(&mut self) -> Gpu {
        Gpu {
            time_ns: Err(UNSUPPORTED),
            allocated_bytes: Err(UNSUPPORTED),
            unified_memory: None,
        }
    }
}
