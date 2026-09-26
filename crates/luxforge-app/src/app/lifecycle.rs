//! Starting and stopping the desktop: the registry this run serves, the capability host's paths,
//! catalog ownership and the Iced application, and the shutdown that releases them.
use super::{Editor, message::Message, tasks};
use crate::{Config, paths::Paths};
use iced::Task;
use luxforge_core::{
    ClientAuthority, ClientId, ErrorKind, HostConfig, LocalServer, ModuleRegistry, OwnerHandle,
    capabilities::secrets::{MemorySecretStore, SecretStore, platform_secret_store},
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

/// Catalog ownership and the live service start before the window so failures are reported, not panics.
pub(crate) struct Boot {
    pub(crate) owner: OwnerHandle,
    pub(crate) join: JoinHandle<()>,
    pub(crate) live_server: Option<LocalServer>,
    pub(crate) config: Config,
    /// Production registers before platform initialization so its first import can already run.
    /// Unit fixtures leave this unset and register when constructing the editor.
    pub(crate) client: Option<ClientId>,
    pub(crate) initial_import: Option<tasks::StartupImport>,
    /// The window's logical size at launch, before any resize event. The clipping overlay's cell
    /// grid is sized against the photo surface, which this and the panel flags give.
    pub(crate) window: (f32, f32),
}

/// The reason a module named by `--disable-module` reports.
pub(super) const DISABLED_REASON: &str = "disabled by --disable-module";

/// The providers this run serves: the core's built-in modules, with any `--disable-module` one
/// registered unavailable. In developer mode the controls proof joins them, and the capability
/// proof too when a proof endpoint is named.
pub(super) fn registry(
    disabled: &[String],
    developer: bool,
    proof_endpoint: Option<&str>,
) -> Result<ModuleRegistry, String> {
    let mut registry = ModuleRegistry::new();
    let mut unknown: Vec<&str> = disabled.iter().map(String::as_str).collect();
    let mut modules = luxforge_core::builtin_modules();
    if developer {
        modules.push(Arc::new(luxforge_core::ControlsModule::new()));
        if let Some(base) = proof_endpoint {
            modules.push(Arc::new(luxforge_core::CapabilitiesProofModule::new(base)));
        }
    }
    for module in modules {
        let id = module.descriptor().id.clone();
        let registered = if disabled.contains(&id) {
            unknown.retain(|named| *named != id);
            registry.register_unavailable(module, DISABLED_REASON)
        } else {
            registry.register(module)
        };
        registered.map_err(|error| error.to_string())?;
    }
    match unknown.first() {
        Some(id) => Err(format!("--disable-module names no registered module: {id}")),
        None => Ok(registry),
    }
}

/// Where the capability host keeps module settings, grants and resources, and which secret store
/// it uses. An evidence run keeps all of it inside its evidence directory with an in-memory store,
/// so it never touches the person's configuration or login keychain. Nothing is created here: the
/// host creates a directory on its first write.
pub(super) fn host_config(config: &Config) -> HostConfig {
    let (paths, secrets): (_, Arc<dyn SecretStore>) = match &config.evidence {
        Some(evidence) => (
            Paths::resolve(Some(&evidence.join("host"))),
            Arc::new(MemorySecretStore::new()),
        ),
        None => (
            Paths::resolve(config.data_root.as_ref()),
            platform_secret_store(),
        ),
    };
    HostConfig {
        config_dir: paths.as_ref().map(Paths::module_config),
        resource_dir: paths.as_ref().map(Paths::module_resources),
        secrets,
        ..HostConfig::unconfigured()
    }
}

pub(crate) fn run(config: Config, size: (f32, f32)) -> Result<(), String> {
    // Evidence runs never touch a real catalog: theirs lives inside the new evidence directory.
    let catalog = match (&config.catalog, &config.evidence) {
        (Some(catalog), _) => catalog.clone(),
        (None, Some(evidence)) => evidence.join("catalog.sqlite"),
        (None, None) => Paths::resolve(config.data_root.as_ref())
            .ok_or("no usable application data directory; pass --data-root")?
            .config
            .join("catalog.sqlite"),
    };
    let registry = Arc::new(registry(
        &config.disabled,
        config.developer,
        config.proof_endpoint.as_deref(),
    )?);
    let (owner, join) = OwnerHandle::start_with_host(&catalog, registry, host_config(&config))
        .map_err(|error| match error.kind {
            ErrorKind::Conflict => format!(
                "another Luxforge instance owns the catalog {}; close it or pass --catalog",
                catalog.display()
            ),
            _ => format!("cannot open catalog {}: {error}", catalog.display()),
        })?;
    let session_file = catalog.with_extension("live-session.json");
    // Owning the catalog proves any same-catalog session file from an earlier process is stale.
    if session_file.exists() {
        let _ = std::fs::remove_file(&session_file);
    }
    let live_server = LocalServer::start(owner.clone(), &session_file).ok();
    // Queue only the first command-line file. The source worker can read and decode it while
    // Iced/AppKit initializes, without making the window thread read the image or changing the
    // normal adoption, history and error path. Evidence mode opens later files in order as usual.
    let client = owner.register_with(ClientAuthority::Permissions);
    let initial_import = config
        .files
        .front()
        .map(|path| tasks::start_import(&owner, client, path));
    let hidden = config.hidden;
    let boot = Mutex::new(Some(Boot {
        owner,
        join,
        live_server,
        config,
        client: Some(client),
        initial_import,
        window: size,
    }));
    let application = iced::application(
        move || {
            Editor::new(
                boot.lock()
                    .expect("boot state is never poisoned")
                    .take()
                    .expect("the editor boots once"),
            )
        },
        Editor::update,
        Editor::view,
    )
    .title("Luxforge")
    // An invisible window still owns a real surface and renders through it, so a hidden launch
    // captures the same renderer readbacks; it is simply never placed on the desktop.
    .window(crate::window_frame::settings(size, !hidden))
    .theme(luxforge_ui::theme::theme())
    .subscription(Editor::subscription);
    // The bundled typeface is registered once, before the first frame, from bytes compiled into
    // the binary; every text run after that resolves it from the renderer's font database.
    luxforge_ui::theme::FONT_FILES
        .into_iter()
        .fold(application, |application, file| application.font(file))
        .default_font(luxforge_ui::theme::FONT)
        .run()
        .map_err(|error| error.to_string())
}

impl Editor {
    /// The window closed: stop the live server and the owner, finish the log and exit once the
    /// owner thread has joined.
    pub(super) fn close(&mut self) -> Task<Message> {
        self.event("shutdown", json!({"while_loading":self.activity.pending}));
        self.live_server.take();
        self.owner.disconnect(self.client);
        self.owner.stop();
        let join = self.owner_join.take();
        let log = self.diagnostics.take();
        Task::perform(
            async move {
                if let Some(log) = log {
                    log.finish();
                }
                if let Some(join) = join {
                    let _ = join.join();
                }
            },
            |_| (),
        )
        .then(|_| iced::exit())
    }
}
