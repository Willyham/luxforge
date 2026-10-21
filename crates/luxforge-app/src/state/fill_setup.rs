//! Setting up AI editing: the one setup a fill module's models need, as the Corrections band's
//! Remove shows it ([design](../../../../docs/design/ai-editing.md)).
//!
//! The setup is derived from what the desktop already holds of the fill module
//! ([`crate::state::remove::fill_module`]): its descriptor's resource sizes and minimums, its
//! `module.status` answer (each resource's state, staged bytes, memory fit and error, and the fill
//! tiers' missing runtime or model and their models' files) and the install jobs it tracks. The
//! copy names the feature, never a model: the model's name, version and provenance are in
//! Settings › Models. Every action is the capability block's own install or cancel, so an install
//! goes through the same consent notice as any other.
use crate::state::{
    capabilities::{ModuleCapabilities, capitalised, human_bytes, job},
    models::{held, memory_text},
    remove::{FillTier, fill_module, shown_tiers, stale},
};
use luxforge_core::{
    ModuleDescriptor,
    capabilities::resources::{ResourceRow, ResourceState},
    flags::FlagList,
    jobs::JobRecord,
};
use serde_json::{Value, json};

/// What a setup button asks the capability driver for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetupAction {
    /// Install the resource: `module.resource.install`, through the consent notice.
    Install { module_id: String, resource: String },
    /// Stop its install job: `job.cancel`.
    Cancel { module_id: String, job: String },
}

impl SetupAction {
    /// Cancel stops work already running, so it stays pressable while other requests are out.
    pub(crate) fn always_pressable(&self) -> bool {
        matches!(self, Self::Cancel { .. })
    }
}

/// One line of the setup: what the feature needs or can do now, and the one thing to press.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupLine {
    /// The resource the line installs, once the status has named it.
    pub(crate) resource: Option<String>,
    pub(crate) text: String,
    /// `One-time download · 2.5 GB`, `35% of 2.5 GB` or the install's step.
    pub(crate) detail: Option<String>,
    /// The install's fraction while it runs.
    pub(crate) progress: Option<f64>,
    /// The button's label and what it sends.
    pub(crate) action: Option<(String, SetupAction)>,
    /// Its resource is installed.
    pub(crate) installed: bool,
    /// A few plain words for a status bar or a disabled Generate: `Set up AI editing first`.
    pub(crate) reason: String,
}

impl SetupLine {
    /// What a captured frame records of the line.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "resource": self.resource,
            "text": self.text,
            "detail": self.detail,
            "progress": self.progress,
            "action": self.action.as_ref().map(|(label, _)| label.clone()),
            "installed": self.installed,
            "reason": self.reason,
        })
    }
}

/// The setup of a fill module: the main model, then the optional add-on once the main one is in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupModel {
    pub(crate) module_id: String,
    /// The model that removes and fills.
    pub(crate) main: SetupLine,
    /// Replace's text encoder, offered only once the main model is installed.
    pub(crate) addon: Option<SetupLine>,
    /// Nothing is in flight for the module, so its buttons may send; Cancel always may.
    pub(crate) enabled: bool,
    /// The resources the setup stands for, which the generic resource rows leave out: every model
    /// the module holds, as Settings › Models lists them.
    pub(crate) covers: Vec<String>,
}

impl SetupModel {
    pub(crate) fn summary(&self) -> Value {
        json!({
            "main": self.main.summary(),
            "addon": self.addon.as_ref().map(SetupLine::summary),
            "enabled": self.enabled,
        })
    }
}

/// What the status says of one of a tier model's files.
struct File<'a> {
    resource: &'a str,
    row: Option<&'a ResourceRow>,
    /// The tier's own word for its state, when the status lists no row for it.
    tier_state: &'a str,
    /// Every file's bytes together.
    bytes: u64,
    min_memory: Option<u64>,
    job: Option<&'a JobRecord>,
}

impl File<'_> {
    fn state(&self) -> ResourceState {
        self.row.map_or(
            match self.tier_state {
                "installed" => ResourceState::Installed,
                "installing" => ResourceState::Installing,
                "failed" => ResourceState::Failed,
                _ => ResourceState::NotInstalled,
            },
            |row| row.state,
        )
    }

    fn fits(&self) -> bool {
        self.row.and_then(|row| row.fits) != Some(false)
    }

    fn memory(&self) -> String {
        memory_text(
            self.row
                .and_then(|row| row.requirements.as_ref()?.min_memory_bytes)
                .or(self.min_memory)
                .unwrap_or_default(),
        )
    }

    fn staged(&self) -> Option<u64> {
        self.row
            .and_then(|row| row.staged_bytes)
            .filter(|bytes| *bytes > 0)
    }

    fn error(&self) -> Option<&str> {
        self.row
            .and_then(|row| row.error.as_ref())
            .map(|error| error.message.as_str())
    }

    fn size(&self) -> String {
        human_bytes(self.bytes)
    }

    /// `Download paused at 1.2 GB of 2.5 GB`, when an interrupted transfer is kept.
    fn paused(&self) -> Option<String> {
        self.staged().map(|staged| {
            format!(
                "Download paused at {} of {}",
                human_bytes(staged),
                self.size()
            )
        })
    }

    /// While it installs: `35% of 2.5 GB` during the download, then the step past it.
    fn installing(&self) -> (Option<String>, Option<f64>) {
        let progress = self.job.map(|job| &job.progress);
        let fraction = progress
            .and_then(|progress| progress.fraction)
            .map(|fraction| fraction.clamp(0.0, 1.0));
        let detail = match (fraction, progress.and_then(|p| p.message.as_deref())) {
            (_, Some(step)) if step != "downloading" => Some(capitalised(step)),
            (Some(fraction), _) => Some(format!("{:.0}% of {}", fraction * 100.0, self.size())),
            (None, _) => None,
        };
        (detail, fraction)
    }

    fn install(&self, module_id: &str, label: &str) -> Option<(String, SetupAction)> {
        Some((
            label.to_owned(),
            SetupAction::Install {
                module_id: module_id.to_owned(),
                resource: self.resource.to_owned(),
            },
        ))
    }

    fn cancel(&self, module_id: &str) -> Option<(String, SetupAction)> {
        let job = self.row?.job_id.as_ref()?;
        Some((
            "Cancel".to_owned(),
            SetupAction::Cancel {
                module_id: module_id.to_owned(),
                job: job.as_str().to_owned(),
            },
        ))
    }
}

/// A line with only its text and reason.
fn said(resource: Option<&str>, text: String, reason: String) -> SetupLine {
    SetupLine {
        resource: resource.map(str::to_owned),
        text,
        detail: None,
        progress: None,
        action: None,
        installed: false,
        reason,
    }
}

/// What the status and the descriptor say of one file of a tier's model.
fn file<'a>(
    module: &'a ModuleDescriptor,
    state: &'a ModuleCapabilities,
    resource: &'a str,
    tier_state: &'a str,
) -> File<'a> {
    let row = state
        .status
        .as_ref()
        .and_then(|status| status.resources.iter().find(|row| row.id == resource));
    let declared = module
        .resources
        .iter()
        .find(|declared| declared.id == resource);
    File {
        resource,
        row,
        tier_state,
        bytes: declared.map_or_else(|| row.map_or(0, |row| row.bytes), |d| d.total_bytes()),
        min_memory: declared.and_then(|declared| declared.min_memory_bytes),
        job: row
            .filter(|row| row.state == ResourceState::Installing)
            .and_then(|row| row.job_id.as_ref())
            .and_then(|id| job(state, id.as_str())),
    }
}

/// The main model's line: what AI editing does and the download it needs, its progress, why it
/// cannot be set up here, or that it is ready.
fn main_line(module_id: &str, title: &str, file: &File<'_>) -> SetupLine {
    let resource = Some(file.resource);
    let set_up_first = format!("Set up {title} first");
    match file.state() {
        ResourceState::Installed => SetupLine {
            installed: true,
            ..said(
                resource,
                format!(
                    "{title} is ready. Choose Remove in Corrections and paint over what to remove."
                ),
                format!("{title} is ready"),
            )
        },
        _ if !file.fits() => said(
            resource,
            format!("{title} needs a computer with {} of memory.", file.memory()),
            format!("{title} needs a computer with {} of memory", file.memory()),
        ),
        ResourceState::Installing => {
            let (detail, progress) = file.installing();
            SetupLine {
                detail,
                progress,
                action: file.cancel(module_id),
                ..said(
                    resource,
                    format!("Setting up {title}\u{2026}"),
                    format!("Setting up {title}\u{2026}"),
                )
            }
        }
        ResourceState::Failed => SetupLine {
            detail: Some(format!("One-time download \u{b7} {}", file.size())),
            action: file.install(module_id, "Try again"),
            ..said(
                resource,
                match file.error() {
                    Some(error) => format!("Setup didn't finish: {error}"),
                    None => "Setup didn't finish.".to_owned(),
                },
                set_up_first,
            )
        },
        ResourceState::NotInstalled => SetupLine {
            detail: Some(
                file.paused()
                    .unwrap_or_else(|| format!("One-time download \u{b7} {}", file.size())),
            ),
            action: file.install(
                module_id,
                if file.staged().is_some() {
                    "Resume"
                } else {
                    "Set up"
                },
            ),
            ..said(
                resource,
                format!(
                    "Remove objects and fill in parts of a photo. {title} runs on this computer, \
                     so your photos stay on it."
                ),
                set_up_first,
            )
        },
    }
}

/// The add-on's line: Replace's text encoder, which only a prompt needs.
fn addon_line(module_id: &str, file: &File<'_>) -> SetupLine {
    let resource = Some(file.resource);
    let add = "Add Replace to fill with what you describe";
    match file.state() {
        ResourceState::Installed => SetupLine {
            installed: true,
            ..said(
                resource,
                "Replace is ready. Type what to fill with under Remove.".into(),
                "Replace is ready".into(),
            )
        },
        _ if !file.fits() => said(
            resource,
            format!(
                "Replace fills with what you describe. It needs a computer with {} of memory.",
                file.memory()
            ),
            format!("Replace needs a computer with {} of memory", file.memory()),
        ),
        ResourceState::Installing => {
            let (detail, progress) = file.installing();
            SetupLine {
                detail,
                progress,
                action: file.cancel(module_id),
                ..said(
                    resource,
                    "Adding Replace\u{2026}".into(),
                    "Adding Replace\u{2026}".into(),
                )
            }
        }
        ResourceState::Failed => SetupLine {
            detail: Some(format!("{} download", file.size())),
            action: file.install(module_id, "Try again"),
            ..said(
                resource,
                match file.error() {
                    Some(error) => format!("Replace didn't install: {error}"),
                    None => "Replace didn't install.".to_owned(),
                },
                add.into(),
            )
        },
        ResourceState::NotInstalled => SetupLine {
            detail: Some(
                file.paused()
                    .unwrap_or_else(|| format!("{} download", file.size())),
            ),
            action: file.install(
                module_id,
                if file.staged().is_some() {
                    "Resume"
                } else {
                    "Add"
                },
            ),
            ..said(resource, format!("{add}."), add.into())
        },
    }
}

/// The setup of `module`, when it is the fill module the desktop offers; `None` for every other.
pub(crate) fn derive(
    module: &ModuleDescriptor,
    state: &ModuleCapabilities,
    flags: Option<&FlagList>,
) -> Option<SetupModel> {
    fill_module(std::slice::from_ref(module), flags)?;
    let title = &module.title;
    let status = state.status.as_ref();
    // A status read before a flag changed is being read again: until it answers, it is not read.
    let tiers = status
        .filter(|status| !stale(&status.fill_tiers, flags))
        .map(|status| shown_tiers(module, &status.fill_tiers, flags))
        .unwrap_or_default();
    let tier = tiers.first();
    let model = tier.and_then(|tier| tier.installed_model().or_else(|| tier.models.first()));
    let (main, addon) = match (tier, model) {
        (Some(tier), Some(model)) => {
            let main = if runtime_missing(tier) {
                said(
                    Some(&model.resource),
                    format!("{title} can't run on this computer."),
                    format!("{title} can't run on this computer"),
                )
            } else {
                main_line(
                    &module.id,
                    title,
                    &file(module, state, &model.resource, &model.state),
                )
            };
            let addon = model
                .text_encoder
                .as_ref()
                .filter(|_| main.installed)
                .map(|encoder| {
                    addon_line(
                        &module.id,
                        &file(module, state, &encoder.resource, &encoder.state),
                    )
                });
            (main, addon)
        }
        _ => {
            let checking = format!("Checking {title}\u{2026}");
            (said(None, checking.clone(), checking), None)
        }
    };
    Some(SetupModel {
        module_id: module.id.clone(),
        main,
        addon,
        enabled: state.pending == 0 && module.is_available(),
        covers: held(module, flags)
            .into_iter()
            .map(|resource| resource.id.clone())
            .collect(),
    })
}

/// The tier lacks a runtime to run its models on this computer.
fn runtime_missing(tier: &FillTier) -> bool {
    tier.missing.iter().any(|missing| missing.kind == "runtime")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::capabilities::ModuleStatus;

    const GIB: u64 = 1 << 30;
    const JOB: &str = "job-0123456789abcdef0123456789abcdef";

    /// A fill module whose model is `gen`, 2.5 GB, with the 2.3 GB add-on `text`.
    fn module() -> ModuleDescriptor {
        let runtime = |id: &str, resource: &str| {
            json!({"id": id, "kind": "local-runtime", "resource": resource, "model": "session",
                   "resident_bytes": 1024, "backends": ["cpu"], "precision": "fp16",
                   "purpose": "p"})
        };
        let resource = |id: &str, bytes: u64, memory: u64| {
            json!({"id": id, "title": format!("Technical {id}"), "version": "klein-4b-q4_0",
                   "url": "https://e.example/a", "bytes": bytes, "sha256": "a".repeat(64),
                   "format": "f", "license": "l", "provenance": "p", "min_memory_bytes": memory})
        };
        serde_json::from_value(json!({
            "id": "ai", "title": "AI editing", "effects": [], "actions": [], "controls": [],
            "canvas": null, "availability": {"kind": "available"},
            "tasks": [{"id": "fill-generative", "title": "Generative fill", "notes": "n"}],
            "capabilities": [runtime("run-gen", "gen"), runtime("run-text", "text")],
            "resources": [
                resource("gen", 2_720_267_065, 16 * GIB),
                resource("text", 2_508_703_966, 24 * GIB),
            ],
        }))
        .unwrap()
    }

    /// A resource row in `state`, with `extra` fields.
    fn row(id: &str, state: &str, extra: Value) -> Value {
        let mut row = json!({
            "id": id, "title": "t", "version": "v", "bytes": 1, "sha256": "a".repeat(64),
            "license": "l", "provenance": "p", "url": "https://e.example/a", "state": state,
            "requirements": {"min_memory_bytes": if id == "gen" { 16 * GIB } else { 24 * GIB }},
            "fits": true,
        });
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    fn state(
        generator: Value,
        encoder: Value,
        missing: Value,
        jobs: Vec<Value>,
    ) -> ModuleCapabilities {
        let named = |row: &Value| row["state"].as_str().unwrap().to_owned();
        let status = ModuleStatus {
            fill_tiers: vec![
                serde_json::from_value(json!({
                    "tier": "generative", "task": "task.fill-generative",
                    "offered": missing.as_array().is_some_and(Vec::is_empty),
                    "missing": missing,
                    "models": [{"model": "gen", "resource": "gen", "state": named(&generator),
                                "text_encoder": {"resource": "text", "state": named(&encoder)}}],
                }))
                .unwrap(),
            ],
            resources: vec![
                serde_json::from_value(generator).unwrap(),
                serde_json::from_value(encoder).unwrap(),
            ],
            permissions: Default::default(),
            jobs: jobs
                .into_iter()
                .map(|job| serde_json::from_value(job).unwrap())
                .collect(),
            runtime: None,
            machine: None,
        };
        ModuleCapabilities {
            status: Some(status),
            ..ModuleCapabilities::default()
        }
    }

    fn missing_model() -> Value {
        json!([{"kind": "model", "id": "generative", "state": "not-installed"}])
    }

    fn installing(resource: &str, fraction: f64, step: &str) -> Value {
        json!({"job_id": JOB, "kind": "install", "status": "running", "ownership": "catalog",
               "module_id": "ai", "resource_id": resource,
               "progress": {"fraction": fraction, "message": step}})
    }

    fn setup(state: &ModuleCapabilities) -> SetupModel {
        derive(&module(), state, None).expect("a fill module's setup")
    }

    fn label(line: &SetupLine) -> Option<&str> {
        line.action.as_ref().map(|(label, _)| label.as_str())
    }

    fn install(resource: &str) -> SetupAction {
        SetupAction::Install {
            module_id: "ai".into(),
            resource: resource.into(),
        }
    }

    #[test]
    fn only_a_fill_module_has_a_setup_and_it_covers_every_model() {
        let mut other = module();
        other.tasks.clear();
        assert_eq!(derive(&other, &ModuleCapabilities::default(), None), None);
        assert_eq!(
            setup(&ModuleCapabilities::default()).covers,
            ["gen", "text"]
        );
    }

    #[test]
    fn before_the_status_is_read_it_is_checking() {
        let setup = setup(&ModuleCapabilities::default());
        assert_eq!(setup.main.text, "Checking AI editing\u{2026}");
        assert_eq!(setup.main.action, None);
        assert_eq!(setup.addon, None);
    }

    #[test]
    fn without_a_runtime_or_enough_memory_there_is_nothing_to_press() {
        let no_runtime = setup(&state(
            row("gen", "not-installed", json!({})),
            row("text", "not-installed", json!({})),
            json!([{"kind": "runtime", "id": "inference", "state": "unavailable"}]),
            Vec::new(),
        ));
        assert_eq!(
            no_runtime.main.text,
            "AI editing can't run on this computer."
        );
        assert_eq!(no_runtime.main.action, None);
        let short = setup(&state(
            row("gen", "not-installed", json!({"fits": false})),
            row("text", "not-installed", json!({})),
            missing_model(),
            Vec::new(),
        ));
        assert_eq!(
            short.main.text,
            "AI editing needs a computer with 16 GB of memory."
        );
        assert_eq!(short.main.action, None);
    }

    #[test]
    fn not_set_up_it_says_what_it_does_and_offers_the_download_or_its_resumption() {
        let fresh = setup(&state(
            row("gen", "not-installed", json!({})),
            row("text", "not-installed", json!({})),
            missing_model(),
            Vec::new(),
        ));
        assert_eq!(
            fresh.main.text,
            "Remove objects and fill in parts of a photo. AI editing runs on this computer, so \
             your photos stay on it."
        );
        assert_eq!(
            fresh.main.detail.as_deref(),
            Some("One-time download \u{b7} 2.5 GB")
        );
        assert_eq!(
            fresh.main.action,
            Some(("Set up".to_owned(), install("gen")))
        );
        assert_eq!(fresh.main.reason, "Set up AI editing first");
        assert_eq!(fresh.addon, None, "the add-on waits for the main model");
        let paused = setup(&state(
            row(
                "gen",
                "not-installed",
                json!({"staged_bytes": 1_288_490_189u64}),
            ),
            row("text", "not-installed", json!({})),
            missing_model(),
            Vec::new(),
        ));
        assert_eq!(
            paused.main.detail.as_deref(),
            Some("Download paused at 1.2 GB of 2.5 GB")
        );
        assert_eq!(label(&paused.main), Some("Resume"));
    }

    #[test]
    fn installing_it_shows_progress_then_the_step_with_cancel() {
        let at = |fraction: f64, step: &str| {
            setup(&state(
                row("gen", "installing", json!({"job_id": JOB})),
                row("text", "not-installed", json!({})),
                missing_model(),
                vec![installing("gen", fraction, step)],
            ))
        };
        let downloading = at(0.35, "downloading");
        assert_eq!(downloading.main.text, "Setting up AI editing\u{2026}");
        assert_eq!(downloading.main.detail.as_deref(), Some("35% of 2.5 GB"));
        assert_eq!(downloading.main.progress, Some(0.35));
        assert_eq!(
            downloading.main.action,
            Some((
                "Cancel".to_owned(),
                SetupAction::Cancel {
                    module_id: "ai".into(),
                    job: JOB.into()
                }
            ))
        );
        assert_eq!(
            at(1.0, "preparing the model").main.detail.as_deref(),
            Some("Preparing the model")
        );
    }

    #[test]
    fn a_failed_setup_says_why_and_tries_again() {
        let failed = setup(&state(
            row(
                "gen",
                "failed",
                json!({"error": {"code": "unavailable", "message": "the connection dropped"}}),
            ),
            row("text", "not-installed", json!({})),
            missing_model(),
            Vec::new(),
        ));
        assert_eq!(
            failed.main.text,
            "Setup didn't finish: the connection dropped"
        );
        assert_eq!(
            failed.main.detail.as_deref(),
            Some("One-time download \u{b7} 2.5 GB")
        );
        assert_eq!(
            failed.main.action,
            Some(("Try again".to_owned(), install("gen")))
        );
    }

    #[test]
    fn once_installed_it_is_ready_and_offers_replace() {
        let with = |encoder: Value, jobs: Vec<Value>| {
            setup(&state(
                row("gen", "installed", json!({})),
                encoder,
                json!([]),
                jobs,
            ))
        };
        let ready = with(row("text", "not-installed", json!({})), Vec::new());
        assert!(ready.main.installed);
        assert_eq!(
            ready.main.text,
            "AI editing is ready. Choose Remove in Corrections and paint over what to remove."
        );
        assert_eq!(ready.main.action, None);
        let addon = ready.addon.expect("the add-on");
        assert_eq!(addon.text, "Add Replace to fill with what you describe.");
        assert_eq!(addon.detail.as_deref(), Some("2.3 GB download"));
        assert_eq!(addon.action, Some(("Add".to_owned(), install("text"))));

        let short = with(
            row("text", "not-installed", json!({"fits": false})),
            Vec::new(),
        );
        assert_eq!(
            short.addon.unwrap().text,
            "Replace fills with what you describe. It needs a computer with 24 GB of memory."
        );
        let adding = with(
            row("text", "installing", json!({"job_id": JOB})),
            vec![installing("text", 0.5, "downloading")],
        )
        .addon
        .unwrap();
        assert_eq!(adding.text, "Adding Replace\u{2026}");
        assert_eq!(adding.detail.as_deref(), Some("50% of 2.3 GB"));
        assert_eq!(label(&adding), Some("Cancel"));
        let failed = with(
            row(
                "text",
                "failed",
                json!({"error": {"code": "unavailable", "message": "the disk is full"}}),
            ),
            Vec::new(),
        )
        .addon
        .unwrap();
        assert_eq!(failed.text, "Replace didn't install: the disk is full");
        assert_eq!(label(&failed), Some("Try again"));
        let added = with(row("text", "installed", json!({})), Vec::new())
            .addon
            .unwrap();
        assert!(added.installed);
        assert_eq!(
            added.text,
            "Replace is ready. Type what to fill with under Remove."
        );
        assert_eq!(added.action, None);
    }
}
