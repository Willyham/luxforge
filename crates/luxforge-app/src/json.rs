// The desktop reads every directory `Paths` names; this client needs only the module ones.
#[allow(dead_code)]
#[path = "paths.rs"]
mod paths;

use luxforge_core::{
    CapabilitiesProofModule, ClientAuthority, HostConfig, ModuleRegistry, OwnerHandle,
    capabilities::secrets::{MemorySecretStore, SecretStore, platform_secret_store},
    serve_json_lines_with,
};
use paths::Paths;
use std::{path::PathBuf, sync::Arc};

const HELP: &str = "luxforge-json --catalog CATALOG [--data-root DIRECTORY] [--secret-store keychain|memory] [--permission-authority] [--proof-endpoint URL] < requests.jsonl

Serves one JSON-lines client on standard input and output.
--data-root DIRECTORY    keep module settings, grants and resources under DIRECTORY, as the desktop
                         does; without it the platform's application directories are used.
--secret-store STORE     keychain (the default) keeps module secrets in the platform's secure store;
                         memory keeps them for this process only, for tests and scripts.
--permission-authority   let this client grant module permissions. It is an explicit local setup
                         step: a client without it, like every loopback live-session client, can
                         deny or revoke a permission but never grant one.
--proof-endpoint URL     register the developer capability proof module, luxforge.capabilities,
                         whose palette resource is served at URL/proof-palette.bin. It is a test
                         fixture for a proof endpoint a harness started, not a feature.";

fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"code":error.0,"message":error.1}})
        );
        std::process::exit(1);
    }
}

fn startup(message: &str) -> (String, String) {
    ("startup".into(), message.into())
}

fn run() -> Result<(), (String, String)> {
    let mut args = std::env::args_os().skip(1);
    let mut catalog: Option<PathBuf> = None;
    let mut data_root: Option<PathBuf> = None;
    let mut authority = ClientAuthority::Edit;
    let mut memory_secrets = false;
    let mut proof_endpoint: Option<String> = None;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--catalog") => {
                catalog = Some(
                    args.next()
                        .ok_or_else(|| startup("--catalog requires a path"))?
                        .into(),
                );
            }
            Some("--data-root") => {
                data_root = Some(
                    args.next()
                        .ok_or_else(|| startup("--data-root requires a path"))?
                        .into(),
                );
            }
            Some("--secret-store") => {
                memory_secrets = match args.next().as_ref().and_then(|store| store.to_str()) {
                    Some("keychain") => false,
                    Some("memory") => true,
                    _ => return Err(startup("--secret-store is keychain or memory")),
                };
            }
            Some("--permission-authority") => authority = ClientAuthority::Permissions,
            Some("--proof-endpoint") => {
                proof_endpoint = Some(
                    args.next()
                        .and_then(|url| url.into_string().ok())
                        .ok_or_else(|| startup("--proof-endpoint requires a URL"))?,
                );
            }
            Some("--help") => {
                println!("{HELP}");
                return Ok(());
            }
            _ => return Err(startup("unknown argument; use --help")),
        }
    }
    let catalog = catalog.ok_or_else(|| startup("--catalog is required"))?;
    let paths = Paths::resolve(data_root.as_ref());
    let secrets: Arc<dyn SecretStore> = if memory_secrets {
        Arc::new(MemorySecretStore::new())
    } else {
        platform_secret_store()
    };
    let host = HostConfig {
        config_dir: paths.as_ref().map(Paths::module_config),
        resource_dir: paths.as_ref().map(Paths::module_resources),
        secrets,
        ..HostConfig::unconfigured()
    };
    let mut registry = ModuleRegistry::builtin();
    if let Some(base) = &proof_endpoint {
        registry
            .register(Arc::new(CapabilitiesProofModule::new(base)))
            .map_err(|error| startup(&format!("--proof-endpoint: {}", error.detail)))?;
    }
    let (owner, join) = OwnerHandle::start_with_host(&catalog, Arc::new(registry), host)
        .map_err(|error| (error.kind.code().into(), error.detail))?;
    let served = serve_json_lines_with(
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        &owner,
        authority,
    );
    owner.stop();
    let _ = join.join();
    served.map_err(|error| (error.kind.code().into(), error.detail))
}
