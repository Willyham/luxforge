//! External modules on the desktop (phase 2 of the module manager): what `module.package.list` and
//! `module.package.inspect` answer, the review sheet Install module… opens, the Uninstall…
//! confirmation, the pending changes Relaunch applies, and the badges and detail an external
//! module's row in Settings › Modules carries
//! ([design](../../../../docs/design/module-manager.md#lifecycle),
//! [boards E and F](../../../../docs/design/module-manager/screens.md)).
//!
//! Everything here is derived from the owner's answers; nothing decides what the store holds or
//! whether a package installs. The words a refusal says are the owner's.
use luxforge_core::{
    ModuleDescriptor,
    module_layout::{ModuleOrigin, Placement},
};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

/// The grey origin badges, as a row, the detail pane and a band draw them.
pub(crate) const COMMUNITY: &str = "Community";
pub(crate) const FROM_FOLDER: &str = "From folder";
/// The state badges: a change waiting for the next launch (accent), and a failure (red).
pub(crate) const RELAUNCH_TO_APPLY: &str = "Relaunch to apply";
pub(crate) const DIDNT_LOAD: &str = "Didn't load";
/// A band whose photo keeps a layer no registered module provides.
pub(crate) const NOT_INSTALLED: &str = "Not installed";

/// What the detail pane says of every external module.
pub(crate) const TRUST: &str = "From its author, not from Luxforge. Luxforge hasn't reviewed \
     it. It runs sandboxed, with no network or file access.";

/// What the review sheet says under its title once the package reads.
pub(crate) const REVIEW_LEAD: &str = "comes from its author, not from Luxforge. Luxforge \
     hasn't reviewed it.";

/// What the review sheet says of when an install applies.
pub(crate) const APPLIES_AT_RELAUNCH: &str = "It is added when Luxforge relaunches.";

/// The badge an external module's origin draws, or `None` for a built-in.
pub(crate) fn origin_badge(origin: ModuleOrigin) -> Option<&'static str> {
    match origin {
        ModuleOrigin::BuiltIn => None,
        ModuleOrigin::Community => Some(COMMUNITY),
        ModuleOrigin::Folder => Some(FROM_FOLDER),
    }
}

/// The module an effect identity names when no registered module provides it: an external
/// module's one effect is `<id>.adjust`; any other unknown effect is named as itself.
pub(crate) fn missing_module(effect: &str) -> &str {
    effect.strip_suffix(".adjust").unwrap_or(effect)
}

/// One side of an external module's load state: what the next launch loads (`installed`) or what
/// this launch read (`running`), as the owner's `Side` writes it; `module.package.inspect`'s
/// `installed` adds `origin`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct Side {
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) hash: Option<String>,
    #[serde(default)]
    pub(crate) format: Option<u32>,
    #[serde(default)]
    pub(crate) program: Option<bool>,
    #[serde(default)]
    pub(crate) path: Option<PathBuf>,
    #[serde(default)]
    pub(crate) origin: Option<ModuleOrigin>,
}

/// An external module's load state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum LoadState {
    Loaded,
    PendingRelaunch,
    Failed,
}

/// The GPU program's state as the owner reports it: `present` or `absent` today. The program
/// loader will add its own states (validated, probed, disabled with a reason); any word it reports
/// is shown as it is, with its reason, so a richer state needs no change here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct ProgramView {
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

impl ProgramView {
    /// `module.package.inspect`'s `program`: whether the package ships one, or the loader's own
    /// state object once it reports one.
    fn of(value: &Value) -> Self {
        match value {
            Value::Bool(true) => Self {
                state: "present".into(),
                reason: None,
            },
            Value::Bool(false) | Value::Null => Self {
                state: "absent".into(),
                reason: None,
            },
            other => serde_json::from_value(other.clone()).unwrap_or_default(),
        }
    }

    /// How the review sheet and the detail pane say it.
    pub(crate) fn words(&self) -> String {
        let said = match self.state.as_str() {
            "present" => "GPU program, checked against the module's CPU reference at first use",
            "absent" => "CPU reference only: it renders correctly, but more slowly",
            other => {
                return match &self.reason {
                    Some(reason) => format!("GPU program {other}: {reason}"),
                    None => format!("GPU program {other}"),
                };
            }
        };
        said.to_owned()
    }
}

/// One row of `module.package.list`, and `module.status`'s `external`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct PackageRow {
    pub(crate) module: String,
    pub(crate) origin: ModuleOrigin,
    pub(crate) state: LoadState,
    #[serde(default)]
    pub(crate) reason: Option<String>,
    pub(crate) installed: Option<Side>,
    pub(crate) running: Option<Side>,
    pub(crate) program: ProgramView,
}

impl PackageRow {
    /// The title the store or this launch knows it by, else its id.
    pub(crate) fn title(&self) -> String {
        self.installed
            .as_ref()
            .and_then(|side| side.title.clone())
            .or_else(|| self.running.as_ref().and_then(|side| side.title.clone()))
            .unwrap_or_else(|| self.module.clone())
    }

    /// What applies at the next launch, for a row waiting for it.
    pub(crate) fn pending(&self) -> Option<String> {
        if self.state != LoadState::PendingRelaunch {
            return None;
        }
        let folder = self.origin == ModuleOrigin::Folder;
        Some(match (&self.running, &self.installed) {
            (Some(_), None) if folder => "Removed: it is unloaded when Luxforge relaunches.".into(),
            (Some(_), None) => "Uninstalled: it is removed when Luxforge relaunches.".into(),
            (None, Some(_)) if folder => "Loaded: it loads when Luxforge relaunches.".into(),
            (None, Some(_)) => "Installed: it loads when Luxforge relaunches.".into(),
            (Some(running), Some(installed)) if folder => format!(
                "Loads from {} when Luxforge relaunches.",
                installed
                    .path
                    .as_ref()
                    .or(running.path.as_ref())
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
            (Some(running), Some(installed)) => format!(
                "Version {} replaces {} when Luxforge relaunches.",
                installed.version.as_deref().unwrap_or("?"),
                running.version.as_deref().unwrap_or("?")
            ),
            (None, None) => "Changes when Luxforge relaunches.".into(),
        })
    }
}

/// What `modules.json` is now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct StoreView {
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

/// `module.package.list`'s answer.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(crate) struct PackageList {
    pub(crate) modules: Vec<PackageRow>,
    pub(crate) pending_relaunch: bool,
    pub(crate) store: StoreView,
    #[serde(default)]
    pub(crate) launch_problem: Option<String>,
}

impl PackageList {
    pub(crate) fn parse(value: Value) -> Result<Self, String> {
        serde_json::from_value(value)
            .map_err(|error| format!("unreadable module packages: {error}"))
    }

    pub(crate) fn row(&self, module: &str) -> Option<&PackageRow> {
        self.modules.iter().find(|row| row.module == module)
    }

    /// How many changes wait for the next launch.
    pub(crate) fn pending(&self) -> usize {
        self.modules
            .iter()
            .filter(|row| row.state == LoadState::PendingRelaunch)
            .count()
    }

    /// The store takes writes: it reads, or does not exist yet.
    pub(crate) fn writable(&self) -> bool {
        self.store.state == "ready"
    }

    /// Why nothing can be installed, or why this launch loaded no external module.
    pub(crate) fn problem(&self) -> Option<String> {
        match self.store.state.as_str() {
            "ready" => self.launch_problem.clone(),
            "unreadable" => Some(format!(
                "The installed modules can't be read: {}",
                self.store.reason.as_deref().unwrap_or("unknown reason")
            )),
            _ => Some("This launch has no module store, so nothing can be installed.".into()),
        }
    }

    /// Adopt one write's answer for its module at once, before the listing is read again.
    pub(crate) fn adopt(&mut self, row: PackageRow) {
        match self
            .modules
            .iter_mut()
            .find(|known| known.module == row.module)
        {
            Some(known) => *known = row,
            None => self.modules.push(row),
        }
        self.pending_relaunch = self.pending() > 0;
    }
}

/// `module.package.inspect`'s manifest, as much of it as the review sheet shows.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct ManifestView {
    pub(crate) api: String,
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) authors: Vec<String>,
    pub(crate) license: String,
    pub(crate) repository: String,
    pub(crate) effect: EffectView,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct EffectView {
    pub(crate) stage: String,
    pub(crate) order: i64,
    pub(crate) format: u32,
}

/// What a package may reach: `none` for both, as every phase-2 module.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Access {
    pub(crate) network: String,
    pub(crate) files: String,
}

/// The notice an update carries when its payload format differs from the installed one's.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct FormatChange {
    pub(crate) installed: u32,
    pub(crate) package: u32,
    pub(crate) notice: String,
}

/// `module.package.inspect`'s answer.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct Inspected {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) hash: String,
    pub(crate) manifest: ManifestView,
    pub(crate) program: Value,
    pub(crate) descriptor: ModuleDescriptor,
    pub(crate) placements: Vec<Placement>,
    pub(crate) methods: Vec<String>,
    pub(crate) access: Access,
    pub(crate) reviewed: bool,
    pub(crate) installed: Option<Side>,
    pub(crate) already_installed: bool,
    pub(crate) format_change: Option<FormatChange>,
    pub(crate) status: Option<PackageRow>,
}

impl Inspected {
    pub(crate) fn parse(value: Value) -> Result<Self, String> {
        serde_json::from_value(value).map_err(|error| format!("unreadable package review: {error}"))
    }
}

/// Where the review sheet is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ReviewState {
    /// `module.package.inspect` is reading the package.
    Inspecting,
    Inspected(Box<Inspected>),
    /// The reader, the engine's check or the store refused the package: the owner's words and its
    /// `data.refusal` code.
    Refused {
        reason: String,
        refusal: Option<String>,
    },
}

/// The review sheet: the one modal step of Install module….
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Review {
    pub(crate) path: PathBuf,
    pub(crate) state: ReviewState,
    /// `module.install` is in flight.
    pub(crate) installing: bool,
    /// Why `module.install` was refused; the sheet stays open and says so.
    pub(crate) failed: Option<String>,
}

impl Review {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: ReviewState::Inspecting,
            installing: false,
            failed: None,
        }
    }

    /// The sheet offers its install: the package read, no refusal stands in its way, and nothing
    /// is in flight.
    pub(crate) fn installable(&self) -> bool {
        derive_review(self)
            .confirm
            .is_some_and(|confirm| confirm.enabled)
    }
}

/// The review sheet's confirming button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Confirm {
    pub(crate) label: String,
    /// Red and not the default: Return does not press it.
    pub(crate) destructive: bool,
    pub(crate) enabled: bool,
}

/// The review sheet as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReviewModel {
    pub(crate) title: String,
    /// Who it comes from, and that Luxforge hasn't reviewed it.
    pub(crate) lead: Option<String>,
    /// Labelled facts: what it adds and where it renders, its program, what it can reach, then the
    /// manifest's authors, licence, source, version and interface, the package hash and the file.
    pub(crate) facts: Vec<(String, String)>,
    /// In red: the refusal, a payload format change or a refused install.
    pub(crate) warning: Option<String>,
    pub(crate) note: Option<String>,
    pub(crate) confirm: Option<Confirm>,
    /// Cancel, or Close when nothing can be installed.
    pub(crate) dismiss: &'static str,
    pub(crate) busy: Option<&'static str>,
}

fn file_fact(path: &std::path::Path, bytes: Option<u64>) -> (String, String) {
    let shown = path.display().to_string();
    let value = match bytes {
        Some(bytes) => format!("{shown} \u{b7} {}", size(bytes)),
        None => shown,
    };
    ("File".into(), value)
}

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// A stage's name as the detail pane names it.
fn stage_name(stage: &str) -> String {
    match stage {
        "color" => "Colour".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

fn capitalised(word: &str) -> String {
    stage_name(word)
}

/// Where a package's placements put it.
fn adds(placements: &[Placement]) -> String {
    let groups: Vec<String> = placements
        .iter()
        .map(|placement| {
            format!(
                "a section in {}, last",
                super::module_settings::group_label(placement.toolset)
            )
        })
        .collect();
    let said = if groups.is_empty() {
        "nothing in the workspace".to_owned()
    } else {
        groups.join("; ")
    };
    capitalised(&said)
}

/// The review sheet for `review`.
pub(crate) fn derive_review(review: &Review) -> ReviewModel {
    match &review.state {
        ReviewState::Inspecting => ReviewModel {
            title: "Install module".into(),
            lead: None,
            facts: vec![file_fact(&review.path, None)],
            warning: None,
            note: None,
            confirm: None,
            dismiss: "Cancel",
            busy: Some("Reading the package\u{2026}"),
        },
        ReviewState::Refused { reason, .. } => ReviewModel {
            title: "This package can't be installed".into(),
            lead: None,
            facts: vec![file_fact(&review.path, None)],
            warning: Some(reason.clone()),
            note: None,
            confirm: None,
            dismiss: "Close",
            busy: None,
        },
        ReviewState::Inspected(inspected) => inspected_review(review, inspected),
    }
}

fn inspected_review(review: &Review, inspected: &Inspected) -> ReviewModel {
    let manifest = &inspected.manifest;
    let (title, version) = (&manifest.title, &manifest.version);
    let program = ProgramView::of(&inspected.program);
    let authors = manifest.authors.join(", ");
    let mut facts = vec![
        ("Adds".to_owned(), adds(&inspected.placements)),
        (
            "Renders".to_owned(),
            format!(
                "In {} (order {}). Panel order doesn't change this.",
                stage_name(&manifest.effect.stage),
                manifest.effect.order
            ),
        ),
        ("Program".to_owned(), program.words()),
        ("Network".to_owned(), capitalised(&inspected.access.network)),
        ("Files".to_owned(), capitalised(&inspected.access.files)),
        (
            if manifest.authors.len() == 1 {
                "Author"
            } else {
                "Authors"
            }
            .to_owned(),
            authors.clone(),
        ),
        ("Licence".to_owned(), manifest.license.clone()),
        ("Source".to_owned(), manifest.repository.clone()),
        (
            "Version".to_owned(),
            format!("{version} \u{b7} interface {}", manifest.api),
        ),
    ];
    if !inspected.methods.is_empty() {
        facts.push(("API".to_owned(), inspected.methods.join(", ")));
    }
    facts.push(("Package".to_owned(), inspected.hash.clone()));
    facts.push(file_fact(&inspected.path, Some(inspected.bytes)));
    let lead = Some(format!("{title} {version} {REVIEW_LEAD}"));
    let installed = inspected.installed.as_ref();
    let folder = installed.and_then(|side| side.origin) == Some(ModuleOrigin::Folder);
    let mut warning = None;
    let mut note = Some(APPLIES_AT_RELAUNCH.to_owned());
    let (sheet_title, confirm) = if folder {
        warning = Some(format!(
            "{title} is loaded from a folder{}. Remove it in Settings \u{203a} Modules before \
             installing a package of it.",
            installed
                .and_then(|side| side.path.as_ref())
                .map(|path| format!(" at {}", path.display()))
                .unwrap_or_default()
        ));
        note = None;
        (format!("Install {title} {version}?"), None)
    } else if inspected.already_installed {
        note = Some("This exact package is already installed.".to_owned());
        (format!("{title} {version} is installed"), None)
    } else if let Some(side) = installed {
        let from = side.version.as_deref().unwrap_or("the installed version");
        let destructive = inspected.format_change.is_some();
        warning = inspected
            .format_change
            .as_ref()
            .map(|change| change.notice.clone());
        (
            format!("Replace {title} {from} with {version}?"),
            Some(Confirm {
                label: if destructive {
                    "Replace anyway"
                } else {
                    "Replace"
                }
                .into(),
                destructive,
                enabled: !review.installing,
            }),
        )
    } else {
        (
            format!("Install {title} {version}?"),
            Some(Confirm {
                label: "Install".into(),
                destructive: false,
                enabled: !review.installing,
            }),
        )
    };
    if let Some(failed) = &review.failed {
        warning = Some(match warning {
            Some(warning) => format!("{warning}\n{failed}"),
            None => failed.clone(),
        });
    }
    let dismiss = if confirm.is_some() { "Cancel" } else { "Close" };
    ReviewModel {
        title: sheet_title,
        lead,
        facts,
        warning,
        note,
        confirm,
        dismiss,
        busy: review.installing.then_some("Installing\u{2026}"),
    }
}

/// The Uninstall… confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Uninstall {
    pub(crate) module: String,
    pub(crate) title: String,
    /// `module.uninstall` is in flight.
    pub(crate) writing: bool,
    /// Why it was refused; the confirmation stays open and says so.
    pub(crate) failed: Option<String>,
}

/// The confirmation as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UninstallModel {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) failed: Option<String>,
    pub(crate) busy: bool,
}

/// What uninstalling does to the photos that use `title`. It names no count: no catalog read
/// answers which photos hold a module's layers off the owner thread (a delegated choice the design
/// records), so the consequence is stated for every such photo.
pub(crate) fn uninstall_body(title: &str) -> String {
    format!(
        "Photos that use {title} keep their edits, but can't be previewed or exported with them \
         until {title} is installed again. Other catalogs aren't checked. {title} is removed when \
         Luxforge relaunches."
    )
}

pub(crate) fn derive_uninstall(uninstall: &Uninstall) -> UninstallModel {
    UninstallModel {
        title: format!("Uninstall {}?", uninstall.title),
        body: uninstall_body(&uninstall.title),
        failed: uninstall.failed.clone(),
        busy: uninstall.writing,
    }
}

/// One lifecycle write: exactly one owner method, answered with the module's new load state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackageWrite {
    Install(PathBuf),
    Uninstall(String),
    FolderLoad(PathBuf),
    FolderRemove(String),
}

impl PackageWrite {
    pub(crate) fn method(&self) -> &'static str {
        match self {
            Self::Install(_) => "module.install",
            Self::Uninstall(_) => "module.uninstall",
            Self::FolderLoad(_) => "module.folder.load",
            Self::FolderRemove(_) => "module.folder.remove",
        }
    }

    pub(crate) fn params(&self) -> Value {
        match self {
            Self::Install(path) | Self::FolderLoad(path) => serde_json::json!({ "path": path }),
            Self::Uninstall(module) | Self::FolderRemove(module) => {
                serde_json::json!({ "module": module })
            }
        }
    }

    /// What the status bar says once the owner answered `answer`.
    pub(crate) fn said(&self, answer: &Value) -> String {
        let row = answer
            .get("status")
            .cloned()
            .and_then(|status| serde_json::from_value::<PackageRow>(status).ok());
        let title = row
            .as_ref()
            .map(PackageRow::title)
            .or_else(|| answer["module"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let side = |key: &str| {
            serde_json::from_value::<Side>(answer[key].clone())
                .ok()
                .and_then(|side| side.version)
        };
        match self {
            Self::Install(_) => match answer["outcome"].as_str() {
                Some("unchanged") => format!("{title} is already installed"),
                Some("replaced") => format!(
                    "Replaced {title} with {} \u{b7} relaunch to apply",
                    side("installed").unwrap_or_default()
                ),
                _ => format!(
                    "Installed {title} {} \u{b7} relaunch to apply",
                    side("installed").unwrap_or_default()
                ),
            },
            Self::Uninstall(_) => format!("Uninstalled {title} \u{b7} relaunch to apply"),
            Self::FolderLoad(_) => match answer["outcome"].as_str() {
                Some("unchanged") => format!("{title} is already loaded from this folder"),
                _ => format!("Loaded {title} from its folder \u{b7} relaunch to apply"),
            },
            Self::FolderRemove(_) => format!("Removed {title} \u{b7} relaunch to apply"),
        }
    }
}

/// How a badge is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Neutral,
    Accent,
    Error,
}

/// One badge on a row or in the detail pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowBadge {
    pub(crate) label: &'static str,
    pub(crate) tone: Tone,
}

/// A row's badges: its origin, then its state.
pub(crate) fn row_badges(origin: ModuleOrigin, row: Option<&PackageRow>) -> Vec<RowBadge> {
    let mut badges: Vec<RowBadge> = origin_badge(origin)
        .map(|label| RowBadge {
            label,
            tone: Tone::Neutral,
        })
        .into_iter()
        .collect();
    badges.extend(row.and_then(state_badge));
    badges
}

/// The state badge of an external module's row: *Relaunch to apply* or *Didn't load*.
pub(crate) fn state_badge(row: &PackageRow) -> Option<RowBadge> {
    match row.state {
        LoadState::Loaded => None,
        LoadState::PendingRelaunch => Some(RowBadge {
            label: RELAUNCH_TO_APPLY,
            tone: Tone::Accent,
        }),
        LoadState::Failed => Some(RowBadge {
            label: DIDNT_LOAD,
            tone: Tone::Error,
        }),
    }
}

/// What the detail pane adds for an external module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExternalDetail {
    /// The module its actions name.
    pub(crate) module: String,
    pub(crate) state: Option<RowBadge>,
    /// The version this launch runs, and the one the next launch loads when it differs.
    pub(crate) version: Option<String>,
    pub(crate) program: String,
    /// A folder module's folder.
    pub(crate) path: Option<String>,
    /// What applies at the next launch.
    pub(crate) pending: Option<String>,
    /// The loader's reason, word for word, for a module that didn't load.
    pub(crate) reason: Option<String>,
    pub(crate) trust: &'static str,
    /// Uninstall… is offered: an installed package.
    pub(crate) uninstall: bool,
    /// Remove is offered: a folder module, in developer mode.
    pub(crate) remove: bool,
    /// Install module… is offered: a module that didn't load can be replaced by a version built
    /// for this Luxforge.
    pub(crate) install: bool,
}

/// The external detail of `row`.
pub(crate) fn external_detail(row: &PackageRow, developer: bool) -> ExternalDetail {
    let running = row.running.as_ref().and_then(|side| side.version.clone());
    let installed = row.installed.as_ref().and_then(|side| side.version.clone());
    let version = match (running, installed) {
        (Some(running), Some(installed)) if running != installed => {
            Some(format!("Version {running} \u{b7} {installed} at relaunch"))
        }
        (Some(version), _) | (None, Some(version)) => Some(format!("Version {version}")),
        (None, None) => None,
    };
    let path = row
        .installed
        .as_ref()
        .or(row.running.as_ref())
        .and_then(|side| side.path.as_ref())
        .filter(|_| row.origin == ModuleOrigin::Folder)
        .map(|path| path.display().to_string());
    let stored = row.installed.is_some();
    ExternalDetail {
        module: row.module.clone(),
        state: state_badge(row),
        version,
        program: row.program.words(),
        path,
        pending: row.pending(),
        reason: (row.state == LoadState::Failed)
            .then(|| row.reason.clone())
            .flatten(),
        trust: TRUST,
        uninstall: row.origin == ModuleOrigin::Community && stored,
        remove: row.origin == ModuleOrigin::Folder && stored && developer,
        install: row.state == LoadState::Failed,
    }
}

#[cfg(test)]
#[path = "external_modules_tests.rs"]
mod tests;
