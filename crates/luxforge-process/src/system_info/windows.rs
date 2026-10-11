//! Windows: `RtlGetVersion`, `GetLogicalProcessorInformationEx`, `GlobalMemoryStatusEx` and, on
//! x86, `cpuid`. Compile-checked only; **unverified on Windows**.
//!
//! The OS version is major, minor and build number (`10.0.26100`); the update revision lives in
//! the registry and is not read. `ullTotalPhys` is the physical memory the OS can use, a little
//! under what is installed, which the banding absorbs. A Qualcomm Arm PC reports vendor `Other`
//! until a source that is not the registry is chosen.
use super::{OsFamily, Raw, Version};
use std::mem::{size_of, zeroed};
use windows_sys::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW,
    RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

pub(super) const FAMILY: OsFamily = OsFamily::Windows;

pub(super) fn read() -> Raw {
    let (physical, logical) = processors().unzip();
    let mut raw = Raw {
        os_version: version(),
        physical_cores: physical,
        logical_cores: logical,
        memory_bytes: memory(),
        ..Raw::default()
    };
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let (vendor, signature) = super::cpuid();
        raw.cpu_vendor = Some(vendor);
        raw.x86 = Some(signature);
    }
    raw
}

fn version() -> Option<Version> {
    // SAFETY: `info` is zeroed plain data whose size field is set before the call, as
    // RtlGetVersion requires; the call only writes it.
    let (status, info) = unsafe {
        let mut info: OSVERSIONINFOW = zeroed();
        info.dwOSVersionInfoSize = size_of::<OSVERSIONINFOW>() as u32;
        (
            windows_sys::Wdk::System::SystemServices::RtlGetVersion(&mut info),
            info,
        )
    };
    (status == 0)
        .then(|| Version::new(&[info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber]))
        .flatten()
}

fn memory() -> Option<u64> {
    // SAFETY: `status` is zeroed plain data with its length field set; the call only writes it.
    let (ok, status) = unsafe {
        let mut status: MEMORYSTATUSEX = zeroed();
        status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        (GlobalMemoryStatusEx(&mut status), status)
    };
    (ok != 0).then_some(status.ullTotalPhys)
}

/// (physical cores, logical processors) over every processor group.
fn processors() -> Option<(u64, u64)> {
    let mut length = 0u32;
    // SAFETY: a null buffer with a zero length asks only for the size needed, which is written to
    // `length`; the call fails with insufficient-buffer, which is expected.
    unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut length);
    }
    if length == 0 || length > 1 << 20 {
        return None;
    }
    // u64 elements give the 8-byte alignment the records need.
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `length` writable, 8-byte-aligned bytes.
    let ok = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            buffer.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if ok == 0 {
        return None;
    }
    let base = buffer.as_ptr().cast::<u8>();
    let (mut offset, mut physical, mut logical) = (0usize, 0u64, 0u64);
    while offset + size_of::<u32>() * 2 <= length as usize {
        // SAFETY: each record starts at `offset`, inside the bytes the call wrote, 8-byte aligned
        // because every record's size is a multiple of 8; its `Size` field says where the next
        // starts and is checked to advance and to stay within `length`. The processor relationship
        // member is the one valid for RelationProcessorCore, and its `GroupCount` group masks
        // follow `GroupMask` inline within `Size`.
        unsafe {
            let record = base
                .add(offset)
                .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
            let size = (*record).Size as usize;
            if size == 0 || offset + size > length as usize {
                return None;
            }
            let relationship = &(*record).Anonymous.Processor;
            let masks = relationship.GroupMask.as_ptr();
            physical += 1;
            for group in 0..usize::from(relationship.GroupCount) {
                logical += u64::from((*masks.add(group)).Mask.count_ones());
            }
            offset += size;
        }
    }
    (physical > 0 && logical > 0).then_some((physical, logical))
}
