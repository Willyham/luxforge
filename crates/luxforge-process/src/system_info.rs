//! The platform facts that become the statistics device profile: operating system, architecture,
//! CPU, installed memory. One call, [`probe`], reads them and returns [`SystemFacts`], which holds
//! only normalized values: enums, bounded integers and numeric version components. A raw system
//! string (a CPU brand, a kernel release, an `os-release` line) is parsed inside this module and
//! dropped; none is ever a field of the result, so none can be sent by mistake.
//!
//! The probe is blocking file and system-call work, a few hundred microseconds at most, and takes
//! no lock. Its caller runs it on a worker, once, never on the UI or catalog owner thread, and
//! never in a loop. It runs no external command and reads no disk, network interface, serial,
//! UUID or host name: macOS reads `sysctlbyname` values, Linux `/proc/cpuinfo`, `/proc/meminfo`,
//! `/proc/sys/kernel/osrelease` and `/etc/os-release`, Windows `RtlGetVersion`,
//! `GetLogicalProcessorInformationEx` and `GlobalMemoryStatusEx`, and x86 CPUs `cpuid`.
//!
//! A fact the platform does not give, or gives in a form no parser here accepts, is `None`.
//!
//! Verified on macOS (arm64). The Linux and Windows readers are compile-checked only.
use std::{collections::HashSet, fmt};

#[cfg(target_os = "linux")]
#[path = "system_info/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "system_info/macos.rs"]
mod platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
#[path = "system_info/other.rs"]
mod platform;
#[cfg(windows)]
#[path = "system_info/windows.rs"]
mod platform;

#[cfg(test)]
mod tests;

/// The operating system family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsFamily {
    Macos,
    Windows,
    Linux,
    Other,
}

/// The process architecture: what this binary was built for, which under emulation (Rosetta)
/// is the emulated one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    Arm64,
    X86_64,
    Other,
}

impl Arch {
    /// The architecture this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::Arm64
        } else if cfg!(target_arch = "x86_64") {
            Self::X86_64
        } else {
            Self::Other
        }
    }
}

/// A Linux distribution family from `os-release`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxDistro {
    Arch,
    Debian,
    Ubuntu,
    Fedora,
    Rhel,
    Opensuse,
    Nixos,
    Alpine,
    Other,
}

/// The CPU maker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuVendor {
    Apple,
    Intel,
    Amd,
    Qualcomm,
    Other,
}

/// An Apple silicon variant within a generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleTier {
    Base,
    Pro,
    Max,
    Ultra,
}

/// One to four numeric version components, such as `[15, 5]` or `[10, 0, 26100]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    parts: [u32; 4],
    len: u8,
}

impl Version {
    /// The components, or `None` unless there are one to four.
    pub fn new(parts: &[u32]) -> Option<Self> {
        if parts.is_empty() || parts.len() > 4 {
            return None;
        }
        let mut stored = [0; 4];
        stored[..parts.len()].copy_from_slice(parts);
        Some(Self {
            parts: stored,
            len: parts.len() as u8,
        })
    }

    pub fn parts(&self) -> &[u32] {
        &self.parts[..usize::from(self.len)]
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.parts()).finish()
    }
}

/// A macOS build such as `24G84`, held only if it matches `^[0-9]{1,3}[A-Z][0-9]{1,5}[a-z]?$`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MacOsBuild(String);

impl MacOsBuild {
    /// `text` if it has the build shape exactly.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let major = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=3).contains(&major) || !bytes.get(major).is_some_and(u8::is_ascii_uppercase) {
            return None;
        }
        let rest = &bytes[major + 1..];
        let minor = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=5).contains(&minor) {
            return None;
        }
        match &rest[minor..] {
            [] => {}
            [suffix] if suffix.is_ascii_lowercase() => {}
            _ => return None,
        }
        Some(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MacOsBuild {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An x86 CPU's family, model and stepping as `cpuid` leaf 1 defines them, each `0..=4095`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X86Signature {
    pub family: u16,
    pub model: u16,
    pub stepping: u16,
}

impl X86Signature {
    /// From `cpuid` leaf 1's `eax`: the extended family adds to a base family of 15, and the
    /// extended model prefixes the model of family 6 and 15.
    pub fn from_cpuid_eax(eax: u32) -> Self {
        let base_family = (eax >> 8) & 0xF;
        let family = if base_family == 0xF {
            base_family + ((eax >> 20) & 0xFF)
        } else {
            base_family
        };
        let mut model = (eax >> 4) & 0xF;
        if base_family == 0x6 || base_family == 0xF {
            model |= ((eax >> 16) & 0xF) << 4;
        }
        Self {
            family: family as u16,
            model: model as u16,
            stepping: (eax & 0xF) as u16,
        }
    }
}

/// Installed memory as the smallest of 8, 16, 32, 64, 128, 256, 512 and 1024 GiB that holds it,
/// or over 1024.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryBand {
    Gib8,
    Gib16,
    Gib32,
    Gib64,
    Gib128,
    Gib256,
    Gib512,
    Gib1024,
    Over1024,
}

impl MemoryBand {
    /// The band holding `bytes`; `None` for zero, which no platform reports as installed memory.
    pub fn of_bytes(bytes: u64) -> Option<Self> {
        const GIB: u64 = 1 << 30;
        if bytes == 0 {
            return None;
        }
        Some(match bytes.div_ceil(GIB) {
            0..=8 => Self::Gib8,
            9..=16 => Self::Gib16,
            17..=32 => Self::Gib32,
            33..=64 => Self::Gib64,
            65..=128 => Self::Gib128,
            129..=256 => Self::Gib256,
            257..=512 => Self::Gib512,
            513..=1024 => Self::Gib1024,
            _ => Self::Over1024,
        })
    }

    /// The upper bound in GiB; `None` for over 1024.
    pub fn upper_gib(self) -> Option<u16> {
        Some(match self {
            Self::Gib8 => 8,
            Self::Gib16 => 16,
            Self::Gib32 => 32,
            Self::Gib64 => 64,
            Self::Gib128 => 128,
            Self::Gib256 => 256,
            Self::Gib512 => 512,
            Self::Gib1024 => 1024,
            Self::Over1024 => return None,
        })
    }
}

/// The operating system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsFacts {
    pub family: OsFamily,
    /// macOS: the product version (`15.5`). Windows: major, minor and build (`10.0.26100`).
    /// Linux: the kernel version, as there is no other OS version; it equals [`Self::kernel`].
    pub version: Option<Version>,
    /// macOS only: the build, when it has the exact shape.
    pub build: Option<MacOsBuild>,
    /// Linux only: the distribution family.
    pub distro: Option<LinuxDistro>,
    /// Linux only: the numeric `VERSION_ID`, present only when `os-release`'s `ID` itself is the
    /// recognised distribution, so a derivative never carries its own release as its parent's.
    pub distro_version: Option<Version>,
    /// Linux only: the numeric kernel release.
    pub kernel: Option<Version>,
}

/// The CPU, without any brand string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuFacts {
    pub vendor: CpuVendor,
    /// Physical cores, `1..=1024`. On Linux without topology in `/proc/cpuinfo` on a non-x86 CPU,
    /// the logical count.
    pub physical_cores: Option<u16>,
    /// Logical processors, `1..=1024`.
    pub logical_cores: Option<u16>,
    /// Apple silicon generation, `1..=64`, parsed from the brand (`Apple M4 Pro` is 4).
    pub apple_generation: Option<u8>,
    /// Apple silicon tier from the brand; `Base` for a brand with no suffix.
    pub apple_tier: Option<AppleTier>,
    /// Intel and AMD only.
    pub x86: Option<X86Signature>,
}

/// Everything [`probe`] reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemFacts {
    pub os: OsFacts,
    pub arch: Arch,
    pub cpu: CpuFacts,
    pub memory: Option<MemoryBand>,
}

/// Read the platform facts. Blocking and cheap, but call it from a worker, once per launch.
pub fn probe() -> SystemFacts {
    normalize(platform::FAMILY, platform::read())
}

/// The machine's installed memory in bytes, exact rather than banded: what the capability host
/// holds a model's machine minimum against. One system call or file read; `None` where the platform
/// does not give it. Never sent anywhere: the statistics profile carries only [`MemoryBand`].
pub fn installed_memory_bytes() -> Option<u64> {
    platform::memory().filter(|bytes| *bytes > 0)
}

/// What a platform reader collected, before anything is parsed. Private: strings stop here.
#[derive(Default)]
pub(crate) struct Raw {
    pub(crate) os_version: Option<Version>,
    pub(crate) mac_build: Option<String>,
    pub(crate) os_release: Option<String>,
    pub(crate) kernel_release: Option<String>,
    pub(crate) cpu_brand: Option<String>,
    pub(crate) cpu_vendor: Option<CpuVendor>,
    pub(crate) x86: Option<X86Signature>,
    pub(crate) physical_cores: Option<u64>,
    pub(crate) logical_cores: Option<u64>,
    pub(crate) memory_bytes: Option<u64>,
}

pub(crate) fn normalize(family: OsFamily, raw: Raw) -> SystemFacts {
    let kernel = raw.kernel_release.as_deref().and_then(parse_version);
    let linux = family == OsFamily::Linux;
    let release = raw
        .os_release
        .as_deref()
        .filter(|_| linux)
        .map(parse_os_release);
    let apple = raw.cpu_brand.as_deref().and_then(parse_apple_brand);
    let vendor = match (&apple, raw.cpu_vendor) {
        (Some(_), _) => CpuVendor::Apple,
        (None, Some(vendor)) => vendor,
        (None, None) => CpuVendor::Other,
    };
    let x86 = raw
        .x86
        .filter(|_| matches!(vendor, CpuVendor::Intel | CpuVendor::Amd))
        .filter(|sig| sig.family <= 4095 && sig.model <= 4095 && sig.stepping <= 4095);
    SystemFacts {
        os: OsFacts {
            family,
            version: if linux { kernel } else { raw.os_version },
            build: raw
                .mac_build
                .as_deref()
                .filter(|_| family == OsFamily::Macos)
                .and_then(MacOsBuild::parse),
            distro: release.as_ref().map(|release| release.distro),
            distro_version: release.as_ref().and_then(|release| release.version),
            kernel: kernel.filter(|_| linux),
        },
        arch: Arch::current(),
        cpu: CpuFacts {
            vendor,
            physical_cores: raw.physical_cores.and_then(core_count),
            logical_cores: raw.logical_cores.and_then(core_count),
            apple_generation: apple.map(|apple| apple.generation),
            apple_tier: apple.and_then(|apple| apple.tier),
            x86,
        },
        memory: raw.memory_bytes.and_then(MemoryBand::of_bytes),
    }
}

/// A core count in `1..=1024`.
pub fn core_count(count: u64) -> Option<u16> {
    u16::try_from(count)
        .ok()
        .filter(|count| (1..=1024).contains(count))
}

/// The leading numeric components of a dotted version, at most four. A component that ends in a
/// non-digit still counts for its digits but ends the version, so `6.8.0-45-generic` is
/// `[6, 8, 0]` and `7.2.6-arch1-1` is `[7, 2, 6]`; text with no leading digit is `None`.
pub fn parse_version(text: &str) -> Option<Version> {
    let mut parts = Vec::with_capacity(4);
    for component in text.trim().split('.') {
        let digits = component.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            break;
        }
        let Ok(value) = component[..digits].parse::<u32>() else {
            break;
        };
        parts.push(value);
        if digits != component.len() || parts.len() == 4 {
            break;
        }
    }
    Version::new(&parts)
}

/// An Apple silicon brand's generation and tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AppleSilicon {
    pub(crate) generation: u8,
    pub(crate) tier: Option<AppleTier>,
}

/// `Apple M4 Pro` is generation 4, `Pro`; `Apple M2` is generation 2, `Base`. Anything that is not
/// `Apple M<1..=64>` optionally followed by `Pro`, `Max` or `Ultra` gives no tier, and a brand that
/// is not `Apple M<n>` gives nothing.
pub(crate) fn parse_apple_brand(brand: &str) -> Option<AppleSilicon> {
    let mut words = brand.split_whitespace();
    if words.next()? != "Apple" {
        return None;
    }
    let chip = words.next()?.strip_prefix('M')?;
    if chip.is_empty() || !chip.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let generation = chip.parse::<u8>().ok().filter(|g| (1..=64).contains(g))?;
    let tier = match (words.next(), words.next()) {
        (None, _) => Some(AppleTier::Base),
        (Some("Pro"), None) => Some(AppleTier::Pro),
        (Some("Max"), None) => Some(AppleTier::Max),
        (Some("Ultra"), None) => Some(AppleTier::Ultra),
        _ => None,
    };
    Some(AppleSilicon { generation, tier })
}

/// The CPU maker named by an x86 `cpuid` vendor string or a Linux `vendor_id`.
#[cfg_attr(
    not(any(target_arch = "x86", target_arch = "x86_64", test)),
    allow(dead_code)
)]
pub(crate) fn vendor_from_id(id: &str) -> CpuVendor {
    match id {
        "GenuineIntel" => CpuVendor::Intel,
        "AuthenticAMD" | "AMDisbetter!" | "HygonGenuine" => CpuVendor::Amd,
        _ => CpuVendor::Other,
    }
}

/// The CPU maker named by an Arm `MIDR` implementer code.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn vendor_from_arm_implementer(code: u32) -> CpuVendor {
    match code {
        0x51 => CpuVendor::Qualcomm,
        0x61 => CpuVendor::Apple,
        _ => CpuVendor::Other,
    }
}

/// The distribution of an `os-release` file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OsRelease {
    pub(crate) distro: LinuxDistro,
    pub(crate) version: Option<Version>,
}

fn distro_named(id: &str) -> Option<LinuxDistro> {
    Some(match id {
        "arch" => LinuxDistro::Arch,
        "debian" => LinuxDistro::Debian,
        "ubuntu" => LinuxDistro::Ubuntu,
        "fedora" => LinuxDistro::Fedora,
        "rhel" => LinuxDistro::Rhel,
        "nixos" => LinuxDistro::Nixos,
        "alpine" => LinuxDistro::Alpine,
        id if id.starts_with("opensuse") || id == "suse" || id == "sles" => LinuxDistro::Opensuse,
        _ => return None,
    })
}

/// `ID`, else the first recognised word of `ID_LIKE`, mapped to the fixed enum; `VERSION_ID`'s
/// numeric components only when `ID` itself was recognised. Nothing else is read.
pub(crate) fn parse_os_release(text: &str) -> OsRelease {
    let (mut id, mut like, mut version_id) = (None, None, None);
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        match key {
            "ID" => id = Some(value),
            "ID_LIKE" => like = Some(value),
            "VERSION_ID" => version_id = Some(value),
            _ => {}
        }
    }
    let own = id
        .map(str::to_ascii_lowercase)
        .and_then(|id| distro_named(&id));
    let distro = own
        .or_else(|| {
            like.into_iter()
                .flat_map(str::split_whitespace)
                .find_map(|word| distro_named(&word.to_ascii_lowercase()))
        })
        .unwrap_or(LinuxDistro::Other);
    OsRelease {
        distro,
        version: own.and(version_id).and_then(parse_version),
    }
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
/// What `/proc/cpuinfo` gives, as numbers and an enum.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CpuInfo {
    pub(crate) vendor: Option<CpuVendor>,
    pub(crate) logical: Option<u64>,
    pub(crate) physical: Option<u64>,
}

/// Logical processors are the `processor` lines; physical cores the distinct (`physical id`,
/// `core id`) pairs; the maker is the Arm `CPU implementer` of the first processor or, on x86,
/// left to `cpuid`.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_cpuinfo(text: &str) -> CpuInfo {
    let (mut logical, mut vendor) = (0u64, None);
    let mut cores = HashSet::new();
    let (mut package, mut core) = (None::<&str>, None::<&str>);
    let mut flush = |package: &mut Option<&str>, core: &mut Option<&str>| {
        if let (Some(p), Some(c)) = (package.take(), core.take()) {
            cores.insert((p.to_owned(), c.to_owned()));
        }
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "processor" => {
                flush(&mut package, &mut core);
                logical += 1;
            }
            "physical id" => package = Some(value),
            "core id" => core = Some(value),
            "CPU implementer" if vendor.is_none() => {
                vendor = u32::from_str_radix(value.trim_start_matches("0x"), 16)
                    .ok()
                    .map(vendor_from_arm_implementer);
            }
            _ => {}
        }
    }
    flush(&mut package, &mut core);
    CpuInfo {
        vendor,
        logical: (logical > 0).then_some(logical),
        physical: (!cores.is_empty()).then_some(cores.len() as u64),
    }
}

/// `MemTotal` in bytes (`/proc/meminfo` states kibibytes).
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_mem_total(text: &str) -> Option<u64> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?;
    let kib = line.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

/// This x86 CPU's maker and signature from `cpuid` leaves 0 and 1, which every x86-64 CPU has.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) fn cpuid() -> (CpuVendor, X86Signature) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::__cpuid;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::__cpuid;
    // `cpuid` is an unprivileged instruction with no memory operands, and leaves 0 and 1 exist on
    // every x86 CPU that runs Rust's x86 targets.
    let (vendor, leaf1) = (__cpuid(0), __cpuid(1));
    let mut id = Vec::with_capacity(12);
    for register in [vendor.ebx, vendor.edx, vendor.ecx] {
        id.extend_from_slice(&register.to_le_bytes());
    }
    (
        std::str::from_utf8(&id).map_or(CpuVendor::Other, vendor_from_id),
        X86Signature::from_cpuid_eax(leaf1.eax),
    )
}
