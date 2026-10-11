//! The graphics adapter facts a device profile may carry, normalized from what wgpu already
//! reported about an adapter the app selected.
//!
//! [`normalize`] is a pure function of an [`AdapterDescription`]: it creates no device, enumerates
//! nothing and reads no system state. The result holds enums, bounded integers and numeric driver
//! version components only. The adapter's name and driver strings are read here, to recognise an
//! Apple generation and tier and a version number through fixed parsers, and are not kept: an
//! unrecognised name, vendor or driver string yields `Other` or `None`, never a copy.

/// What wgpu reported for one adapter, as the app already holds it
/// (`luxforge_gpu::adapters::Adapter`, whose strings are wgpu's `Debug` spellings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterDescription<'a> {
    pub name: &'a str,
    /// The backend's vendor identity: a PCI vendor id in its low 16 bits where the backend has one.
    pub vendor: u32,
    pub device: u32,
    /// `DiscreteGpu`, `IntegratedGpu`, `VirtualGpu`, `Cpu` or `Other`.
    pub device_type: &'a str,
    /// `Metal`, `Vulkan`, `Dx12` or `Gl`.
    pub backend: &'a str,
    pub driver: &'a str,
    pub driver_info: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuVendor {
    Apple,
    Nvidia,
    Amd,
    Intel,
    Qualcomm,
    Arm,
    Imagination,
    Microsoft,
    Mesa,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuBackend {
    Metal,
    Vulkan,
    Dx12,
    Gl,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuType {
    Integrated,
    Discrete,
    Virtual,
    /// A software rasterizer: lavapipe, llvmpipe, WARP.
    Cpu,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleTier {
    Base,
    Pro,
    Max,
    Ultra,
}

/// One to four numeric driver version components.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DriverVersion {
    parts: [u32; 4],
    len: u8,
}

impl DriverVersion {
    pub fn parts(&self) -> &[u32] {
        &self.parts[..usize::from(self.len)]
    }
}

impl std::fmt::Debug for DriverVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.parts()).finish()
    }
}

/// The approved facts of one adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuFacts {
    pub vendor: GpuVendor,
    pub backend: GpuBackend,
    pub device_type: GpuType,
    /// Whether the GPU shares system memory with the CPU: Apple silicon, an integrated GPU or a
    /// software rasterizer.
    pub unified_memory: bool,
    /// The PCI vendor id, when the backend reports one that fits `0..=65535`.
    pub vendor_id: Option<u16>,
    /// The PCI device id, when it fits `0..=65535` and a vendor id is present.
    pub device_id: Option<u16>,
    /// Apple silicon generation, `1..=64`, from the adapter name.
    pub apple_generation: Option<u8>,
    pub apple_tier: Option<AppleTier>,
    pub driver_version: Option<DriverVersion>,
}

/// Normalize one adapter.
pub fn normalize(adapter: &AdapterDescription<'_>) -> GpuFacts {
    let backend = match adapter.backend {
        "Metal" => GpuBackend::Metal,
        "Vulkan" => GpuBackend::Vulkan,
        "Dx12" => GpuBackend::Dx12,
        "Gl" => GpuBackend::Gl,
        _ => GpuBackend::Other,
    };
    let device_type = match adapter.device_type {
        "IntegratedGpu" => GpuType::Integrated,
        "DiscreteGpu" => GpuType::Discrete,
        "VirtualGpu" => GpuType::Virtual,
        "Cpu" => GpuType::Cpu,
        _ => GpuType::Other,
    };
    let apple = (backend == GpuBackend::Metal)
        .then(|| parse_apple_name(adapter.name))
        .flatten();
    let vendor = if apple.is_some() || (backend == GpuBackend::Metal && adapter.vendor == 0x106B) {
        GpuVendor::Apple
    } else if is_mesa_software(adapter) || adapter.vendor == 0x10005 {
        GpuVendor::Mesa
    } else {
        match adapter.vendor {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 | 0x8087 => GpuVendor::Intel,
            0x5143 => GpuVendor::Qualcomm,
            0x13B5 => GpuVendor::Arm,
            0x1010 => GpuVendor::Imagination,
            0x1414 => GpuVendor::Microsoft,
            0x106B => GpuVendor::Apple,
            _ => GpuVendor::Other,
        }
    };
    // Only a vendor id that is a real PCI id is a model identifier; Vulkan's vendor ids above
    // 0xFFFF (Mesa and other non-PCI drivers) and an absent id (0) are omitted along with the
    // device id they qualify; a device id of 0 is absent too.
    let vendor_id = u16::try_from(adapter.vendor).ok().filter(|&id| id != 0);
    let device_id = vendor_id.and(u16::try_from(adapter.device).ok().filter(|&id| id != 0));
    GpuFacts {
        vendor,
        backend,
        device_type,
        unified_memory: apple.is_some()
            || matches!(device_type, GpuType::Integrated | GpuType::Cpu),
        vendor_id,
        device_id,
        apple_generation: apple.map(|(generation, _)| generation),
        apple_tier: apple.and_then(|(_, tier)| tier),
        driver_version: parse_driver_version(adapter.driver_info),
    }
}

/// Mesa's software rasterizers name themselves `llvmpipe (...)`, `softpipe`, `lavapipe`.
fn is_mesa_software(adapter: &AdapterDescription<'_>) -> bool {
    let name = adapter.name.trim_start().to_ascii_lowercase();
    ["llvmpipe", "lavapipe", "softpipe"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// `Apple M4 Pro` is generation 4, `Pro`; `Apple M2` is generation 2, `Base`; any other suffix
/// keeps the generation and no tier; a name that is not `Apple M<1..=64>` gives nothing.
pub fn parse_apple_name(name: &str) -> Option<(u8, Option<AppleTier>)> {
    let mut words = name.split_whitespace();
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
    Some((generation, tier))
}

/// The one shape a driver version is read from: an optional `Mesa ` prefix, then dotted decimal
/// components (`560.35.03`, `32.0.101.6078`, `24.2.3`), at most four kept, ending the string or
/// followed by a space, `-`, `+`, `~` or `(`. Anything else is not a version and gives `None`.
pub fn parse_driver_version(info: &str) -> Option<DriverVersion> {
    let text = info.trim();
    let text = text.strip_prefix("Mesa ").unwrap_or(text);
    let run = text
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
    if !matches!(
        text[run..].chars().next(),
        None | Some(' ' | '-' | '+' | '~' | '(')
    ) {
        return None;
    }
    let mut parts = [0u32; 4];
    let mut len = 0;
    for component in text[..run].split('.') {
        if component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if len < 4 {
            parts[len] = component.parse().ok()?;
            len += 1;
        }
    }
    (len > 0).then_some(DriverVersion {
        parts,
        len: len as u8,
    })
}

#[cfg(test)]
mod tests;
