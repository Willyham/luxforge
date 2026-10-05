//! The graphics adapters wgpu offers this host, for diagnostics: the adapter an evidence run records
//! as the one that drew it, and the list `--gpu-adapters` prints; and a device of its own on the
//! adapter that drew, for the tile runner ([`open`]).
//!
//! Iced hands the photo surface a device and a queue but not the adapter they came from, and its
//! system information names only the adapter and its backend. So the editor learns the rest the
//! one way it can without Iced's own handle: an instance of its own enumerates the adapters of that
//! backend, and the one whose backend and name match what the renderer reported is the adapter that
//! drew ([`matching`]). Nothing here draws, compiles or allocates on an adapter but [`open`], which
//! requests a device on the adapter of a backend and name, as Iced's renderer requests its own.
//!
//! Enumerating creates a wgpu instance, which loads the platform's drivers, and opening a device
//! creates one too: never call either on the UI thread or in a future the UI loop polls, only on a
//! blocking thread or before the window exists.
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

/// The limits Iced's renderer requests its device with, in the order it requests them: wgpu's
/// defaults, then the downlevel defaults, each with two bind groups and 2,048 non-sampler bindings
/// (`iced_wgpu` 0.14.0, `src/window/compositor.rs`, `Compositor::request`).
pub fn renderer_limits() -> [wgpu::Limits; 2] {
    [wgpu::Limits::default(), wgpu::Limits::downlevel_defaults()].map(|limits| wgpu::Limits {
        max_bind_groups: 2,
        max_non_sampler_bindings: 2048,
        ..limits
    })
}

/// A device of its own on an adapter, opened by [`open`].
pub struct Opened {
    /// The adapter it was opened on, as wgpu describes it.
    pub adapter: Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Why [`open`] opened no device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unopened {
    /// wgpu offers this host no adapter of the renderer's backends.
    NoAdapter,
    /// No adapter it offers has the backend and name asked for: what it offers instead.
    Mismatch { offered: Vec<Adapter> },
    /// The adapter refused every device request, each for the reason given.
    NoDevice(Vec<String>),
}

/// A device of its own on the adapter whose backend and name are `backend` and `name` — the
/// adapter Iced's renderer reports drawing with — among the adapters of the renderer's backends
/// ([`renderer_backends`]), the first when two identical adapters share them, as [`matching`]
/// finds it: never another adapter in its place. Requested as Iced's renderer requests its own
/// (`iced_wgpu` 0.14.0, `src/window/compositor.rs`): an instance with no flags, then no features,
/// [`renderer_limits`] in order until one is granted, memory hints for usage, no trace and no
/// experimental features; only the label differs. Blocking: see the module documentation.
pub fn open(backend: &str, name: &str) -> Result<Opened, Unopened> {
    let backends = renderer_backends();
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::empty(),
        ..wgpu::InstanceDescriptor::default()
    });
    let offered = instance.enumerate_adapters(backends);
    if offered.is_empty() {
        return Err(Unopened::NoAdapter);
    }
    let found = offered.iter().position(|adapter| {
        let info = adapter.get_info();
        format!("{:?}", info.backend) == backend && info.name == name
    });
    let Some(adapter) = found.map(|at| &offered[at]) else {
        return Err(Unopened::Mismatch {
            offered: offered
                .iter()
                .map(|adapter| Adapter::of(adapter.get_info()))
                .collect(),
        });
    };
    let mut refusals = Vec::new();
    for required_limits in renderer_limits() {
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("luxforge.tiles.device"),
            required_features: wgpu::Features::empty(),
            required_limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        };
        match ready(adapter.request_device(&descriptor)) {
            Some(Ok((device, queue))) => {
                return Ok(Opened {
                    adapter: Adapter::of(adapter.get_info()),
                    device,
                    queue,
                });
            }
            Some(Err(error)) => refusals.push(error.to_string()),
            None => refusals.push("the request was not answered without waiting".into()),
        }
    }
    Err(Unopened::NoDevice(refusals))
}

/// `future`'s output when it is ready at its first poll, as wgpu's native requests are.
fn ready<F: std::future::Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(value) => Some(value),
        std::task::Poll::Pending => None,
    }
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

    /// A device of its own is requested with Iced's limits, in Iced's order: wgpu's defaults, then
    /// the downlevel ones, each with two bind groups and 2,048 non-sampler bindings, nothing else
    /// changed.
    #[test]
    fn a_device_of_its_own_asks_for_iceds_limits_in_iceds_order() {
        let [first, second] = renderer_limits();
        for (asked, base) in [
            (first, wgpu::Limits::default()),
            (second, wgpu::Limits::downlevel_defaults()),
        ] {
            assert_eq!(asked.max_bind_groups, 2);
            assert_eq!(asked.max_non_sampler_bindings, 2048);
            assert_eq!(
                wgpu::Limits {
                    max_bind_groups: base.max_bind_groups,
                    max_non_sampler_bindings: base.max_non_sampler_bindings,
                    ..asked
                },
                base
            );
        }
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
