//! The graphics adapters wgpu offers this host, for diagnostics: the adapter an evidence run records
//! as the one that drew it, and the list `--gpu-adapters` prints.
//!
//! Iced hands the photo surface a device and a queue but not the adapter they came from, and its
//! system information names only the adapter and its backend. So the editor learns the rest the
//! one way it can without Iced's own handle: an instance of its own enumerates the adapters of that
//! backend, and the one whose backend and name match what the renderer reported is the adapter that
//! drew ([`matching`]). Nothing here draws, compiles or allocates on an adapter.
//!
//! Enumerating creates a wgpu instance, which loads the platform's drivers: never call it on the UI
//! thread or in a future the UI loop polls, only on a blocking thread or before the window exists.
//!
//! Names are wgpu's own `Debug` spellings, so they read as Iced's system information does: the
//! backend `Metal`, `Vulkan`, `Dx12` or `Gl`, and the device type `DiscreteGpu`, `IntegratedGpu`,
//! `VirtualGpu`, `Cpu` (a software rasterizer, such as Mesa's lavapipe or llvmpipe) or `Other`.

/// One adapter as wgpu describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adapter {
    pub name: String,
    /// The backend's vendor identity: a PCI vendor id in its low 16 bits where the backend has one.
    pub vendor: u32,
    /// The backend's device identity, likewise.
    pub device: u32,
    pub device_type: String,
    pub backend: String,
    pub driver: String,
    pub driver_info: String,
}

impl Adapter {
    fn of(info: wgpu::AdapterInfo) -> Self {
        Self {
            device_type: format!("{:?}", info.device_type),
            backend: format!("{:?}", info.backend),
            name: info.name,
            vendor: info.vendor,
            device: info.device,
            driver: info.driver,
            driver_info: info.driver_info,
        }
    }
}

/// The native backends, as wgpu names them, with the set that enumerates each alone.
const BACKENDS: [(wgpu::Backend, wgpu::Backends); 4] = [
    (wgpu::Backend::Metal, wgpu::Backends::METAL),
    (wgpu::Backend::Vulkan, wgpu::Backends::VULKAN),
    (wgpu::Backend::Dx12, wgpu::Backends::DX12),
    (wgpu::Backend::Gl, wgpu::Backends::GL),
];

/// The backends Iced's renderer chooses its adapter among: `WGPU_BACKEND` when set, as Iced reads
/// it, and otherwise every one.
pub fn renderer_backends() -> wgpu::Backends {
    wgpu::Backends::from_env().unwrap_or(wgpu::Backends::all())
}

/// The backend set named `name` as wgpu's `Debug` spells a backend (`Metal`, `Vulkan`, `Dx12`,
/// `Gl`): what Iced's system information reports. `None` for any other name.
pub fn backends_named(name: &str) -> Option<wgpu::Backends> {
    BACKENDS
        .iter()
        .find(|(backend, _)| format!("{backend:?}") == name)
        .map(|(_, backends)| *backends)
}

/// Every adapter of `backends`, through an instance created for this call alone, with the flags
/// Iced's own instance is created with. Blocking: see the module documentation.
pub fn enumerate(backends: wgpu::Backends) -> Vec<Adapter> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::empty(),
        ..wgpu::InstanceDescriptor::default()
    });
    instance
        .enumerate_adapters(backends)
        .iter()
        .map(|adapter| Adapter::of(adapter.get_info()))
        .collect()
}

/// The adapter among `adapters` whose backend and name are `backend` and `name`, as the renderer
/// reports the adapter it drew with; the first when two identical adapters share them.
pub fn matching<'a>(adapters: &'a [Adapter], backend: &str, name: &str) -> Option<&'a Adapter> {
    adapters
        .iter()
        .find(|adapter| adapter.backend == backend && adapter.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(name: &str, backend: &str, device_type: &str) -> Adapter {
        Adapter {
            name: name.into(),
            vendor: 0x10005,
            device: 0,
            device_type: device_type.into(),
            backend: backend.into(),
            driver: "driver".into(),
            driver_info: "info".into(),
        }
    }

    /// The names are wgpu's own spellings, which Iced's system information uses for the backend.
    #[test]
    fn backend_names_are_wgpus_own_spelling() {
        assert_eq!(backends_named("Metal"), Some(wgpu::Backends::METAL));
        assert_eq!(backends_named("Vulkan"), Some(wgpu::Backends::VULKAN));
        assert_eq!(backends_named("Dx12"), Some(wgpu::Backends::DX12));
        assert_eq!(backends_named("Gl"), Some(wgpu::Backends::GL));
        assert_eq!(
            backends_named("vulkan"),
            None,
            "Iced reports the Debug spelling"
        );
        assert_eq!(backends_named("Noop"), None);
        assert_eq!(format!("{:?}", wgpu::DeviceType::Cpu), "Cpu");
        assert_eq!(
            format!("{:?}", wgpu::DeviceType::IntegratedGpu),
            "IntegratedGpu"
        );
    }

    /// The adapter that drew is the one whose backend and name the renderer reported, and no other.
    #[test]
    fn the_adapter_that_drew_matches_its_backend_and_name() {
        let adapters = [
            adapter("llvmpipe (LLVM 17.0.6, 256 bits)", "Gl", "Cpu"),
            adapter("llvmpipe (LLVM 17.0.6, 256 bits)", "Vulkan", "Cpu"),
            adapter("Apple M4 Pro", "Metal", "IntegratedGpu"),
        ];
        let drew = matching(&adapters, "Vulkan", "llvmpipe (LLVM 17.0.6, 256 bits)");
        assert_eq!(drew, Some(&adapters[1]));
        assert_eq!(matching(&adapters, "Metal", "Apple M4"), None);
        assert_eq!(
            matching(&adapters, "Metal", "Apple M4 Pro").map(|a| a.device_type.as_str()),
            Some("IntegratedGpu")
        );
    }

    /// Enumerating this host's adapters of one backend names only that backend, whatever it finds;
    /// a host with none finds none, which is a report, not GPU evidence.
    #[test]
    fn an_enumeration_names_only_the_backends_asked_for() {
        for (backend, backends) in BACKENDS {
            let found = enumerate(backends);
            eprintln!("{backend:?}: {found:?}");
            assert!(
                found
                    .iter()
                    .all(|adapter| adapter.backend == format!("{backend:?}"))
            );
        }
    }
}
