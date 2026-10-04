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
//! At Fit, 33% and 50% the report holds two candidates for the picture, each judged by the same
//! limits: the frame a drag draws there, which processes a source reduced to the view's size, and
//! process-first, the whole output stage drawn on the GPU at full resolution in tiles and reduced as
//! the reference is (the owner chooses the picture at rest between them). The gate judges the
//! first, the frame the GPU draws on this branch; the second, and the first's jump to it, are
//! reported beside it.
//!
//! A kind the GPU does not render yet is reported as such, never as a pass and never as a failure.
//! The gate fails (an ordinary failure) when any cell passes a limit, when the harness did not run
//! to its end, or when a source's SHA-256 changed during the run; it is incomplete (exit 3) when
//! any selected cell could not be run — a source missing on this host, a stack the GPU cannot draw
//! there yet, an error — when there is no GPU adapter, or when the run measured a selection
//! narrower than the whole corpus. Only a run of everything, every cell within its limit, passes.
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
    /// [--kind picture|histogram|sample|export|all] [--families F,...] [--recipes ID,...]
    /// [--sources ID,...] [--frames missed|all]`; `--zoom` and `--kind` take a comma-separated
    /// list too.
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
                                "--kind is picture, histogram, sample, export or all, not {name}"
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
    let binary_sha256 = hash(&harness.executable)?;
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
    report["build"] = json!({
        "revision": output(root, "git", &["rev-parse", "HEAD"]).map(|text| text.trim().to_owned()).ok(),
        "working_tree_dirty": output(root, "git", &["status", "--porcelain"]).map(|text| !text.trim().is_empty()).ok(),
        "target": host(root).ok(),
        "profile": "release",
        "harness_binary": harness.executable,
        "harness_binary_sha256": binary_sha256,
        "lock_sha256": hash(&root.join("Cargo.lock"))?,
    });
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

/// What one view and class of the picture holds over the corpus.
#[derive(Default)]
struct Tally {
    cells: usize,
    passed: usize,
    missed: usize,
    gaps: usize,
    errors: usize,
    against: Extremes,
    cpu: Extremes,
    drag: Extremes,
    settled_cpu: Extremes,
    settled_gpu: Extremes,
    /// Candidate 2, process-first, at a view the reference is reduced for: judged by the same
    /// limits, not by the gate.
    second_cells: usize,
    second_passed: usize,
    second_missed: usize,
    second_gaps: usize,
    second: Extremes,
    jump: Extremes,
}

/// Judge the harness's `cells.json` (`None` when it wrote none) against every recorded limit,
/// `expected` stacks being what the selection holds. Answers the report without its build and
/// corpus identity, which the caller adds.
pub fn judge(cells: Option<&Value>, expected: usize, options: &Options) -> Value {
    let views: Vec<String> = options
        .views
        .clone()
        .unwrap_or_else(|| VIEWS.iter().map(|id| (*id).to_owned()).collect());
    let kinds: Vec<Kind> = options.kinds.clone().unwrap_or_else(|| Kind::ALL.to_vec());
    let mut report = json!({
        "format": 1,
        "gate": "gpu-qualification",
        "status": "passed",
        "error": null,
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
        "gaps": [],
        "errors": [],
        "not_rendered": [],
        "process_first": {"missed": [], "gaps": [], "frames": []},
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
    let (mut second_missed, mut second_gaps, mut second_frames) =
        (Vec::new(), Vec::new(), Vec::new());
    // Per kind other than the picture: how many stacks it was measured on, passed, missed, and was
    // not rendered on.
    let mut other: Vec<(Kind, [usize; 4])> = kinds
        .iter()
        .filter(|kind| **kind != Kind::Picture)
        .map(|kind| (*kind, [0; 4]))
        .collect();
    let mut not_rendered: Vec<(Kind, String)> = Vec::new();
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
            second_frames.push(json!({
                "cell": cell,
                "source": pair["source"],
                "family": pair["family"],
                "tiles": frame["tiles"],
                "clock": frame["clock"],
            }));
        }
        if kinds.contains(&Kind::Picture) {
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
                let against = statistics(&measured["statistics"]);
                tally.against.add(&against, cell);
                tally
                    .cpu
                    .add(&statistics(&measured["cpu_against_reference"]), cell);
                tally
                    .drag
                    .add(&statistics(&measured["drag"]["statistics"]), cell);
                if let Some(settled) = measured.get("settled_from_exact") {
                    tally
                        .settled_cpu
                        .add(&statistics(&settled["cpu_proxy"]), cell);
                    tally.settled_gpu.add(&statistics(&settled["gpu"]), cell);
                }
                let second = &measured["process_first"];
                match second["status"].as_str() {
                    None => {}
                    Some("measured") => {
                        tally.second_cells += 1;
                        let figures = statistics(&second["statistics"]);
                        tally.second.add(&figures, cell);
                        tally
                            .jump
                            .add(&statistics(&second["jump_from_motion"]), cell);
                        let past = exceeded(&figures, class);
                        if past.is_empty() {
                            tally.second_passed += 1;
                        } else {
                            tally.second_missed += 1;
                            second_missed.push(json!({
                                "cell": cell,
                                "view": view,
                                "class": class.name(),
                                "exceeded": past,
                                "statistics": second["statistics"],
                                "limits": limits_value(class),
                            }));
                        }
                    }
                    Some(_) => {
                        tally.second_cells += 1;
                        tally.second_gaps += 1;
                        second_gaps.push(json!({
                            "cell": cell,
                            "view": view,
                            "reason": second["reason"].as_str().unwrap_or("unrecorded"),
                        }));
                    }
                }
                let past = exceeded(&against, class);
                if past.is_empty() {
                    tally.passed += 1;
                } else {
                    tally.missed += 1;
                    missed.push(json!({
                        "kind": "picture",
                        "cell": cell,
                        "view": view,
                        "class": class.name(),
                        "exceeded": past,
                        "statistics": measured["statistics"],
                        "limits": limits_value(class),
                        "cpu_against_reference": measured["cpu_against_reference"],
                        "frames": measured["frames"],
                    }));
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
                    if !not_rendered.iter().any(|(k, _)| k == kind) {
                        not_rendered.push((
                            *kind,
                            measured["reason"].as_str().unwrap_or_default().to_owned(),
                        ));
                    }
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
    if kinds.contains(&Kind::Picture) {
        for (view_index, view) in views.iter().enumerate() {
            for (class_index, class) in classes.iter().enumerate() {
                let tally = &tallies[view_index][class_index];
                if tally.cells == 0 {
                    continue;
                }
                let verdict = if tally.missed > 0 {
                    "failed"
                } else if tally.gaps + tally.errors > 0 {
                    "incomplete"
                } else {
                    "passed"
                };
                results.push(json!({
                    "kind": "picture",
                    "view": view,
                    "class": class.name(),
                    "cells": tally.cells,
                    "measured": tally.passed + tally.missed,
                    "within": tally.passed,
                    "past_a_limit": tally.missed,
                    "gaps": tally.gaps,
                    "errors": tally.errors,
                    "limits": limits_value(*class),
                    "against_reference": tally.against.value(),
                    "verdict": verdict,
                    "information": {
                        "cpu_against_reference": tally.cpu.value(),
                        "gpu_against_cpu": tally.drag.value(),
                        "settle_jump_cpu_proxy": tally.settled_cpu.value(),
                        "settle_jump_gpu": tally.settled_gpu.value(),
                    },
                    "process_first": if tally.second_cells == 0 {
                        Value::Null
                    } else {
                        json!({
                            "cells": tally.second_cells,
                            "within": tally.second_passed,
                            "past_a_limit": tally.second_missed,
                            "gaps": tally.second_gaps,
                            "against_reference": tally.second.value(),
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
            "{} cell(s) pass a limit, the first {} at {} ({})",
            missed.len(),
            miss["cell"].as_str().unwrap_or("?"),
            miss["view"].as_str().unwrap_or("the whole stage"),
            miss["exceeded"]
                .as_array()
                .map(|names| names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_else(|| miss["kind"].as_str().unwrap_or("?").to_owned())
        )
    });
    report["missed"] = json!(missed);
    report["gaps"] = json!(gaps);
    report["errors"] = json!(errors);
    report["process_first"] = json!({
        "note": "Candidate 2 for the picture at rest, judged by the same limits and not by the gate: \
                 the whole output stage drawn on the GPU at full resolution as 100% region plans \
                 over tiles, stitched and reduced to the view's size as the reference is",
        "missed": second_missed,
        "gaps": second_gaps,
        "frames": second_frames,
    });
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
        report["build"]["harness_binary_sha256"].as_str().unwrap_or("unknown"),
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
    let pictures: Vec<&Value> = results.iter().filter(|r| r["kind"] == "picture").collect();
    if !pictures.is_empty() {
        text.push_str(
            "\n## The picture against the reference\n\nEach cell's mean ΔE00 / worst 16 × 16 block / p99 ΔE00 / signed mean ΔL\\*: the largest magnitude over the measured cells, their mean, and the limit (ΔL\\* within ±).\n\n| View | Class | Cells | Within | Past a limit | Gaps | Errors | Max | Mean | Limit | Verdict |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- |\n",
        );
        for result in &pictures {
            let limits = &result["limits"];
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} / {} / {} / {} | {} |\n",
                result["view"].as_str().unwrap_or("?"),
                result["class"].as_str().unwrap_or("?"),
                result["cells"],
                result["within"],
                result["past_a_limit"],
                result["gaps"],
                result["errors"],
                four(&result["against_reference"], "max"),
                four(&result["against_reference"], "mean"),
                number(&limits["mean"]),
                number(&limits["worst_block"]),
                number(&limits["p99"]),
                number(&limits["mean_delta_l_abs"]),
                result["verdict"].as_str().unwrap_or("?"),
            ));
        }
        text.push_str(
            "\n### Information, not judged\n\nThe largest magnitude of each statistic: the CPU frame of the same view against the reference (what the CPU path shows there), the GPU frame against that CPU frame (the drag-time comparison the program qualification makes), and at Fit the jump a stack that settles from the exact render makes, its CPU proxy and its GPU frame against the frame it settles to.\n\n| View | Class | CPU against the reference | GPU against the CPU frame | Settle jump: CPU proxy | Settle jump: GPU |\n| --- | --- | --- | --- | --- | --- |\n",
        );
        for result in &pictures {
            let information = &result["information"];
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                result["view"].as_str().unwrap_or("?"),
                result["class"].as_str().unwrap_or("?"),
                four(&information["cpu_against_reference"], "max"),
                four(&information["gpu_against_cpu"], "max"),
                four(&information["settle_jump_cpu_proxy"], "max"),
                four(&information["settle_jump_gpu"], "max"),
            ));
        }
    }
    let seconds: Vec<&Value> = pictures
        .iter()
        .copied()
        .filter(|result| !result["process_first"].is_null())
        .collect();
    if !seconds.is_empty() {
        text.push_str(
            "\n## Candidate 2 for the picture at rest: process-first\n\nThe whole output stage drawn on the GPU at full resolution as 100% region plans over tiles, each over its own boundary and reading the exact stage's estimates, stitched and reduced to the view's size as the reference is; judged by the same limits, not by the gate. The jump is the motion frame's (the first candidate's) to it.\n\n| View | Class | Cells | Within | Past a limit | Gaps | Max | Mean | Jump from the motion frame: max |\n| --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- |\n",
        );
        for result in seconds {
            let second = &result["process_first"];
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                result["view"].as_str().unwrap_or("?"),
                result["class"].as_str().unwrap_or("?"),
                second["cells"],
                second["within"],
                second["past_a_limit"],
                second["gaps"],
                four(&second["against_reference"], "max"),
                four(&second["against_reference"], "mean"),
                four(&second["jump_from_motion"], "max"),
            ));
        }
        let missed = report["process_first"]["missed"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for miss in &missed {
            text.push_str(&format!(
                "- Past a limit: {} at {} ({}): {}\n",
                miss["cell"].as_str().unwrap_or("?"),
                miss["view"].as_str().unwrap_or("?"),
                miss["class"].as_str().unwrap_or("?"),
                STATISTICS
                    .iter()
                    .map(|name| format!("{name} {}", number(&miss["statistics"][name])))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let frames = report["process_first"]["frames"]
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
    let others: Vec<&Value> = results.iter().filter(|r| r["kind"] != "picture").collect();
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
            "\n## Past a limit ({})\n\nEach cell's figures against the reference, then the CPU frame of the same view against the reference.\n\n| Cell | View | Class | Past | Mean / block / p99 / ΔL\\* | CPU: mean / block / p99 / ΔL\\* |\n| --- | --- | --- | --- | --- | --- |\n",
            missed.len()
        ));
        for miss in &missed {
            let row = |value: &Value| {
                STATISTICS
                    .iter()
                    .map(|name| number(&value[name]))
                    .collect::<Vec<_>>()
                    .join(" / ")
            };
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                miss["cell"].as_str().unwrap_or("?"),
                miss["view"].as_str().unwrap_or("whole stage"),
                miss["class"].as_str().unwrap_or("?"),
                miss["exceeded"]
                    .as_array()
                    .map(|names| names
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_else(|| miss["kind"].as_str().unwrap_or("?").to_owned()),
                row(&miss["statistics"]),
                row(&miss["cpu_against_reference"]),
            ));
        }
    }
    for (title, entries) in [
        ("Cells that could not run here", &report["gaps"]),
        ("Cells that failed to run", &report["errors"]),
        (
            "Process-first frames that could not be drawn here",
            &report["process_first"]["gaps"],
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
        }
    }

    fn stats(mean: f64, worst_block: f64, p99: f64, mean_delta_l: f64) -> Value {
        json!({"pixels": 100, "mean": mean, "worst_block": worst_block, "p99": p99,
               "mean_delta_l": mean_delta_l, "max": p99})
    }

    fn view(statistics: Value) -> Value {
        json!({"status": "measured", "statistics": statistics,
               "cpu_against_reference": stats(0.0, 0.0, 0.0, 0.0),
               "drag": {"statistics": stats(0.01, 0.1, 0.2, 0.0)}, "frames": []})
    }

    fn not_rendered() -> Value {
        json!({
            "histogram": {"status": "not-rendered", "reason": "stage 2's"},
            "sample": {"status": "not-rendered", "reason": "stage 4's"},
            "export": {"status": "not-rendered", "reason": "stage 4's"},
        })
    }

    /// One stack measured at every view with `statistics`.
    fn pair(cell: &str, class: &str, statistics: Value) -> Value {
        let views: serde_json::Map<String, Value> = VIEWS
            .iter()
            .map(|id| ((*id).to_owned(), view(statistics.clone())))
            .collect();
        json!({"cell": cell, "class": class, "status": "measured", "views": views,
               "kinds": not_rendered()})
    }

    fn cells(pairs: Vec<Value>) -> Value {
        json!({"format": 1, "adapter": "Test (Metal)", "skipped": null, "complete": true,
               "pairs": pairs})
    }

    #[test]
    fn the_gate_passes_only_when_every_cell_the_gpu_renders_is_within_its_limit() {
        let report = judge(
            Some(&cells(vec![
                pair("a--b", "pointwise", stats(0.1, 0.5, 1.0, -0.1)),
                pair("c--d", "spatial", stats(0.9, 2.4, 4.0, 0.4)),
            ])),
            2,
            &options(),
        );
        assert_eq!(report["status"], "passed", "{report:#}");
        // Every view and class is a row of the picture, with its maximum and mean.
        let rows: Vec<&Value> = report["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "picture")
            .collect();
        assert_eq!(rows.len(), VIEWS.len() * 2);
        assert_eq!(rows[0]["against_reference"]["max"]["worst_block"], 0.5);
        assert_eq!(rows[0]["against_reference"]["mean"]["mean_delta_l"], -0.1);
        // The kinds the GPU does not render are reported as such, neither passed nor failed.
        for kind in ["histogram", "sample", "export"] {
            let row = report["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["kind"] == kind)
                .unwrap();
            assert_eq!(row["verdict"], "not rendered by the GPU yet");
        }
        assert_eq!(report["not_rendered"].as_array().unwrap().len(), 3);
        let summary = markdown(&report);
        assert!(summary.contains("**PASSED**") && summary.contains("No: stage 2's"));
    }

    #[test]
    fn a_cell_past_any_limit_fails_the_gate_and_is_listed_with_what_it_passed() {
        // The spatial worst block just past 2.5.
        let report = judge(
            Some(&cells(vec![
                pair("a--b", "pointwise", stats(0.1, 0.5, 1.0, 0.0)),
                pair("c--d", "spatial", stats(0.9, 2.6, 4.0, 0.0)),
            ])),
            2,
            &options(),
        );
        assert_eq!(report["status"], "failed");
        let missed = report["missed"].as_array().unwrap();
        assert_eq!(missed.len(), VIEWS.len(), "one miss per view");
        assert_eq!(missed[0]["exceeded"], json!(["worst_block"]));
        assert!(
            report["error"].as_str().unwrap().contains("c--d"),
            "{}",
            report["error"]
        );
        // A NaN, which the harness writes as null, is past every limit.
        let report = judge(
            Some(&cells(vec![pair(
                "a--b",
                "pointwise",
                json!({"mean": null, "worst_block": 0.0, "p99": 0.0, "mean_delta_l": 0.0}),
            )])),
            1,
            &options(),
        );
        assert_eq!(report["status"], "failed");
        assert!(markdown(&report).contains("**FAILED**"));
    }

    #[test]
    fn a_cell_that_could_not_run_makes_the_gate_incomplete_never_a_pass() {
        let mut gap = pair("a--b", "pointwise", stats(0.1, 0.5, 1.0, 0.0));
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
            Some(&cells(vec![pair(
                "a--b",
                "pointwise",
                stats(0.1, 0.5, 1.0, 0.0),
            )])),
            1,
            &selected,
        );
        assert_eq!(report["status"], "incomplete");
        assert!(report["error"].as_str().unwrap().contains("families basic"));
    }

    #[test]
    fn a_harness_that_did_not_finish_or_wrote_nothing_fails_the_gate() {
        assert_eq!(judge(None, 1, &options())["status"], "failed");
        let mut unfinished = cells(vec![pair("a--b", "pointwise", stats(0.1, 0.5, 1.0, 0.0))]);
        unfinished["complete"] = json!(false);
        assert_eq!(judge(Some(&unfinished), 1, &options())["status"], "failed");
        // Fewer stacks than the selection holds.
        let short = cells(vec![pair("a--b", "pointwise", stats(0.1, 0.5, 1.0, 0.0))]);
        assert_eq!(judge(Some(&short), 2, &options())["status"], "failed");
    }

    #[test]
    fn the_process_first_candidate_is_reported_beside_the_motion_frame_but_not_gated() {
        let mut both = pair("c--d", "spatial", stats(0.9, 2.4, 4.0, 0.0));
        both["process_first"] = json!({"status": "drawn", "tiles": 6,
                                      "clock": {"total_s": 0.5}});
        both["views"]["fit"]["process_first"] = json!({"status": "measured",
            "statistics": stats(0.01, 0.05, 0.1, 0.0),
            "jump_from_motion": stats(0.9, 2.4, 4.0, 0.0)});
        both["views"]["33%"]["process_first"] = json!({"status": "measured",
            "statistics": stats(1.2, 0.05, 0.1, 0.0),
            "jump_from_motion": stats(0.9, 2.4, 4.0, 0.0)});
        both["views"]["50%"]["process_first"] = json!({"status": "gap",
            "reason": "the tile at (0, 0): budget-exceeded"});
        let report = judge(Some(&cells(vec![both])), 1, &options());
        // The first candidate is within its limits, so the gate passes whatever the second does.
        assert_eq!(report["status"], "passed", "{report:#}");
        let row = |view: &str| {
            report["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["kind"] == "picture" && r["view"] == view)
                .unwrap()["process_first"]
                .clone()
        };
        assert_eq!(row("fit")["within"], 1);
        assert_eq!(row("33%")["past_a_limit"], 1);
        assert_eq!(row("50%")["gaps"], 1);
        assert!(row("100%").is_null(), "no second candidate at 100%");
        assert_eq!(
            report["process_first"]["missed"][0]["exceeded"],
            json!(["mean"])
        );
        assert_eq!(report["process_first"]["frames"][0]["tiles"], 6);
        let summary = markdown(&report);
        assert!(
            summary.contains("Candidate 2 for the picture at rest"),
            "{summary}"
        );
        assert!(summary.contains("not a timing measurement"));
    }

    #[test]
    fn a_kind_the_gpu_renders_is_judged_by_what_the_harness_measured() {
        let mut measured = pair("a--b", "pointwise", stats(0.1, 0.5, 1.0, 0.0));
        measured["kinds"]["histogram"] = json!({"status": "measured", "passed": false});
        let report = judge(Some(&cells(vec![measured])), 1, &options());
        assert_eq!(report["status"], "failed");
        assert_eq!(report["missed"][0]["kind"], "histogram");
    }

    #[test]
    fn the_command_reads_its_selection_and_refuses_what_it_does_not_know() {
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
            "picture",
            "--families",
            "detail",
        ])
        .unwrap();
        assert_eq!(left, 0);
        assert_eq!(
            options.views,
            Some(vec!["33%".to_owned(), "100%".to_owned()])
        );
        assert_eq!(options.kinds, Some(vec![Kind::Picture]));
        assert_eq!(options.fixtures, root.join("fixtures/generated"));
        let (all, _) = parse(&["--output", "/tmp/x", "--zoom", "all", "--kind", "all"]).unwrap();
        assert!(all.views.is_none() && all.kinds.is_none() && all.narrowed().is_empty());
        assert!(parse(&["--output", "/tmp/x", "--zoom", "25"]).is_err());
        assert!(parse(&["--output", "/tmp/x", "--kind", "frame"]).is_err());
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
