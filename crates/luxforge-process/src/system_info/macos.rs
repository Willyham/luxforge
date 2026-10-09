//! macOS: `sysctlbyname`, which reads kernel values by name and touches nothing else.
use super::{Arch, CpuVendor, OsFamily, Raw, parse_version};
use std::ffi::CStr;

pub(super) const FAMILY: OsFamily = OsFamily::Macos;

pub(super) fn read() -> Raw {
    let mut raw = Raw {
        os_version: sysctl_string(c"kern.osproductversion")
            .as_deref()
            .and_then(parse_version),
        mac_build: sysctl_string(c"kern.osversion"),
        cpu_brand: sysctl_string(c"machdep.cpu.brand_string"),
        physical_cores: sysctl_u64(c"hw.physicalcpu"),
        logical_cores: sysctl_u64(c"hw.logicalcpu"),
        memory_bytes: sysctl_u64(c"hw.memsize"),
        ..Raw::default()
    };
    if Arch::current() == Arch::X86_64 {
        #[cfg(target_arch = "x86_64")]
        {
            let (vendor, signature) = super::cpuid();
            raw.cpu_vendor = Some(vendor);
            raw.x86 = Some(signature);
        }
    } else {
        raw.cpu_vendor = Some(CpuVendor::Other);
    }
    raw
}

/// A string value of at most 255 bytes; a longer one is not wanted and reads as `None`.
fn sysctl_string(name: &CStr) -> Option<String> {
    let mut buffer = [0u8; 256];
    let mut length = buffer.len();
    // SAFETY: `name` is NUL-terminated; `buffer` is writable for `length` bytes and `length`
    // holds its size, which the call updates to the bytes written; no new value is passed.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || length == 0 || length > buffer.len() {
        return None;
    }
    let bytes = &buffer[..length];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).ok().map(str::to_owned)
}

/// An integer value the kernel reports as 4 or 8 bytes.
fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut buffer = [0u8; 8];
    let mut length = buffer.len();
    // SAFETY: as in `sysctl_string`, with an 8-byte buffer.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    match length {
        4 => Some(u64::from(u32::from_ne_bytes(buffer[..4].try_into().ok()?))),
        8 => Some(u64::from_ne_bytes(buffer)),
        _ => None,
    }
}
