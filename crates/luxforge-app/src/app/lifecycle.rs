//! Starting and stopping the desktop: the registry this run serves, the capability host's paths,
//! catalog ownership and the Iced application, and the shutdown that releases them.
use super::{
    Editor,
    gpu_tiles::GpuTiles,
    message::{Message, view::ViewMessage},
    tasks,
};
use crate::Config;
use iced::Task;
use luxforge_cli::Paths;
use luxforge_core::{
    ClientAuthority, ClientId, ErrorKind, HostConfig, LocalServer, ModuleRegistry, OwnerHandle,
    capabilities::secrets::{MemorySecretStore, SecretStore},
};
use luxforge_net::{HttpTransport, platform_secret_store};
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
    /// The window's size at launch in the system's points, before any resize event; the view state
    /// takes it to logical pixels by the interface size. The clipping overlay's cell grid is sized
    /// against the photo surface, which this and the panel flags give.
    pub(crate) window: (f32, f32),
}

/// Where the capability host keeps module settings, grants and resources, under the run's
/// resolved paths, and the secret store and network transport it uses. An evidence run keeps all
/// of it inside its evidence directory with an in-memory store, so it never touches the person's
/// configuration or login keychain. Nothing is created here: the host creates a directory on its
/// first write, and the transport builds its TLS configuration on its first request. `tiles` is
/// the launch's GPU tile worker, which the owner's export lane streams every export through and
/// which starts nothing until the first export asks it.
pub(super) fn host_config(config: &Config, tiles: Arc<GpuTiles>) -> HostConfig {
    let secrets: Arc<dyn SecretStore> = match &config.evidence {
        Some(_) => Arc::new(MemorySecretStore::new()),
        None => platform_secret_store(),
    };
    let paths = config.paths.as_ref();
    HostConfig {
        preferences_dir: paths.map(|paths| paths.config.clone()),
        config_dir: paths.map(Paths::module_config),
        resource_dir: paths.map(Paths::module_resources),
        secrets,
        transport: Arc::new(HttpTransport::system()),
        launch_flags: config.launch_flags.clone(),
        // What the launch knows of its renderer before the window opens, which every client's
        // session reports until the photo surface has checked its GPU stage.
        renderer: super::renderer::launched(config.launch_renderer().refused()),
        tiles: Some(tiles),
        ..HostConfig::unconfigured()
    }
}

pub(crate) fn run(mut config: Config, size: (f32, f32)) -> Result<(), String> {
    super::renderer::launch_began();
    // The renderer, chosen before the window opens from what the host offers: off macOS, an
    // enumeration of the renderer's backends, so a host whose only adapter is a software one is
    // known before Iced draws on it ([`crate::adapters::choose`]).
    let offered = if config.no_gpu_render {
        crate::adapters::Offered::NotProbed
    } else {
        crate::adapters::probe()
    };
    let launch = crate::adapters::choose(
        config.no_gpu_render,
        config.software_adapter,
        crate::adapters::SOFTWARE_ADAPTER_ADOPTED,
        &offered,
    );
    config.launch_renderer = Some(launch);
    // Refused before the window, and so before Iced creates the photo surface's pipeline, whose
    // capability check then answers unavailable: the editor runs as on a machine whose adapter
    // cannot run the GPU stage. `--no-gpu-render` refuses it, and so does a host whose only
    // adapter is a software one that is neither adopted nor asked for.
    if launch.refused() {
        luxforge_gpu::refuse_gpu_stage();
    }
    // Evidence runs never touch a real catalog: theirs lives inside the new evidence directory.
    // Nothing here moves, copies or deletes a catalog: the one chosen is opened, or created.
    let catalog = config
        .launch_catalog
        .as_ref()
        .ok_or("no usable application data directory; pass --data-root")?
        .path
        .clone();
    let registry = Arc::new(ModuleRegistry::assemble(&config.registry_options())?);
    // The launch's one GPU tile worker, refused with the GPU stage: the owner's export lane
    // streams through it, and the renderer report names it the window's adapter once known.
    let tiles = super::gpu_tiles::launch(launch);
    let host = host_config(&config, tiles);
    let (owner, join) =
        OwnerHandle::start_with_host(&catalog, registry, host).map_err(|error| {
            match error.kind {
                ErrorKind::Conflict => format!(
                    "another Luxforge instance owns the catalog {}; close it or pass --catalog",
                    catalog.display()
                ),
                _ => format!("cannot open catalog {}: {error}", catalog.display()),
            }
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
    let position = config.opening.map(|opening| opening.position());
    // Iced opens the window at its settings' size times the application's scale factor, the
    // interface size; `size` is in the system's points whatever the interface size.
    let points = crate::window_frame::interface_scale(
        config
            .interface_size
            .unwrap_or(luxforge_core::preferences::DEFAULT_INTERFACE_SIZE),
    );
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
    .window(crate::window_frame::settings(
        (size.0 / points, size.1 / points),
        position,
        !hidden,
    ))
    // The interface size scales everything Iced draws. Every physical-pixel computation reads the
    // view state's combined factor, so 100% zoom stays one source pixel per display pixel.
    .scale_factor(Editor::interface_scale)
    // A function of the state, which Iced reads again after every update.
    .theme(Editor::theme)
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
        // The window's frame is asked for once and stored through the writer, which the close
        // then waits for like any other write.
        let memory = &mut self.view_state.memory;
        if memory.remember && !memory.asked {
            memory.asked = true;
            memory.waiting = true;
            return crate::window_frame::report()
                .map(|report| Message::View(ViewMessage::ClosingFrame(report)));
        }
        if memory.waiting {
            return Task::none();
        }
        // A preference change is stored before the owner stops: closing waits for the last write.
        if !self.preferences.idle() {
            self.preferences.closing = true;
            return Task::none();
        }
        // So does a flag change.
        if !self.settings.idle() {
            self.settings.closing = true;
            return Task::none();
        }
        self.event(
            "shutdown",
            || json!({"while_loading":self.activity.pending}),
        );
        self.live_server.take();
        self.owner.disconnect(self.client);
        self.owner.stop();
        let join = self.owner_join.take();
        let log = self.log.diagnostics.take();
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
