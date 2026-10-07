//! Graphics enumeration and device opening. Renderer/refusal policy is supplied by the host.
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

/// Open the exact backend/name among `backends`, with the caller's limit requests in order.
/// The instance has no flags; requests have no features, memory hints for usage, no trace and
/// no experimental features. No other adapter is substituted. Blocking: call off the UI thread.
pub fn open(
    backend: &str,
    name: &str,
    backends: wgpu::Backends,
    limits: &[wgpu::Limits],
) -> Result<Opened, Unopened> {
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
    for required_limits in limits.iter().cloned() {
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
