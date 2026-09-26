//! The registry a run serves and where the capability host keeps its state.
use super::lifecycle::{host_config, registry};
use crate::Config;
use luxforge_core::ModuleRegistry;

#[test]
fn desktop_registry_contains_every_core_builtin_including_raw() {
    let desktop = registry(&[], false, None).unwrap();
    let core = ModuleRegistry::builtin();
    let ids = |registry: &ModuleRegistry| {
        registry
            .descriptors()
            .iter()
            .map(|module| module.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&desktop), ids(&core));
    assert!(!ids(&desktop).contains(&"luxforge.controls".to_owned()));
    let developer = registry(&[], true, None).unwrap();
    assert!(ids(&developer).contains(&"luxforge.controls".to_owned()));
    assert!(!ids(&developer).contains(&"luxforge.capabilities".to_owned()));
    assert!(registry(&["luxforge.controls".into()], false, None).is_err());
    assert!(
        !registry(&["luxforge.controls".into()], true, None)
            .unwrap()
            .descriptors()
            .iter()
            .find(|module| module.id == "luxforge.controls")
            .unwrap()
            .is_available()
    );
    let disabled = registry(&["luxforge.raw".into()], false, None).unwrap();
    assert!(
        !disabled
            .descriptors()
            .iter()
            .find(|module| module.id == "luxforge.raw")
            .unwrap()
            .is_available()
    );
    // The capability proof joins a developer run that names a proof endpoint, and no other.
    let proof = registry(&[], true, Some("http://127.0.0.1:9")).unwrap();
    let proof_module = proof
        .descriptors()
        .into_iter()
        .find(|module| module.id == "luxforge.capabilities")
        .expect("the capability proof is registered");
    assert!(proof_module.developer);
    assert_eq!(
        proof_module.resources[0].url,
        "http://127.0.0.1:9/proof-palette.bin"
    );
    assert!(
        !ids(&registry(&[], false, Some("http://127.0.0.1:9")).unwrap())
            .contains(&"luxforge.capabilities".to_owned())
    );
    let refused = registry(&[], true, Some("http://example.com")).unwrap_err();
    assert!(refused.contains("proof-palette"), "{refused}");
}

#[test]
fn an_evidence_run_keeps_module_state_in_its_directory_and_memory() {
    let evidence = std::env::temp_dir().join("luxforge-evidence-host-paths");
    let host = host_config(&Config {
        evidence: Some(evidence.clone()),
        data_root: Some(std::env::temp_dir().join("luxforge-ignored-root")),
        ..Config::default()
    });
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
    let host = host_config(&Config {
        data_root: Some(root.clone()),
        ..Config::default()
    });
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
