use luxforge_cli::Paths;
use luxforge_core::{
    ClientAuthority, HostConfig, ModuleRegistry, OwnerHandle, RegistryOptions,
    capabilities::secrets::{MemorySecretStore, SecretStore},
    serve_json_lines_with,
};
use luxforge_net::{HttpTransport, platform_secret_store};
use std::{path::PathBuf, sync::Arc};

const HELP: &str = "luxforge-json --catalog CATALOG [--data-root DIRECTORY] [--secret-store keychain|memory] [--permission-authority] [--developer] [--proof-endpoint URL] < requests.jsonl

Serves one JSON-lines client on standard input and output.
--data-root DIRECTORY    keep module settings, grants, resources and installed modules under
                         DIRECTORY, as the desktop does; without it the platform's application
                         directories are used. Installed modules are read once, at launch.
--secret-store STORE     keychain (the default) keeps module secrets in the platform's secure store;
                         memory keeps them for this process only, for tests and scripts.
--permission-authority   let this client grant module permissions. It is an explicit local setup
                         step: a client without it, like every loopback live-session client, can
                         deny or revoke a permission but never grant one.
--developer              serve the test modules, the pixel and controls proofs, as the desktop's
                         --developer does. Unlike the desktop, a debug build is never in developer
                         mode without it.
--proof-endpoint URL     register the developer capability proof module, luxforge.capabilities,
                         whose palette resource is served at URL/proof-palette.bin; developer mode
                         only. It is a test fixture for a proof endpoint a harness started, not a
                         feature.";

fn main() {
    if luxforge_cli::version_requested(std::env::args_os().skip(1)) {
        println!("luxforge-json {}", luxforge_cli::VERSION);
        return;
    }
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
    let mut developer = false;
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
            Some("--developer") => developer = true,
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
    // The installed external modules under the data root, as the desktop reads them. It draws
    // nothing on a GPU, so their programs are left unchecked and render through the reference.
    let installed = luxforge_cli::installed_modules(paths.as_ref(), developer, None);
    let registry = ModuleRegistry::assemble(&RegistryOptions {
        disabled: &[],
        developer,
        proof_endpoint: proof_endpoint.as_deref(),
        external: &installed.external,
    })
    .map_err(|error| startup(&error))?;
    let secrets: Arc<dyn SecretStore> = if memory_secrets {
        Arc::new(MemorySecretStore::new())
    } else {
        platform_secret_store()
    };
    let host = HostConfig {
        preferences_dir: paths.as_ref().map(|paths| paths.config.clone()),
        config_dir: paths.as_ref().map(Paths::module_config),
        resource_dir: paths.as_ref().map(Paths::module_resources),
        secrets,
        // A developer run installs the developer models from memory.
        transport: luxforge_core::DeveloperModels::for_run(
            developer,
            Arc::new(HttpTransport::system()),
        ),
        machine_memory_bytes: luxforge_core::capabilities::host::installed_memory_bytes(),
        // ONNX Runtime, compiling under the data root, and stable-diffusion.cpp for the
        // generative model (`metal` on macOS, `diffusion-cpu` on Linux); a developer run also
        // serves the deterministic fake, as the `fake` backend. Nothing starts until a model
        // opens.
        inference: Some(luxforge_onnx::host_runtime_with(
            paths.as_ref().map(Paths::inference_cache),
            developer,
            std::time::Duration::ZERO,
            vec![luxforge_diffusion::DiffusionRuntime::shared()],
        )),
        modules: installed.host,
        ..HostConfig::unconfigured()
    };
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
