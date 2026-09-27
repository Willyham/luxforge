//! Windows: `GetProcessTimes` for CPU time and `K32GetProcessMemoryInfo` for private bytes and the
//! working set. GPU time and GPU allocations are unavailable until a source is chosen; the
//! per-process GPU performance counters need a PDH query this crate does not make.
use crate::{Gpu, Memory, MemoryKind, Unavailable};
use std::{fs::File, io, os::windows::io::AsRawHandle};
use windows_sys::Win32::{
    Foundation::FILETIME,
    Storage::FileSystem::{FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx},
    System::{
        ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::{GetCurrentProcess, GetProcessTimes},
    },
};

/// NTFS change time catches an in-place rewrite even when the source's length and last-write
/// timestamp are restored. The caller supplies its existing read handle to avoid a path race.
pub fn file_change_time(file: &File) -> io::Result<i64> {
    let mut info = FILE_BASIC_INFO::default();
    // SAFETY: `file` owns a valid handle for this call; `info` is writable and its exact size is
    // passed with FileBasicInfo. The handle remains open throughout the call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FileBasicInfo,
            (&raw mut info).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info.ChangeTime)
    }
}

pub(crate) struct State;

/// A `FILETIME` duration in its own unit, 100 nanoseconds.
fn hundreds_of_ns(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

impl State {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn enable_gpu_allocations(&mut self) {}

    pub(crate) fn cpu_and_memory(&mut self) -> (Result<u64, Unavailable>, Memory) {
        (cpu_time_ns(), memory())
    }

    pub(crate) fn gpu(&mut self) -> Gpu {
        Gpu {
            time_ns: Err(Unavailable("GPU time is not reported on Windows yet")),
            allocated_bytes: Err(Unavailable(
                "GPU allocations are not reported on Windows yet",
            )),
            unified_memory: None,
        }
    }
}

fn cpu_time_ns() -> Result<u64, Unavailable> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that is always valid for this process and
    // needs no closing; the four pointers are to live, writable FILETIMEs on this stack frame.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return Err(Unavailable("GetProcessTimes failed"));
    }
    Ok(hundreds_of_ns(kernel)
        .saturating_add(hundreds_of_ns(user))
        .saturating_mul(100))
}

fn memory() -> Memory {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle is valid as above; the pointer is to a live, writable
    // PROCESS_MEMORY_COUNTERS_EX whose size is passed in `cb`, which is how the call is told it may
    // fill the extended structure, whose leading fields are PROCESS_MEMORY_COUNTERS's.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    if ok == 0 {
        let failed = Err(Unavailable("GetProcessMemoryInfo failed"));
        return Memory {
            kind: MemoryKind::Private,
            bytes: failed,
            peak_bytes: failed,
            resident_bytes: failed,
        };
    }
    Memory {
        kind: MemoryKind::Private,
        bytes: Ok(counters.PrivateUsage as u64),
        peak_bytes: Ok(counters.PeakWorkingSetSize as u64),
        resident_bytes: Ok(counters.WorkingSetSize as u64),
    }
}
