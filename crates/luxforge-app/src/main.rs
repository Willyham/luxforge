// The evidence summary is one `json!` object holding every key this desktop publishes about a
// frame, and the merge of the masking and performance work took it past the default macro recursion
// depth. Raising the limit keeps it one object: splitting it would let two halves disagree about
// what a frame reported.
#![recursion_limit = "256"]

mod adapters;
mod app;
mod browser;
mod brush;
mod coalesce;
mod crop_draft;
mod diagnostics;
mod layout;
mod mask_draft;
mod range_pick;
mod state;
mod view;
mod window_frame;
use app::evidence::Step;
use diagnostics::Diagnostics;
use luxforge_cli::Paths;
use std::{collections::VecDeque, path::PathBuf};

#[derive(Clone, Default)]
struct Config {
    files: VecDeque<PathBuf>,
    evidence: Option<PathBuf>,
    /// Evidence steps run after the last `--open` outcome, one captured frame each.
    script: VecDeque<Step>,
    size: Option<(f32, f32)>,
    /// Create the window invisible, so an automated launch cannot appear on the owner's desktop.
    /// The renderer, the window surface and its readbacks are unaffected.
    hidden: bool,
    data_root: Option<PathBuf>,
    /// Where this run keeps its files, resolved once at startup by [`Config::resolve_paths`].
    paths: Option<Paths>,
    catalog: Option<PathBuf>,
    /// The catalog this launch opens and why: `--catalog`, an evidence run's own, the stored
    /// location or the default. Resolved once by [`Config::resolve_launch`].
    launch_catalog: Option<state::preferences::LaunchCatalog>,
    /// The remembered window frame this launch opens at: none with `--window-size`, in a hidden
    /// or evidence launch, or with nothing stored.
    opening: Option<window_frame::Opening>,
    /// The close stores the window's frame: a launch that shows its window, outside an evidence
    /// run.
    remember_window: bool,
    /// The stored interface size the window opens at, read with the catalog and the frame; the
    /// default size until [`Config::resolve_launch`] reads it.
    interface_size: Option<u16>,
    /// The active theme the first frame is drawn in, read with the catalog and the frame, and
    /// taken by the editor as it is built; `None` until [`Config::resolve_launch`] reads it.
    launch_theme: Option<luxforge_core::theme::LaunchTheme>,
    /// Where Omarchy's current theme is looked for, at launch and when the window gains focus:
    /// the home directory on Linux outside evidence and hidden launches, or `None`.
    omarchy_home: Option<PathBuf>,
    /// Omarchy's current theme as the launch found it, and the theme to store as the active one
    /// because the active theme follows it; read with the launch theme.
    launch_omarchy: Option<luxforge_core::theme::OmarchyTheme>,
    launch_follow: Option<String>,
    diagnostics: Option<Diagnostics>,
    run_id: String,
    /// Serve the test modules and show the components gallery; the default workspace stays a photo
    /// editor. The `developer` flag as this launch resolved it: [`Config::resolve_flags`].
    developer: bool,
    /// `--developer`: developer mode for this launch, whatever the flag says.
    developer_forced: bool,
    /// What this launch resolved for each launch flag, reported by `flags.list` as `active`.
    launch_flags: luxforge_core::flags::LaunchFlags,
    /// Built-in module identities to register as unavailable, so an unavailable provider can be
    /// rendered and reported without removing it from the catalog's readable effects.
    disabled: Vec<String>,
    /// The base URL of a capability proof endpoint a harness started; registers the developer
    /// capability proof module against it. Developer mode only.
    proof_endpoint: Option<String>,
    /// Draw the photograph at Fit through the photo surface's GPU stage with the identity program,
    /// for a rendered check of the stage. Evidence runs only; see `app/gpu_identity.rs`.
    gpu_identity: bool,
    /// `--no-gpu-render`: refuse the photo surface's GPU stage for this launch, so the editor runs
    /// as on a machine whose adapter cannot run it and every frame is the reference renderer's.
    /// Read once at launch; neither a preference nor a session field. See `app/renderer.rs`.
    no_gpu_render: bool,
    /// `--software-adapter`: on a host whose only adapter is a software one (lavapipe, WARP), draw
    /// through the GPU stage on it for this launch, before the software adapter is adopted
    /// ([`crate::adapters::SOFTWARE_ADAPTER_ADOPTED`]). Read once at launch.
    software_adapter: bool,
    /// The renderer the launch chose before its window opened, from what the host offers
    /// (`app/lifecycle.rs`); `None` until then, when [`Config::launch_renderer`] answers as for a
    /// host it did not look at.
    launch_renderer: Option<crate::adapters::LaunchRenderer>,
    /// Why the live session could not be started or registered at launch (`app/lifecycle.rs`),
    /// which the status bar shows and the log records.
    live_problem: Option<String>,
    /// This launch may show the usage statistics notice and the Settings sheet's Privacy tab:
    /// the eligibility its host is started with ([`app::usage`]).
    usage_eligible: bool,
    /// `--usage-test-receiver`: explicit usage statistics test eligibility with a validated
    /// loopback or hosted test receiver, for an evidence run's isolated configuration. It never
    /// reaches the production URL, and grants no consent.
    usage_receiver: Option<luxforge_core::usage::TestReceiver>,
    /// `--evidence-config`: an earlier evidence run's configuration directory (its `host/config`),
    /// kept by this evidence run in place of a new one, so a scenario can follow what one launch
    /// stored into the next, as the usage statistics notice's ask-once does. Evidence runs only.
    evidence_config: Option<PathBuf>,
    /// `--evidence-catalog`: the catalog an evidence run opens as its remembered one, in place of
    /// one inside its evidence directory, so a scenario can launch past a catalog this version
    /// refuses. Evidence runs only; made absolute when parsed.
    evidence_catalog: Option<PathBuf>,
    /// `--evidence-unsupported-catalog`: the choice an evidence run takes on the unsupported-catalog
    /// sheet once the sheet is captured. Evidence runs only.
    unsupported_choice: Option<state::unsupported_catalog::Choice>,
    /// `--update-fixture`: a scripted update port in place of the platform's, for an evidence
    /// run's captures of the update offer and Settings › General's Updates group. Evidence runs
    /// only; see `app/update_fixture.rs`.
    update_fixture: Option<app::update_fixture::UpdateFixture>,
    /// `--evidence-move-to-applications`: the evidence run shows the move into Applications
    /// question once its workspace is usable, wherever it runs, so it can be captured; it never
    /// moves anything. Evidence runs only.
    evidence_move: bool,
    /// `--evidence-select-introduction`: the evidence run shows the Select introduction the first
    /// time Select is shown, as a person's launch does, so it can be captured. Evidence runs only.
    evidence_introduction: bool,
    /// `--evidence-paced-downloads` and `--evidence-cut-download RESOURCE`: a developer evidence
    /// run's model downloads arrive in paced pieces, so a setup's progress can be captured, and the
    /// named resource's first transfer is cut halfway, so a failed and resumed setup can be.
    /// Developer evidence runs only.
    evidence_downloads: luxforge_core::DeveloperDownloads,
    /// `--evidence-machine-memory BYTES`: the machine memory the capability host holds model
    /// minimums against, in place of this machine's, so a machine too small for a tool can be
    /// captured. Evidence runs only.
    evidence_memory: Option<u64>,
    /// Whether this launch shows the Select introduction by itself the first time Select is
    /// shown, decided once the arguments are read: a person's launch, or an evidence run that
    /// asks. Off by default, so a test's launch never shows it.
    introduces: bool,
}

impl Config {
    /// The renderer this launch draws with: the one chosen before its window opened, or, before
    /// that, as for a host whose adapters were not looked at.
    fn launch_renderer(&self) -> crate::adapters::LaunchRenderer {
        use crate::adapters::{Offered, SOFTWARE_ADAPTER_ADOPTED, choose};
        self.launch_renderer.unwrap_or_else(|| {
            choose(
                self.no_gpu_render,
                self.software_adapter,
                SOFTWARE_ADAPTER_ADOPTED,
                &Offered::NotProbed,
            )
        })
    }

    /// What this run's registry serves, for the one assembly `luxforge-json` shares.
    fn registry_options(&self) -> luxforge_core::RegistryOptions<'_> {
        luxforge_core::RegistryOptions {
            disabled: &self.disabled,
            developer: self.developer,
            proof_endpoint: self.proof_endpoint.as_deref(),
        }
    }

    /// Structured events are wanted when a data root or evidence directory was requested, even
    /// if the log file could not be created; they then fall back to stderr.
    fn wants_events(&self) -> bool {
        self.evidence.is_some() || self.data_root.is_some()
    }

    /// Read the launch flags once, from this run's preferences, with `--developer` forcing developer
    /// mode on. Reading creates nothing, and a file that cannot be read leaves the defaults.
    fn resolve_flags(&mut self) {
        use luxforge_core::flags::{DEVELOPER, LaunchFlags};
        let overrides = if self.developer_forced {
            vec![(DEVELOPER, serde_json::Value::Bool(true), "--developer")]
        } else {
            Vec::new()
        };
        self.launch_flags = LaunchFlags::resolve(
            self.paths.as_ref().map(|paths| paths.config.clone()),
            &overrides,
        );
        self.developer = self.launch_flags.toggle(DEVELOPER);
    }

    /// Read the launch preferences once, from this run's preferences: the catalog to open, the
    /// window's remembered frame and the active theme, Omarchy's current one while it follows it.
    /// `--catalog` and an evidence run's own catalog take precedence over the stored location, and
    /// a stored catalog whose folder is missing opens the default.
    /// `--window-size` and a hidden or evidence launch open at their own size, placed by the
    /// system. Reading creates nothing, and a file that cannot be read leaves the defaults.
    fn resolve_launch(&mut self) {
        use state::preferences::LaunchCatalog;
        let config = self.paths.as_ref().map(|paths| paths.config.clone());
        self.omarchy_home = app::omarchy_current::home(self.evidence.is_some(), self.hidden);
        let omarchy = self
            .omarchy_home
            .as_deref()
            .and_then(app::omarchy_current::current_slug);
        let stored =
            luxforge_core::preferences::LaunchPreferences::read(config, omarchy.as_deref());
        // An evidence run given a catalog of its own opens it as a remembered one, never the
        // person's stored location.
        let (evidence, stored_catalog) = match &self.evidence_catalog {
            Some(catalog) => (None, Some(catalog.clone())),
            None => (self.evidence.as_deref(), stored.catalog),
        };
        self.launch_catalog = LaunchCatalog::resolve(
            self.catalog.as_deref(),
            evidence,
            self.paths.as_ref().map(Paths::default_catalog),
            stored_catalog,
        );
        self.remember_window = !self.hidden && self.evidence.is_none();
        self.opening = stored
            .window
            .filter(|_| self.remember_window && self.size.is_none())
            .map(window_frame::Opening::new);
        self.interface_size = Some(stored.interface_size);
        self.launch_theme = Some(stored.theme);
        self.launch_omarchy = stored.omarchy;
        self.launch_follow = stored.follow;
    }

    /// The window's size at launch in the system's points: `--window-size`, the remembered
    /// frame's, or the default.
    fn window_size(&self) -> (f32, f32) {
        self.size
            .or(self.opening.map(|opening| opening.size()))
            .unwrap_or((1440., 900.))
    }

    /// Where this run keeps its files. An evidence run keeps them inside its evidence directory,
    /// so it never touches the person's configuration; any other run keeps them under
    /// `--data-root` or the platform's application directories. Resolving creates nothing.
    fn resolve_paths(&self) -> Option<Paths> {
        match &self.evidence {
            Some(evidence) => Paths::resolve(Some(&evidence.join("host"))).map(|paths| Paths {
                config: self.evidence_config.clone().unwrap_or(paths.config),
                ..paths
            }),
            None => Paths::resolve(self.data_root.as_ref()),
        }
    }
}

fn arguments() -> Result<Config, String> {
    // The end-to-end update journey's bundles never show a window and keep their files under the
    // data root their bundle names, the relaunch Sparkle makes without arguments included.
    let journey = app::update_port::journey_launch();
    let mut config = Config {
        hidden: journey,
        data_root: journey.then(app::update_port::journey_root).flatten(),
        ..Config::default()
    };
    if journey && config.data_root.is_none() {
        return Err(format!(
            "an update journey bundle must name its data root ({})",
            app::update_port::JOURNEY_ROOT_KEY
        ));
    }
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--open") => config
                .files
                .push_back(args.next().ok_or("--open requires a path")?.into()),
            Some("--evidence-dir") => {
                config.evidence = Some(args.next().ok_or("--evidence-dir requires a path")?.into())
            }
            Some("--evidence-script") => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or("--evidence-script requires a path")?
                        .to_owned(),
                );
                let text = std::fs::read_to_string(&path).map_err(|error| {
                    format!("cannot read the evidence script: {}", error.kind())
                })?;
                config.script = app::evidence::parse_script(&text)?;
            }
            Some("--data-root") => {
                config.data_root = Some(args.next().ok_or("--data-root requires a path")?.into());
            }
            Some("--catalog") => {
                config.catalog = Some(args.next().ok_or("--catalog requires a path")?.into());
            }
            Some("--developer") => config.developer_forced = true,
            Some("--proof-endpoint") => {
                config.proof_endpoint = Some(
                    args.next()
                        .and_then(|url| url.into_string().ok())
                        .ok_or("--proof-endpoint requires a URL")?,
                );
            }
            Some("--usage-test-receiver") => {
                let url = args
                    .next()
                    .and_then(|url| url.into_string().ok())
                    .ok_or("--usage-test-receiver requires a URL")?;
                config.usage_receiver = Some(
                    luxforge_core::usage::TestReceiver::loopback(&url)
                        .map_err(|error| format!("--usage-test-receiver: {}", error.detail))?,
                );
            }
            Some("--evidence-config") => {
                config.evidence_config = Some(
                    args.next()
                        .ok_or("--evidence-config requires a path")?
                        .into(),
                );
            }
            Some("--evidence-catalog") => {
                let catalog = PathBuf::from(
                    args.next()
                        .ok_or("--evidence-catalog requires a path")?
                        .to_owned(),
                );
                config.evidence_catalog = Some(
                    std::path::absolute(&catalog)
                        .map_err(|e| format!("--evidence-catalog: {}", e.kind()))?,
                );
            }
            Some("--evidence-unsupported-catalog") => {
                config.unsupported_choice = Some(
                    args.next()
                        .and_then(|choice| {
                            state::unsupported_catalog::Choice::parse(choice.to_str()?)
                        })
                        .ok_or("--evidence-unsupported-catalog requires start or quit")?,
                );
            }
            Some("--update-fixture") => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or("--update-fixture requires a path")?
                        .to_owned(),
                );
                config.update_fixture = Some(app::update_fixture::UpdateFixture::read(&path)?);
            }
            Some("--hidden-window") => config.hidden = true,
            Some("--evidence-move-to-applications") => config.evidence_move = true,
            Some("--evidence-select-introduction") => config.evidence_introduction = true,
            Some("--evidence-paced-downloads") => {
                config.evidence_downloads.pace = Some(EVIDENCE_DOWNLOAD_PIECE);
            }
            Some("--evidence-cut-download") => {
                config.evidence_downloads.cut = Some(
                    args.next()
                        .and_then(|resource| resource.to_str().map(str::to_owned))
                        .ok_or("--evidence-cut-download requires a resource")?,
                );
            }
            Some("--evidence-machine-memory") => {
                config.evidence_memory = Some(
                    args.next()
                        .and_then(|bytes| bytes.to_str()?.parse().ok())
                        .filter(|bytes: &u64| *bytes > 0)
                        .ok_or("--evidence-machine-memory requires a positive byte count")?,
                );
            }
            Some("--evidence-gpu-identity") => config.gpu_identity = true,
            Some("--no-gpu-render") => config.no_gpu_render = true,
            Some("--software-adapter") => config.software_adapter = true,
            Some("--gpu-adapters") => {
                // A diagnostic, before any window, catalog or log: the adapters wgpu offers the
                // backends the renderer chooses among, one JSON object a line.
                let backends = crate::adapters::renderer_backends();
                for adapter in crate::adapters::enumerate(backends) {
                    println!(
                        "{}",
                        app::renderer::adapter_record(
                            &adapter.backend,
                            &adapter.name,
                            Some(&adapter)
                        )
                    );
                }
                std::process::exit(0)
            }
            Some("--disable-module") => {
                let id = args
                    .next()
                    .ok_or("--disable-module requires a module identity")?
                    .into_string()
                    .map_err(|_| "--disable-module requires a module identity")?;
                config.disabled.push(id);
            }
            Some("--window-size") => {
                let mut number = || {
                    args.next()
                        .and_then(|v| v.to_str().and_then(|s| s.parse::<u32>().ok()))
                        .filter(|n| (320..=4096).contains(n))
                        .ok_or("Window dimensions must be 320..4096")
                };
                config.size = Some((number()? as f32, number()? as f32));
            }
            Some("--help") => {
                println!(
                    "Luxforge: [--open IMAGE]... [--catalog CATALOG] [--data-root DIRECTORY] [--developer] [--proof-endpoint URL] [--disable-module MODULE_ID]... [--evidence-dir NEW_DIRECTORY] [--evidence-script FILE] [--evidence-gpu-identity] [--no-gpu-render] [--software-adapter] [--window-size WIDTH HEIGHT] [--hidden-window] [--usage-test-receiver URL] [--evidence-config DIRECTORY] [--evidence-catalog CATALOG] [--evidence-unsupported-catalog start|quit] [--update-fixture FILE] [--evidence-move-to-applications] [--evidence-select-introduction] [--evidence-paced-downloads] [--evidence-cut-download RESOURCE] [--evidence-machine-memory BYTES]\n       Luxforge --gpu-adapters\n--no-gpu-render refuses the GPU stage for this launch, as on a machine whose graphics adapter cannot run it: every frame is drawn by the reference renderer, and the session and the status bar say so. On a host whose only graphics adapter is a software one (lavapipe, WARP), the GPU stage is refused the same way until the software adapter is adopted; --software-adapter draws through it on that adapter for this launch, and the session and the status bar name it. --gpu-adapters prints the graphics adapters wgpu offers the renderer, one JSON object a line, and exits.\n--developer serves the test modules (the pixel and controls proofs) and shows the components gallery for this launch, whatever the Developer mode flag in Settings says (that flag is on by default in debug builds); --proof-endpoint registers the capability proof module against a proof endpoint a test harness started, and only in developer mode; --disable-module registers a built-in as unavailable, so a stack that uses it reports the unavailable effect instead of rendering without it.\n--hidden-window creates the window invisible: it renders and captures as usual but is never placed on screen, which is what automated launches use.\n--usage-test-receiver, in evidence mode only, gives the launch explicit usage statistics test eligibility with a loopback receiver (http on localhost, 127.0.0.0/8 or [::1] with a port) or exactly https://api.luxforge.app/v1/usage/batches?tenant=test: it shows the startup notice and Settings › Privacy, and sends nothing until someone opts in in that run's own isolated preferences; its statistics worker reports a fixed test release and runs on a clock evidence script steps can move forward.\n--update-fixture, in evidence mode only, replaces the platform's update port with one scripted by the JSON file: the mechanism, what each check finds, the download's progress or readiness and a refusal of the next install; it contacts no feed and installs nothing.\n--evidence-move-to-applications, in evidence mode only, shows the Move Luxforge to Applications question once the workspace is usable, wherever the run's app is, so it can be captured; its buttons move nothing.\n--evidence-select-introduction, in evidence mode only, shows the Select introduction the first time Select is shown while the run's preferences say it was not closed, as a launch with its window shown does, so it can be captured.\n--evidence-paced-downloads, in evidence mode only, makes a developer run's model downloads arrive in paced pieces, so a setup's progress can be captured; --evidence-cut-download cuts the named developer resource's first transfer halfway as a lost connection, keeping what it fetched, so a failed setup and its resumption can be. --evidence-machine-memory, in evidence mode only, holds model minimums against BYTES of memory instead of this machine's.\n--evidence-config, in evidence mode only, keeps an earlier evidence run's host/config directory as this run's configuration, so what one launch stored, such as its usage statistics choice, is what the next one reads.\n--evidence-catalog, in evidence mode only, opens CATALOG as the run's remembered catalog instead of one inside its evidence directory; when this version refuses it as another version's, the run captures the unsupported-catalog sheet, then takes the choice --evidence-unsupported-catalog names, Start a new catalog (start) or Quit (quit), and fails without one.\nEvidence mode imports each --open in order into an isolated catalog, captures a frame after each, runs any evidence script with a frame per step and exits. --evidence-gpu-identity, in evidence mode only, draws the photograph at Fit through the GPU preview stage with the identity program, for a rendered check of that stage."
                );
                std::process::exit(0)
            }
            _ => return Err("Unknown argument; use --help".into()),
        }
    }
    // The directories are deliberately not created until they have real work.
    config.paths = config.resolve_paths();
    config.resolve_flags();
    config.resolve_launch();
    // Refused here, before an evidence directory or log exists; the assembly refuses it again.
    config.registry_options().check()?;
    if config.files.len() > 16 {
        return Err("At most 16 evidence requests are supported per run".into());
    }
    if config.files.len() > 1 && config.evidence.is_none() {
        return Err("Repeated --open requires --evidence-dir".into());
    }
    // A script exists to produce captured frames, so it is meaningless without an evidence run.
    if !config.script.is_empty() && config.evidence.is_none() {
        return Err("--evidence-script requires --evidence-dir".into());
    }
    // A test hook: an ordinary launch never draws through a forced GPU plan.
    if config.gpu_identity && config.evidence.is_none() {
        return Err("--evidence-gpu-identity requires --evidence-dir".into());
    }
    // Only an earlier evidence run's own configuration may be carried into another.
    if let Some(dir) = &config.evidence_config {
        if config.evidence.is_none() {
            return Err("--evidence-config requires --evidence-dir".into());
        }
        if !dir.is_dir() || !dir.ends_with("host/config") {
            return Err(
                "--evidence-config names an earlier evidence run's existing host/config directory"
                    .into(),
            );
        }
    }
    // A refused catalog's sheet is captured only by an evidence run, over a catalog it names.
    if (config.evidence_catalog.is_some() || config.unsupported_choice.is_some())
        && config.evidence.is_none()
    {
        return Err(
            "--evidence-catalog and --evidence-unsupported-catalog require --evidence-dir".into(),
        );
    }
    // The move question is captured only by an evidence run; a person's launch decides for itself.
    if config.evidence_move && config.evidence.is_none() {
        return Err("--evidence-move-to-applications requires --evidence-dir".into());
    }
    // So is the Select introduction: an automated launch never shows it unless its run asks.
    if config.evidence_introduction && config.evidence.is_none() {
        return Err("--evidence-select-introduction requires --evidence-dir".into());
    }
    // The developer downloads' pace and cut, and the machine's memory, are an evidence run's; the
    // pace and cut apply only to the developer models a developer run serves.
    if config.evidence_downloads != luxforge_core::DeveloperDownloads::default()
        && config.evidence.is_none()
    {
        return Err(
            "--evidence-paced-downloads and --evidence-cut-download require --evidence-dir".into(),
        );
    }
    if config.evidence_memory.is_some() && config.evidence.is_none() {
        return Err("--evidence-machine-memory requires --evidence-dir".into());
    }
    config.introduces = state::select_introduction::introduces(
        config.hidden,
        config.evidence.is_some(),
        config.evidence_introduction,
    );
    if config.evidence_catalog.is_some() && config.catalog.is_some() {
        return Err("--evidence-catalog and --catalog name two catalogs".into());
    }
    // A scripted update port stands in for the platform's only where frames are captured.
    if config.update_fixture.is_some() && config.evidence.is_none() {
        return Err("--update-fixture requires --evidence-dir".into());
    }
    // Test eligibility belongs to an isolated configuration, which only an evidence run keeps.
    if config.usage_receiver.is_some() && config.evidence.is_none() {
        return Err("--usage-test-receiver requires --evidence-dir".into());
    }
    if let Some(path) = &config.evidence {
        if path.exists() {
            return Err("Evidence directory must be new to prevent stale evidence".into());
        }
        std::fs::create_dir_all(path)
            .map_err(|e| format!("Cannot create evidence directory: {}", e.kind()))?;
    }
    config.run_id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    // The diagnostics log is real work on every launch: an ordinary session keeps a bounded
    // `events.jsonl` in the platform log directory, so a stall or a failure can be read back
    // afterwards, and the launch before it as `events.previous.jsonl`. An evidence run writes into
    // its own new directory.
    let evidence_log = config.evidence.is_some();
    let log_dir = config
        .evidence
        .clone()
        .or_else(|| config.paths.as_ref().map(|p| p.logs.clone()));
    if let Some(dir) = log_dir {
        let start = || -> std::io::Result<Diagnostics> {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("events.jsonl");
            if !evidence_log && path.exists() {
                std::fs::rename(&path, dir.join("events.previous.jsonl"))?;
            }
            Diagnostics::start(&path)
        };
        match start() {
            Ok(log) => {
                log.panic_hook(config.run_id.clone());
                config.diagnostics = Some(log);
            }
            Err(error) if config.evidence.is_some() => {
                return Err(format!(
                    "diagnostics: cannot initialize log: {}",
                    error.kind()
                ));
            }
            Err(error) => eprintln!(
                "diagnostics: logging unavailable: {}; viewing continues",
                error.kind()
            ),
        }
    }
    Ok(config)
}

/// The environment variable that admits the RAW test cameras for this launch, read once at
/// startup before any source is opened.
const TEST_CAMERAS: &str = "LUXFORGE_TEST_CAMERAS";

/// Each piece of a paced developer download (`--evidence-paced-downloads`): a download takes eight
/// pieces, about three seconds, long enough to capture a setup's bar between them.
const EVIDENCE_DOWNLOAD_PIECE: std::time::Duration = std::time::Duration::from_millis(400);

/// Admit the RAW test cameras, such as the synthetic HDR bracket body, when the launch's
/// environment sets [`TEST_CAMERAS`] to `1`: only a harness does, for a journey over files a test
/// wrote. Otherwise the editor refuses them as any camera outside the catalog.
fn admit_test_cameras_from_environment() {
    if test_cameras_requested(std::env::var_os(TEST_CAMERAS).as_deref()) {
        luxforge_core::admit_test_cameras();
    }
}

/// Whether the variable's value asks for the test cameras: exactly `1`.
fn test_cameras_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn main() {
    if luxforge_cli::version_requested(std::env::args_os().skip(1)) {
        println!("luxforge {}", luxforge_cli::VERSION);
        return;
    }
    let config = arguments().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2)
    });
    let size = config.window_size();
    admit_test_cameras_from_environment();
    // The editor presents through the GPU device the counters read, so reading it opens nothing new.
    luxforge_core::resources::declare_gpu_presenter();
    if let Err(error) = app::run(config, size) {
        eprintln!("Could not start Luxforge editor: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only `LUXFORGE_TEST_CAMERAS=1` admits the RAW test cameras.
    #[test]
    fn only_one_admits_the_test_cameras() {
        assert!(test_cameras_requested(Some("1".as_ref())));
        for refused in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("1 "),
            Some("yes"),
        ] {
            assert!(
                !test_cameras_requested(refused.map(AsRef::as_ref)),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn a_proof_endpoint_is_refused_outside_developer_mode() {
        let proof = Config {
            proof_endpoint: Some("http://127.0.0.1:9".into()),
            ..Config::default()
        };
        assert_eq!(
            proof.registry_options().check().unwrap_err(),
            "--proof-endpoint requires developer mode (--developer)"
        );
        assert!(
            Config {
                developer: true,
                ..proof
            }
            .registry_options()
            .check()
            .is_ok()
        );
        assert!(Config::default().registry_options().check().is_ok());
    }

    #[test]
    fn developer_mode_follows_the_stored_flag_unless_the_command_line_forces_it() {
        let root = luxforge_testbase::paths::temp_path("developer-flag");
        let launch = |forced: bool| {
            let mut config = Config {
                data_root: Some(root.clone()),
                developer_forced: forced,
                ..Config::default()
            };
            config.paths = config.resolve_paths();
            config.resolve_flags();
            config
        };
        assert_eq!(launch(false).developer, cfg!(debug_assertions));
        let config = launch(false).paths.unwrap().config;
        for stored in [true, false] {
            std::fs::create_dir_all(&config).unwrap();
            std::fs::write(
                config.join("preferences.json"),
                format!(
                    r#"{{"format":1,"performance_expanded":true,"flags":{{"developer":{stored}}}}}"#
                ),
            )
            .unwrap();
            assert_eq!(launch(false).developer, stored);
            assert!(launch(true).developer, "--developer forces it on");
        }
        // The proof endpoint is checked against the resolved mode, not the switch alone.
        let refused = Config {
            proof_endpoint: Some("http://127.0.0.1:9".into()),
            ..launch(false)
        };
        assert!(refused.registry_options().check().is_err());
        std::fs::write(
            config.join("preferences.json"),
            r#"{"format":1,"performance_expanded":true,"flags":{"developer":true}}"#,
        )
        .unwrap();
        let allowed = Config {
            proof_endpoint: Some("http://127.0.0.1:9".into()),
            ..launch(false)
        };
        assert!(allowed.registry_options().check().is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A launch over `root` as `arguments` resolves it after parsing.
    fn resolved(config: Config) -> Config {
        let mut config = config;
        config.paths = config.resolve_paths();
        config.resolve_flags();
        config.resolve_launch();
        config
    }

    fn store(root: &std::path::Path, preferences: &str) {
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("preferences.json"), preferences).unwrap();
    }

    #[test]
    fn the_launch_catalog_follows_the_stored_location_unless_the_command_line_or_evidence_overrides_it()
     {
        use state::preferences::CatalogOverride;
        let root = luxforge_testbase::paths::temp_path("launch-catalog");
        let default = root.join("config").join("catalog.sqlite");
        let launch = || Config {
            data_root: Some(root.clone()),
            ..Config::default()
        };
        // Nothing stored: the default, as before.
        let catalog = resolved(launch()).launch_catalog.unwrap();
        assert_eq!(
            (&catalog.path, catalog.forced, &catalog.missing),
            (&default, None, &None)
        );
        assert_eq!(catalog.default.as_ref(), Some(&default));

        // A stored location whose folder exists opens there, though it holds no catalog yet.
        let folder = root.join("Photos");
        std::fs::create_dir_all(&folder).unwrap();
        let stored = folder.join("catalog.sqlite");
        store(
            &root,
            &format!(r#"{{"format":1,"catalog":{}}}"#, json(&stored)),
        );
        let catalog = resolved(launch()).launch_catalog.unwrap();
        assert_eq!((&catalog.path, &catalog.missing), (&stored, &None));
        assert!(!stored.exists(), "resolving creates nothing");

        // --catalog and an evidence run take precedence.
        let named = root.join("named.sqlite");
        let catalog = resolved(Config {
            catalog: Some(named.clone()),
            ..launch()
        })
        .launch_catalog
        .unwrap();
        assert_eq!(
            (&catalog.path, catalog.forced),
            (&named, Some(CatalogOverride::CommandLine))
        );
        let evidence = root.join("evidence");
        let catalog = resolved(Config {
            evidence: Some(evidence.clone()),
            ..launch()
        })
        .launch_catalog
        .unwrap();
        assert_eq!(
            (&catalog.path, catalog.forced),
            (
                &evidence.join("catalog.sqlite"),
                Some(CatalogOverride::Evidence)
            )
        );

        // A missing folder, as with an unplugged drive, opens the default with the reason and
        // keeps the stored location.
        let unplugged = root.join("Unplugged").join("catalog.sqlite");
        let stored_text = format!(r#"{{"format":1,"catalog":{}}}"#, json(&unplugged));
        store(&root, &stored_text);
        let catalog = resolved(launch()).launch_catalog.unwrap();
        assert_eq!(
            (&catalog.path, &catalog.missing),
            (&default, &Some(unplugged.clone()))
        );
        assert_eq!(
            catalog.missing_note().unwrap(),
            format!(
                "Catalog folder not found: {}; using the default catalog",
                root.join("Unplugged").display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(root.join("config").join("preferences.json")).unwrap(),
            stored_text,
            "the stored location is kept"
        );
        // --catalog still wins over a missing folder, with no reason to give.
        let catalog = resolved(Config {
            catalog: Some(named.clone()),
            ..launch()
        })
        .launch_catalog
        .unwrap();
        assert_eq!((&catalog.path, &catalog.missing), (&named, &None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_stored_window_frame_opens_unless_window_size_or_a_hidden_or_evidence_launch_overrides_it()
     {
        let root = luxforge_testbase::paths::temp_path("launch-window");
        let launch = || Config {
            data_root: Some(root.clone()),
            ..Config::default()
        };
        // Nothing stored: the default size, placed by the system, and the close remembers it.
        let config = resolved(launch());
        assert_eq!(
            (config.opening, config.window_size()),
            (None, (1440., 900.))
        );
        assert!(config.remember_window);

        store(
            &root,
            r#"{"format":1,"window":{"width":1250,"height":750,"x":-1200.5,"y":40},"interface_size":125}"#,
        );
        let config = resolved(launch());
        let opening = config.opening.expect("the stored frame");
        assert_eq!(opening.position(), (-1200.5, 40.0));
        // In the system's points; the launch hands Iced 1000 by 600 at 125%.
        assert_eq!(config.window_size(), (1250.0, 750.0));
        assert_eq!(config.interface_size, Some(125));
        assert!(config.remember_window);

        // --window-size opens at its own size, placed by the system.
        let config = resolved(Config {
            size: Some((800.0, 600.0)),
            ..launch()
        });
        assert_eq!(
            (config.opening, config.window_size()),
            (None, (800.0, 600.0))
        );
        assert!(config.remember_window);

        // A hidden launch and an evidence run neither open at nor store the frame.
        for config in [
            Config {
                hidden: true,
                ..launch()
            },
            Config {
                evidence: Some(root.join("evidence")),
                ..launch()
            },
        ] {
            let config = resolved(config);
            assert_eq!(config.opening, None);
            assert!(!config.remember_window);
            assert_eq!(
                config.omarchy_home, None,
                "an automated launch never reads Omarchy's"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The launch reads the active theme with the catalog and the frame: Luxforge Dark with nothing
    /// stored, and in place of a stored choice the library cannot show, with the problem naming it.
    #[test]
    fn the_launch_reads_the_active_theme_with_the_other_launch_preferences() {
        let root = luxforge_testbase::paths::temp_path("launch-theme");
        // Hidden, so a host running Omarchy does not choose the theme.
        let launch = || Config {
            data_root: Some(root.clone()),
            hidden: true,
            ..Config::default()
        };
        let theme = resolved(launch()).launch_theme.expect("read at launch");
        assert_eq!(theme.id, luxforge_core::theme::LUXFORGE_DARK_ID);
        assert!(theme.problem.is_none());

        store(&root, r#"{"format":1,"theme":"theme-0123456789abcdef"}"#);
        let theme = resolved(launch()).launch_theme.expect("read at launch");
        assert_eq!(theme.id, luxforge_core::theme::LUXFORGE_DARK_ID);
        let problem = theme.problem.expect("the chosen theme cannot be shown");
        assert!(
            problem.detail.contains("theme-0123456789abcdef"),
            "{}",
            problem.detail
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn json(path: &std::path::Path) -> String {
        serde_json::to_string(path).unwrap()
    }
}
