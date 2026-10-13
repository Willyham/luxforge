//! Launching past an unsupported catalog ([design](../../../../docs/design/auto-update.md#launching-past-an-unsupported-catalog)):
//! which refusals the desktop offers a new catalog for, where that catalog goes, and what the
//! sheet says. Everything here is decided from the launch and the core's refusal; nothing reads or
//! writes the refused catalog.
use super::preferences::LaunchCatalog;
use luxforge_core::{CATALOG_FORMAT, Error, ErrorKind};
use std::path::{Path, PathBuf};

/// Where earlier versions of Luxforge are published, opened through the checked HTTPS opener.
pub(crate) const RELEASES_URL: &str = "https://github.com/Willyham/luxforge/releases";

/// The sheet's title.
pub(crate) const TITLE: &str = "This catalog was made by a different version of Luxforge";
/// What the sheet promises about the refused catalog.
pub(crate) const UNCHANGED: &str = "The catalog and its originals are unchanged. The version of \
     Luxforge that made it can still open it.";
pub(crate) const RELEASES_NOTE: &str = "Earlier versions are on the releases page.";
pub(crate) const RELEASES_LABEL: &str = "Open releases page";
pub(crate) const START_LABEL: &str = "Start a new catalog";
pub(crate) const STARTING_LABEL: &str = "Starting…";
pub(crate) const QUIT_LABEL: &str = "Quit";

/// The most numbered names tried beside the refused catalog before Start a new catalog gives up.
const MAX_NAMES: u32 = 999;

/// The catalog a launch could not open, as the core refused it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    /// The refused catalog file.
    pub(crate) path: PathBuf,
    /// The format marker it carries, when the refusal names one.
    pub(crate) format: Option<i64>,
    /// The catalog format this version opens.
    pub(crate) expected: i64,
    /// Whether it carries the catalog's application mark.
    pub(crate) marked: bool,
    /// The core's refusal, as it would have been printed.
    pub(crate) detail: String,
}

impl Refusal {
    /// The refusal the desktop offers a new catalog for: an `incompatible` refusal of the catalog
    /// the launch chose itself, the remembered or the default one. `--catalog` and an evidence
    /// run's own catalog keep the command line's refusal, and every other failure, another
    /// instance owning the catalog among them, keeps today's message.
    pub(crate) fn offered(launch: &LaunchCatalog, error: &Error) -> Option<Self> {
        if error.kind != ErrorKind::Incompatible || launch.forced.is_some() {
            return None;
        }
        Some(Self {
            path: launch.path.clone(),
            format: error.refused_catalog_format(),
            expected: error.expected_catalog_format().unwrap_or(CATALOG_FORMAT),
            marked: error.refused_catalog_marked(),
            detail: error.detail.clone(),
        })
    }

    /// What the sheet says of the two formats.
    pub(crate) fn formats(&self) -> String {
        let version = luxforge_cli::VERSION;
        match self.format {
            Some(format) if self.marked => format!(
                "It is catalog format {format}. Luxforge {version} opens catalog format {}.",
                self.expected
            ),
            Some(_) => format!(
                "It is not marked as a Luxforge catalog of format {}, the format Luxforge \
                 {version} opens.",
                self.expected
            ),
            None => format!(
                "Luxforge {version} opens catalog format {} and refused it: {}.",
                self.expected, self.detail
            ),
        }
    }
}

/// Where Start a new catalog creates its catalog: beside the refused one, named after it and the
/// format it is made in — `<stem> (format N).sqlite`, then `<stem> (format N) 2.sqlite` and so on —
/// with a refused catalog's own format suffix replaced rather than repeated. A name is taken only
/// when neither it nor anything a catalog keeps beside itself (its log, its artifacts, its index,
/// its live session) exists, so the new catalog shares nothing with the refused one or any other.
/// `None` when every name is taken.
pub(crate) fn new_catalog_path(
    refused: &Path,
    format: i64,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let folder = refused.parent().unwrap_or(Path::new(""));
    let stem = refused
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "catalog".into());
    let base = without_format_suffix(&stem);
    let extension = refused
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sqlite".into());
    (1..=MAX_NAMES).find_map(|number| {
        let stem = match number {
            1 => format!("{base} (format {format})"),
            number => format!("{base} (format {format}) {number}"),
        };
        let path = folder.join(format!("{stem}.{extension}"));
        let taken = [
            path.clone(),
            suffixed(&path, "-wal"),
            suffixed(&path, "-shm"),
            suffixed(&path, "-journal"),
            folder.join(format!("{stem}.artifacts")),
            folder.join(format!("{stem}.index")),
            luxforge_core::live_session_file(&path),
        ]
        .iter()
        .any(|sibling| exists(sibling));
        (!taken).then_some(path)
    })
}

/// `stem` without a trailing ` (format N)` or ` (format N) M` that an earlier new catalog was
/// named with.
fn without_format_suffix(stem: &str) -> &str {
    let Some(start) = stem.rfind(" (format ") else {
        return stem;
    };
    let rest = &stem[start + " (format ".len()..];
    let Some(close) = rest.find(')') else {
        return stem;
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let after = &rest[close + 1..];
    let numbered = after.is_empty() || after.strip_prefix(' ').is_some_and(digits);
    if digits(&rest[..close]) && numbered && start > 0 {
        &stem[..start]
    } else {
        stem
    }
}

/// `<file name> in <folder>`, as the sheet names the refused catalog.
fn named(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(folder) => format!("{name} in {}", folder.display()),
        None => name,
    }
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The sheet's two choices, as an evidence run names the one it takes once the sheet is captured
/// (`--evidence-unsupported-catalog start|quit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Start,
    Quit,
}

impl Choice {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "start" => Some(Self::Start),
            "quit" => Some(Self::Quit),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Quit => "quit",
        }
    }
}

/// Where the sheet is in its one choice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Both choices are offered.
    #[default]
    Offered,
    /// Start a new catalog is creating and opening the new catalog.
    Starting,
    /// Start a new catalog failed, for this reason; both choices are offered again.
    Failed(String),
}

/// What the sheet shows, derived from the refusal, the name a new catalog would take and the
/// phase. The view reads this alone.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SheetModel {
    pub(crate) title: &'static str,
    /// The refused catalog: its file name, then its folder, so a long folder is the part cut short.
    pub(crate) catalog: String,
    pub(crate) formats: String,
    pub(crate) unchanged: &'static str,
    pub(crate) releases: &'static str,
    pub(crate) link: &'static str,
    /// What Start a new catalog does, or why it cannot.
    pub(crate) new_catalog: String,
    pub(crate) start: &'static str,
    pub(crate) start_enabled: bool,
    pub(crate) quit_enabled: bool,
    /// Why the last Start a new catalog or the releases page failed.
    pub(crate) problem: Option<String>,
}

impl SheetModel {
    pub(crate) fn new(
        refusal: &Refusal,
        next: Option<&Path>,
        phase: &Phase,
        link_problem: Option<&str>,
    ) -> Self {
        let starting = *phase == Phase::Starting;
        let new_catalog = match next {
            Some(next) => format!(
                "Start a new catalog creates {} in the same folder and opens it from now on.",
                next.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            None => "Every name for a new catalog beside it is taken; choose a catalog with \
                     --catalog."
                .into(),
        };
        let problem = match phase {
            Phase::Failed(reason) => Some(format!("Could not start a new catalog: {reason}")),
            _ => link_problem.map(str::to_owned),
        };
        Self {
            title: TITLE,
            catalog: named(&refusal.path),
            formats: refusal.formats(),
            unchanged: UNCHANGED,
            releases: RELEASES_NOTE,
            link: RELEASES_LABEL,
            new_catalog,
            start: if starting {
                STARTING_LABEL
            } else {
                START_LABEL
            },
            start_enabled: next.is_some() && !starting,
            quit_enabled: !starting,
            problem,
        }
    }

    /// The sheet as an evidence frame's state records it.
    pub(crate) fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "title": self.title,
            "catalog": self.catalog,
            "formats": self.formats,
            "unchanged": self.unchanged,
            "releases": self.releases,
            "link": self.link,
            "new_catalog": self.new_catalog,
            "start": {"label": self.start, "enabled": self.start_enabled},
            "quit": {"label": QUIT_LABEL, "enabled": self.quit_enabled},
            "problem": self.problem,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::preferences::CatalogOverride;
    use std::collections::HashSet;

    fn launch(forced: Option<CatalogOverride>) -> LaunchCatalog {
        LaunchCatalog {
            path: PathBuf::from("/Photos/catalog.sqlite"),
            default: Some(PathBuf::from("/config/catalog.sqlite")),
            forced,
            missing: None,
        }
    }

    /// Only an incompatible refusal of the catalog the launch chose itself is offered a new
    /// catalog; `--catalog`, an evidence run's own catalog, another owner and any other failure
    /// keep today's refusal.
    #[test]
    fn unsupported_catalog_only_a_chosen_catalogs_incompatible_refusal_is_offered() {
        let refused = Error::unsupported_catalog(16, 1);
        let offered = Refusal::offered(&launch(None), &refused).expect("offered");
        assert_eq!(
            offered,
            Refusal {
                path: PathBuf::from("/Photos/catalog.sqlite"),
                format: Some(16),
                expected: 1,
                marked: true,
                detail: refused.detail.clone(),
            }
        );
        for forced in [CatalogOverride::CommandLine, CatalogOverride::Evidence] {
            assert_eq!(Refusal::offered(&launch(Some(forced)), &refused), None);
        }
        for other in [
            Error::new(ErrorKind::Conflict, "database is locked"),
            Error::new(ErrorKind::Catalog, "cannot create catalog directory"),
        ] {
            assert_eq!(Refusal::offered(&launch(None), &other), None);
        }
        let unmarked = Refusal::offered(&launch(None), &Error::unmarked_catalog(1, 1)).unwrap();
        assert_eq!((unmarked.format, unmarked.marked), (Some(1), false));
        // An incompatible refusal that names no marker is still offered, with its reason.
        let unnamed = Refusal::offered(
            &launch(None),
            &Error::incompatible("catalog has no identity; choose a new catalog path"),
        )
        .unwrap();
        assert_eq!((unnamed.format, unnamed.expected), (None, CATALOG_FORMAT));
        assert!(unnamed.formats().contains("catalog has no identity"));
    }

    /// The sheet names both formats, says the catalog is unchanged and the version that made it
    /// can still open it, names the new catalog, and offers both choices until Start runs.
    #[test]
    fn unsupported_catalog_the_sheet_names_both_formats_and_the_new_catalog() {
        let refusal = Refusal::offered(&launch(None), &Error::unsupported_catalog(16, 1)).unwrap();
        let next = PathBuf::from("/Photos/catalog (format 1).sqlite");
        let model = SheetModel::new(&refusal, Some(&next), &Phase::Offered, None);
        assert_eq!(model.title, TITLE);
        assert_eq!(model.catalog, "catalog.sqlite in /Photos");
        assert_eq!(
            model.formats,
            format!(
                "It is catalog format 16. Luxforge {} opens catalog format 1.",
                luxforge_cli::VERSION
            )
        );
        assert_eq!(model.unchanged, UNCHANGED);
        assert_eq!(
            model.new_catalog,
            "Start a new catalog creates catalog (format 1).sqlite in the same folder and opens it \
             from now on."
        );
        assert!(model.start_enabled && model.quit_enabled && model.problem.is_none());

        let starting = SheetModel::new(&refusal, Some(&next), &Phase::Starting, None);
        assert_eq!(starting.start, STARTING_LABEL);
        assert!(!starting.start_enabled && !starting.quit_enabled);

        let failed = SheetModel::new(
            &refusal,
            Some(&next),
            &Phase::Failed("read-only folder".into()),
            None,
        );
        assert!(failed.start_enabled && failed.quit_enabled);
        assert_eq!(
            failed.problem.as_deref(),
            Some("Could not start a new catalog: read-only folder")
        );
        let full = SheetModel::new(&refusal, None, &Phase::Offered, None);
        assert!(!full.start_enabled && full.quit_enabled);
    }

    /// The new catalog is named after the refused one and its format, numbered past any name a
    /// catalog or anything a catalog keeps beside itself already has, with an earlier format
    /// suffix replaced rather than repeated.
    #[test]
    fn unsupported_catalog_the_new_catalog_is_named_beside_the_refused_one_without_collisions() {
        let refused = Path::new("/Photos/catalog.sqlite");
        let none = |_: &Path| false;
        assert_eq!(
            new_catalog_path(refused, 1, none),
            Some(PathBuf::from("/Photos/catalog (format 1).sqlite"))
        );
        // Each sibling a catalog keeps takes its name.
        for taken in [
            "/Photos/catalog (format 1).sqlite",
            "/Photos/catalog (format 1).sqlite-wal",
            "/Photos/catalog (format 1).artifacts",
            "/Photos/catalog (format 1).index",
            "/Photos/catalog (format 1).live-session.json",
        ] {
            let taken: HashSet<PathBuf> = [PathBuf::from(taken)].into();
            assert_eq!(
                new_catalog_path(refused, 1, |path| taken.contains(path)),
                Some(PathBuf::from("/Photos/catalog (format 1) 2.sqlite")),
                "{taken:?}"
            );
        }
        let taken: HashSet<PathBuf> = [
            PathBuf::from("/Photos/catalog (format 1).sqlite"),
            PathBuf::from("/Photos/catalog (format 1) 2.index"),
        ]
        .into();
        assert_eq!(
            new_catalog_path(refused, 1, |path| taken.contains(path)),
            Some(PathBuf::from("/Photos/catalog (format 1) 3.sqlite"))
        );
        // A catalog an earlier new catalog was named for keeps its base name.
        for earlier in [
            "/Photos/catalog (format 1).sqlite",
            "/Photos/catalog (format 1) 4.sqlite",
        ] {
            assert_eq!(
                new_catalog_path(Path::new(earlier), 2, none),
                Some(PathBuf::from("/Photos/catalog (format 2).sqlite"))
            );
        }
        // A name that only looks like one is kept whole, and a file without an extension gets
        // the catalog's.
        assert_eq!(
            new_catalog_path(Path::new("/Photos/trip (format x).db"), 1, none),
            Some(PathBuf::from("/Photos/trip (format x) (format 1).db"))
        );
        assert_eq!(
            new_catalog_path(Path::new("/Photos/library"), 1, none),
            Some(PathBuf::from("/Photos/library (format 1).sqlite"))
        );
        assert_eq!(new_catalog_path(refused, 1, |_| true), None);
    }
}
