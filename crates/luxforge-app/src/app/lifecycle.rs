//! Starting and stopping the desktop: the registry this run serves, the capability host's paths,
//! catalog ownership and the Iced application, and the shutdown that releases them.
use super::{
    Editor,
    gpu_tiles::GpuTiles,
    message::{Message, view::ViewMessage},
    tasks,
    unsupported_catalog::{Shell, Unsupported},
};
use crate::{Config, state::unsupported_catalog::Refusal};
use iced::Task;
use luxforge_cli::Paths;
use luxforge_core::{
    ClientAuthority, ClientId, ErrorKind, HostConfig, LocalServer, ModuleRegistry, OwnerHandle,
    capabilities::secrets::{MemorySecretStore, SecretStore},
};
use luxforge_net::{HttpTransport, platform_secret_store};
use serde_json::json;
use std::{
    path::Path,
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

/// Each step of the developer fake generator in an evidence run: a three-candidate generation of
/// eight steps each takes about three and a half seconds, long enough to capture its progress and
/// cancel it.
const EVIDENCE_FAKE_STEP: std::time::Duration = std::time::Duration::from_millis(150);

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
        usage_consent: Arc::new(luxforge_core::usage::Usage::new(usage_eligibility(config))),
        preferences_dir: paths.map(|paths| paths.config.clone()),
        config_dir: paths.map(Paths::module_config),
        resource_dir: paths.map(Paths::module_resources),
        secrets,
        // A developer run installs the developer models from memory.
        transport: luxforge_core::DeveloperModels::for_run(
            config.developer,
            Arc::new(HttpTransport::system()),
        ),
        // What a model's machine minimum is held against in the Models tab and at install.
        machine_memory_bytes: luxforge_core::capabilities::host::installed_memory_bytes(),
        launch_flags: config.launch_flags.clone(),
        // What the launch knows of its renderer before the window opens, which every client's
        // session reports until the photo surface has checked its GPU stage.
        renderer: super::renderer::launched(config.launch_renderer().refused()),
        tiles: Some(tiles),
        // ONNX Runtime, compiling under the data root, and stable-diffusion.cpp for the
        // generative model (`metal` on macOS, `diffusion-cpu` on Linux); a developer run also
        // serves the deterministic fake, as the `fake` backend. Nothing starts until a model
        // opens. An evidence run paces the fake's steps, so a generation is still running when
        // its frame is captured.
        inference: Some(luxforge_onnx::host_runtime_with(
            paths.map(Paths::inference_cache),
            config.developer,
            if config.evidence.is_some() {
                EVIDENCE_FAKE_STEP
            } else {
                std::time::Duration::ZERO
            },
            vec![luxforge_diffusion::DiffusionRuntime::shared()],
        )),
        ..HostConfig::unconfigured()
    }
}

/// Whether this launch may collect usage statistics once someone opts in: explicit test
/// eligibility with an evidence run's loopback receiver, or production's, which the core decides
/// from the launch mode, the build and its activation switch. Eligibility is never consent.
pub(super) fn usage_eligibility(config: &Config) -> luxforge_core::usage::Eligibility {
    use luxforge_core::usage::{Eligibility, LaunchMode};
    if let Some(receiver) = &config.usage_receiver {
        return Eligibility::test(receiver.clone());
    }
    Eligibility::production(if config.evidence.is_some() {
        LaunchMode::Evidence
    } else if config.hidden {
        LaunchMode::Hidden
    } else if config.developer {
        LaunchMode::Developer
    } else {
        LaunchMode::Interactive
    })
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
    // The notice and the Privacy tab exist exactly where the host can act on them.
    config.usage_eligible = host.usage_consent.eligibility().is_eligible();
    // The process's statistics supervisor, for an eligible launch only, registered before the
    // owner reads the stored consent. A hidden launch's window is presented only as an evidence
    // run's invisible window is, by the visibility gate's override.
    super::usage_supervisor::start(
        &host,
        config.usage_receiver.is_some(),
        !config.hidden || config.evidence.is_some(),
    );
    let hidden = config.hidden;
    let position = config.opening.map(|opening| opening.position());
    // Iced opens the window at its settings' size times the application's scale factor, the
    // interface size; `size` is in the system's points whatever the interface size.
    let points = crate::window_frame::interface_scale(
        config
            .interface_size
            .unwrap_or(luxforge_core::preferences::DEFAULT_INTERFACE_SIZE),
    );
    let prepared = Prepared {
        config,
        registry,
        host,
        window: size,
    };
    // A refused evidence run that was given no choice for the sheet says so here, after its window.
    let problem = Arc::new(Mutex::new(None));
    let start = match own(&prepared, &catalog) {
        Ok((owner, join)) => Start::Editor(boot(prepared, owner, join, &catalog)),
        Err(error) => {
            let offered = prepared
                .config
                .launch_catalog
                .as_ref()
                .and_then(|launch| Refusal::offered(launch, &error));
            match offered {
                // The remembered or default catalog is refused as another version's: the window
                // opens on the sheet instead of the launch failing.
                Some(refusal) => Start::Unsupported(refusal, prepared),
                None => return Err(refused(&catalog, &error)),
            }
        }
    };
    let shell = Mutex::new(Some(start));
    let exit = problem.clone();
    let application = iced::application(
        move || match shell
            .lock()
            .expect("boot state is never poisoned")
            .take()
            .expect("the shell boots once")
        {
            Start::Editor(boot) => {
                let (editor, task) = Editor::new(boot);
                (Shell::Editor(Box::new(editor)), task)
            }
            Start::Unsupported(refusal, prepared) => {
                let (sheet, task) = Unsupported::new(refusal, prepared, exit.clone());
                (Shell::Unsupported(Box::new(sheet)), task)
            }
        },
        Shell::update,
        Shell::view,
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
    .scale_factor(Shell::interface_scale)
    // A function of the state, which Iced reads again after every update.
    .theme(Shell::theme)
    .subscription(Shell::subscription);
    // The bundled typeface is registered once, before the first frame, from bytes compiled into
    // the binary; every text run after that resolves it from the renderer's font database.
    luxforge_ui::theme::FONT_FILES
        .into_iter()
        .fold(application, |application, file| application.font(file))
        .default_font(luxforge_ui::theme::FONT)
        .run()
        .map_err(|error| error.to_string())?;
    match problem.lock().expect("never poisoned").take() {
        Some(problem) => Err(problem),
        None => Ok(()),
    }
}

/// What the window boots into: the editor over the catalog the launch opened, or the
/// unsupported-catalog sheet over the one it could not.
enum Start {
    Editor(Boot),
    Unsupported(Refusal, Prepared),
}

/// What a launch holds before its catalog opens: its configuration, registry, capability host and
/// window size. The unsupported-catalog sheet keeps it for the catalog Start a new catalog opens.
#[derive(Clone)]
pub(crate) struct Prepared {
    pub(crate) config: Config,
    pub(crate) registry: Arc<ModuleRegistry>,
    pub(crate) host: HostConfig,
    pub(crate) window: (f32, f32),
}

/// Take ownership of `catalog` for this launch, or the core's refusal.
pub(crate) fn own(
    prepared: &Prepared,
    catalog: &Path,
) -> Result<(OwnerHandle, JoinHandle<()>), luxforge_core::Error> {
    OwnerHandle::start_with_host(catalog, prepared.registry.clone(), prepared.host.clone())
}

/// The command line's message for a catalog the launch could not open and offers nothing for.
pub(crate) fn refused(catalog: &Path, error: &luxforge_core::Error) -> String {
    match error.kind {
        ErrorKind::Conflict => format!(
            "another Luxforge instance owns the catalog {}; close it or pass --catalog",
            catalog.display()
        ),
        _ => format!("cannot open catalog {}: {error}", catalog.display()),
    }
}

/// The editor's boot over `catalog`, which `owner` now holds: its live session, its own client and
/// the first command-line file's import, started before the window opens.
pub(crate) fn boot(
    prepared: Prepared,
    owner: OwnerHandle,
    join: JoinHandle<()>,
    catalog: &Path,
) -> Boot {
    let Prepared {
        mut config, window, ..
    } = prepared;
    let session_file = luxforge_core::live_session_file(catalog);
    // Owning the catalog proves any same-catalog session file from an earlier process is stale.
    if session_file.exists() {
        let _ = std::fs::remove_file(&session_file);
    }
    let live_server = start_live_session(&mut config, &owner, catalog, &session_file);
    // Queue only the first command-line file. The source worker can read and decode it while
    // Iced/AppKit initializes, without making the window thread read the image or changing the
    // normal adoption, history and error path. Evidence mode opens later files in order as usual.
    let client = owner.register_with(ClientAuthority::Permissions);
    let initial_import = config
        .files
        .front()
        .map(|path| tasks::start_import(&owner, client, path));
    Boot {
        owner,
        join,
        live_server,
        config,
        client: Some(client),
        initial_import,
        window,
    }
}

/// Serve the live session on loopback and register it in the per-user registry of running
/// sessions (`<config>/live-sessions`), so `luxforge-ctl` finds this catalog without being told
/// it. A session that cannot start leaves the editor without one; one that cannot be registered is
/// still served and found with `--catalog`. Either problem is kept in
/// [`Config::live_problem`] for the status bar and the log, never dropped.
fn start_live_session(
    config: &mut Config,
    owner: &OwnerHandle,
    catalog: &std::path::Path,
    session_file: &std::path::Path,
) -> Option<LocalServer> {
    let mut server = match LocalServer::start(owner.clone(), session_file) {
        Ok(server) => server,
        Err(error) => {
            config.live_problem = Some(format!("live API unavailable: {}", error.detail));
            return None;
        }
    };
    if let Some(paths) = &config.paths
        && let Err(error) = server.register(&paths.live_sessions(), catalog)
    {
        config.live_problem = Some(format!(
            "live session not registered for luxforge-ctl: {}",
            error.detail
        ));
    }
    Some(server)
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
        // Statistics are cancelled and discarded, never sent on the way out or waited for.
        super::usage_supervisor::shutdown();
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
