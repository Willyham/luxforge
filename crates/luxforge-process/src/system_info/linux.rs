//! Linux: `/proc/cpuinfo`, `/proc/meminfo`, `/proc/sys/kernel/osrelease` (the `uname -r` string)
//! and `/etc/os-release`. Compile-checked only; not run on Linux here.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use super::cpuid;
use super::{OsFamily, Raw, parse_cpuinfo, parse_mem_total};

pub(super) const FAMILY: OsFamily = OsFamily::Linux;

/// `MemTotal` from `/proc/meminfo`.
pub(super) fn memory() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| parse_mem_total(&meminfo))
}

pub(super) fn read() -> Raw {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let cpu = parse_cpuinfo(&cpuinfo);
    let logical = cpu.logical.or_else(|| {
        std::thread::available_parallelism()
            .ok()
            .map(|n| n.get() as u64)
    });
    #[cfg_attr(
        not(any(target_arch = "x86", target_arch = "x86_64")),
        allow(unused_mut)
    )]
    let mut raw = Raw {
        os_release: std::fs::read_to_string("/etc/os-release")
            .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
            .ok(),
        kernel_release: std::fs::read_to_string("/proc/sys/kernel/osrelease").ok(),
        cpu_vendor: cpu.vendor,
        // Without topology fields (Arm kernels omit them) one logical processor is one core.
        physical_cores: cpu.physical.or(logical),
        logical_cores: logical,
        memory_bytes: memory(),
        ..Raw::default()
    };
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let (vendor, signature) = cpuid();
        raw.cpu_vendor = Some(vendor);
        raw.x86 = Some(signature);
    }
    raw
}
