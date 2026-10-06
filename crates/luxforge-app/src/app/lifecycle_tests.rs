//! The registry a run serves and where the capability host keeps its state.
use super::{
    gpu_tiles::{AdapterNaming, GpuTiles},
    lifecycle::host_config,
};
use crate::Config;
use luxforge_core::ModuleRegistry;
use std::sync::Arc;

/// `config`'s capability host, with a worker of its own as the launch makes it.
fn host_of(config: &Config) -> luxforge_core::HostConfig {
    host_config(config, Arc::new(GpuTiles::pending(config.no_gpu_render)))
}

/// The desktop's `--disable-module`, `--developer` and `--proof-endpoint` reach the one assembly
/// the core owns; what that assembly serves for each is the core's own test
/// (`the_one_assembly_serves_test_modules_only_in_developer_mode`).
#[test]
fn desktop_registry_contains_every_core_builtin_including_raw() {
    let registry = |config: Config| ModuleRegistry::assemble(&config.registry_options());
    let ids = |registry: &ModuleRegistry| {
        registry
            .descriptors()
            .iter()
            .map(|module| module.id.clone())
            .collect::<Vec<_>>()
    };
    let desktop = registry(Config::default()).unwrap();
    assert_eq!(ids(&desktop), ids(&ModuleRegistry::builtin()));
    assert!(ids(&desktop).contains(&"luxforge.raw".to_owned()));
    let developer = registry(Config {
        developer: true,
        proof_endpoint: Some("http://127.0.0.1:9".into()),
        disabled: vec!["luxforge.raw".into()],
        ..Config::default()
    })
    .unwrap();
    for id in [
        "luxforge.pixel",
        "luxforge.controls",
        "luxforge.capabilities",
    ] {
        assert!(ids(&developer).contains(&id.to_owned()), "{id}");
    }
    assert!(
        !developer
            .descriptors()
            .iter()
            .find(|module| module.id == "luxforge.raw")
            .unwrap()
            .is_available()
    );
}

#[test]
fn an_evidence_run_keeps_module_state_in_its_directory_and_memory() {
    let evidence = std::env::temp_dir().join("luxforge-evidence-host-paths");
    let mut config = Config {
        evidence: Some(evidence.clone()),
        data_root: Some(std::env::temp_dir().join("luxforge-ignored-root")),
        ..Config::default()
    };
    config.paths = config.resolve_paths();
    let host = host_of(&config);
    assert_eq!(
        host.config_dir,
        Some(evidence.join("host").join("config").join("modules"))
    );
    assert_eq!(
        host.resource_dir,
        Some(
            evidence
                .join("host")
                .join("data")
                .join("modules")
                .join("resources")
        )
    );
    assert_eq!(host.secrets.name(), "in-memory secret store");
    let root = std::env::temp_dir().join("luxforge-data-root");
    let mut config = Config {
        data_root: Some(root.clone()),
        ..Config::default()
    };
    config.paths = config.resolve_paths();
    let host = host_of(&config);
    assert_eq!(host.config_dir, Some(root.join("config").join("modules")));
    assert_eq!(
        host.resource_dir,
        Some(root.join("data").join("modules").join("resources"))
    );
    assert!(
        !evidence.exists() && !root.exists(),
        "choosing directories creates none"
    );
}

/// What a launch knows of its renderer reaches the owner before the window opens: `--no-gpu-render`
/// is the reference renderer for `no-adapter` from the start, and an ordinary launch the reference
/// until its photo surface has checked the GPU stage.
#[test]
fn fallback_a_forced_launch_tells_the_owner_the_reference_before_its_window_opens() {
    use luxforge_core::{Renderer, RendererReason};
    let evidence = std::env::temp_dir().join("luxforge-evidence-renderer");
    let launch = |no_gpu_render| {
        let mut config = Config {
            evidence: Some(evidence.clone()),
            no_gpu_render,
            ..Config::default()
        };
        config.paths = config.resolve_paths();
        host_of(&config).renderer
    };
    assert_eq!(launch(true), Renderer::reference(RendererReason::NoAdapter));
    assert_eq!(
        launch(false),
        Renderer::reference(RendererReason::SurfacePending)
    );
    assert!(!evidence.exists(), "choosing creates nothing");
}

/// A host whose only adapter is a software one, not adopted, is the reference renderer for
/// `no-adapter` from before the window opens, as a forced launch is, and its tile worker answers
/// every export with the reference for `no-adapter` and takes no adapter; a launch that asked for
/// the software adapter waits for its surface like any other, and its worker for the window's
/// adapter.
#[test]
fn fallback_a_software_only_host_tells_the_owner_its_renderer_before_its_window_opens() {
    use luxforge_core::tiles::{TileFallback, TileService, TileStatus, TileUnavailable};
    use luxforge_core::{Renderer, RendererReason};
    use luxforge_ui::adapters::{LaunchRenderer, Refusal};
    let evidence = std::env::temp_dir().join("luxforge-evidence-software");
    let launch = |chosen| {
        let mut config = Config {
            evidence: Some(evidence.clone()),
            launch_renderer: Some(chosen),
            ..Config::default()
        };
        config.paths = config.resolve_paths();
        host_of(&config).renderer
    };
    assert_eq!(
        launch(LaunchRenderer::Reference(Refusal::SoftwareNotAdopted)),
        Renderer::reference(RendererReason::NoAdapter)
    );
    assert_eq!(
        launch(LaunchRenderer::Gpu { software: true }),
        Renderer::reference(RendererReason::SurfacePending)
    );
    let unavailable = |reason| TileStatus::Reference(Some(TileFallback::Unavailable(reason)));
    let worker = GpuTiles::unavailable(TileUnavailable::NoAdapter);
    assert_eq!(worker.status(), unavailable(TileUnavailable::NoAdapter));
    assert!(!worker.adopt_adapter(
        "Vulkan",
        "llvmpipe (LLVM 19.1.7, 128 bits)",
        AdapterNaming::Window
    ));
    assert_eq!(worker.status(), unavailable(TileUnavailable::NoAdapter));
    assert!(!worker.started());
    assert!(!evidence.exists(), "choosing creates nothing");
}

/// The launch hands the owner its GPU tile worker, which starts nothing until an export asks it
/// and waits for the desktop to name the adapter its window draws with, answering the reference as
/// `surface-pending` until then: named once, it is the GPU's. A `--no-gpu-render` launch's worker is
/// refused and takes no adapter.
#[test]
fn a_launch_hands_the_owner_its_tile_worker_which_waits_for_the_windows_adapter() {
    use luxforge_core::tiles::{TileFallback, TileService, TileStatus, TileUnavailable};
    let unavailable = |reason| TileStatus::Reference(Some(TileFallback::Unavailable(reason)));
    let evidence = std::env::temp_dir().join("luxforge-evidence-tiles");
    let mut config = Config {
        evidence: Some(evidence.clone()),
        ..Config::default()
    };
    config.paths = config.resolve_paths();
    let worker = Arc::new(GpuTiles::pending(false));
    let host = host_config(&config, Arc::clone(&worker));
    let tiles = host.tiles.as_ref().expect("the launch's worker");
    assert_eq!(tiles.status(), unavailable(TileUnavailable::Pending));
    assert!(!worker.started(), "nothing starts before an export asks it");
    assert!(worker.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Window));
    assert_eq!(tiles.status(), TileStatus::Gpu, "the owner's worker, named");
    assert!(
        !worker.adopt_adapter("Vulkan", "llvmpipe", AdapterNaming::Window),
        "the adapter is named once"
    );
    assert!(!worker.started());

    let refused = GpuTiles::pending(true);
    assert_eq!(refused.status(), unavailable(TileUnavailable::Refused));
    assert!(!refused.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Window));
    assert_eq!(refused.status(), unavailable(TileUnavailable::Refused));
    assert!(!evidence.exists(), "choosing creates nothing");
}

/// One adapter as wgpu describes it: `device_type` `IntegratedGpu`, `DiscreteGpu` or `Cpu`.
fn adapter(backend: &str, name: &str, device_type: &str) -> luxforge_ui::adapters::Adapter {
    luxforge_ui::adapters::Adapter {
        name: name.into(),
        vendor: 0,
        device: 0,
        device_type: device_type.into(),
        backend: backend.into(),
        driver: String::new(),
        driver_info: String::new(),
    }
}

/// The adapter a launch names its tile worker before its window opens: the one hardware adapter,
/// beside a software one too; none of several hardware adapters, which the window's request could
/// land on either; for a launch drawing on the software adapter, the one there is, and none of
/// two; and nothing for a launch that refused the GPU stage, `--no-gpu-render` or a software-only
/// host not adopted, or for a host that offers nothing.
#[test]
fn a_launch_names_its_tile_workers_adapter_only_when_the_host_leaves_no_doubt() {
    use super::renderer::launch_candidate;
    use luxforge_ui::adapters::{LaunchRenderer, Refusal};
    let m4 = adapter("Metal", "Apple M4 Pro", "IntegratedGpu");
    let discrete = adapter("Vulkan", "NVIDIA GeForce RTX 4070", "DiscreteGpu");
    let integrated = adapter("Vulkan", "Intel(R) UHD Graphics 770", "IntegratedGpu");
    let lavapipe = adapter("Vulkan", "llvmpipe (LLVM 19.1.7, 128 bits)", "Cpu");
    let warp = adapter("Dx12", "Microsoft Basic Render Driver", "Cpu");
    let gpu = LaunchRenderer::Gpu { software: false };
    let software = LaunchRenderer::Gpu { software: true };
    let named = |launch, offered: &[luxforge_ui::adapters::Adapter]| {
        launch_candidate(launch, offered).map(|adapter| adapter.name.clone())
    };
    assert_eq!(named(gpu, std::slice::from_ref(&m4)), Some(m4.name.clone()));
    assert_eq!(
        named(gpu, &[lavapipe.clone(), discrete.clone()]),
        Some(discrete.name.clone()),
        "a hardware adapter ranks before a software one"
    );
    assert_eq!(named(gpu, &[discrete.clone(), integrated]), None, "several");
    assert_eq!(named(gpu, &[]), None);
    assert_eq!(
        named(software, std::slice::from_ref(&lavapipe)),
        Some(lavapipe.name.clone())
    );
    assert_eq!(
        named(software, &[lavapipe.clone(), warp]),
        None,
        "two software"
    );
    for refused in [
        Refusal::Requested,
        Refusal::SoftwareNotAdopted,
        Refusal::NoAdapter,
    ] {
        assert_eq!(
            named(
                LaunchRenderer::Reference(refused),
                &[m4.clone(), lavapipe.clone()]
            ),
            None,
            "{refused:?}"
        );
    }
}

/// A worker the launch named is the GPU's at once; the window's naming of the same adapter confirms
/// it and of another replaces it, the window's naming then being final. The launch's naming never
/// replaces the window's, and a refused worker takes neither.
#[test]
fn the_windows_naming_follows_the_launchs() {
    use luxforge_core::tiles::{TileService, TileStatus};
    let named = |worker: &GpuTiles| worker.figures().named;

    let confirmed = GpuTiles::pending(false);
    assert!(confirmed.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Launch));
    assert_eq!(
        confirmed.status(),
        TileStatus::Gpu,
        "the GPU's before any photograph"
    );
    assert_eq!(named(&confirmed), Some(AdapterNaming::Launch));
    assert_eq!(
        confirmed.figures().record()["named"],
        serde_json::json!("launch")
    );
    assert!(confirmed.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Window));
    assert_eq!(named(&confirmed), Some(AdapterNaming::Window));
    assert!(
        !confirmed.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Window),
        "the window names it once"
    );

    let replaced = GpuTiles::pending(false);
    assert!(replaced.adopt_adapter("Vulkan", "Intel(R) UHD Graphics 770", AdapterNaming::Launch));
    assert!(replaced.adopt_adapter("Vulkan", "NVIDIA GeForce RTX 4070", AdapterNaming::Window));
    assert_eq!(named(&replaced), Some(AdapterNaming::Window));
    assert_eq!(replaced.status(), TileStatus::Gpu);

    let window_first = GpuTiles::pending(false);
    assert!(window_first.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Window));
    assert!(!window_first.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Launch));
    assert_eq!(named(&window_first), Some(AdapterNaming::Window));

    let refused = GpuTiles::pending(true);
    assert!(!refused.adopt_adapter("Metal", "Apple M4 Pro", AdapterNaming::Launch));
    assert_eq!(named(&refused), None);
    assert!(!confirmed.started() && !replaced.started() && !refused.started());
}
