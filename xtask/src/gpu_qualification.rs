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
//! The picture is two kinds, judged by the design's recorded default of 2026-10-05: **in motion**,
//! the frame a drag draws, against the frame it settles to, its distance from the reference
//! reported beside it and not gated; and **at rest** against the reference. The GPU draws no picture
//! at rest yet, so that kind is reported as not rendered, with its candidate's figures beside it:
//! process-first at Fit, 33% and 50%, the whole output stage drawn on the GPU at full resolution in
//! tiles and reduced as the reference is, and the region plan over the visible window at 100%.
//! `--hold-motion-to-reference` judges the picture in motion against the reference instead, the
//! other reading the owner may choose.
//!
//! A kind the GPU does not render yet is reported as such, never as a pass and never as a failure.
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
    /// Judge the picture in motion against the reference rather than against the frame it settles
    /// to, the recorded default.
    pub hold_motion_to_reference: bool,
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
    /// [--recipes ID,...] [--sources ID,...] [--frames missed|all] [--hold-motion-to-reference]`;
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
        let hold_motion_to_reference = a.flag("--hold-motion-to-reference");
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
            hold_motion_to_reference,
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
    if options.hold_motion_to_reference {
        command.env("LUXFORGE_GPU_QUALIFICATION_MOTION", "reference");
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
    cpu: Extremes,
    /// The picture at rest: the GPU's frame against the reference where a stage draws one, and how
    /// many cells it does not draw.
    at_rest: Judged,
    at_rest_unrendered: usize,
    /// The at-rest candidate: its renderers, figures and gaps, and the motion frame's jump to it.
    candidate: Judged,
    candidate_gaps: usize,
    candidate_renderers: Vec<String>,
    jump: Extremes,
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
    let to_reference = options.hold_motion_to_reference;
    let (motion, at_rest) = (
        kinds.contains(&Kind::PictureInMotion),
        kinds.contains(&Kind::PictureAtRest),
    );
    let (judged_against, information_against) = if to_reference {
        ("the reference", "the frame it settles to")
    } else {
        ("the frame it settles to", "the reference")
    };
    let mut report = json!({
        "format": 1,
        "gate": "gpu-qualification",
        "status": "passed",
        "error": null,
        "reading": {
            "picture_in_motion": judged_against,
            "picture_at_rest": "the reference",
            "hold_motion_to_reference": to_reference,
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
                Kind::Histogram => json!({"share_of_output_pixels": tolerance::HISTOGRAM_FRACTION}),
                _ => json!({
                    "pointwise": limits_value(Class::Pointwise),
                    "spatial": limits_value(Class::Spatial),
                }),
            },
        })).collect::<Vec<_>>(),
        "adapter": null,
        "results": [],
        "missed": [],
        "information": {"against": information_against, "motion_past": []},
        "at_rest_candidate": {"missed": [], "gaps": [], "frames": []},
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
    let (mut candidate_missed, mut candidate_gaps, mut candidate_frames) =
        (Vec::new(), Vec::new(), Vec::new());
    // Per kind other than the pictures: how many stacks it was measured on, passed, missed, and
    // was not rendered on.
    let mut other: Vec<(Kind, [usize; 4])> = kinds
        .iter()
        .filter(|kind| !kind.picture())
        .map(|kind| (*kind, [0; 4]))
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
        if let Some(frame) = pair
            .get("process_first")
            .filter(|frame| frame["status"] == "drawn")
        {
            candidate_frames.push(json!({
                "cell": cell,
                "source": pair["source"],
                "family": pair["family"],
                "tiles": frame["tiles"],
                "clock": frame["clock"],
            }));
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
                let candidate = &measured["at_rest"]["candidate"];
                let past_settled =
                    tally
                        .settled
                        .add(&statistics(&in_motion["against_settled"]), cell, class);
                let past_reference =
                    tally
                        .reference
                        .add(&statistics(&in_motion["against_reference"]), cell, class);
                tally
                    .cpu
                    .add(&statistics(&measured["cpu_against_reference"]), cell);
                if motion {
                    let ((judged_past, judged), (information_past, information)) = if to_reference {
                        (
                            (past_reference, "against_reference"),
                            (past_settled, "against_settled"),
                        )
                    } else {
                        (
                            (past_settled, "against_settled"),
                            (past_reference, "against_reference"),
                        )
                    };
                    let entry = |past: Vec<&'static str>, field: &str| {
                        json!({
                            "kind": Kind::PictureInMotion.name(),
                            "cell": cell,
                            "view": view,
                            "class": class.name(),
                            "exceeded": past,
                            "statistics": in_motion[field],
                            "limits": limits_value(class),
                            "at_rest_candidate": candidate["against_reference"],
                            "cpu_against_reference": measured["cpu_against_reference"],
                            "frames": measured["frames"],
                        })
                    };
                    if !judged_past.is_empty() {
                        let mut entry = entry(judged_past, judged);
                        entry["against"] = json!(judged_against);
                        missed.push(entry);
                    }
                    if !information_past.is_empty() {
                        let mut entry = entry(information_past, information);
                        entry["against"] = json!(information_against);
                        motion_past.push(entry);
                    }
                }
                if at_rest {
                    let rest = &measured["at_rest"];
                    match rest["status"].as_str() {
                        Some("measured") => {
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
                                    "at_rest_candidate": candidate["against_reference"],
                                    "cpu_against_reference": measured["cpu_against_reference"],
                                    "frames": measured["frames"],
                                }));
                            }
                        }
                        Some("not-rendered") => {
                            tally.at_rest_unrendered += 1;
                            note(&mut not_rendered, Kind::PictureAtRest, &rest["reason"]);
                        }
                        _ => errors.push(json!({
                            "cell": cell,
                            "view": view,
                            "reason": "no picture-at-rest figures recorded",
                        })),
                    }
                    if let Some(renderer) = candidate["renderer"].as_str()
                        && !tally.candidate_renderers.iter().any(|r| r == renderer)
                    {
                        tally.candidate_renderers.push(renderer.to_owned());
                    }
                    match candidate["status"].as_str() {
                        Some("measured") => {
                            tally
                                .jump
                                .add(&statistics(&candidate["jump_from_motion"]), cell);
                            let past = tally.candidate.add(
                                &statistics(&candidate["against_reference"]),
                                cell,
                                class,
                            );
                            if !past.is_empty() {
                                candidate_missed.push(json!({
                                    "cell": cell,
                                    "view": view,
                                    "class": class.name(),
                                    "renderer": candidate["renderer"],
                                    "exceeded": past,
                                    "statistics": candidate["against_reference"],
                                    "limits": limits_value(class),
                                }));
                            }
                        }
                        Some(_) => {
                            tally.candidate_gaps += 1;
                            candidate_gaps.push(json!({
                                "cell": cell,
                                "view": view,
                                "reason": candidate["reason"].as_str().unwrap_or("unrecorded"),
                            }));
                        }
                        None => {}
                    }
                }
            }
        }
        for (kind, counts) in &mut other {
            let measured = &pair["kinds"][kind.name()];
            match measured["status"].as_str() {
                Some("measured") => {
                    counts[0] += 1;
                    if measured["passed"] == true {
                        counts[1] += 1;
                    } else {
                        counts[2] += 1;
                        missed.push(json!({
                            "kind": kind.name(), "cell": cell, "view": null,
                            "class": class.name(), "figures": measured,
                        }));
                    }
                }
                Some("not-rendered") => {
                    counts[3] += 1;
                    note(&mut not_rendered, *kind, &measured["reason"]);
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
                let (judged, information) = if to_reference {
                    (&tally.reference, &tally.settled)
                } else {
                    (&tally.settled, &tally.reference)
                };
                let verdict = if judged.past > 0 {
                    "failed"
                } else if tally.gaps + tally.errors > 0 {
                    "incomplete"
                } else {
                    "passed"
                };
                results.push(json!({
                    "kind": Kind::PictureInMotion.name(),
                    "view": view,
                    "class": class.name(),
                    "against": judged_against,
                    "cells": tally.cells,
                    "measured": judged.within + judged.past,
                    "within": judged.within,
                    "past_a_limit": judged.past,
                    "gaps": tally.gaps,
                    "errors": tally.errors,
                    "limits": limits_value(*class),
                    "figures": judged.figures.value(),
                    "verdict": verdict,
                    "information": {
                        "against": information_against,
                        "comparison": information.value(),
                        "cpu_against_reference": tally.cpu.value(),
                    },
                }));
            }
            if at_rest {
                let measured = tally.at_rest.within + tally.at_rest.past;
                let verdict = if tally.at_rest.past > 0 {
                    "failed"
                } else if measured == 0 && tally.at_rest_unrendered > 0 {
                    "not rendered by the GPU yet"
                } else if tally.gaps + tally.errors > 0 {
                    "incomplete"
                } else {
                    "passed"
                };
                let candidate_cells =
                    tally.candidate.within + tally.candidate.past + tally.candidate_gaps;
                results.push(json!({
                    "kind": Kind::PictureAtRest.name(),
                    "view": view,
                    "class": class.name(),
                    "against": "the reference",
                    "cells": tally.cells,
                    "measured": measured,
                    "within": tally.at_rest.within,
                    "past_a_limit": tally.at_rest.past,
                    "not_rendered": tally.at_rest_unrendered,
                    "gaps": tally.gaps,
                    "errors": tally.errors,
                    "limits": limits_value(*class),
                    "figures": tally.at_rest.figures.value(),
                    "verdict": verdict,
                    "candidate": if candidate_cells == 0 {
                        Value::Null
                    } else {
                        json!({
                            "renderer": tally.candidate_renderers.join(", "),
                            "cells": candidate_cells,
                            "within": tally.candidate.within,
                            "past_a_limit": tally.candidate.past,
                            "gaps": tally.candidate_gaps,
                            "against_reference": tally.candidate.figures.value(),
                            "jump_from_motion": tally.jump.value(),
                        })
                    },
                }));
            }
        }
    }
    for (kind, [measured, passed, past, unrendered]) in &other {
        results.push(json!({
            "kind": kind.name(),
            "view": "the whole output stage",
            "measured": measured,
            "within": passed,
            "past_a_limit": past,
            "not_rendered": unrendered,
            "verdict": if *past > 0 {
                "failed"
            } else if *measured == 0 && *unrendered > 0 {
                "not rendered by the GPU yet"
            } else {
                "passed"
            },
        }));
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
    report["at_rest_candidate"] = json!({
        "note": "The candidate for the picture at rest, judged by the same limits and not by the \
                 gate: process-first at Fit, 33% and 50%, the whole output stage drawn on the GPU at \
                 full resolution as 100% region plans over tiles, stitched and reduced to the view's \
                 size as the reference is; at 100% the region plan over the visible window",
        "missed": candidate_missed,
        "gaps": candidate_gaps,
        "frames": candidate_frames,
    });
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
    let judged_against = reading["picture_in_motion"]
        .as_str()
        .unwrap_or("the frame it settles to");
    if reading["hold_motion_to_reference"] == true {
        text.push_str(&format!(
            "Reading: `--hold-motion-to-reference`. The picture in motion, the frame a drag draws, \
             is held to the reference at every view, which the recorded default ({DEFAULT}) does \
             not ask; the picture at rest is held to the reference.\n\n"
        ));
    } else {
        text.push_str(&format!(
            "Reading: the recorded default ({DEFAULT}). The picture in motion, the frame a drag \
             draws, is held to the frame it settles to, its distance from the reference reported \
             beside it and not gated; the picture at rest is held to the reference. \
             `--hold-motion-to-reference` holds the picture in motion to the reference instead.\n\n"
        ));
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
        text.push_str(&format!(
            "\n## The picture in motion, against {judged_against}\n\nThe frame a drag draws at each view, against {judged_against}. {FIGURES}\n\n{table_head}"
        ));
        for result in &motion {
            text.push_str(&table_row(result));
        }
    }
    if !rest.is_empty() {
        text.push_str("\n## The picture at rest, against the reference\n\n");
        let measured = rest.iter().any(|result| result["measured"] != 0);
        if measured {
            text.push_str(&format!("{FIGURES}\n\n{table_head}"));
            for result in &rest {
                text.push_str(&table_row(result));
            }
        } else {
            let reason = not_rendered
                .iter()
                .find(|entry| entry["kind"] == Kind::PictureAtRest.name())
                .and_then(|entry| entry["reason"].as_str())
                .unwrap_or("the GPU does not draw it yet");
            text.push_str(&format!(
                "Not rendered by the GPU yet, neither a pass nor a failure: {reason}.\n"
            ));
        }
        let candidates: Vec<&&Value> = rest
            .iter()
            .filter(|result| !result["candidate"].is_null())
            .collect();
        if !candidates.is_empty() {
            text.push_str("\nIts candidate, judged by the same limits and not by the gate: process-first at Fit, 33% and 50%, the whole output stage drawn on the GPU at full resolution as 100% region plans over tiles, each over its own boundary and reading the exact stage's estimates, stitched and reduced to the view's size as the reference is; at 100% the region plan over the visible window, the motion frame's own. The jump is the motion frame's to it.\n\n| View | Class | Candidate | Cells | Within | Past a limit | Gaps | Max | Mean | Jump from the motion frame: max |\n| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- |\n");
            for result in candidates {
                let candidate = &result["candidate"];
                text.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                    result["view"].as_str().unwrap_or("?"),
                    result["class"].as_str().unwrap_or("?"),
                    candidate["renderer"].as_str().unwrap_or("?"),
                    candidate["cells"],
                    candidate["within"],
                    candidate["past_a_limit"],
                    candidate["gaps"],
                    four(&candidate["against_reference"], "max"),
                    four(&candidate["against_reference"], "mean"),
                    four(&candidate["jump_from_motion"], "max"),
                ));
            }
        }
        let missed = report["at_rest_candidate"]["missed"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for miss in &missed {
            text.push_str(&format!(
                "- The candidate past a limit: {} at {} ({}): {}\n",
                miss["cell"].as_str().unwrap_or("?"),
                miss["view"].as_str().unwrap_or("?"),
                miss["class"].as_str().unwrap_or("?"),
                row(&miss["statistics"])
            ));
        }
        let frames = report["at_rest_candidate"]["frames"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !frames.is_empty() {
            // Per source, the range of a whole frame's time on the harness's own clock.
            let mut sources: Vec<(String, f64, f64, u64)> = Vec::new();
            for frame in &frames {
                let source = frame["source"].as_str().unwrap_or("?").to_owned();
                let total = frame["clock"]["total_s"].as_f64().unwrap_or(f64::NAN);
                let tiles = frame["tiles"].as_u64().unwrap_or(0);
                match sources.iter_mut().find(|(known, ..)| *known == source) {
                    Some((_, low, high, _)) => {
                        *low = low.min(total);
                        *high = high.max(total);
                    }
                    None => sources.push((source, total, total, tiles)),
                }
            }
            text.push_str("\nA process-first frame on the harness's own clock, each tile's boundary rendered on the CPU and its codes read back, programs compiled on first use, on a loaded host: an order of magnitude, not a timing measurement.\n\n| Source | Tiles | Whole frame, fastest to slowest stack |\n| --- | ---: | --- |\n");
            for (source, low, high, tiles) in sources {
                text.push_str(&format!(
                    "| {source} | {tiles} | {} to {} s |\n",
                    number(&json!(low)),
                    number(&json!(high))
                ));
            }
        }
    }
    if !motion.is_empty() {
        let information_against = report["information"]["against"]
            .as_str()
            .unwrap_or("the reference");
        text.push_str(&format!(
            "\n## Information, not judged\n\n### The picture in motion against {information_against}\n\n| View | Class | Within the limits | Past them | Max | Mean |\n| --- | --- | ---: | ---: | --- | --- |\n"
        ));
        for result in &motion {
            let comparison = &result["information"]["comparison"];
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                result["view"].as_str().unwrap_or("?"),
                result["class"].as_str().unwrap_or("?"),
                comparison["within"],
                comparison["past_a_limit"],
                four(&comparison["figures"], "max"),
                four(&comparison["figures"], "mean"),
            ));
        }
        let past = report["information"]["motion_past"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !past.is_empty() {
            text.push_str(&format!(
                "\n#### The motion frame past a limit against {information_against} ({})\n\nEach cell's figures: the motion frame against {information_against}, then the at-rest candidate and the CPU frame of the same view against the reference.\n\n| Cell | View | Class | Past | Motion frame | At-rest candidate | CPU frame |\n| --- | --- | --- | --- | --- | --- | --- |\n",
                past.len()
            ));
            for entry in &past {
                text.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} | {} |\n",
                    entry["cell"].as_str().unwrap_or("?"),
                    entry["view"].as_str().unwrap_or("?"),
                    entry["class"].as_str().unwrap_or("?"),
                    names(&entry["exceeded"]).unwrap_or_default(),
                    row(&entry["statistics"]),
                    row(&entry["at_rest_candidate"]),
                    row(&entry["cpu_against_reference"]),
                ));
            }
        }
        text.push_str("\n### The CPU frame\n\nThe largest magnitude of each statistic: the CPU frame of the view against the reference, what the CPU path shows there.\n\n| View | Class | CPU frame against the reference |\n| --- | --- | --- |\n");
        for result in &motion {
            let information = &result["information"];
            text.push_str(&format!(
                "| {} | {} | {} |\n",
                result["view"].as_str().unwrap_or("?"),
                result["class"].as_str().unwrap_or("?"),
                four(&information["cpu_against_reference"], "max"),
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
        text.push_str("\n## The other output kinds\n\n| Kind | Measured | Within | Past a limit | Not rendered by the GPU | Verdict |\n| --- | ---: | ---: | ---: | ---: | --- |\n");
        for result in others {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                result["kind"].as_str().unwrap_or("?"),
                result["measured"],
                result["within"],
                result["past_a_limit"],
                result["not_rendered"],
                result["verdict"].as_str().unwrap_or("?"),
            ));
        }
    }
    let missed = report["missed"].as_array().cloned().unwrap_or_default();
    if !missed.is_empty() {
        text.push_str(&format!(
            "\n## Past a limit ({})\n\nEach cell the gate judges past its limit: its figures against what it is judged by, then the at-rest candidate and the CPU frame of the same view against the reference.\n\n| Cell | View | Kind | Against | Class | Past | Figures | At-rest candidate | CPU frame |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
            missed.len()
        ));
        for miss in &missed {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                miss["cell"].as_str().unwrap_or("?"),
                miss["view"].as_str().unwrap_or("whole stage"),
                miss["kind"].as_str().unwrap_or("?"),
                miss["against"].as_str().unwrap_or("its reference"),
                miss["class"].as_str().unwrap_or("?"),
                names(&miss["exceeded"]).unwrap_or_else(|| "its limit".to_owned()),
                row(&miss["statistics"]),
                row(&miss["at_rest_candidate"]),
                row(&miss["cpu_against_reference"]),
            ));
        }
    }
    for (title, entries) in [
        ("Cells that could not run here", &report["gaps"]),
        ("Cells that failed to run", &report["errors"]),
        (
            "At-rest candidates that could not be drawn here",
            &report["at_rest_candidate"]["gaps"],
        ),
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
            hold_motion_to_reference: false,
        }
    }

    fn stats(mean: f64, worst_block: f64, p99: f64, mean_delta_l: f64) -> Value {
        json!({"pixels": 100, "mean": mean, "worst_block": worst_block, "p99": p99,
               "mean_delta_l": mean_delta_l, "max": p99})
    }

    /// A view's record: the motion frame against the frame it settles to and against the
    /// reference, the at-rest kind not rendered with its candidate.
    fn view(settled: Value, reference: Value) -> Value {
        json!({
            "status": "measured",
            "in_motion": {"settles_to": "the CPU's proxy frame", "against_settled": settled,
                          "program_against_settled": settled, "against_reference": reference},
            "at_rest": {"status": "not-rendered", "reason": "stage 2's",
                        "candidate": {"renderer": "process-first", "status": "measured",
                                      "against_reference": stats(0.01, 0.05, 0.1, 0.0),
                                      "jump_from_motion": reference}},
            "cpu_against_reference": reference,
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
        assert_eq!(
            report["reading"]["picture_in_motion"],
            "the frame it settles to"
        );
        // Every view and class is a row of each picture kind.
        let motion = rows(&report, Kind::PictureInMotion);
        assert_eq!(motion.len(), VIEWS.len() * 2);
        assert_eq!(motion[0]["figures"]["max"]["worst_block"], 0.5);
        assert_eq!(motion[0]["figures"]["mean"]["mean_delta_l"], -0.1);
        // The picture at rest and the other kinds are not rendered by the GPU, neither passed nor
        // failed, the at-rest candidate's figures beside the picture.
        let rest = rows(&report, Kind::PictureAtRest);
        assert_eq!(rest.len(), VIEWS.len() * 2);
        assert!(
            rest.iter()
                .all(|r| r["verdict"] == "not rendered by the GPU yet")
        );
        assert_eq!(rest[0]["candidate"]["within"], 1);
        assert_eq!(rest[0]["candidate"]["renderer"], "process-first");
        for kind in [Kind::Histogram, Kind::Sample, Kind::Export] {
            assert_eq!(
                rows(&report, kind)[0]["verdict"],
                "not rendered by the GPU yet"
            );
        }
        assert_eq!(report["not_rendered"].as_array().unwrap().len(), 4);
        let summary = markdown(&report);
        assert!(summary.contains("**PASSED**") && summary.contains("No: stage 2's"));
        assert!(summary.contains("Reading: the recorded default"));
        assert!(summary.contains("## The picture in motion, against the frame it settles to"));
    }

    #[test]
    fn a_motion_frame_past_its_limit_of_the_frame_it_settles_to_fails_the_gate() {
        let report = judge(
            Some(&cells(vec![pair(
                "c--d",
                "spatial",
                stats(0.9, 2.6, 4.0, 0.0),
                within(),
            )])),
            1,
            &options(),
        );
        assert_eq!(report["status"], "failed");
        let missed = report["missed"].as_array().unwrap();
        assert_eq!(missed.len(), VIEWS.len(), "one miss per view");
        assert_eq!(missed[0]["exceeded"], json!(["worst_block"]));
        assert_eq!(missed[0]["against"], "the frame it settles to");
        assert_eq!(missed[0]["at_rest_candidate"]["worst_block"], 0.05);
        assert!(report["error"].as_str().unwrap().contains("c--d"));
        assert!(markdown(&report).contains(
            "| c--d | fit | picture-in-motion | the frame it settles to | spatial | worst_block | 0.900 / 2.60 / 4.00 / 0.000 | 0.010 / 0.050 / 0.100 / 0.000 |"
        ));
        // A NaN, which the harness writes as null, is past every limit.
        let report = judge(
            Some(&cells(vec![pair(
                "a--b",
                "pointwise",
                json!({"mean": null, "worst_block": 0.0, "p99": 0.0, "mean_delta_l": 0.0}),
                within(),
            )])),
            1,
            &options(),
        );
        assert_eq!(report["status"], "failed");
        assert!(markdown(&report).contains("**FAILED**"));
    }

    #[test]
    fn a_motion_frame_far_from_the_reference_is_reported_and_gated_only_when_asked() {
        let far = stats(1.3, 4.1, 3.8, 0.3);
        let measured = cells(vec![pair("e--f", "spatial", within(), far.clone())]);
        // By the recorded default the distance from the reference is information.
        let report = judge(Some(&measured), 1, &options());
        assert_eq!(report["status"], "passed", "{report:#}");
        let past = report["information"]["motion_past"].as_array().unwrap();
        assert_eq!(past.len(), VIEWS.len());
        assert_eq!(past[0]["exceeded"], json!(["mean", "worst_block"]));
        let information = &rows(&report, Kind::PictureInMotion)[0]["information"];
        assert_eq!(information["against"], "the reference");
        assert_eq!(information["comparison"]["past_a_limit"], 1);
        let summary = markdown(&report);
        assert!(summary.contains("#### The motion frame past a limit against the reference (4)"));
        // Held to the reference, the same cells fail the gate, and the frame they settle to is
        // the information.
        let held = Options {
            hold_motion_to_reference: true,
            ..options()
        };
        let report = judge(Some(&measured), 1, &held);
        assert_eq!(report["status"], "failed");
        assert_eq!(report["missed"].as_array().unwrap().len(), VIEWS.len());
        assert_eq!(report["missed"][0]["against"], "the reference");
        assert_eq!(report["reading"]["hold_motion_to_reference"], true);
        assert_eq!(
            rows(&report, Kind::PictureInMotion)[0]["information"]["against"],
            "the frame it settles to"
        );
        assert!(markdown(&report).contains("Reading: `--hold-motion-to-reference`"));
    }

    #[test]
    fn an_at_rest_frame_the_gpu_draws_is_judged_against_the_reference() {
        let mut drawn = pair("g--h", "pointwise", within(), within());
        drawn["views"]["fit"]["at_rest"]["status"] = json!("measured");
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
    fn the_at_rest_candidate_is_reported_beside_the_kind_but_never_gated() {
        let mut both = pair("c--d", "spatial", within(), within());
        both["views"]["33%"]["at_rest"]["candidate"]["against_reference"] =
            stats(1.2, 0.05, 0.1, 0.0);
        both["views"]["50%"]["at_rest"]["candidate"] = json!({"renderer": "process-first",
            "status": "gap", "reason": "the tile at (0, 0): budget-exceeded"});
        both["process_first"] = json!({"status": "drawn", "tiles": 6, "clock": {"total_s": 0.5}});
        let report = judge(Some(&cells(vec![both])), 1, &options());
        assert_eq!(report["status"], "passed", "{report:#}");
        let candidate = |view: &str| {
            rows(&report, Kind::PictureAtRest)
                .into_iter()
                .find(|r| r["view"] == view)
                .unwrap()["candidate"]
                .clone()
        };
        assert_eq!(candidate("fit")["within"], 1);
        assert_eq!(candidate("33%")["past_a_limit"], 1);
        assert_eq!(candidate("50%")["gaps"], 1);
        let candidates = &report["at_rest_candidate"];
        assert_eq!(candidates["missed"][0]["exceeded"], json!(["mean"]));
        assert_eq!(candidates["gaps"].as_array().unwrap().len(), 1);
        assert_eq!(candidates["frames"][0]["tiles"], 6);
        let summary = markdown(&report);
        assert!(summary.contains("Its candidate, judged by the same limits and not by the gate"));
        assert!(summary.contains("not a timing measurement"));
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
            "--hold-motion-to-reference",
        ])
        .unwrap();
        assert_eq!(left, 0);
        assert_eq!(
            options.views,
            Some(vec!["33%".to_owned(), "100%".to_owned()])
        );
        assert_eq!(options.kinds, Some(vec![Kind::PictureInMotion]));
        assert!(options.hold_motion_to_reference);
        assert_eq!(options.fixtures, root.join("fixtures/generated"));
        let (all, _) = parse(&["--output", "/tmp/x", "--zoom", "all", "--kind", "all"]).unwrap();
        assert!(all.views.is_none() && all.kinds.is_none() && all.narrowed().is_empty());
        assert!(!all.hold_motion_to_reference, "the recorded default");
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
