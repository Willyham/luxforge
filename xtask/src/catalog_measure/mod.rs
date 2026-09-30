//! `cargo xtask catalog-measure`: the catalog's provisional performance targets
//! (`docs/design/catalog.md`, "Performance") measured in one run, into one report.
//!
//! ```text
//! cargo run --release --locked --package xtask -- catalog-measure --output NEW_DIR \
//!     [--samples N] [--scale tiny|full] [--binary PATH] [--raw-corpus DIR] [--card DIR]
//! ```
//!
//! The run makes its data under `<output>/scratch` ([`data`]), then takes each step's figures in
//! turn, each into the report ([`report`]) with the load at its start and end:
//!
//! 1. `browse-generated`: `browse.view` over the generated index's files and catalog's
//!    photographs, `browse.rows` of 200 over each, and what the owner grows by for the view.
//! 2. `bracket`: the preview bracket check per run, through the core's ignored bench.
//! 3. `first-browse`, `return` and `develop-picks` over the RAW trip: its first browse as an
//!    indexed folder (the first screen, every file, event and moment, every grid preview),
//!    returning to it, and developing RAW picks of it; skipped when the corpus is absent.
//!    `first-browse-card` does the first browse of the mounted card `--card` is on, as a card, and
//!    is skipped without one.
//! 4. `idle-watchers`: idle CPU with the index lane watching two indexed folders, in the core and
//!    in the editor, and the editor's own idle over a catalog with none beside them.
//! 5. `drag-baseline`, `first-index` (a first index of the tree with owner round trips sampled
//!    throughout), `drag-during-indexing` and `drag-during-preview-backlog`: a Basic drag through
//!    `editor-latency`, alone and under the catalog's background work.
//! 6. `desktop` and `develop-switch`: the desktop's frame-time probes ([`desktop`], lane B's) and
//!    the Develop switch ([`develop_switch`], lane D's), `not_measured` until they are built.
//!
//! Every step is one release editor or one in-process core; editor launches are background-only
//! through the scenario library. The run holds the host-wide timing lock, as every timing tool
//! does. `--samples` (default 30, the count a p95 needs) is each figure's sample count; a first
//! browse, which reads the whole trip and its previews into a new catalog each time, takes
//! `min(samples, 5)`. `--scale tiny` proves the harness in a few minutes and claims nothing.
mod client;
mod data;
pub mod desktop;
pub mod develop_switch;
mod measures;
mod report;
#[cfg(test)]
mod tests;

use crate::{fixtures, launch, *};
use desktop::DesktopContext;
use report::Report;

/// A figure's samples when `--samples` does not say: the count a p95 claim needs.
pub const DEFAULT_SAMPLES: usize = 30;
/// Where the RAW corpus is when `--raw-corpus` does not say.
pub const CORPUS_ENV: &str = "LUXFORGE_RAW_CORPUS_DIR";
/// The most first browses a run takes, each a whole trip read into a new catalog.
const MAX_JOURNEYS: usize = 5;

/// How much data the run makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// The design's scale.
    Full,
    /// A run that proves the harness end to end and claims nothing.
    Tiny,
}

impl Scale {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "full" => Ok(Self::Full),
            "tiny" => Ok(Self::Tiny),
            other => Err(format!("--scale is tiny or full, not {other}").into()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Tiny => "tiny",
        }
    }

    fn sizes(self) -> data::Sizes {
        match self {
            Self::Full => data::FULL,
            Self::Tiny => data::TINY,
        }
    }
}

/// What the command was given.
pub struct Options {
    pub samples: usize,
    pub scale: Scale,
    /// The release editor.
    pub binary: PathBuf,
    /// The RAW corpus the trip is copied from, if any.
    pub corpus: Option<PathBuf>,
    /// A folder on a mounted card to browse for the first time, if any. Only read.
    pub card: Option<PathBuf>,
}

/// The commit the run measured, and whether the tree had uncommitted changes.
fn git(root: &Path) -> Value {
    let sha = output(root, "git", &["rev-parse", "HEAD"])
        .ok()
        .map(|sha| sha.trim().to_owned());
    let dirty = output(
        root,
        "git",
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .ok()
    .map(|status| !status.trim().is_empty());
    json!({"sha": sha, "dirty": dirty})
}

/// The host: its model, processor, logical CPUs, memory and operating system, each null where
/// the platform cannot say.
fn host(root: &Path) -> Value {
    let read = |program: &str, args: &[&str]| {
        output(root, program, args)
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    };
    let macos = cfg!(target_os = "macos");
    let sysctl = |key: &str| macos.then(|| read("sysctl", &["-n", key])).flatten();
    json!({
        "platform": crate::host(root).ok(),
        "model": sysctl("hw.model"),
        "cpu": sysctl("machdep.cpu.brand_string"),
        "logical_cpus": std::thread::available_parallelism().ok().map(|count| count.get()),
        "memory_bytes": sysctl("hw.memsize").and_then(|bytes| bytes.parse::<u64>().ok()),
        "os": if macos {
            read("sw_vers", &["-productVersion"]).map(|version| format!("macOS {version}"))
        } else {
            Some(std::env::consts::OS.to_owned())
        },
        "os_build": macos.then(|| read("sw_vers", &["-buildVersion"])).flatten(),
    })
}

/// The profile this harness, and so the in-process core and the bench, was built with.
fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn header(root: &Path, options: &Options, journeys: usize) -> Value {
    json!({
        "tool": "catalog-measure",
        "command": std::env::args().collect::<Vec<_>>(),
        "git": git(root),
        "host": host(root),
        "profile": {
            "harness": profile(),
            "bench": profile(),
            "editor": null,
            "binary": options.binary,
            "binary_sha256": hash(&options.binary).ok(),
            "lockfile_sha256": hash(&root.join("Cargo.lock")).ok(),
        },
        "launch_mode": launch::MODE,
        "scale": options.scale.name(),
        "samples": options.samples,
        "journeys": journeys,
        "load_threshold": launch::LOAD_THRESHOLD,
        "method": "Core figures come from the catalog owner in this harness's own process, over scratch catalogs, through the JSON API's methods; editor figures from background launches of the release binary with hidden windows. Jobs are waited for on the owner's activity board, and a job's end is the board's own record of it. The OS file cache is never purged; each row says what was warm.",
    })
}

/// The editor's build profile, as its first launch reported it: the idle launch's provenance,
/// else the baseline drag's.
fn editor_profile(out: &Path) -> Value {
    [
        out.join("idle-editor-baseline/result.json"),
        out.join("drag-baseline/latency.json"),
    ]
    .iter()
    .filter_map(|path| read_json(path).ok())
    .map(|report| report["profile"].clone())
    .find(Value::is_string)
    .unwrap_or(Value::Null)
}

pub fn run(root: &Path, out: &Path, options: &Options) -> Result {
    ensure(!out.exists(), "catalog-measure output must be new")?;
    ensure(
        (1..=1000).contains(&options.samples),
        "Samples must be 1..1000",
    )?;
    ensure(
        options.binary.is_file(),
        format!(
            "No editor at {}: build it with `cargo xtask build --release`, or name one with --binary",
            options.binary.display()
        ),
    )?;
    if let Some(card) = &options.card {
        ensure(
            card.is_dir(),
            format!("--card {} is not a folder", card.display()),
        )?;
    }
    if cfg!(debug_assertions) {
        eprintln!(
            "catalog-measure: this harness is a debug build, so its in-process core and bench figures are too; `cargo run --release --locked --package xtask -- catalog-measure` takes figures"
        );
    }
    fs::create_dir_all(out)?;
    let sizes = options.scale.sizes();
    let journeys = options.samples.min(MAX_JOURNEYS);
    let mut report = Report::new(out, header(root, options, journeys));
    report.write()?;

    // Set-up, none of it timed: the drags' fixture, the bench built before anything measures, and
    // the data.
    println!("catalog-measure: set-up");
    let fixtures = root.join("fixtures/generated");
    if !fixtures::present(&fixtures) {
        fixtures::generate(&fixtures)?;
    }
    measures::build_bench(root, &out.join("bracket-bench-build.log"))?;
    let data = data::DataSet::prepare(&out.join("scratch"), sizes, options.corpus.as_deref())?;
    report.set("data", data.record.clone());
    report.write()?;
    let cx = measures::Context {
        root,
        out,
        binary: &options.binary,
        samples: options.samples,
        journeys,
        data: &data,
        fixture: fixtures.join("24mp.jpg"),
    };

    report.step(root, "browse-generated", || measures::browse_generated(&cx));
    report.step(root, "bracket", || measures::bracket(&cx));
    match &data.trip {
        Ok(trip) => {
            let mut kept = None;
            report.step(root, "first-browse", || {
                measures::first_browse(
                    &cx,
                    "first_browse",
                    measures::Browsed::Folder(&trip.dir),
                    &mut kept,
                )
            });
            report.step(root, "return", || {
                let core = kept.as_ref().ok_or("no first browse to return to")?;
                measures::returning(&cx, core, &trip.dir)
            });
            report.step(root, "develop-picks", || {
                let core = kept.as_ref().ok_or("no first browse to develop from")?;
                measures::develop(core, trip, sizes.picks)
            });
            if let Some(core) = kept
                && let Err(error) = core.close()
            {
                eprintln!("catalog-measure: the trip's catalog owner did not stop: {error}");
            }
        }
        Err(reason) => {
            report.step(root, "first-browse", || {
                Ok(measures::skipped(
                    "first_browse",
                    &measures::FIRST_BROWSE,
                    reason,
                ))
            });
            report.step(root, "return", || {
                Ok(measures::skipped("return", &measures::RETURN, reason))
            });
            report.step(root, "develop-picks", || {
                Ok(measures::skipped("develop", &measures::DEVELOP, reason))
            });
        }
    }
    match &options.card {
        Some(card) => {
            let mut kept = None;
            report.step(root, "first-browse-card", || {
                measures::first_browse(
                    &cx,
                    "first_browse_card",
                    measures::Browsed::Card(card),
                    &mut kept,
                )
            });
            if let Some(core) = kept {
                let _ = core.close();
            }
        }
        None => report.step(root, "first-browse-card", || {
            Ok(measures::skipped(
                "first_browse_card",
                &measures::FIRST_BROWSE,
                "no --card: a first browse from a card reader needs a trip on a mounted card",
            ))
        }),
    }
    report.step(root, "idle-watchers", || measures::idle_watchers(&cx));
    report.step(root, "drag-baseline", || measures::drag_baseline(&cx));
    report.step(root, "first-index", || measures::first_index(&cx));
    report.step(root, "drag-during-indexing", || {
        measures::drag_during_indexing(&cx)
    });
    report.step(root, "drag-during-preview-backlog", || {
        measures::drag_during_preview_backlog(&cx)
    });
    let desktop = DesktopContext {
        binary: options.binary.clone(),
        scratch: data.scratch.join("desktop"),
        samples: options.samples,
        folder_10k: data.folder.dir.clone(),
        raw_trip: data.trip.as_ref().ok().map(|trip| trip.dir.clone()),
    };
    report.step(root, "desktop", || desktop::desktop_probes(&desktop));
    report.step(root, "develop-switch", || {
        develop_switch::develop_switch(&desktop)
    });

    let unchanged = data.corpus_unchanged()?;
    report.set("corpus_unchanged", json!(unchanged));
    let editor = editor_profile(out);
    let mut profiles = report.get("profile").clone();
    profiles["editor"] = editor;
    report.set("profile", profiles);
    let finished = report.finish()?;
    println!(
        "catalog-measure: {} ({} rows); see {} and {}",
        finished["status"].as_str().unwrap_or_default(),
        stats::rows(&finished).len(),
        out.join(report::JSON).display(),
        out.join(report::MARKDOWN).display()
    );
    ensure(unchanged, "A corpus file changed during the run")?;
    ensure(
        finished["status"] != "failed",
        "A catalog-measure step failed; its row names why",
    )
}
