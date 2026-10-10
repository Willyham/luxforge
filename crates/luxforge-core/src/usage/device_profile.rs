//! The wire `device` object as typed values: OS, architecture, CPU, memory band, at most two GPUs
//! and the renderer path, each an enum, a bounded integer or numeric version components. No field
//! can hold a free-form system string, a hardware identifier or a serial.
//!
//! [`DeviceProfile::new`] takes already-normalized values and validates the combinations the wire
//! format allows (a macOS build only on macOS, Apple generation and tier only on Apple, x86
//! signatures only on Intel or AMD, distinct GPUs, a fallback only with the reference renderer) and
//! the 2 KiB serialized cap. The platform probes (`luxforge_process::system_info`) and adapter
//! normalization (`luxforge_gpu_types::adapter_facts`) supply the values; the `from_facts`
//! conversions here only change their type. Choosing and deduplicating adapters is the caller's.
use super::{
    encode::{Json, SchemaError, closed, int},
    registry::{MAX_DEVICE_BYTES, Renderer, wire_enum},
};
use luxforge_gpu_types::adapter_facts as gpu;
use luxforge_process::system_info as sys;
use std::io::{self, Write};

wire_enum! {
    pub enum OsFamily {
        Macos => "macos",
        Windows => "windows",
        Linux => "linux",
        Other => "other",
    }
}

wire_enum! {
    pub enum LinuxDistro {
        Arch => "arch",
        Debian => "debian",
        Ubuntu => "ubuntu",
        Fedora => "fedora",
        Rhel => "rhel",
        Opensuse => "opensuse",
        Nixos => "nixos",
        Alpine => "alpine",
        Other => "other",
    }
}

wire_enum! {
    pub enum Arch {
        Arm64 => "arm64",
        X86_64 => "x86_64",
        Other => "other",
    }
}

wire_enum! {
    pub enum CpuVendor {
        Apple => "apple",
        Intel => "intel",
        Amd => "amd",
        Qualcomm => "qualcomm",
        Other => "other",
    }
}

wire_enum! {
    /// An Apple silicon variant within a generation.
    pub enum AppleTier {
        Base => "base",
        Pro => "pro",
        Max => "max",
        Ultra => "ultra",
    }
}

wire_enum! {
    pub enum GpuVendor {
        Apple => "apple",
        Nvidia => "nvidia",
        Amd => "amd",
        Intel => "intel",
        Qualcomm => "qualcomm",
        Arm => "arm",
        Imagination => "imagination",
        Microsoft => "microsoft",
        Mesa => "mesa",
        Other => "other",
    }
}

wire_enum! {
    pub enum GpuBackend {
        Metal => "metal",
        Vulkan => "vulkan",
        Dx12 => "dx12",
        Gl => "gl",
        Other => "other",
    }
}

wire_enum! {
    pub enum GpuType {
        Integrated => "integrated",
        Discrete => "discrete",
        Virtual => "virtual",
        Cpu => "cpu",
        Other => "other",
    }
}

wire_enum! {
    /// Why the reference renderer draws instead of the GPU. No error text.
    pub enum RendererFallback {
        NoAdapter => "no-adapter",
        AdapterLost => "adapter-lost",
        Unsupported => "unsupported",
        Disabled => "disabled",
        Other => "other",
    }
}

/// An installed-memory (or GPU memory) upper bound in GiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    const BANDS: [(u16, Self); 8] = [
        (8, Self::Gib8),
        (16, Self::Gib16),
        (32, Self::Gib32),
        (64, Self::Gib64),
        (128, Self::Gib128),
        (256, Self::Gib256),
        (512, Self::Gib512),
        (1024, Self::Gib1024),
    ];

    /// The band's upper bound in GiB; `None` for over 1024.
    pub fn upper_gib(self) -> Option<u16> {
        Self::BANDS
            .iter()
            .find(|(_, band)| *band == self)
            .map(|(gib, _)| *gib)
    }

    fn write(self, w: &mut impl Write) -> io::Result<()> {
        match self.upper_gib() {
            Some(gib) => write!(w, "{gib}"),
            None => w.write_all(b"\"over1024\""),
        }
    }

    fn decode(json: &Json) -> Result<Self, SchemaError> {
        match json {
            Json::Int(gib) => Self::BANDS
                .iter()
                .find(|(band, _)| u64::from(*band) == *gib)
                .map(|(_, band)| *band)
                .ok_or_else(|| SchemaError::new(format!("memory band {gib} is not a band"))),
            Json::Str(text) if text == "over1024" => Ok(Self::Over1024),
            _ => Err(SchemaError::new(
                "a memory band is a band integer or \"over1024\"",
            )),
        }
    }
}

/// One to four numeric version components, each `0..=4294967295`.
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

    fn write(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(b"[")?;
        for (index, part) in self.parts().iter().enumerate() {
            if index > 0 {
                w.write_all(b",")?;
            }
            write!(w, "{part}")?;
        }
        w.write_all(b"]")
    }

    fn decode(json: &Json, what: &str) -> Result<Self, SchemaError> {
        let Json::Array(items) = json else {
            return Err(SchemaError::new(format!("{what} is an array of integers")));
        };
        if items.is_empty() || items.len() > 4 {
            return Err(SchemaError::new(format!("{what} has 1 to 4 components")));
        }
        let mut parts = [0u32; 4];
        for (part, item) in parts.iter_mut().zip(items) {
            *part = int(item, u64::from(u32::MAX), what)? as u32;
        }
        Ok(Self::new(&parts[..items.len()]).expect("1 to 4 components"))
    }
}

impl std::fmt::Debug for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.parts()).finish()
    }
}

impl From<sys::Version> for Version {
    fn from(version: sys::Version) -> Self {
        Self::new(version.parts()).expect("a probed version has 1 to 4 components")
    }
}

impl From<gpu::DriverVersion> for Version {
    fn from(version: gpu::DriverVersion) -> Self {
        Self::new(version.parts()).expect("a driver version has 1 to 4 components")
    }
}

/// A macOS build such as `24G84`: `^[0-9]{1,3}[A-Z][0-9]{1,5}[a-z]?$`, held inline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacBuild {
    bytes: [u8; 10],
    len: u8,
}

impl MacBuild {
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
        let mut stored = [0; 10];
        stored[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            bytes: stored,
            len: bytes.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).expect("ASCII")
    }
}

impl std::fmt::Debug for MacBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OsProfile {
    pub family: OsFamily,
    pub version: Version,
    /// macOS only.
    pub build: Option<MacBuild>,
    /// Linux only.
    pub distro: Option<LinuxDistro>,
    /// Linux only.
    pub distro_version: Option<Version>,
    /// Linux only.
    pub kernel: Option<Version>,
}

impl OsProfile {
    /// The probed facts as profile values; `None` without an OS version, which the wire requires.
    pub fn from_facts(facts: &sys::OsFacts) -> Option<Self> {
        Some(Self {
            family: match facts.family {
                sys::OsFamily::Macos => OsFamily::Macos,
                sys::OsFamily::Windows => OsFamily::Windows,
                sys::OsFamily::Linux => OsFamily::Linux,
                sys::OsFamily::Other => OsFamily::Other,
            },
            version: facts.version?.into(),
            build: facts
                .build
                .as_ref()
                .and_then(|build| MacBuild::parse(build.as_str())),
            distro: facts.distro.map(|distro| match distro {
                sys::LinuxDistro::Arch => LinuxDistro::Arch,
                sys::LinuxDistro::Debian => LinuxDistro::Debian,
                sys::LinuxDistro::Ubuntu => LinuxDistro::Ubuntu,
                sys::LinuxDistro::Fedora => LinuxDistro::Fedora,
                sys::LinuxDistro::Rhel => LinuxDistro::Rhel,
                sys::LinuxDistro::Opensuse => LinuxDistro::Opensuse,
                sys::LinuxDistro::Nixos => LinuxDistro::Nixos,
                sys::LinuxDistro::Alpine => LinuxDistro::Alpine,
                sys::LinuxDistro::Other => LinuxDistro::Other,
            }),
            distro_version: facts.distro_version.map(Version::from),
            kernel: facts.kernel.map(Version::from),
        })
    }
}

/// The CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuProfile {
    pub vendor: CpuVendor,
    /// `1..=1024`.
    pub physical_cores: u16,
    /// `1..=1024`.
    pub logical_cores: u16,
    /// Apple only, `1..=64`.
    pub generation: Option<u8>,
    /// Apple only.
    pub tier: Option<AppleTier>,
    /// Intel or AMD only, each `0..=4095`.
    pub family: Option<u16>,
    pub model: Option<u16>,
    pub stepping: Option<u16>,
}

impl CpuProfile {
    /// The probed facts as profile values; `None` without both core counts, which the wire
    /// requires.
    pub fn from_facts(facts: &sys::CpuFacts) -> Option<Self> {
        Some(Self {
            vendor: match facts.vendor {
                sys::CpuVendor::Apple => CpuVendor::Apple,
                sys::CpuVendor::Intel => CpuVendor::Intel,
                sys::CpuVendor::Amd => CpuVendor::Amd,
                sys::CpuVendor::Qualcomm => CpuVendor::Qualcomm,
                sys::CpuVendor::Other => CpuVendor::Other,
            },
            physical_cores: facts.physical_cores?,
            logical_cores: facts.logical_cores?,
            generation: facts.apple_generation,
            tier: facts.apple_tier.map(|tier| match tier {
                sys::AppleTier::Base => AppleTier::Base,
                sys::AppleTier::Pro => AppleTier::Pro,
                sys::AppleTier::Max => AppleTier::Max,
                sys::AppleTier::Ultra => AppleTier::Ultra,
            }),
            family: facts.x86.map(|x86| x86.family),
            model: facts.x86.map(|x86| x86.model),
            stepping: facts.x86.map(|x86| x86.stepping),
        })
    }
}

impl From<sys::Arch> for Arch {
    fn from(arch: sys::Arch) -> Self {
        match arch {
            sys::Arch::Arm64 => Self::Arm64,
            sys::Arch::X86_64 => Self::X86_64,
            sys::Arch::Other => Self::Other,
        }
    }
}

impl From<sys::MemoryBand> for MemoryBand {
    fn from(band: sys::MemoryBand) -> Self {
        match band {
            sys::MemoryBand::Gib8 => Self::Gib8,
            sys::MemoryBand::Gib16 => Self::Gib16,
            sys::MemoryBand::Gib32 => Self::Gib32,
            sys::MemoryBand::Gib64 => Self::Gib64,
            sys::MemoryBand::Gib128 => Self::Gib128,
            sys::MemoryBand::Gib256 => Self::Gib256,
            sys::MemoryBand::Gib512 => Self::Gib512,
            sys::MemoryBand::Gib1024 => Self::Gib1024,
            sys::MemoryBand::Over1024 => Self::Over1024,
        }
    }
}

/// One selected graphics adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuProfile {
    pub vendor: GpuVendor,
    pub backend: GpuBackend,
    pub gpu_type: GpuType,
    pub unified_memory: bool,
    pub vendor_id: Option<u16>,
    pub device_id: Option<u16>,
    /// Apple only, `1..=64`.
    pub generation: Option<u8>,
    /// Apple only.
    pub tier: Option<AppleTier>,
    /// Dedicated memory, when already known.
    pub memory: Option<MemoryBand>,
    pub driver_version: Option<Version>,
}

impl GpuProfile {
    /// The normalized adapter facts as profile values, with a dedicated-memory band if one is
    /// already known.
    pub fn from_facts(facts: &gpu::GpuFacts, memory: Option<MemoryBand>) -> Self {
        Self {
            vendor: match facts.vendor {
                gpu::GpuVendor::Apple => GpuVendor::Apple,
                gpu::GpuVendor::Nvidia => GpuVendor::Nvidia,
                gpu::GpuVendor::Amd => GpuVendor::Amd,
                gpu::GpuVendor::Intel => GpuVendor::Intel,
                gpu::GpuVendor::Qualcomm => GpuVendor::Qualcomm,
                gpu::GpuVendor::Arm => GpuVendor::Arm,
                gpu::GpuVendor::Imagination => GpuVendor::Imagination,
                gpu::GpuVendor::Microsoft => GpuVendor::Microsoft,
                gpu::GpuVendor::Mesa => GpuVendor::Mesa,
                gpu::GpuVendor::Other => GpuVendor::Other,
            },
            backend: match facts.backend {
                gpu::GpuBackend::Metal => GpuBackend::Metal,
                gpu::GpuBackend::Vulkan => GpuBackend::Vulkan,
                gpu::GpuBackend::Dx12 => GpuBackend::Dx12,
                gpu::GpuBackend::Gl => GpuBackend::Gl,
                gpu::GpuBackend::Other => GpuBackend::Other,
            },
            gpu_type: match facts.device_type {
                gpu::GpuType::Integrated => GpuType::Integrated,
                gpu::GpuType::Discrete => GpuType::Discrete,
                gpu::GpuType::Virtual => GpuType::Virtual,
                gpu::GpuType::Cpu => GpuType::Cpu,
                gpu::GpuType::Other => GpuType::Other,
            },
            unified_memory: facts.unified_memory,
            vendor_id: facts.vendor_id,
            device_id: facts.device_id,
            generation: facts.apple_generation,
            tier: facts.apple_tier.map(|tier| match tier {
                gpu::AppleTier::Base => AppleTier::Base,
                gpu::AppleTier::Pro => AppleTier::Pro,
                gpu::AppleTier::Max => AppleTier::Max,
                gpu::AppleTier::Ultra => AppleTier::Ultra,
            }),
            memory,
            driver_version: facts.driver_version.map(Version::from),
        }
    }

    /// What two listed adapters may not share.
    fn identity(
        &self,
    ) -> (
        GpuVendor,
        Option<u16>,
        Option<u16>,
        Option<u8>,
        Option<AppleTier>,
    ) {
        (
            self.vendor,
            self.vendor_id,
            self.device_id,
            self.generation,
            self.tier,
        )
    }
}

/// The wire `device` object: validated, inline (no heap), at most 2 KiB serialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceProfile {
    os: OsProfile,
    arch: Arch,
    cpu: CpuProfile,
    memory: MemoryBand,
    gpus: [Option<GpuProfile>; 2],
    renderer: Renderer,
    renderer_fallback: Option<RendererFallback>,
}

/// Why a device profile was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileError(pub &'static str);

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ProfileError {}

impl DeviceProfile {
    /// A profile of already-normalized values, if the wire format allows the combination.
    pub fn new(
        os: OsProfile,
        arch: Arch,
        cpu: CpuProfile,
        memory: MemoryBand,
        gpus: &[GpuProfile],
        renderer: Renderer,
        renderer_fallback: Option<RendererFallback>,
    ) -> Result<Self, ProfileError> {
        if os.family != OsFamily::Macos && os.build.is_some() {
            return Err(ProfileError("only macOS carries a build"));
        }
        if os.family != OsFamily::Linux
            && (os.distro.is_some() || os.distro_version.is_some() || os.kernel.is_some())
        {
            return Err(ProfileError("only Linux carries a distribution or kernel"));
        }
        for cores in [cpu.physical_cores, cpu.logical_cores] {
            if !(1..=1024).contains(&cores) {
                return Err(ProfileError("core counts are 1 to 1024"));
            }
        }
        if cpu.vendor != CpuVendor::Apple && (cpu.generation.is_some() || cpu.tier.is_some()) {
            return Err(ProfileError(
                "only an Apple CPU carries a generation or tier",
            ));
        }
        if cpu.generation.is_some_and(|g| !(1..=64).contains(&g)) {
            return Err(ProfileError("an Apple generation is 1 to 64"));
        }
        let signature = [cpu.family, cpu.model, cpu.stepping];
        if !matches!(cpu.vendor, CpuVendor::Intel | CpuVendor::Amd)
            && signature.iter().any(Option::is_some)
        {
            return Err(ProfileError(
                "only an Intel or AMD CPU carries family, model or stepping",
            ));
        }
        if signature.iter().flatten().any(|value| *value > 4095) {
            return Err(ProfileError("family, model and stepping are 0 to 4095"));
        }
        if gpus.len() > 2 {
            return Err(ProfileError("at most two GPUs"));
        }
        for gpu in gpus {
            if gpu.vendor != GpuVendor::Apple && (gpu.generation.is_some() || gpu.tier.is_some()) {
                return Err(ProfileError(
                    "only an Apple GPU carries a generation or tier",
                ));
            }
            if gpu.generation.is_some_and(|g| !(1..=64).contains(&g)) {
                return Err(ProfileError("an Apple generation is 1 to 64"));
            }
        }
        if gpus.len() == 2 && gpus[0].identity() == gpus[1].identity() {
            return Err(ProfileError("the GPUs are the same model"));
        }
        if renderer_fallback.is_some() && renderer != Renderer::Reference {
            return Err(ProfileError(
                "a fallback reason needs the reference renderer",
            ));
        }
        let profile = Self {
            os,
            arch,
            cpu,
            memory,
            gpus: [gpus.first().copied(), gpus.get(1).copied()],
            renderer,
            renderer_fallback,
        };
        if profile.encoded_len() > MAX_DEVICE_BYTES {
            return Err(ProfileError("the device profile exceeds 2048 bytes"));
        }
        Ok(profile)
    }

    pub fn os(&self) -> &OsProfile {
        &self.os
    }

    pub fn arch(&self) -> Arch {
        self.arch
    }

    pub fn cpu(&self) -> &CpuProfile {
        &self.cpu
    }

    pub fn memory(&self) -> MemoryBand {
        self.memory
    }

    pub fn gpus(&self) -> impl Iterator<Item = &GpuProfile> {
        self.gpus.iter().flatten()
    }

    pub fn renderer(&self) -> Renderer {
        self.renderer
    }

    pub fn renderer_fallback(&self) -> Option<RendererFallback> {
        self.renderer_fallback
    }

    /// The serialized object's length in bytes.
    pub fn encoded_len(&self) -> usize {
        let mut counter = super::encode::Counter::default();
        self.write_json(&mut counter).expect("counting cannot fail");
        counter.0
    }

    /// Write the compact `device` object.
    pub fn write_json(&self, w: &mut impl Write) -> io::Result<()> {
        let os = &self.os;
        write!(
            w,
            "{{\"os\":{{\"family\":\"{}\",\"version\":",
            os.family.as_str()
        )?;
        os.version.write(w)?;
        if let Some(build) = &os.build {
            write!(w, ",\"build\":\"{}\"", build.as_str())?;
        }
        if let Some(distro) = os.distro {
            write!(w, ",\"distro\":\"{}\"", distro.as_str())?;
        }
        if let Some(version) = &os.distro_version {
            w.write_all(b",\"distro_version\":")?;
            version.write(w)?;
        }
        if let Some(kernel) = &os.kernel {
            w.write_all(b",\"kernel\":")?;
            kernel.write(w)?;
        }
        let cpu = &self.cpu;
        write!(
            w,
            "}},\"arch\":\"{}\",\"cpu\":{{\"vendor\":\"{}\"",
            self.arch.as_str(),
            cpu.vendor.as_str()
        )?;
        if let Some(generation) = cpu.generation {
            write!(w, ",\"generation\":{generation}")?;
        }
        if let Some(tier) = cpu.tier {
            write!(w, ",\"tier\":\"{}\"", tier.as_str())?;
        }
        for (key, value) in [
            ("family", cpu.family),
            ("model", cpu.model),
            ("stepping", cpu.stepping),
        ] {
            if let Some(value) = value {
                write!(w, ",\"{key}\":{value}")?;
            }
        }
        write!(
            w,
            ",\"physical_cores\":{},\"logical_cores\":{}}},\"memory_gib_band\":",
            cpu.physical_cores, cpu.logical_cores
        )?;
        self.memory.write(w)?;
        w.write_all(b",\"gpus\":[")?;
        for (index, gpu) in self.gpus().enumerate() {
            if index > 0 {
                w.write_all(b",")?;
            }
            write!(
                w,
                "{{\"vendor\":\"{}\",\"backend\":\"{}\",\"type\":\"{}\"",
                gpu.vendor.as_str(),
                gpu.backend.as_str(),
                gpu.gpu_type.as_str()
            )?;
            if let Some(id) = gpu.vendor_id {
                write!(w, ",\"vendor_id\":{id}")?;
            }
            if let Some(id) = gpu.device_id {
                write!(w, ",\"device_id\":{id}")?;
            }
            if let Some(generation) = gpu.generation {
                write!(w, ",\"generation\":{generation}")?;
            }
            if let Some(tier) = gpu.tier {
                write!(w, ",\"tier\":\"{}\"", tier.as_str())?;
            }
            write!(w, ",\"unified_memory\":{}", gpu.unified_memory)?;
            if let Some(memory) = gpu.memory {
                w.write_all(b",\"memory_gib_band\":")?;
                memory.write(w)?;
            }
            if let Some(version) = &gpu.driver_version {
                w.write_all(b",\"driver_version\":")?;
                version.write(w)?;
            }
            w.write_all(b"}")?;
        }
        write!(w, "],\"renderer\":\"{}\"", self.renderer.as_str())?;
        if let Some(fallback) = self.renderer_fallback {
            write!(w, ",\"renderer_fallback\":\"{}\"", fallback.as_str())?;
        }
        w.write_all(b"}")
    }

    /// The validator's reading of a `device` object: closed keys, fixed enums, ranges, and the
    /// combinations [`Self::new`] allows.
    pub(super) fn decode(json: &Json) -> Result<Self, SchemaError> {
        let device = closed(
            json,
            &["os", "arch", "cpu", "memory_gib_band", "gpus", "renderer"],
            &["renderer_fallback"],
            "device",
        )?;
        let os = closed(
            device.req("os")?,
            &["family", "version"],
            &["build", "distro", "distro_version", "kernel"],
            "device.os",
        )?;
        let os = OsProfile {
            family: enum_of(os.req("family")?, OsFamily::parse, "os.family")?,
            version: Version::decode(os.req("version")?, "os.version")?,
            build: os
                .get("build")
                .map(|b| match b {
                    Json::Str(text) => MacBuild::parse(text)
                        .ok_or_else(|| SchemaError::new("os.build has the macOS build shape")),
                    _ => Err(SchemaError::new("os.build is a string")),
                })
                .transpose()?,
            distro: os
                .get("distro")
                .map(|d| enum_of(d, LinuxDistro::parse, "os.distro"))
                .transpose()?,
            distro_version: os
                .get("distro_version")
                .map(|v| Version::decode(v, "os.distro_version"))
                .transpose()?,
            kernel: os
                .get("kernel")
                .map(|v| Version::decode(v, "os.kernel"))
                .transpose()?,
        };
        let cpu = closed(
            device.req("cpu")?,
            &["vendor", "physical_cores", "logical_cores"],
            &["generation", "tier", "family", "model", "stepping"],
            "device.cpu",
        )?;
        let small = |key: &str, max: u64| -> Result<Option<u64>, SchemaError> {
            cpu.get(key).map(|value| int(value, max, key)).transpose()
        };
        let cores = |key: &str| -> Result<u16, SchemaError> {
            let cores = int(cpu.req(key)?, 1024, key)?;
            if cores == 0 {
                return Err(SchemaError::new(format!("cpu.{key} is 1 to 1024")));
            }
            Ok(cores as u16)
        };
        let cpu = CpuProfile {
            vendor: enum_of(cpu.req("vendor")?, CpuVendor::parse, "cpu.vendor")?,
            physical_cores: cores("physical_cores")?,
            logical_cores: cores("logical_cores")?,
            generation: generation(cpu.get("generation"))?,
            tier: cpu
                .get("tier")
                .map(|t| enum_of(t, AppleTier::parse, "cpu.tier"))
                .transpose()?,
            family: small("family", 4095)?.map(|v| v as u16),
            model: small("model", 4095)?.map(|v| v as u16),
            stepping: small("stepping", 4095)?.map(|v| v as u16),
        };
        let Json::Array(gpu_items) = device.req("gpus")? else {
            return Err(SchemaError::new("gpus is an array"));
        };
        if gpu_items.len() > 2 {
            return Err(SchemaError::new("at most two GPUs"));
        }
        let mut gpus = Vec::with_capacity(gpu_items.len());
        for item in gpu_items {
            let gpu = closed(
                item,
                &["vendor", "backend", "type", "unified_memory"],
                &[
                    "vendor_id",
                    "device_id",
                    "generation",
                    "tier",
                    "memory_gib_band",
                    "driver_version",
                ],
                "gpu",
            )?;
            let id = |key: &str| -> Result<Option<u16>, SchemaError> {
                gpu.get(key)
                    .map(|value| int(value, 65_535, key).map(|v| v as u16))
                    .transpose()
            };
            gpus.push(GpuProfile {
                vendor: enum_of(gpu.req("vendor")?, GpuVendor::parse, "gpu.vendor")?,
                backend: enum_of(gpu.req("backend")?, GpuBackend::parse, "gpu.backend")?,
                gpu_type: enum_of(gpu.req("type")?, GpuType::parse, "gpu.type")?,
                unified_memory: match gpu.req("unified_memory")? {
                    Json::Bool(value) => *value,
                    _ => return Err(SchemaError::new("gpu.unified_memory is a boolean")),
                },
                vendor_id: id("vendor_id")?,
                device_id: id("device_id")?,
                generation: generation(gpu.get("generation"))?,
                tier: gpu
                    .get("tier")
                    .map(|t| enum_of(t, AppleTier::parse, "gpu.tier"))
                    .transpose()?,
                memory: gpu
                    .get("memory_gib_band")
                    .map(MemoryBand::decode)
                    .transpose()?,
                driver_version: gpu
                    .get("driver_version")
                    .map(|v| Version::decode(v, "gpu.driver_version"))
                    .transpose()?,
            });
        }
        let profile = Self::new(
            os,
            enum_of(device.req("arch")?, Arch::parse, "arch")?,
            cpu,
            MemoryBand::decode(device.req("memory_gib_band")?)?,
            &gpus,
            enum_of(device.req("renderer")?, Renderer::parse, "renderer")?,
            device
                .get("renderer_fallback")
                .map(|f| enum_of(f, RendererFallback::parse, "renderer_fallback"))
                .transpose()?,
        )
        .map_err(|error| SchemaError::new(error.0))?;
        Ok(profile)
    }
}

fn generation(json: Option<&Json>) -> Result<Option<u8>, SchemaError> {
    json.map(|value| {
        let generation = int(value, 64, "generation")?;
        if generation == 0 {
            return Err(SchemaError::new("a generation is 1 to 64"));
        }
        Ok(generation as u8)
    })
    .transpose()
}

pub(super) fn enum_of<T>(
    json: &Json,
    parse: fn(&str) -> Option<T>,
    what: &str,
) -> Result<T, SchemaError> {
    match json {
        Json::Str(text) => {
            parse(text).ok_or_else(|| SchemaError::new(format!("{what}: unknown value")))
        }
        _ => Err(SchemaError::new(format!("{what} is a string"))),
    }
}
