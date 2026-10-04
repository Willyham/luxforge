// The evidence summary is one `json!` object holding every key this desktop publishes about a
// frame, and the merge of the masking and performance work took it past the default macro recursion
// depth. Raising the limit keeps it one object: splitting it would let two halves disagree about
// what a frame reported.
#![recursion_limit = "256"]

mod app;
mod browser;
mod coalesce;
mod crop_draft;
mod diagnostics;
mod layout;
mod mask_draft;
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
}

impl Config {
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

    /// Where this run keeps its files. An evidence run keeps them inside its evidence directory,
    /// so it never touches the person's configuration; any other run keeps them under
    /// `--data-root` or the platform's application directories. Resolving creates nothing.
    fn resolve_paths(&self) -> Option<Paths> {
        match &self.evidence {
            Some(evidence) => Paths::resolve(Some(&evidence.join("host"))),
            None => Paths::resolve(self.data_root.as_ref()),
        }
    }
}

fn arguments() -> Result<Config, String> {
    let mut config = Config::default();
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
            Some("--hidden-window") => config.hidden = true,
            Some("--evidence-gpu-identity") => config.gpu_identity = true,
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
                    "Luxforge: [--open IMAGE]... [--catalog CATALOG] [--data-root DIRECTORY] [--developer] [--proof-endpoint URL] [--disable-module MODULE_ID]... [--evidence-dir NEW_DIRECTORY] [--evidence-script FILE] [--evidence-gpu-identity] [--window-size WIDTH HEIGHT] [--hidden-window]\n--developer serves the test modules (the pixel and controls proofs) and shows the components gallery for this launch, whatever the Developer mode flag in Settings says (that flag is on by default in debug builds); --proof-endpoint registers the capability proof module against a proof endpoint a test harness started, and only in developer mode; --disable-module registers a built-in as unavailable, so a stack that uses it reports the unavailable effect instead of rendering without it.\n--hidden-window creates the window invisible: it renders and captures as usual but is never placed on screen, which is what automated launches use.\nEvidence mode imports each --open in order into an isolated catalog, captures a frame after each, runs any evidence script with a frame per step and exits. --evidence-gpu-identity, in evidence mode only, draws the photograph at Fit through the GPU preview stage with the identity program, for a rendered check of that stage."
                );
                std::process::exit(0)
            }
            _ => return Err("Unknown argument; use --help".into()),
        }
    }
    // The directories are deliberately not created until they have real work.
    config.paths = config.resolve_paths();
    config.resolve_flags();
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

fn main() {
    let config = arguments().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2)
    });
    let size = config.size.unwrap_or((1440., 900.));
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
}
