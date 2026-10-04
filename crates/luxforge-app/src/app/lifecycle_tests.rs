//! The registry a run serves and where the capability host keeps its state.
use super::lifecycle::host_config;
use crate::Config;
use luxforge_core::ModuleRegistry;

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
    let host = host_config(&config);
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
    let host = host_config(&config);
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
        host_config(&config).renderer
    };
    assert_eq!(launch(true), Renderer::reference(RendererReason::NoAdapter));
    assert_eq!(
        launch(false),
        Renderer::reference(RendererReason::SurfacePending)
    );
    assert!(!evidence.exists(), "choosing creates nothing");
}
