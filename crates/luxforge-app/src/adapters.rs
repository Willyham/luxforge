//! The graphics adapters wgpu offers this host, for diagnostics: the adapter an evidence run records
//! as the one that drew it, and the list `--gpu-adapters` prints; and a device of its own on the
//! adapter that drew, for the tile runner ([`tile_runner`]).
//!
//! Iced hands the photo surface a device and a queue but not the adapter they came from, and its
//! system information names only the adapter and its backend. So the editor learns the rest the
//! one way it can without Iced's own handle: an instance of its own enumerates the adapters of that
//! backend, and the one whose backend and name match what the renderer reported is the adapter that
//! drew ([`matching`]). Nothing here draws, compiles or allocates on an adapter but [`tile_runner`], which
//! requests a device on the adapter of a backend and name, as Iced's renderer requests its own.
//!
//! Before the window opens, a launch also asks what the renderer's backends offer ([`probe`]) and
//! chooses its renderer ([`choose`]): wgpu's request ranks a software adapter (lavapipe, WARP)
//! last but never leaves it out, so a host with no hardware adapter draws on the software one,
//! and until the software adapter is adopted ([`SOFTWARE_ADAPTER_ADOPTED`]) such a launch refuses
//! the GPU stage, as `--no-gpu-render` does, unless it asks for the software adapter.
//!
//! Enumerating creates a wgpu instance, which loads the platform's drivers, and opening a device
//! creates one too: never call either on the UI thread or in a future the UI loop polls, only on a
//! blocking thread or before the window exists.
//!
//! Names are wgpu's own `Debug` spellings, so they read as Iced's system information does: the
//! backend `Metal`, `Vulkan`, `Dx12` or `Gl`, and the device type `DiscreteGpu`, `IntegratedGpu`,
//! `VirtualGpu`, `Cpu` (a software rasterizer, such as Mesa's lavapipe or llvmpipe) or `Other`.

pub use luxforge_gpu::adapters::{Adapter, backends_named, enumerate, matching};
#[cfg(test)]
use luxforge_gpu::adapters::{Opened, Unopened};
/// The backend set selected by the renderer's environment.
pub fn renderer_backends() -> wgpu::Backends {
    wgpu::Backends::from_env().unwrap_or(wgpu::Backends::all())
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

/// Whether the software adapter is adopted: the owner's decision of 2026-10-05
/// (`docs/decisions.md`, "GPU-first rendering") that a session with no usable hardware adapter
/// draws through the GPU path on the platform's software adapter (Mesa's lavapipe on Linux, WARP
/// on Windows) once it is measured fast enough that dragging is not badly laggy, and until then is
/// a proposal. Off: on 2026-10-06 lavapipe missed the drag thresholds in a container on the
/// owner's M4 (`docs/specs/performance.md`, "Software adapters"), so a session whose only adapter
/// is a software one draws the reference renderer's frames unless its launch passes
/// `--software-adapter` ([`choose`]).
pub const SOFTWARE_ADAPTER_ADOPTED: bool = false;

/// Whether wgpu describes an adapter of `device_type` (its `Debug` spelling) as a software one: a
/// rasterizer on the CPU, such as lavapipe, llvmpipe or WARP.
pub fn is_software(device_type: &str) -> bool {
    device_type == "Cpu"
}

/// What the renderer's backends offer this host, as a launch finds it before its window opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offered {
    /// Not looked for: on macOS, which has no software Metal, so every adapter is a hardware one,
    /// or for a launch that refused the GPU stage.
    NotProbed,
    /// At least one hardware adapter, which wgpu's request ranks before any software one, so the
    /// window draws on a hardware adapter.
    Hardware,
    /// Only software adapters: the window's request lands on the first, which is named.
    SoftwareOnly(Adapter),
    /// No adapter at all.
    Nothing,
}

/// What `adapters`, the adapters of the renderer's backends, offer: a hardware adapter before a
/// software one, as wgpu's request ranks them (it ranks a software adapter last but never leaves
/// it out, so a host with nothing else draws on it).
pub fn offered(adapters: &[Adapter]) -> Offered {
    if adapters
        .iter()
        .any(|adapter| !is_software(&adapter.device_type))
    {
        return Offered::Hardware;
    }
    adapters
        .first()
        .cloned()
        .map_or(Offered::Nothing, Offered::SoftwareOnly)
}

/// What this host offers the renderer's backends, found before the window opens: an enumeration
/// of every adapter of [`renderer_backends`] off macOS, and [`Offered::NotProbed`] on it, which
/// has no software adapter to find. Blocking: see the module documentation.
pub fn probe() -> Offered {
    if cfg!(target_os = "macos") {
        return Offered::NotProbed;
    }
    offered(&enumerate(renderer_backends()))
}

/// Which renderer a launch draws with, chosen before its window opens ([`choose`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchRenderer {
    /// The photo surface's GPU stage on the adapter the window draws with, a software one when
    /// `software`.
    Gpu { software: bool },
    /// The reference renderer's frames: the GPU stage is refused before the window opens, for the
    /// reason given.
    Reference(Refusal),
}

/// Why a launch refuses the GPU stage before its window opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `--no-gpu-render`.
    Requested,
    /// The host offers only a software adapter, which is not adopted
    /// ([`SOFTWARE_ADAPTER_ADOPTED`]) and the launch did not ask for (`--software-adapter`).
    SoftwareNotAdopted,
    /// The host offers no adapter at all.
    NoAdapter,
}

impl LaunchRenderer {
    /// Whether the launch refuses the GPU stage.
    pub fn refused(self) -> bool {
        matches!(self, Self::Reference(_))
    }

    /// Whether the GPU stage draws on a software adapter.
    pub fn software(self) -> bool {
        matches!(self, Self::Gpu { software: true })
    }
}

/// The renderer a launch draws with, in this order: `--no-gpu-render` (`refused`) refuses the
/// GPU stage whatever the host offers; a hardware adapter, or a host not probed, draws on the GPU;
/// a host with only a software adapter draws on it through the GPU path when the software adapter
/// is `adopted` or the launch asked for it (`--software-adapter`), and otherwise refuses the GPU
/// stage, so every frame is the reference renderer's; a host with no adapter refuses it too.
pub fn choose(refused: bool, asked: bool, adopted: bool, offered: &Offered) -> LaunchRenderer {
    match offered {
        _ if refused => LaunchRenderer::Reference(Refusal::Requested),
        Offered::NotProbed | Offered::Hardware => LaunchRenderer::Gpu { software: false },
        Offered::SoftwareOnly(_) if adopted || asked => LaunchRenderer::Gpu { software: true },
        Offered::SoftwareOnly(_) => LaunchRenderer::Reference(Refusal::SoftwareNotAdopted),
        Offered::Nothing => LaunchRenderer::Reference(Refusal::NoAdapter),
    }
}

/// Open the separate tile-worker device with the renderer's current backend/limit policy.
#[cfg(test)]
pub fn open(backend: &str, name: &str) -> Result<Opened, Unopened> {
    luxforge_gpu::adapters::open(backend, name, renderer_backends(), &renderer_limits())
}

/// The desktop owns refusal, backend and device-limit policy for its separate worker device.
pub fn tile_runner(
    backend: &str,
    name: &str,
) -> Result<luxforge_gpu::tiles::TileRunner, luxforge_gpu::tiles::TileRefusal> {
    luxforge_gpu::tiles::TileRunner::open_with(
        luxforge_gpu::gpu_stage_refused(),
        backend,
        name,
        renderer_backends(),
        &renderer_limits(),
    )
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

    /// A hardware adapter is offered before any software one, as wgpu's request ranks them, and a
    /// host with only software adapters offers the first, which the window's request lands on.
    #[test]
    fn a_hardware_adapter_is_offered_before_a_software_one() {
        let lavapipe = adapter("llvmpipe (LLVM 19.1.7, 128 bits)", "Vulkan", "Cpu");
        let warp = adapter("Microsoft Basic Render Driver", "Dx12", "Cpu");
        let gpu = adapter("AMD Radeon RX 7600", "Vulkan", "DiscreteGpu");
        assert_eq!(
            offered(&[lavapipe.clone(), gpu.clone()]),
            Offered::Hardware,
            "a hardware adapter after a software one is still the one drawn with"
        );
        assert_eq!(offered(std::slice::from_ref(&gpu)), Offered::Hardware);
        assert_eq!(
            offered(&[lavapipe.clone(), warp]),
            Offered::SoftwareOnly(lavapipe)
        );
        assert_eq!(offered(&[]), Offered::Nothing);
        // A virtual or unknown adapter is not a software one.
        assert_eq!(
            offered(&[adapter("virgl", "Vulkan", "VirtualGpu")]),
            Offered::Hardware
        );
        assert!(is_software("Cpu"));
        assert!(!is_software("IntegratedGpu") && !is_software("Other"));
    }

    /// The selection order: `--no-gpu-render` refuses the GPU stage whatever the host offers; a
    /// hardware adapter, or a host not probed, draws on the GPU; a software adapter alone draws
    /// through the GPU path only when adopted or asked for, and otherwise refuses it; no adapter
    /// refuses it.
    #[test]
    fn a_software_adapter_draws_only_when_adopted_or_asked_for() {
        use LaunchRenderer::{Gpu, Reference};
        let software = Offered::SoftwareOnly(adapter("llvmpipe", "Vulkan", "Cpu"));
        let every = [
            Offered::NotProbed,
            Offered::Hardware,
            software.clone(),
            Offered::Nothing,
        ];
        for offered in &every {
            for (asked, adopted) in [(false, false), (true, false), (false, true), (true, true)] {
                assert_eq!(
                    choose(true, asked, adopted, offered),
                    Reference(Refusal::Requested),
                    "--no-gpu-render refuses the stage on {offered:?}"
                );
                if !matches!(offered, Offered::SoftwareOnly(_)) {
                    let expected = match offered {
                        Offered::Nothing => Reference(Refusal::NoAdapter),
                        _ => Gpu { software: false },
                    };
                    assert_eq!(choose(false, asked, adopted, offered), expected);
                }
            }
        }
        assert_eq!(
            choose(false, false, false, &software),
            Reference(Refusal::SoftwareNotAdopted)
        );
        assert_eq!(
            choose(false, true, false, &software),
            Gpu { software: true }
        );
        assert_eq!(
            choose(false, false, true, &software),
            Gpu { software: true }
        );
        assert!(Gpu { software: true }.software() && !Gpu { software: true }.refused());
        assert!(Reference(Refusal::SoftwareNotAdopted).refused());
        assert!(!Reference(Refusal::NoAdapter).software());
    }

    /// The decision constant stays off until the measurement passes: a software-only session draws
    /// the reference renderer's frames unless its launch asks for the software adapter.
    #[test]
    fn the_software_adapter_is_not_adopted() {
        const { assert!(!SOFTWARE_ADAPTER_ADOPTED) };
        let software = Offered::SoftwareOnly(adapter("llvmpipe", "Vulkan", "Cpu"));
        assert_eq!(
            choose(false, false, SOFTWARE_ADAPTER_ADOPTED, &software),
            LaunchRenderer::Reference(Refusal::SoftwareNotAdopted)
        );
    }

    /// macOS has no software Metal: the launch never enumerates there, and elsewhere it finds what
    /// an enumeration of the renderer's backends finds.
    #[test]
    fn a_probe_looks_for_a_software_adapter_only_off_macos() {
        let found = probe();
        eprintln!("probed: {found:?}");
        if cfg!(target_os = "macos") {
            assert_eq!(found, Offered::NotProbed);
        } else {
            assert_eq!(found, offered(&enumerate(renderer_backends())));
        }
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
        for (backend, backends) in [
            (wgpu::Backend::Metal, wgpu::Backends::METAL),
            (wgpu::Backend::Vulkan, wgpu::Backends::VULKAN),
            (wgpu::Backend::Dx12, wgpu::Backends::DX12),
            (wgpu::Backend::Gl, wgpu::Backends::GL),
        ] {
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
