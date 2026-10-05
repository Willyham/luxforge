//! `gpu-qualification`: the release gate of the GPU-first renderer
//! ([design](../../docs/design/gpu-first.md#the-contract)). It renders every stack of the
//! qualification corpus on the reference renderer and on the GPU, at every view the corpus lists,
//! and holds every cell of every output kind the GPU renders to that kind's recorded limit
//! (`luxforge_reference::tolerance`, the preview error limits of `luxforge_reference::preview_error`).
//!
//! The rendering is the desktop crate's release-built harness, an ignored test of its binary that
//! needs the photo surface's qualification readback
//! (`crates/luxforge-app/src/app/gpu_qualification/reference.rs`); this command builds it, runs it
//! with the environment it reads, and judges what it wrote: `report.json`, every figure and verdict,
//! and `summary.md`, the tables a person reads. It launches no editor.
//!
//! The picture is two kinds. **At rest**, the editor's GPU picture of the stack at rest — its
//! tiles at full resolution reduced to the view at Fit, 33% and 50%, its view plan over the visible
//! window at 100% — is gated against the reference, as the owner decided. **In motion**, the frame a
//! drag draws over the boundary the surface derives from the source is reported against the
//! picture at rest it settles to and against the reference, with the same statistics and limits,
//! but not gated: what a drag's frame is held to is an open owner question (2026-10-05,
//! [design](../../docs/design/gpu-first.md#proposals-with-recorded-defaults)).
//! `--gate-motion against-rest` or `--gate-motion against-reference` gates it on demand, so the
//! owner's own run shows its misses as failures.
//!
//! Export is judged over the whole output stage: the GPU's export of each stack, streamed in tiles
//! by the desktop's GPU tile worker as the export lane streams it, against the reference export by
//! the display limit of the stack's class, and a second GPU export, on a second device of the same
//! adapter, the same bytes. A stack the GPU cannot export is a gap naming why.
//!
//! A kind the GPU does not render yet is reported as such, never as a pass and never as a failure;
//! nor is the picture in motion unless it is gated.
//! The gate fails (an ordinary failure) when any cell it judges passes a limit, when the harness did
//! not run to its end, or when a source's SHA-256 changed during the run; it is incomplete (exit 3)
//! when any selected cell it judges could not be run — a source missing on this host, a stack the
//! GPU cannot draw there yet, an error — when there is no GPU adapter, or when the run measured a
//! selection narrower than the whole corpus. Only a run of everything, every judged cell within its
//! limit, passes.
use crate::{
    preview_error::corpus::{self, Corpus, VIEWS},
    *,
};
use luxforge_reference::{
    preview_error::{Class, Limits, Statistics, verdict},
    tolerance::{self, Kind},
};
use std::process::Stdio;

/// The harness: the ignored test of the desktop crate's binary that renders the corpus.
const HARNESS: &str = "app::gpu_qualification::reference::gpu_qualification_against_reference";

/// The picture's statistics, in the order every table lists them.
const STATISTICS: [&str; 4] = ["mean", "worst_block", "p99", "mean_delta_l"];

/// What the picture in motion is gated against, when the owner asks for it to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateMotion {
    /// The GPU's picture at rest the drag's frame settles to.
    AgainstRest,
    /// The reference frame at the view's size.
    AgainstReference,
}

impl GateMotion {
    fn name(self) -> &'static str {
        match self {
            Self::AgainstRest => "against-rest",
            Self::AgainstReference => "against-reference",
        }
    }
}

/// What the picture in motion is, until the owner decides what a drag's frame is held to.
const MOTION_NOT_GATED: &str = "not gated: an open owner question (2026-10-05)";

/// What one run measures.
pub struct Options {
    pub output: PathBuf,
    pub manifest: Option<PathBuf>,
    /// The generated JPEGs.
    pub fixtures: PathBuf,
    /// The selection, each `None` for everything: view ids, kinds, families, recipes and sources.
    pub views: Option<Vec<String>>,
    pub kinds: Option<Vec<Kind>>,
    pub families: Option<Vec<String>>,
    pub recipes: Option<Vec<String>>,
    pub sources: Option<Vec<String>>,
    /// Write every measured cell's frames, not only those past a limit.
    pub all_frames: bool,
    /// Gate the picture in motion, against the picture at rest or the reference; `None` reports it
    /// ungated.
    pub gate_motion: Option<GateMotion>,
}

/// A comma-separated value of `key`, `None` when it is absent or `all`.
fn selection(a: &mut Args, key: &str) -> Result<Option<Vec<String>>> {
    Ok(a.value(key)?.and_then(|value| {
        let items: Vec<String> = value
            .to_string_lossy()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect();
        (!items.iter().any(|item| item == "all")).then_some(items)
    }))
}

impl Options {
    /// `--output NEW_DIR [--manifest FILE] [--fixtures DIR] [--zoom fit|33|50|100|all]
    /// [--kind picture-at-rest|picture-in-motion|histogram|sample|export|all] [--families F,...]
    /// [--recipes ID,...] [--sources ID,...] [--frames missed|all]
    /// [--gate-motion against-rest|against-reference]`;
    /// `--zoom` and `--kind` take a comma-separated list too.
    pub fn parse(root: &Path, a: &mut Args) -> Result<Self> {
        let output = absolute(root, &a.path("--output")?);
        let manifest = a
            .value("--manifest")?
            .map(|path| absolute(root, Path::new(&path)));
        let fixtures = a.value("--fixtures")?.map_or_else(
            || root.join(corpus::GENERATED),
            |path| absolute(root, Path::new(&path)),
        );
        let views = selection(a, "--zoom")?
            .map(|zooms| {
                zooms
                    .iter()
                    .map(|zoom| {
                        let id = if zoom == "fit" {
                            zoom.clone()
                        } else {
                            format!("{}%", zoom.trim_end_matches('%'))
                        };
                        if VIEWS.contains(&id.as_str()) {
                            Ok(id)
                        } else {
                            Err(format!("--zoom is fit, 33, 50, 100 or all, not {zoom}"))
                        }
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()
            })
            .transpose()?;
        let kinds = selection(a, "--kind")?
            .map(|names| {
                names
                    .iter()
                    .map(|name| {
                        Kind::parse(name).ok_or_else(|| {
                            format!(
                                "--kind is picture-at-rest, picture-in-motion, histogram, sample, \
                                 export or all, not {name}"
                            )
                        })
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()
            })
            .transpose()?;
        let families = selection(a, "--families")?;
        let recipes = selection(a, "--recipes")?;
        let sources = selection(a, "--sources")?;
        let all_frames = match a.value("--frames")?.as_deref().and_then(OsStr::to_str) {
            None | Some("missed") => false,
            Some("all") => true,
            Some(other) => return Err(format!("--frames is missed or all, not {other}").into()),
        };
        let gate_motion = match a.value("--gate-motion")?.as_deref().and_then(OsStr::to_str) {
            None => None,
            Some("against-rest") => Some(GateMotion::AgainstRest),
            Some("against-reference") => Some(GateMotion::AgainstReference),
            Some(other) => {
                return Err(format!(
                    "--gate-motion is against-rest or against-reference, not {other}"
                )
                .into());
            }
        };
        Ok(Self {
            output,
            manifest,
            fixtures,
            views,
            kinds,
            families,
            recipes,
            sources,
            all_frames,
            gate_motion,
        })
    }

    /// What the selection leaves out of the whole corpus, in words, or nothing for a whole run.
    fn narrowed(&self) -> Vec<String> {
        let mut narrowed = Vec::new();
        for (name, list) in [
            ("views", &self.views),
            ("families", &self.families),
            ("recipes", &self.recipes),
            ("sources", &self.sources),
        ] {
            if let Some(list) = list {
                narrowed.push(format!("{name} {}", list.join(", ")));
            }
        }
        if let Some(kinds) = &self.kinds {
            let names: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
            narrowed.push(format!("kinds {}", names.join(", ")));
        }
        narrowed
    }
}

/// Build the harness in release, run it over the corpus and judge it. See the module's docs for
/// the outcomes.
pub fn run(root: &Path, options: &Options) -> Result {
    let out = &options.output;
    ensure(
        !out.exists(),
        format!("{} exists: use a new directory", out.display()),
    )?;
    let corpus = Corpus::load(root)?;
    check_selection(&corpus, options)?;
    fs::create_dir_all(out)?;
    let before = corpus.resolve(&options.fixtures, options.manifest.as_deref())?;
    write_json(&out.join("sources-before.json"), &before)?;

    let log = out.join("harness.log");
    let harness = build(root, &log)?;
    // What the harness was built from, read as it is built: the tree may change while it runs.
    let built = json!({
        "revision": output(root, "git", &["rev-parse", "HEAD"]).map(|text| text.trim().to_owned()).ok(),
        "working_tree_dirty": output(root, "git", &["status", "--porcelain"]).map(|text| !text.trim().is_empty()).ok(),
        "target": host(root).ok(),
        "profile": "release",
        "harness_binary": harness.executable,
        "harness_binary_sha256": hash(&harness.executable)?,
        "lock_sha256": hash(&root.join("Cargo.lock"))?,
    });
    let run_dir = out.join("run");
    let mut command = Command::new(&harness.executable);
    command
        .current_dir(&harness.dir)
        .env("CARGO_MANIFEST_DIR", &harness.dir)
        .env("LUXFORGE_GPU_CORPUS_OUTPUT", &run_dir)
        .env("LUXFORGE_GENERATED_FIXTURES", &options.fixtures)
        .args(["--ignored", "--exact", HARNESS, "--nocapture"])
        .stdin(Stdio::null());
    if let Some(manifest) = &options.manifest {
        command.env("LUXFORGE_RAW_MANIFEST", manifest);
    }
    for (key, list) in [
        ("LUXFORGE_GPU_QUALIFICATION_VIEWS", &options.views),
        ("LUXFORGE_GPU_CORPUS_FAMILIES", &options.families),
        ("LUXFORGE_GPU_CORPUS_RECIPES", &options.recipes),
        ("LUXFORGE_GPU_CORPUS_SOURCES", &options.sources),
    ] {
        if let Some(list) = list {
            command.env(key, list.join(","));
        }
    }
    if let Some(kinds) = &options.kinds {
        let names: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
        command.env("LUXFORGE_GPU_QUALIFICATION_KINDS", names.join(","));
    }
    if options.all_frames {
        command.env("LUXFORGE_GPU_QUALIFICATION_FRAMES", "all");
    }
    if let Some(gate) = options.gate_motion {
        command.env(
            "LUXFORGE_GPU_QUALIFICATION_MOTION",
            match gate {
                GateMotion::AgainstRest => "rest",
                GateMotion::AgainstReference => "reference",
            },
        );
    }
    let file = fs::OpenOptions::new().append(true).open(&log)?;
    let status = command.stdout(file.try_clone()?).stderr(file).status()?;

    let after = corpus.resolve(&options.fixtures, options.manifest.as_deref());
    let cells = fs::read(run_dir.join("cells.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let expected = expected_pairs(&corpus, options);
    let mut report = judge(cells.as_ref(), expected, options);
    report["command"] = json!({
        "harness": HARNESS,
        "exit_code": status.code(),
        "log": "harness.log",
        "cells": "run/cells.json",
    });
    report["build"] = built;
    report["date"] = json!(time::OffsetDateTime::now_utc().date().to_string());
    report["corpus"] = json!({
        "file": corpus::FILE,
        "sha256": hash(&root.join(corpus::FILE))?,
        "views": VIEWS,
        "fixtures": options.fixtures,
        "manifest": options.manifest,
        "sources": before["entries"],
    });
    // A source changed or vanished while the harness read it would be a defect of the harness,
    // which only ever reads originals.
    let unchanged = match &after {
        Ok(after) => after["entries"] == before["entries"],
        Err(_) => false,
    };
    report["sources_unchanged"] = json!(unchanged);
    if let Ok(after) = &after {
        write_json(&out.join("sources-after.json"), after)?;
    }
    if !unchanged {
        let why = match &after {
            Ok(_) => "a source's status changed during the run".to_owned(),
            Err(error) => format!("a source changed during the run: {error}"),
        };
        failed(&mut report, why);
    }
    if cells.is_some() && !status.success() && report["status"] != "failed" {
        failed(
            &mut report,
            format!("the harness exited with {status}; see harness.log"),
        );
    }
    write_json(&out.join("report.json"), &report)?;
    let summary = markdown(&report);
    fs::write(out.join("summary.md"), &summary)?;
    print!("{summary}");
    let error = report["error"].as_str().unwrap_or_default().to_owned();
    match report["status"].as_str() {
        Some("passed") => Ok(()),
        Some("incomplete") => Err(Box::new(verify::Incomplete(format!(
            "GPU qualification incomplete: {error}. Report: {}",
            out.join("summary.md").display()
        )))),
        _ => Err(format!(
            "GPU qualification failed: {error}. Report: {}",
            out.join("summary.md").display()
        )
        .into()),
    }
}

/// Refuse a selection that names what the corpus does not hold, before anything is built.
fn check_selection(corpus: &Corpus, options: &Options) -> Result {
    for (what, wanted, known) in [
        (
            "family",
            &options.families,
            corpus.families().collect::<Vec<_>>(),
        ),
        (
            "recipe",
            &options.recipes,
            corpus.recipe_ids().collect::<Vec<_>>(),
        ),
        (
            "source",
            &options.sources,
            corpus.source_ids().collect::<Vec<_>>(),
        ),
    ] {
        for name in wanted.iter().flatten() {
            ensure(
                known.contains(&name.as_str()),
                format!("The corpus holds no {what} {name}"),
            )?;
        }
    }
    ensure(
        expected_pairs(corpus, options) > 0,
        "The selection holds no stack of the corpus",
    )
}

/// How many stacks (recipe and source) the selection holds.
fn expected_pairs(corpus: &Corpus, options: &Options) -> usize {
    corpus
        .pairs()
        .filter(|(recipe, family, source)| {
            options
                .families
                .as_ref()
                .is_none_or(|families| families.iter().any(|f| f == family))
                && options
                    .recipes
                    .as_ref()
                    .is_none_or(|recipes| recipes.iter().any(|r| r == recipe))
                && options
                    .sources
                    .as_ref()
                    .is_none_or(|sources| sources.iter().any(|s| s == source))
        })
        .count()
}

/// The harness's test binary, and the package directory Cargo runs it in.
struct Harness {
    executable: PathBuf,
    dir: PathBuf,
}

/// The desktop crate's binary's tests, built in release with its dev-dependencies' qualification
/// features, the compiler's diagnostics in `log`.
fn build(root: &Path, log: &Path) -> Result<Harness> {
    let file = fs::File::create(log)?;
    let out = cargo_command()
        .current_dir(root)
        .args([
            "test",
            "--release",
            "--locked",
            "--package",
            "luxforge-app",
            "--bin",
            "luxforge",
            "--no-run",
            "--message-format=json-render-diagnostics",
        ])
        .stdin(Stdio::null())
        .stderr(file)
        .output()?;
    ensure(
        out.status.success(),
        format!("Building the harness failed; see {}", log.display()),
    )?;
    for line in String::from_utf8(out.stdout)?.lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" || message["profile"]["test"] != true {
            continue;
        }
        if let (Some(executable), Some(manifest)) = (
            message["executable"].as_str(),
            message["manifest_path"].as_str(),
        ) {
            let dir = Path::new(manifest)
                .parent()
                .ok_or("A manifest path without a directory")?;
            return Ok(Harness {
                executable: executable.into(),
                dir: dir.to_path_buf(),
            });
        }
    }
    Err("Cargo built no harness binary".into())
}

/// `report` marked failed for `why`, unless it already names an earlier failure.
fn failed(report: &mut Value, why: String) {
    if report["status"] != "failed" {
        report["error"] = json!(why);
    }
    report["status"] = json!("failed");
}

/// A recorded statistics object as the measure's [`Statistics`]; a figure that is missing or not a
/// number (a NaN is written as `null`) reads as NaN, which no limit admits.
fn statistics(value: &Value) -> Statistics {
    let figure = |key: &str| value[key].as_f64().unwrap_or(f64::NAN);
    Statistics {
        pixels: value["pixels"].as_u64().unwrap_or(0),
        mean: figure("mean"),
        worst_block: figure("worst_block"),
        worst_block_origin: [0, 0],
        p99: figure("p99"),
        mean_delta_l: figure("mean_delta_l"),
        max: figure("max"),
    }
}

/// The four judged figures of `s`, in [`STATISTICS`]' order.
fn figures(s: &Statistics) -> [f64; 4] {
    [s.mean, s.worst_block, s.p99, s.mean_delta_l]
}

/// The limits of `class`, in [`STATISTICS`]' order, ΔL\* as the bound on its magnitude.
fn limits(class: Class) -> [f64; 4] {
    let Limits {
        mean,
        worst_block,
        p99,
        mean_delta_l,
    } = class.limits();
    [mean, worst_block, p99, mean_delta_l]
}

/// The statistics of `s` past `class`'s limits, by name.
fn exceeded(s: &Statistics, class: Class) -> Vec<&'static str> {
    let verdict = verdict(s, class);
    [
        verdict.mean,
        verdict.worst_block,
        verdict.p99,
        verdict.mean_delta_l,
    ]
    .iter()
    .zip(STATISTICS)
    .filter(|(within, _)| !**within)
    .map(|(_, name)| name)
    .collect()
}

/// The largest magnitude of each statistic over some cells, with the cell that holds it, and their
/// mean (the signed ΔL\* averaged with its sign).
#[derive(Default)]
struct Extremes {
    count: usize,
    largest: [Option<(f64, String)>; 4],
    sum: [f64; 4],
}

impl Extremes {
    fn add(&mut self, s: &Statistics, cell: &str) {
        self.count += 1;
        for (index, value) in figures(s).into_iter().enumerate() {
            self.sum[index] += value;
            // A figure that is not a number is the worst there is, and stays so.
            let replace = match &self.largest[index] {
                None => true,
                Some((largest, _)) if largest.is_nan() => false,
                Some((largest, _)) => value.is_nan() || value.abs() > largest.abs(),
            };
            if replace {
                self.largest[index] = Some((value, cell.to_owned()));
            }
        }
    }

    fn value(&self) -> Value {
        if self.count == 0 {
            return Value::Null;
        }
        let mut max = serde_json::Map::new();
        let mut mean = serde_json::Map::new();
        let mut at = serde_json::Map::new();
        for (index, name) in STATISTICS.iter().enumerate() {
            if let Some((value, cell)) = &self.largest[index] {
                max.insert((*name).to_owned(), json!(value));
                at.insert((*name).to_owned(), json!(cell));
            }
            mean.insert(
                (*name).to_owned(),
                json!(self.sum[index] / self.count as f64),
            );
        }
        json!({"cells": self.count, "max": max, "mean": mean, "max_at": at})
    }
}

/// Where the reading the gate judges by is recorded.
const DEFAULT: &str = "docs/design/gpu-first.md, Proposals with recorded defaults, the default recorded on 2026-10-05";

/// One comparison over a view's cells of a class: how many are within the limits and past them,
/// and the figures.
#[derive(Default)]
struct Judged {
    within: usize,
    past: usize,
    figures: Extremes,
}

impl Judged {
    /// `s`, `cell`'s figures, held to `class`'s limits: the statistics it passes.
    fn add(&mut self, s: &Statistics, cell: &str, class: Class) -> Vec<&'static str> {
        self.figures.add(s, cell);
        let past = exceeded(s, class);
        if past.is_empty() {
            self.within += 1;
        } else {
            self.past += 1;
        }
        past
    }

    fn value(&self) -> Value {
        json!({
            "within": self.within,
            "past_a_limit": self.past,
            "figures": self.figures.value(),
        })
    }
}

/// What one output kind other than the picture holds over the corpus: stacks measured, within and
/// past its limit, the GPU could not render there and does not render yet, with the figures of
/// those measured; for the histogram, its largest differences.
#[derive(Default)]
struct Other {
    measured: usize,
    within: usize,
    past: usize,
    gaps: usize,
    unrendered: usize,
    figures: Extremes,
    counts: CountsWorst,
}

/// The histogram's largest differences over the corpus, each a share of its stack's output pixel
/// count, with the cell it came from, and how many stacks' counts were the reference's exactly.
#[derive(Default)]
struct CountsWorst {
    bins: Option<(f64, String)>,
    clipping: Option<(f64, String, String)>,
    exact: usize,
    /// The largest earth mover's distance in codes over R, G, B and luminance, the judged figure,
    /// with its histogram and its cell.
    emd: Option<(f64, String, String)>,
}

impl CountsWorst {
    /// One stack's measured counts, `measured`, of `cell`.
    fn add(&mut self, measured: &Value, cell: &str) {
        let bins = measured["worst_bins"].as_f64().unwrap_or(f64::NAN);
        let clipping = measured["worst_clipping"]["share"]
            .as_f64()
            .unwrap_or(f64::NAN);
        if self.bins.as_ref().is_none_or(|(worst, _)| bins > *worst) {
            self.bins = Some((bins, cell.to_owned()));
        }
        if self
            .clipping
            .as_ref()
            .is_none_or(|(worst, _, _)| clipping > *worst)
        {
            let counter = measured["worst_clipping"]["counter"]
                .as_str()
                .unwrap_or("?")
                .to_owned();
            self.clipping = Some((clipping, counter, cell.to_owned()));
        }
        if bins == 0.0 && clipping == 0.0 {
            self.exact += 1;
        }
        let emd = measured["worst_emd"]["codes"].as_f64().unwrap_or(f64::NAN);
        if self.emd.as_ref().is_none_or(|(worst, _, _)| emd > *worst) {
            let side = measured["worst_emd"]["side"]
                .as_str()
                .unwrap_or("?")
                .to_owned();
            self.emd = Some((emd, side, cell.to_owned()));
        }
    }

    fn value(&self) -> Value {
        json!({
            "worst_bins": self.bins.as_ref().map(|(share, cell)| json!({"share": share, "cell": cell})),
            "worst_clipping": self.clipping.as_ref().map(|(share, counter, cell)| {
                json!({"share": share, "counter": counter, "cell": cell})
            }),
            "exact": self.exact,
            "worst_emd": self.emd.as_ref().map(|(codes, side, cell)| {
                json!({"codes": codes, "side": side, "cell": cell})
            }),
        })
    }
}

/// What one view and class of the picture holds over the corpus.
#[derive(Default)]
struct Tally {
    /// Cells selected, and of those the ones whose frame could not be drawn here or failed to.
    cells: usize,
    gaps: usize,
    errors: usize,
    /// The motion frame against the frame it settles to, and against the reference.
    settled: Judged,
    reference: Judged,
    /// The picture at rest against the reference, and what drew it.
    at_rest: Judged,
    renderers: Vec<String>,
}

/// Judge the harness's `cells.json` (`None` when it wrote none) against every recorded limit,
/// `expected` stacks being what the selection holds, by the reading `options` asks for. Answers
/// the report without its build and corpus identity, which the caller adds.
pub fn judge(cells: Option<&Value>, expected: usize, options: &Options) -> Value {
    let views: Vec<String> = options
        .views
        .clone()
        .unwrap_or_else(|| VIEWS.iter().map(|id| (*id).to_owned()).collect());
    let kinds: Vec<Kind> = options.kinds.clone().unwrap_or_else(|| Kind::ALL.to_vec());
    let gate_motion = options.gate_motion;
    let (motion, at_rest) = (
        kinds.contains(&Kind::PictureInMotion),
        kinds.contains(&Kind::PictureAtRest),
    );
    let mut report = json!({
        "format": 1,
        "gate": "gpu-qualification",
        "status": "passed",
        "error": null,
        "reading": {
            "picture_at_rest": "gated against the reference",
            "picture_in_motion": match gate_motion {
                None => MOTION_NOT_GATED,
                Some(GateMotion::AgainstRest) => "gated against the picture at rest it settles to",
                Some(GateMotion::AgainstReference) => "gated against the reference",
            },
            "gate_motion": gate_motion.map(GateMotion::name),
            "default": DEFAULT,
        },
        "selection": {
            "whole_corpus": options.narrowed().is_empty(),
            "narrowed": options.narrowed(),
            "views": views,
            "kinds": kinds.iter().map(|kind| kind.name()).collect::<Vec<_>>(),
        },
        "kinds": Kind::ALL.iter().map(|kind| json!({
            "kind": kind.name(),
            "reference": kind.reference(),
            "limit": kind.limit(),
            "limits": match kind {
                Kind::Histogram => json!({
                    "emd_codes": tolerance::HISTOGRAM_EMD_CODES,
                    "clipping_share_of_output_pixels": tolerance::HISTOGRAM_FRACTION,
                }),
                _ => json!({
                    "pointwise": limits_value(Class::Pointwise),
                    "spatial": limits_value(Class::Spatial),
                }),
            },
        })).collect::<Vec<_>>(),
        "adapter": null,
        "results": [],
        "missed": [],
        "information": {"motion_past": []},
        "gaps": [],
        "errors": [],
        "not_rendered": [],
    });
    let Some(cells) = cells else {
        failed(
            &mut report,
            "the harness wrote no cells.json; see harness.log".to_owned(),
        );
        return report;
    };
    report["adapter"] = cells["adapter"].clone();
    if let Some(skipped) = cells["skipped"].as_str() {
        report["status"] = json!("incomplete");
        report["error"] = json!(skipped);
        return report;
    }
    let pairs = cells["pairs"].as_array().cloned().unwrap_or_default();
    if cells["complete"] != true || pairs.len() != expected {
        failed(
            &mut report,
            format!(
                "the harness recorded {} of the selection's {expected} stacks{}; see harness.log",
                pairs.len(),
                if cells["complete"] == true {
                    ""
                } else {
                    " and did not finish"
                }
            ),
        );
    }

    let classes = [Class::Pointwise, Class::Spatial];
    let mut tallies: Vec<Vec<Tally>> = views
        .iter()
        .map(|_| classes.iter().map(|_| Tally::default()).collect())
        .collect();
    let (mut missed, mut gaps, mut errors) = (Vec::new(), Vec::new(), Vec::new());
    let mut motion_past = Vec::new();
    // Per kind other than the pictures: how many stacks it was measured on, passed, missed, could
    // not be rendered by the GPU there and was not rendered on, with its figures.
    let mut other: Vec<(Kind, Other)> = kinds
        .iter()
        .filter(|kind| !kind.picture())
        .map(|kind| (*kind, Other::default()))
        .collect();
    let mut not_rendered: Vec<(Kind, String)> = Vec::new();
    let note = |not_rendered: &mut Vec<(Kind, String)>, kind: Kind, reason: &Value| {
        if !not_rendered.iter().any(|(known, _)| *known == kind) {
            not_rendered.push((kind, reason.as_str().unwrap_or_default().to_owned()));
        }
    };
    for pair in &pairs {
        let cell = pair["cell"].as_str().unwrap_or("?");
        let class = pair["class"]
            .as_str()
            .and_then(Class::parse)
            .unwrap_or(Class::Pointwise);
        let class_index = usize::from(class == Class::Spatial);
        match pair["status"].as_str() {
            Some("measured") => {}
            status => {
                let reason = pair["error"].as_str().unwrap_or("unrecorded").to_owned();
                let entry = json!({"cell": cell, "view": null, "reason": reason});
                if status == Some("missing-source") {
                    gaps.push(entry);
                } else {
                    errors.push(entry);
                }
                for tally in &mut tallies {
                    tally[class_index].cells += 1;
                    if status == Some("missing-source") {
                        tally[class_index].gaps += 1;
                    } else {
                        tally[class_index].errors += 1;
                    }
                }
                continue;
            }
        }
        if motion || at_rest {
            for (view_index, view) in views.iter().enumerate() {
                let tally = &mut tallies[view_index][class_index];
                tally.cells += 1;
                let measured = &pair["views"][view];
                match measured["status"].as_str() {
                    Some("measured") => {}
                    Some("gap") => {
                        tally.gaps += 1;
                        gaps.push(
                            json!({"cell": cell, "view": view, "reason": measured["reason"]}),
                        );
                        continue;
                    }
                    _ => {
                        tally.errors += 1;
                        errors.push(json!({
                            "cell": cell,
                            "view": view,
                            "reason": measured["reason"].as_str().unwrap_or("no figures recorded"),
                        }));
                        continue;
                    }
                }
                let in_motion = &measured["in_motion"];
                let rest = &measured["at_rest"];
                let past_settled =
                    tally
                        .settled
                        .add(&statistics(&in_motion["against_settled"]), cell, class);
                let past_reference =
                    tally
                        .reference
                        .add(&statistics(&in_motion["against_reference"]), cell, class);
                if motion {
                    // Both comparisons, each a miss of the run where the owner gates it and
                    // information otherwise.
                    for (past, field, against, gate) in [
                        (
                            past_settled,
                            "against_settled",
                            "the picture at rest it settles to",
                            GateMotion::AgainstRest,
                        ),
                        (
                            past_reference,
                            "against_reference",
                            "the reference",
                            GateMotion::AgainstReference,
                        ),
                    ] {
                        if past.is_empty() {
                            continue;
                        }
                        let entry = json!({
                            "kind": Kind::PictureInMotion.name(),
                            "cell": cell,
                            "view": view,
                            "class": class.name(),
                            "against": against,
                            "exceeded": past,
                            "statistics": in_motion[field],
                            "limits": limits_value(class),
                            "at_rest": rest["against_reference"],
                            "frames": measured["frames"],
                        });
                        if gate_motion == Some(gate) {
                            missed.push(entry);
                        } else {
                            motion_past.push(entry);
                        }
                    }
                }
                if at_rest {
                    match rest["status"].as_str() {
                        Some("measured") => {
                            if let Some(renderer) = rest["renderer"].as_str()
                                && !tally.renderers.iter().any(|known| known == renderer)
                            {
                                tally.renderers.push(renderer.to_owned());
                            }
                            let figures = statistics(&rest["against_reference"]);
                            let past = tally.at_rest.add(&figures, cell, class);
                            if !past.is_empty() {
                                missed.push(json!({
                                    "kind": Kind::PictureAtRest.name(),
                                    "cell": cell,
                                    "view": view,
                                    "class": class.name(),
                                    "against": "the reference",
                                    "exceeded": past,
                                    "statistics": rest["against_reference"],
                                    "limits": limits_value(class),
                                    "at_rest": rest["against_reference"],
                                    "frames": measured["frames"],
                                }));
                            }
                        }
                        _ => errors.push(json!({
                            "cell": cell,
                            "view": view,
                            "reason": "no picture-at-rest figures recorded",
                        })),
                    }
                }
            }
        }
        for (kind, counts) in &mut other {
            let measured = &pair["kinds"][kind.name()];
            match measured["status"].as_str() {
                Some("measured") => {
                    counts.measured += 1;
                    if *kind == Kind::Histogram {
                        counts.counts.add(measured, cell);
                    }
                    let figures = measured.get("statistics").map(statistics);
                    if let Some(figures) = &figures {
                        counts.figures.add(figures, cell);
                    }
                    if measured["passed"] == true {
                        counts.within += 1;
                    } else {
                        counts.past += 1;
                        let mut exceeded = figures
                            .as_ref()
                            .map(|figures| exceeded(figures, class))
                            .unwrap_or_default();
                        if measured["repeatable"] == false {
                            exceeded.push("repeatable");
                        }
                        missed.push(json!({
                            "kind": kind.name(), "cell": cell, "view": null,
                            "class": class.name(), "exceeded": exceeded,
                            "statistics": measured["statistics"], "figures": measured,
                        }));
                    }
                }
                Some("not-rendered") => {
                    counts.unrendered += 1;
                    note(&mut not_rendered, *kind, &measured["reason"]);
                }
                // The GPU could not render it here, for the reason given: never a pass.
                Some("gap") => {
                    counts.gaps += 1;
                    gaps.push(json!({
                        "cell": cell,
                        "view": null,
                        "kind": kind.name(),
                        "reason": format!(
                            "{}: {}",
                            kind.name(),
                            measured["reason"].as_str().unwrap_or("unrecorded")
                        ),
                    }));
                }
                _ => errors.push(json!({
                    "cell": cell,
                    "view": null,
                    "reason": format!("no {} figures recorded", kind.name()),
                })),
            }
        }
    }

    let mut results = Vec::new();
    for (view_index, view) in views.iter().enumerate() {
        for (class_index, class) in classes.iter().enumerate() {
            let tally = &tallies[view_index][class_index];
            if tally.cells == 0 {
                continue;
            }
            if motion {
                let gated = gate_motion.map(|gate| match gate {
                    GateMotion::AgainstRest => &tally.settled,
                    GateMotion::AgainstReference => &tally.reference,
                });
                let verdict = match gated {
                    None => MOTION_NOT_GATED,
                    Some(judged) if judged.past > 0 => "failed",
                    Some(_) if tally.gaps + tally.errors > 0 => "incomplete",
                    Some(_) => "passed",
                };
                results.push(json!({
                    "kind": Kind::PictureInMotion.name(),
                    "view": view,
                    "class": class.name(),
                    "cells": tally.cells,
                    "gaps": tally.gaps,
                    "errors": tally.errors,
                    "limits": limits_value(*class),
                    "against_rest": tally.settled.value(),
                    "against_reference": tally.reference.value(),
                    "gated": gate_motion.map(GateMotion::name),
                    "verdict": verdict,
                }));
            }
            if at_rest {
                let measured = tally.at_rest.within + tally.at_rest.past;
                let verdict = if tally.at_rest.past > 0 {
                    "failed"
                } else if tally.gaps + tally.errors > 0 {
                    "incomplete"
                } else {
                    "passed"
                };
                results.push(json!({
                    "kind": Kind::PictureAtRest.name(),
                    "view": view,
                    "class": class.name(),
                    "against": "the reference",
                    "cells": tally.cells,
                    "measured": measured,
                    "within": tally.at_rest.within,
                    "past_a_limit": tally.at_rest.past,
                    "gaps": tally.gaps,
                    "errors": tally.errors,
                    "limits": limits_value(*class),
                    "figures": tally.at_rest.figures.value(),
                    "renderers": tally.renderers,
                    "verdict": verdict,
                }));
            }
        }
    }
    for (kind, counts) in &other {
        let mut result = json!({
            "kind": kind.name(),
            "view": "the whole output stage",
            "measured": counts.measured,
            "within": counts.within,
            "past_a_limit": counts.past,
            "gaps": counts.gaps,
            "not_rendered": counts.unrendered,
            "figures": counts.figures.value(),
            "verdict": if counts.past > 0 {
                "failed"
            } else if counts.gaps > 0 {
                "incomplete"
            } else if counts.measured == 0 && counts.unrendered > 0 {
                "not rendered by the GPU yet"
            } else {
                "passed"
            },
        });
        if *kind == Kind::Histogram {
            result["counts"] = counts.counts.value();
        }
        results.push(result);
    }
    report["results"] = json!(results);
    report["not_rendered"] = json!(
        not_rendered
            .iter()
            .map(|(kind, reason)| json!({"kind": kind.name(), "reason": reason}))
            .collect::<Vec<_>>()
    );
    let first_miss = missed.first().map(|miss| {
        format!(
            "{} cell(s) pass a limit, the first {} {} at {} ({})",
            missed.len(),
            miss["kind"].as_str().unwrap_or("?"),
            miss["cell"].as_str().unwrap_or("?"),
            miss["view"].as_str().unwrap_or("the whole stage"),
            names(&miss["exceeded"]).unwrap_or_else(|| "its limit".to_owned())
        )
    });
    report["missed"] = json!(missed);
    report["information"]["motion_past"] = json!(motion_past);
    report["gaps"] = json!(gaps);
    report["errors"] = json!(errors);
    if let Some(first) = first_miss {
        failed(&mut report, first);
    }
    if report["status"] == "passed" {
        let mut why = Vec::new();
        if !gaps.is_empty() {
            why.push(format!("{} cell(s) could not be run here", gaps.len()));
        }
        if !errors.is_empty() {
            why.push(format!("{} cell(s) failed to run", errors.len()));
        }
        if !options.narrowed().is_empty() {
            why.push(format!(
                "a selection, not the whole corpus ({})",
                options.narrowed().join("; ")
            ));
        }
        if !why.is_empty() {
            report["status"] = json!("incomplete");
            report["error"] = json!(why.join("; "));
        }
    }
    report
}

/// A list of statistic names, joined.
fn names(value: &Value) -> Option<String> {
    value.as_array().map(|names| {
        names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

fn limits_value(class: Class) -> Value {
    let [mean, worst_block, p99, mean_delta_l] = limits(class);
    json!({"mean": mean, "worst_block": worst_block, "p99": p99, "mean_delta_l_abs": mean_delta_l})
}

/// A figure for a table: three significant decimals below 1, two below 10, one above.
fn number(value: &Value) -> String {
    match value.as_f64() {
        None => "—".to_owned(),
        Some(v) if v.is_nan() => "NaN".to_owned(),
        Some(v) if v.abs() >= 10.0 => format!("{v:.1}"),
        Some(v) if v.abs() >= 1.0 => format!("{v:.2}"),
        Some(v) => format!("{v:.3}"),
    }
}

/// The four figures of an extremes object's `field` (`max` or `mean`), as `a / b / c / d`.
fn four(extremes: &Value, field: &str) -> String {
    if extremes.is_null() {
        return "—".to_owned();
    }
    STATISTICS
        .iter()
        .map(|name| number(&extremes[field][name]))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// A recorded statistics object's four figures, as `a / b / c / d`, or a dash.
fn row(value: &Value) -> String {
    if value.is_null() {
        return "—".to_owned();
    }
    STATISTICS
        .iter()
        .map(|name| number(&value[name]))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// The report as the Markdown a person reads.
pub fn markdown(report: &Value) -> String {
    let status = report["status"].as_str().unwrap_or("?");
    let mut text = String::from("# GPU qualification against the reference\n\n");
    text.push_str(&format!(
        "**{}**{}\n\n",
        status.to_uppercase(),
        report["error"]
            .as_str()
            .map(|why| format!(": {why}."))
            .unwrap_or_else(|| ".".to_owned())
    ));
    let reading = &report["reading"];
    match reading["gate_motion"].as_str() {
        Some(gate) => text.push_str(&format!(
            "Reading: `--gate-motion {gate}`. The picture at rest is gated against the reference; \
             the picture in motion, the frame a drag draws, is {} at every view, which the owner \
             has not decided ({DEFAULT}).\n\n",
            reading["picture_in_motion"].as_str().unwrap_or("gated")
        )),
        None => text.push_str(&format!(
            "Reading: the picture at rest is gated against the reference. The picture in motion, \
             the frame a drag draws, is reported against the picture at rest it settles to and \
             against the reference, and {MOTION_NOT_GATED} ({DEFAULT}); `--gate-motion \
             against-rest` or `--gate-motion against-reference` gates it.\n\n"
        )),
    }
    text.push_str(
        "The release gate (docs/design/gpu-first.md): every stack of the corpus on the reference \
         renderer and on the GPU, each output kind the GPU renders held to its recorded limit. A kind \
         the GPU does not render yet is neither a pass nor a failure; a cell that could not run makes \
         the run incomplete.\n\n",
    );
    text.push_str(&format!(
        "Adapter {}. Build {}{}, harness SHA-256 {}. Corpus SHA-256 {}. {}. Sources unchanged by the run: {}. Date {}.\n\n",
        report["adapter"].as_str().unwrap_or("none"),
        report["build"]["revision"].as_str().unwrap_or("unknown"),
        if report["build"]["working_tree_dirty"] == true {
            " with uncommitted changes"
        } else {
            ""
        },
        report["build"]["harness_binary_sha256"]
            .as_str()
            .unwrap_or("unknown"),
        report["corpus"]["sha256"].as_str().unwrap_or("unknown"),
        if report["selection"]["whole_corpus"] == true {
            "The whole corpus".to_owned()
        } else {
            format!(
                "A selection: {}",
                report["selection"]["narrowed"]
                    .as_array()
                    .map(|items| items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; "))
                    .unwrap_or_default()
            )
        },
        report["sources_unchanged"],
        report["date"].as_str().unwrap_or("unknown"),
    ));

    text.push_str("## Output kinds\n\n| Kind | Reference | Limit | The GPU renders it |\n| --- | --- | --- | --- |\n");
    let not_rendered = report["not_rendered"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for kind in report["kinds"].as_array().into_iter().flatten() {
        let name = kind["kind"].as_str().unwrap_or("?");
        let rendered = match not_rendered.iter().find(|entry| entry["kind"] == name) {
            Some(entry) => format!("No: {}", entry["reason"].as_str().unwrap_or("")),
            None => "Yes".to_owned(),
        };
        text.push_str(&format!(
            "| {name} | {} | {} | {rendered} |\n",
            kind["reference"].as_str().unwrap_or(""),
            kind["limit"].as_str().unwrap_or(""),
        ));
    }

    let results = report["results"].as_array().cloned().unwrap_or_default();
    let of_kind = |kind: Kind| -> Vec<&Value> {
        results
            .iter()
            .filter(|result| result["kind"] == kind.name())
            .collect()
    };
    let motion = of_kind(Kind::PictureInMotion);
    let rest = of_kind(Kind::PictureAtRest);
    let table_head = "| View | Class | Cells | Within | Past a limit | Gaps | Errors | Max | Mean | Limit | Verdict |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- |\n";
    let table_row = |result: &Value| {
        let limits = &result["limits"];
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} / {} / {} / {} | {} |\n",
            result["view"].as_str().unwrap_or("?"),
            result["class"].as_str().unwrap_or("?"),
            result["cells"],
            result["within"],
            result["past_a_limit"],
            result["gaps"],
            result["errors"],
            four(&result["figures"], "max"),
            four(&result["figures"], "mean"),
            number(&limits["mean"]),
            number(&limits["worst_block"]),
            number(&limits["p99"]),
            number(&limits["mean_delta_l_abs"]),
            result["verdict"].as_str().unwrap_or("?"),
        )
    };
    const FIGURES: &str = "Each cell's mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / signed mean ΔL\\*: the largest magnitude over the measured cells, their mean, and the limit (ΔL\\* within ±).";
    if !motion.is_empty() {
        let gated = reading["gate_motion"].as_str();
        text.push_str(&format!(
            "\n## The picture in motion, {}\n\nThe frame a drag draws at each view, against the GPU's picture at rest it settles to and against the reference. {FIGURES}\n\n| View | Class | Cells | Against the picture at rest: within / past | Max | Against the reference: within / past | Max | Limit | Verdict |\n| --- | --- | ---: | --- | --- | --- | --- | --- | --- |\n",
            gated.map_or_else(|| "not gated".to_owned(), |gate| format!("gated {gate}"))
        ));
        for result in &motion {
            let limits = &result["limits"];
            let pair = |comparison: &Value| {
                format!("{} / {}", comparison["within"], comparison["past_a_limit"])
            };
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} / {} / {} / {} | {} |\n",
                result["view"].as_str().unwrap_or("?"),
                result["class"].as_str().unwrap_or("?"),
                result["cells"],
                pair(&result["against_rest"]),
                four(&result["against_rest"]["figures"], "max"),
                pair(&result["against_reference"]),
                four(&result["against_reference"]["figures"], "max"),
                number(&limits["mean"]),
                number(&limits["worst_block"]),
                number(&limits["p99"]),
                number(&limits["mean_delta_l_abs"]),
                result["verdict"].as_str().unwrap_or("?"),
            ));
        }
    }
    if !rest.is_empty() {
        text.push_str(&format!(
            "\n## The picture at rest, against the reference\n\nThe editor's GPU picture of the stack at rest at each view. {FIGURES}\n\n{table_head}"
        ));
        for result in &rest {
            text.push_str(&table_row(result));
        }
        text.push_str("\nWhat drew it:\n\n");
        for result in &rest {
            let renderers = result["renderers"]
                .as_array()
                .map(|renderers| {
                    renderers
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            text.push_str(&format!(
                "- {} {}: {}\n",
                result["view"].as_str().unwrap_or("?"),
                result["class"].as_str().unwrap_or("?"),
                if renderers.is_empty() {
                    "nothing measured"
                } else {
                    &renderers
                }
            ));
        }
    }
    let past = report["information"]["motion_past"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !past.is_empty() {
        text.push_str(&format!(
            "\n## Information, not judged\n\n### The motion frame past a limit ({})\n\nEach cell's figures: the motion frame against what the row names, then the picture at rest of the same view against the reference.\n\n| Cell | View | Class | Against | Past | Motion frame | Picture at rest |\n| --- | --- | --- | --- | --- | --- | --- |\n",
            past.len()
        ));
        for entry in &past {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                entry["cell"].as_str().unwrap_or("?"),
                entry["view"].as_str().unwrap_or("?"),
                entry["class"].as_str().unwrap_or("?"),
                entry["against"].as_str().unwrap_or("?"),
                names(&entry["exceeded"]).unwrap_or_default(),
                row(&entry["statistics"]),
                row(&entry["at_rest"]),
            ));
        }
    }
    let others: Vec<&Value> = results
        .iter()
        .filter(|result| {
            result["kind"]
                .as_str()
                .and_then(Kind::parse)
                .is_some_and(|kind| !kind.picture())
        })
        .collect();
    if !others.is_empty() {
        text.push_str("\n## The other output kinds\n\nOver the whole output stage, each stack's statistics as the picture's are; for export, the GPU's export against the reference export by the display limit of the stack's class, and two GPU exports the same bytes.\n\n| Kind | Measured | Within | Past a limit | Gaps | Not rendered by the GPU | Max | Mean | Verdict |\n| --- | ---: | ---: | ---: | ---: | ---: | --- | --- | --- |\n");
        for result in &others {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                result["kind"].as_str().unwrap_or("?"),
                result["measured"],
                result["within"],
                result["past_a_limit"],
                result["gaps"],
                result["not_rendered"],
                four(&result["figures"], "max"),
                four(&result["figures"], "mean"),
                result["verdict"].as_str().unwrap_or("?"),
            ));
        }
        if let Some(histogram) = others
            .iter()
            .find(|result| result["kind"] == Kind::Histogram.name())
        {
            let counts = &histogram["counts"];
            let share = |value: &Value| {
                value
                    .as_f64()
                    .map_or_else(|| "—".to_owned(), |share| format!("{:.4}%", share * 100.0))
            };
            if counts["worst_emd"].is_object() {
                text.push_str(&format!(
                    "\nHistogram: the GPU's counts over each stack's tiles at full resolution against the reference frame's, by the core's reducer, held to an earth mover's distance of {} code on each of R, G, B and luminance and each clipping count within {}% of the output pixels. Largest earth mover's distance {:.4} codes (`{}`, {}); largest clipping difference {} (`{}`, {}); the reference's counts exactly on {} of {} stacks. Reported and not gated: the largest summed bin difference {} of the output pixels ({}); each stack's diagnostics, and the bins of those past a limit, are in the run's cells.\n",
                    tolerance::HISTOGRAM_EMD_CODES,
                    tolerance::HISTOGRAM_FRACTION * 100.0,
                    counts["worst_emd"]["codes"].as_f64().unwrap_or(f64::NAN),
                    counts["worst_emd"]["side"].as_str().unwrap_or("?"),
                    counts["worst_emd"]["cell"].as_str().unwrap_or("?"),
                    share(&counts["worst_clipping"]["share"]),
                    counts["worst_clipping"]["counter"].as_str().unwrap_or("?"),
                    counts["worst_clipping"]["cell"].as_str().unwrap_or("?"),
                    counts["exact"],
                    histogram["measured"],
                    share(&counts["worst_bins"]["share"]),
                    counts["worst_bins"]["cell"].as_str().unwrap_or("?"),
                ));
            }
        }
    }
    let missed = report["missed"].as_array().cloned().unwrap_or_default();
    if !missed.is_empty() {
        text.push_str(&format!(
            "\n## Past a limit ({})\n\nEach cell the gate judges past its limit: its figures against what it is judged by, then the picture at rest of the same view against the reference.\n\n| Cell | View | Kind | Against | Class | Past | Figures | Picture at rest |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n",
            missed.len()
        ));
        for miss in &missed {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                miss["cell"].as_str().unwrap_or("?"),
                miss["view"].as_str().unwrap_or("whole stage"),
                miss["kind"].as_str().unwrap_or("?"),
                miss["against"].as_str().unwrap_or("its reference"),
                miss["class"].as_str().unwrap_or("?"),
                names(&miss["exceeded"]).unwrap_or_else(|| "its limit".to_owned()),
                row(&miss["statistics"]),
                row(&miss["at_rest"]),
            ));
        }
    }
    for (title, entries) in [
        ("Cells that could not run here", &report["gaps"]),
        ("Cells that failed to run", &report["errors"]),
    ] {
        let entries = entries.as_array().cloned().unwrap_or_default();
        if entries.is_empty() {
            continue;
        }
        text.push_str(&format!("\n## {title} ({})\n\n", entries.len()));
        // Grouped by reason, so a reason shared by many cells is read once.
        let mut reasons: Vec<(String, Vec<String>)> = Vec::new();
        for entry in &entries {
            let reason = entry["reason"].as_str().unwrap_or("unrecorded").to_owned();
            let at = match entry["view"].as_str() {
                Some(view) => format!("{} at {view}", entry["cell"].as_str().unwrap_or("?")),
                None => entry["cell"].as_str().unwrap_or("?").to_owned(),
            };
            match reasons.iter_mut().find(|(known, _)| *known == reason) {
                Some((_, cells)) => cells.push(at),
                None => reasons.push((reason, vec![at])),
            }
        }
        for (reason, cells) in reasons {
            text.push_str(&format!(
                "- {} ({}): {}\n",
                reason.replace('\n', " "),
                cells.len(),
                cells.join(", ")
            ));
        }
    }
    text.push_str(
        "\nEvery figure is CIEDE2000 in f64 from 8-bit sRGB through linear light, XYZ and CIELAB under D65 (luxforge_reference::preview_error). The pixels are deterministic on one machine and driver; across machines the last digit may differ. A run's duration is not a measurement.\n",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            output: PathBuf::from("/nowhere"),
            manifest: None,
            fixtures: PathBuf::from("/nowhere"),
            views: None,
            kinds: None,
            families: None,
            recipes: None,
            sources: None,
            all_frames: false,
            gate_motion: None,
        }
    }

    fn stats(mean: f64, worst_block: f64, p99: f64, mean_delta_l: f64) -> Value {
        json!({"pixels": 100, "mean": mean, "worst_block": worst_block, "p99": p99,
               "mean_delta_l": mean_delta_l, "max": p99})
    }

    /// A view's record: the motion frame against the picture at rest it settles to and against
    /// the reference, and the picture at rest, drawn in tiles, against the reference.
    fn view(settled: Value, reference: Value) -> Value {
        json!({
            "status": "measured",
            "in_motion": {"settles_to": "the GPU's picture at rest", "against_settled": settled,
                          "against_reference": reference},
            "at_rest": {"status": "measured", "renderer": "the picture at rest in tiles",
                        "tiles": 6, "against_reference": stats(0.01, 0.05, 0.1, 0.0),
                        "passed": true},
            "frames": [],
        })
    }

    fn not_rendered() -> Value {
        json!({
            "histogram": {"status": "not-rendered", "reason": "stage 2's"},
            "sample": {"status": "not-rendered", "reason": "stage 4's"},
            "export": {"status": "not-rendered", "reason": "stage 4's"},
        })
    }

    /// One stack measured at every view with the same figures.
    fn pair(cell: &str, class: &str, settled: Value, reference: Value) -> Value {
        let views: serde_json::Map<String, Value> = VIEWS
            .iter()
            .map(|id| ((*id).to_owned(), view(settled.clone(), reference.clone())))
            .collect();
        json!({"cell": cell, "class": class, "status": "measured", "views": views,
               "kinds": not_rendered()})
    }

    fn cells(pairs: Vec<Value>) -> Value {
        json!({"format": 1, "adapter": "Test (Metal)", "skipped": null, "complete": true,
               "pairs": pairs})
    }

    fn rows(report: &Value, kind: Kind) -> Vec<&Value> {
        report["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == kind.name())
            .collect()
    }

    fn within() -> Value {
        stats(0.1, 0.5, 1.0, -0.1)
    }

    #[test]
    fn the_gate_passes_when_every_motion_frame_is_within_its_limit_of_the_frame_it_settles_to() {
        let report = judge(
            Some(&cells(vec![
                pair("a--b", "pointwise", within(), within()),
                pair("c--d", "spatial", stats(0.9, 2.4, 4.0, 0.4), within()),
            ])),
            2,
            &options(),
        );
        assert_eq!(report["status"], "passed", "{report:#}");
        assert_eq!(report["reading"]["picture_in_motion"], MOTION_NOT_GATED);
        // Every view and class is a row of each picture kind; the picture in motion carries both
        // comparisons and is not gated.
        let motion = rows(&report, Kind::PictureInMotion);
        assert_eq!(motion.len(), VIEWS.len() * 2);
        assert!(motion.iter().all(|r| r["verdict"] == MOTION_NOT_GATED));
        assert_eq!(
            motion[0]["against_rest"]["figures"]["max"]["worst_block"],
            0.5
        );
        assert_eq!(
            motion[0]["against_reference"]["figures"]["mean"]["mean_delta_l"],
            -0.1
        );
        // The picture at rest, drawn by the GPU, passes against the reference, with what drew it;
        // the other kinds are not rendered by the GPU, neither passed nor failed.
        let rest = rows(&report, Kind::PictureAtRest);
        assert_eq!(rest.len(), VIEWS.len() * 2);
        assert!(rest.iter().all(|r| r["verdict"] == "passed"));
        assert_eq!(rest[0]["within"], 1);
        assert_eq!(
            rest[0]["renderers"],
            json!(["the picture at rest in tiles"])
        );
        for kind in [Kind::Histogram, Kind::Sample, Kind::Export] {
            assert_eq!(
                rows(&report, kind)[0]["verdict"],
                "not rendered by the GPU yet"
            );
        }
        assert_eq!(report["not_rendered"].as_array().unwrap().len(), 3);
        let summary = markdown(&report);
        assert!(summary.contains("**PASSED**") && summary.contains("No: stage 2's"));
        assert!(summary.contains("## The picture at rest, against the reference"));
        assert!(summary.contains("Reading: the picture at rest is gated against the reference"));
        assert!(summary.contains("## The picture in motion, not gated"));
    }

    #[test]
    fn a_motion_frame_past_its_limit_is_information_and_fails_only_where_the_owner_gates_it() {
        let measured = cells(vec![pair(
            "c--d",
            "spatial",
            stats(0.9, 2.6, 4.0, 0.0),
            within(),
        )]);
        // Not gated: the run passes, and the miss is information.
        let report = judge(Some(&measured), 1, &options());
        assert_eq!(report["status"], "passed", "{report:#}");
        let past = report["information"]["motion_past"].as_array().unwrap();
        assert_eq!(past.len(), VIEWS.len(), "one entry per view");
        assert_eq!(past[0]["exceeded"], json!(["worst_block"]));
        assert_eq!(past[0]["against"], "the picture at rest it settles to");
        assert_eq!(past[0]["at_rest"]["worst_block"], 0.05);
        assert_eq!(
            rows(&report, Kind::PictureInMotion)[0]["against_rest"]["past_a_limit"],
            1
        );
        assert!(markdown(&report).contains("### The motion frame past a limit (4)"));
        // Gated against the picture at rest, the same cells fail the run.
        let gated = Options {
            gate_motion: Some(GateMotion::AgainstRest),
            ..options()
        };
        let report = judge(Some(&measured), 1, &gated);
        assert_eq!(report["status"], "failed");
        let missed = report["missed"].as_array().unwrap();
        assert_eq!(missed.len(), VIEWS.len(), "one miss per view");
        assert_eq!(missed[0]["against"], "the picture at rest it settles to");
        assert!(report["error"].as_str().unwrap().contains("c--d"));
        assert_eq!(report["reading"]["gate_motion"], "against-rest");
        assert_eq!(rows(&report, Kind::PictureInMotion)[0]["verdict"], "failed");
        assert!(markdown(&report).contains(
            "| c--d | fit | picture-in-motion | the picture at rest it settles to | spatial | worst_block | 0.900 / 2.60 / 4.00 / 0.000 | 0.010 / 0.050 / 0.100 / 0.000 |"
        ));
        assert!(markdown(&report).contains("Reading: `--gate-motion against-rest`"));
        // A NaN, which the harness writes as null, is past every limit where it is gated.
        let report = judge(
            Some(&cells(vec![pair(
                "a--b",
                "pointwise",
                json!({"mean": null, "worst_block": 0.0, "p99": 0.0, "mean_delta_l": 0.0}),
                within(),
            )])),
            1,
            &gated,
        );
        assert_eq!(report["status"], "failed");
        assert!(markdown(&report).contains("**FAILED**"));
    }

    #[test]
    fn a_motion_frame_far_from_the_reference_is_reported_and_gated_only_when_asked() {
        let far = stats(1.3, 4.1, 3.8, 0.3);
        let measured = cells(vec![pair("e--f", "spatial", within(), far.clone())]);
        let report = judge(Some(&measured), 1, &options());
        assert_eq!(report["status"], "passed", "{report:#}");
        let past = report["information"]["motion_past"].as_array().unwrap();
        assert_eq!(past.len(), VIEWS.len());
        assert_eq!(past[0]["exceeded"], json!(["mean", "worst_block"]));
        assert_eq!(past[0]["against"], "the reference");
        assert_eq!(
            rows(&report, Kind::PictureInMotion)[0]["against_reference"]["past_a_limit"],
            1
        );
        // Gated against the reference, the same cells fail the run.
        let held = Options {
            gate_motion: Some(GateMotion::AgainstReference),
            ..options()
        };
        let report = judge(Some(&measured), 1, &held);
        assert_eq!(report["status"], "failed");
        assert_eq!(report["missed"].as_array().unwrap().len(), VIEWS.len());
        assert_eq!(report["missed"][0]["against"], "the reference");
        assert_eq!(report["reading"]["gate_motion"], "against-reference");
        assert!(markdown(&report).contains("Reading: `--gate-motion against-reference`"));
    }

    #[test]
    fn an_at_rest_frame_the_gpu_draws_is_judged_against_the_reference() {
        let mut drawn = pair("g--h", "pointwise", within(), within());
        drawn["views"]["fit"]["at_rest"]["against_reference"] = stats(0.6, 0.5, 1.0, 0.0);
        let report = judge(Some(&cells(vec![drawn.clone()])), 1, &options());
        assert_eq!(report["status"], "failed");
        assert_eq!(report["missed"][0]["kind"], "picture-at-rest");
        assert_eq!(report["missed"][0]["exceeded"], json!(["mean"]));
        assert_eq!(rows(&report, Kind::PictureAtRest)[0]["verdict"], "failed");
        drawn["views"]["fit"]["at_rest"]["against_reference"] = within();
        let report = judge(Some(&cells(vec![drawn])), 1, &options());
        assert_eq!(report["status"], "passed");
        assert_eq!(rows(&report, Kind::PictureAtRest)[0]["verdict"], "passed");
    }

    #[test]
    fn a_picture_at_rest_with_no_figures_is_an_error_never_a_pass() {
        let mut unrecorded = pair("c--d", "spatial", within(), within());
        unrecorded["views"]["33%"]["at_rest"] = json!({"status": "unknown"});
        let report = judge(Some(&cells(vec![unrecorded])), 1, &options());
        assert_eq!(report["status"], "incomplete", "{report:#}");
        assert_eq!(
            report["errors"][0]["reason"],
            "no picture-at-rest figures recorded"
        );
    }

    #[test]
    fn a_cell_that_could_not_run_makes_the_gate_incomplete_never_a_pass() {
        let mut gap = pair("a--b", "pointwise", within(), within());
        gap["views"]["100%"] = json!({"status": "gap", "reason": "budget-exceeded: too big"});
        let missing = json!({"cell": "c--raw", "class": "spatial", "status": "missing-source",
                             "error": "no file on this host"});
        let report = judge(Some(&cells(vec![gap, missing])), 2, &options());
        assert_eq!(report["status"], "incomplete", "{report:#}");
        assert_eq!(report["gaps"].as_array().unwrap().len(), 2);
        // A run without an adapter measured nothing.
        let skipped = json!({"adapter": null, "skipped": "no GPU adapter", "complete": true,
                             "pairs": []});
        assert_eq!(judge(Some(&skipped), 2, &options())["status"], "incomplete");
        // A selection is never the release gate's pass.
        let selected = Options {
            families: Some(vec!["basic".to_owned()]),
            ..options()
        };
        let report = judge(
            Some(&cells(vec![pair("a--b", "pointwise", within(), within())])),
            1,
            &selected,
        );
        assert_eq!(report["status"], "incomplete");
        assert!(report["error"].as_str().unwrap().contains("families basic"));
    }

    #[test]
    fn a_harness_that_did_not_finish_or_wrote_nothing_fails_the_gate() {
        assert_eq!(judge(None, 1, &options())["status"], "failed");
        let mut unfinished = cells(vec![pair("a--b", "pointwise", within(), within())]);
        unfinished["complete"] = json!(false);
        assert_eq!(judge(Some(&unfinished), 1, &options())["status"], "failed");
        // Fewer stacks than the selection holds.
        let short = cells(vec![pair("a--b", "pointwise", within(), within())]);
        assert_eq!(judge(Some(&short), 2, &options())["status"], "failed");
    }

    #[test]
    fn a_kind_the_gpu_renders_is_judged_by_what_the_harness_measured() {
        let mut measured = pair("a--b", "pointwise", within(), within());
        measured["kinds"]["histogram"] = json!({"status": "measured", "passed": false});
        let report = judge(Some(&cells(vec![measured])), 1, &options());
        assert_eq!(report["status"], "failed");
        assert_eq!(report["missed"][0]["kind"], "histogram");
    }

    /// The histogram's results carry its largest differences over the corpus, the judged earth
    /// mover's distance with its histogram, each with its cell, and how many stacks were counted
    /// exactly; a stack the GPU could not count is a gap, never a pass.
    #[test]
    fn the_histograms_largest_differences_are_reported_with_their_cells() {
        let counted = |bins: f64, clipping: f64, counter: &str, emd: f64| {
            json!({"status": "measured", "passed": true, "worst_bins": bins,
                "worst_clipping": {"counter": counter, "share": clipping},
                "worst_emd": {"side": "luminance", "codes": emd}})
        };
        let mut exact = pair("a--b", "pointwise", within(), within());
        exact["kinds"]["histogram"] = counted(0.0, 0.0, "r0", 0.0);
        let mut off = pair("a--c", "pointwise", within(), within());
        off["kinds"]["histogram"] = counted(0.0002, 0.0001, "any_highlight", 0.01);
        let report = judge(Some(&cells(vec![exact, off])), 2, &options());
        let histogram = rows(&report, Kind::Histogram)[0].clone();
        assert_eq!(histogram["verdict"], "passed", "{histogram:#}");
        assert_eq!(histogram["counts"]["exact"], 1);
        assert_eq!(histogram["counts"]["worst_bins"]["cell"], "a--c");
        assert_eq!(
            (
                histogram["counts"]["worst_emd"]["side"].clone(),
                histogram["counts"]["worst_emd"]["cell"].clone()
            ),
            (json!("luminance"), json!("a--c"))
        );
        assert_eq!(
            histogram["counts"]["worst_clipping"]["counter"],
            "any_highlight"
        );
        let mut gap = pair("a--b", "pointwise", within(), within());
        gap["kinds"]["histogram"] = json!({"status": "gap", "reason": "budget-exceeded"});
        let report = judge(Some(&cells(vec![gap])), 1, &options());
        assert_eq!(report["status"], "incomplete", "{report:#}");
    }

    /// The GPU's export of a stack is judged by its figures against the reference export and by
    /// whether a second GPU export was the same bytes; a stack the GPU could not export there is a
    /// gap with its reason, which makes the run incomplete, never a pass; and the stacks whose
    /// export is the reference's while no render has stored their light are counted beside it.
    #[test]
    fn an_export_is_judged_by_its_figures_and_its_repeat_and_a_gap_is_never_a_pass() {
        let exported = |statistics: Value, repeatable: bool, passed: bool| {
            let mut stack = pair("a--b", "pointwise", within(), within());
            stack["kinds"]["export"] = json!({"status": "measured", "statistics": statistics,
                "repeatable": repeatable, "passed": passed});
            stack
        };
        let report = judge(
            Some(&cells(vec![exported(within(), true, true)])),
            1,
            &options(),
        );
        assert_eq!(report["status"], "passed", "{report:#}");
        let row = rows(&report, Kind::Export)[0].clone();
        assert_eq!(
            (row["within"].clone(), row["verdict"].clone()),
            (json!(1), json!("passed"))
        );
        assert_eq!(row["figures"]["max"]["p99"], 1.0);

        let report = judge(
            Some(&cells(vec![exported(within(), false, false)])),
            1,
            &options(),
        );
        assert_eq!(report["status"], "failed");
        assert_eq!(report["missed"][0]["exceeded"], json!(["repeatable"]));
        let report = judge(
            Some(&cells(vec![exported(
                stats(0.6, 0.5, 1.0, 0.0),
                true,
                false,
            )])),
            1,
            &options(),
        );
        assert_eq!(report["missed"][0]["exceeded"], json!(["mean"]));

        let mut gap = pair("c--d", "spatial", within(), within());
        gap["kinds"]["export"] = json!({"status": "gap",
            "reason": "the reference renders it: tiles-budget"});
        let report = judge(Some(&cells(vec![gap])), 1, &options());
        assert_eq!(report["status"], "incomplete", "{report:#}");
        assert_eq!(rows(&report, Kind::Export)[0]["verdict"], "incomplete");
        assert_eq!(
            report["gaps"][0]["reason"],
            "export: the reference renders it: tiles-budget"
        );
        assert!(markdown(&report).contains("## Cells that could not run here (1)"));
    }

    #[test]
    fn the_command_reads_its_selection_and_reading_and_refuses_what_it_does_not_know() {
        let root = root().unwrap();
        let parse = |args: &[&str]| {
            let mut a = Args(args.iter().map(OsString::from).collect());
            Options::parse(&root, &mut a).map(|options| (options, a.0.len()))
        };
        let (options, left) = parse(&[
            "--output",
            "/tmp/x",
            "--zoom",
            "33,100",
            "--kind",
            "picture-in-motion",
            "--families",
            "detail",
            "--gate-motion",
            "against-reference",
        ])
        .unwrap();
        assert_eq!(left, 0);
        assert_eq!(
            options.views,
            Some(vec!["33%".to_owned(), "100%".to_owned()])
        );
        assert_eq!(options.kinds, Some(vec![Kind::PictureInMotion]));
        assert_eq!(options.gate_motion, Some(GateMotion::AgainstReference));
        assert_eq!(options.fixtures, root.join("fixtures/generated"));
        let (all, _) = parse(&["--output", "/tmp/x", "--zoom", "all", "--kind", "all"]).unwrap();
        assert!(all.views.is_none() && all.kinds.is_none() && all.narrowed().is_empty());
        assert_eq!(all.gate_motion, None, "the picture in motion is not gated");
        assert!(parse(&["--output", "/tmp/x", "--gate-motion", "always"]).is_err());
        assert!(parse(&["--output", "/tmp/x", "--zoom", "25"]).is_err());
        assert!(parse(&["--output", "/tmp/x", "--kind", "picture"]).is_err());
        assert!(parse(&["--output", "/tmp/x", "--frames", "some"]).is_err());
        let corpus = Corpus::load(&root).unwrap();
        let unknown = Options {
            recipes: Some(vec!["sepia".to_owned()]),
            ..options
        };
        assert!(check_selection(&corpus, &unknown).is_err());
        assert_eq!(
            expected_pairs(&corpus, &self::options()),
            corpus.cells() / VIEWS.len()
        );
    }
}
