//! `cargo xtask hdr-merge-measure`: what an HDR merge costs at photo size
//! (`docs/design/hdr-merge.md`, "Evidence and acceptance"), in one report.
//!
//! ```text
//! cargo run --release --locked --package xtask -- hdr-merge-measure --output NEW_DIR \
//!     [--workloads 24mp-3,60mp-9,24mp-9,60mp-3] [--cold N] [--warm N] [--cancellations N]
//!     [--rounds N] [--binary PATH] [--natural DIR]... [--only STEP,...] [--scale tiny|full]
//! ```
//!
//! The run writes each workload's bracket (`luxforge_testkit::hdr_bracket` at 24 or 60 MP, three
//! or nine handheld frames with a moving ball, [`Workload::spec`]) under `<output>/scratch`, then,
//! for each workload in turn, takes these steps, each in worker processes of its own
//! ([`worker`]), each merge into a new catalog through the JSON API's methods on an owner in the
//! worker's process:
//!
//! 1. `cold`: `--cold` processes, one merge each, the first in its process.
//! 2. `warm`: one process: a first merge, then `--warm` merges, `--cancellations` merges cancelled
//!    at each stage (as the second input is read, as alignment begins, half way through the
//!    chunks, as the last chunk is written), and editor work (`browse.rows` of 200, a
//!    `render.sample`, a Basic commit and a JPEG export of a 24 MP photograph) alone and beside
//!    merges in the same owner, `--rounds` samples of each.
//! 3. `reopen`: merge copies of the inputs, undo and redo the merge; move the copies away and
//!    reopen the catalog in a new process: every input missing, undo and redo, prepare and export
//!    the result. No merge may appear on the activity board and no saved object may change: a
//!    failure is a failed step.
//!
//! Then `editor-drag`: `editor-latency`'s Basic drag on the 24 MP JPEG in the release editor,
//! alone and while a worker merges the largest workload back to back (skipped without the
//! editor), and `natural`: each `--natural` folder indexed, read only, in a new catalog, the
//! brackets Select finds there and up to three merged with their diagnostics.
//!
//! Each merge's stages come from its job's phases on the activity board, seen as they change:
//! `reading` (from the request, with its queueing, to `aligning`: each input read, hashed,
//! developed and spooled), `aligning` (proxies, registration, coverage, motion), `merging` (every
//! chunk fused and written) and `saving` (the last chunk's and the manifest's writes, the
//! signature checks and the owner's publication). Every 100 ms while it runs the worker reads
//! `resources.read` (the process's memory) and `job.read` (the job's scratch, temporary bytes and
//! development). Scratch is the job's own accounting against its 256 MiB limit; the process's
//! memory is everything else beside it, and neither bounds the other.
//!
//! The run holds the host-wide timing lock and records the load at each step's start and end;
//! above the threshold a figure is `unreliable`. `--scale tiny` runs the test-size bracket
//! (1520 × 1008) through every step in a few minutes, to prove the harness; it claims nothing.
mod session;
#[cfg(test)]
mod tests;
pub mod worker;

use crate::{editor_latency, launch, stats, *};
use luxforge_testkit::hdr_bracket::{Bracket, BracketSpec, PhotoSize};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use worker::{CancelAt, Contention, Job, Mode};

/// The report's files in the output directory.
pub const JSON: &str = "hdr-merge-measure.json";
pub const MARKDOWN: &str = "hdr-merge-measure.md";
const MIB: f64 = 1024.0 * 1024.0;

/// How large the brackets are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// The workloads' photo sizes.
    Full,
    /// The test size, for proving the harness; claims nothing.
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
}

/// One workload: a bracket's size and frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Workload {
    pub size: PhotoSize,
    pub frames: usize,
}

impl Workload {
    /// The order the run takes them in when `--workloads` does not say.
    pub const DEFAULT: [Self; 4] = [
        Self::new(PhotoSize::Mp24, 3),
        Self::new(PhotoSize::Mp60, 9),
        Self::new(PhotoSize::Mp24, 9),
        Self::new(PhotoSize::Mp60, 3),
    ];

    pub const fn new(size: PhotoSize, frames: usize) -> Self {
        Self { size, frames }
    }

    /// `24mp-3`, `60mp-9`.
    pub fn parse(text: &str) -> Result<Self> {
        let (size, frames) = text
            .split_once('-')
            .ok_or_else(|| format!("a workload is SIZE-FRAMES, such as 24mp-3, not {text}"))?;
        let size = PhotoSize::ALL
            .into_iter()
            .find(|candidate| candidate.name() == size)
            .ok_or_else(|| format!("a workload's size is 24mp or 60mp, not {size}"))?;
        let frames: usize = frames.parse()?;
        ensure(
            (2..=9).contains(&frames),
            format!("a bracket has 2 to 9 frames, not {frames}"),
        )?;
        Ok(Self::new(size, frames))
    }

    pub fn name(self) -> String {
        format!("{}-{}", self.size.name(), self.frames)
    }

    /// The bracket: handheld, each frame after the metered one turned and shifted, the third
    /// frame's ball moved; two stops apart up to five frames (±2 EV for three), one stop apart
    /// beyond (±4 EV for nine). Noise on.
    pub fn spec(self) -> BracketSpec {
        BracketSpec {
            frames: self.frames,
            step_ev: if self.frames > 5 { 1 } else { 2 },
            seed: 7,
            handheld: true,
            moving_subject: Some(2),
            noise: true,
            first_number: 1,
        }
    }
}

/// What the command was given.
pub struct Options {
    pub scale: Scale,
    pub workloads: Vec<Workload>,
    pub cold: usize,
    pub warm: usize,
    pub cancellations: usize,
    pub rounds: usize,
    /// The release editor `editor-drag` launches.
    pub binary: PathBuf,
    pub natural: Vec<PathBuf>,
    /// The steps to take; every step when absent.
    pub only: Option<Vec<String>>,
}

/// The steps, in order, for `--only`.
pub const STEPS: [&str; 5] = ["cold", "warm", "reopen", "editor-drag", "natural"];

/// The report while the run fills it: rewritten after every step.
struct Report {
    out: PathBuf,
    value: Value,
}

/// A row's status, as `catalog-measure`'s.
const MEASURED: &str = "measured";
const FAILED: &str = "failed";
const SKIPPED: &str = "skipped";

impl Report {
    fn write(&self) -> Result {
        write_json(&self.out.join(JSON), &self.value)?;
        fs::write(self.out.join(MARKDOWN), markdown(&self.value))?;
        Ok(())
    }

    /// Run one step and add its rows with the load at its start and end; a step that fails adds
    /// one `failed` row naming why.
    fn step(&mut self, root: &Path, name: &str, body: impl FnOnce() -> Result<Vec<Value>>) {
        println!("hdr-merge-measure: {name}");
        let start = launch::load_average(root);
        let rows = body().unwrap_or_else(|error| {
            eprintln!("hdr-merge-measure: {name} failed: {error}");
            vec![failed(name, &error.to_string())]
        });
        let end = launch::load_average(root);
        let load = json!({"start": launch::load(start), "end": launch::load(end)});
        let list = self.value["rows"].as_array_mut().expect("rows is a list");
        for mut row in rows {
            row["step"] = json!(name);
            row["load"] = load.clone();
            list.push(row);
        }
        if let Err(error) = self.write() {
            eprintln!("hdr-merge-measure: the report could not be written: {error}");
        }
    }
}

/// A measured row over `samples`, with its scope.
fn row(metric: &str, unit: &str, samples: impl IntoIterator<Item = f64>, scope: &str) -> Value {
    let mut row = stats::row(metric, unit, samples);
    row["status"] = json!(if row["distribution"].is_object() {
        MEASURED
    } else {
        "not_measured"
    });
    row["scope"] = json!(scope);
    row
}

fn failed(metric: &str, reason: &str) -> Value {
    let mut row = stats::row(metric, "", Vec::new());
    row["status"] = json!(FAILED);
    row["reason"] = json!(reason);
    row
}

fn skipped(metric: &str, reason: &str) -> Value {
    let mut row = stats::row(metric, "", Vec::new());
    row["status"] = json!(SKIPPED);
    row["reason"] = json!(reason);
    row
}

/// The report's status: `failed` when a row failed, `incomplete` when one was not measured.
fn status(rows: &[Value]) -> &'static str {
    if rows.iter().any(|row| row["status"] == FAILED) {
        "failed"
    } else if rows.iter().all(|row| row["status"] == MEASURED) {
        "passed"
    } else {
        "incomplete"
    }
}

pub fn run(root: &Path, out: &Path, options: &Options) -> Result {
    ensure(!out.exists(), "hdr-merge-measure output must be new")?;
    ensure(
        !cfg!(debug_assertions) || options.scale == Scale::Tiny,
        "the in-process merges must be a release build: `cargo run --release --locked --package xtask -- hdr-merge-measure`",
    )?;
    fs::create_dir_all(out)?;
    let scratch = out.join("scratch");
    let mut report = Report {
        out: out.into(),
        value: json!({
            "tool": "hdr-merge-measure",
            "status": "in_progress",
            "command": std::env::args().collect::<Vec<_>>(),
            "git": crate::catalog_measure::git(root),
            "host": crate::catalog_measure::host(root),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "binary": options.binary,
            "binary_sha256": hash(&options.binary).ok(),
            "scale": if options.scale == Scale::Tiny { "tiny" } else { "full" },
            "samples": {"cold": options.cold, "warm": options.warm,
                        "cancellations": options.cancellations, "rounds": options.rounds},
            "load_threshold": launch::LOAD_THRESHOLD,
            "method": "Each merge runs in a worker process of its own kind through the JSON API's methods on an in-process catalog owner over a new catalog, the bracket's folder indexed and every job settled before the clock starts; stages from the job's phases on the activity board as they change; memory from resources.read and the job's accounting every 100 ms. The OS file cache is never purged: the brackets were just written, so their reads are warm.",
            "rows": [],
        }),
    };
    report.write()?;
    let wanted = |step: &str| {
        options
            .only
            .as_ref()
            .is_none_or(|only| only.iter().any(|name| name == step))
    };

    // Set-up, untimed: the brackets, the 24 MP photograph and a folder of 1,000 small JPEGs.
    let fixtures_dir = root.join("fixtures/generated");
    if !crate::fixtures::present(&fixtures_dir) {
        crate::fixtures::generate(&fixtures_dir)?;
    }
    let photo = fixtures_dir.join("24mp.jpg").canonicalize()?;
    let browse = browse_folder(&scratch.join("browse"), &fixtures_dir.join("range.jpg"))?;
    let mut data = Vec::new();
    let mut brackets = Vec::new();
    for workload in &options.workloads {
        println!("hdr-merge-measure: writing {}", workload.name());
        let dir = scratch.join("brackets").join(workload.name());
        let (inputs, record) = write_bracket(&dir, *workload, options.scale)?;
        data.push(record);
        brackets.push((*workload, inputs));
    }
    report.value["data"] = json!(data);
    report.write()?;

    let workers = scratch.join("workers");
    for (workload, inputs) in &brackets {
        let name = workload.name();
        let worker_dir = |step: &str, index: usize| workers.join(format!("{name}-{step}-{index}"));
        let job = |mode: Mode, dir: PathBuf| Job {
            mode,
            workload: name.clone(),
            inputs: inputs.clone(),
            dir,
            warm: 0,
            cancellations: 0,
            contention: None,
            catalog: None,
            merge_id: None,
            stop: None,
            natural: None,
        };
        if wanted("cold") {
            report.step(root, &format!("{name}.cold"), || {
                let mut results = Vec::new();
                for index in 0..options.cold {
                    results.push(spawn(&job(Mode::Cold, worker_dir("cold", index)))?);
                }
                Ok(cold_rows(&name, &results))
            });
        }
        if wanted("warm") {
            report.step(root, &format!("{name}.warm"), || {
                let result = spawn(&Job {
                    warm: options.warm,
                    cancellations: options.cancellations,
                    contention: Some(Contention {
                        photo: photo.clone(),
                        browse: browse.clone(),
                        rounds: options.rounds,
                        merges: 3,
                    }),
                    ..job(Mode::Warm, worker_dir("warm", 0))
                })?;
                warm_rows(&name, &result)
            });
        }
        if wanted("reopen") {
            report.step(root, &format!("{name}.reopen"), || {
                let created = spawn(&job(Mode::ReopenCreate, worker_dir("reopen-create", 0)))?;
                let inputs: Vec<PathBuf> = serde_json::from_value(created["inputs"].clone())?;
                let folder = inputs
                    .first()
                    .and_then(|input| input.parent())
                    .ok_or("the copies have no folder")?;
                fs::rename(folder, folder.with_file_name("inputs-moved-away"))?;
                let reopened = spawn(&Job {
                    inputs,
                    catalog: serde_json::from_value(created["catalog"].clone())?,
                    merge_id: created["merge_id"].as_str().map(str::to_owned),
                    ..job(Mode::Reopen, worker_dir("reopen", 0))
                })?;
                Ok(reopen_rows(&name, &created, &reopened))
            });
        }
        prune(&workers, &name);
    }

    if wanted("editor-drag") {
        let largest = brackets
            .iter()
            .max_by_key(|(workload, _)| {
                let (width, height) = workload.size.frame();
                u64::from(width) * u64::from(height) * workload.frames as u64
            })
            .cloned();
        report.step(root, "editor-drag", || match largest {
            None => Ok(vec![skipped("editor.drag", "no workload")]),
            Some(_) if !options.binary.is_file() => Ok(vec![skipped(
                "editor.drag",
                &format!(
                    "no release editor at {}: build it with `cargo xtask build --release`",
                    options.binary.display()
                ),
            )]),
            Some((workload, inputs)) => editor_drag(
                root,
                &out.join("editor-drag"),
                &options.binary,
                &photo,
                &workers.join("editor-drag-busy"),
                workload,
                &inputs,
            ),
        });
        prune(&workers, "editor-drag");
    }

    if wanted("natural") {
        report.step(root, "natural", || {
            if options.natural.is_empty() {
                return Ok(vec![skipped(
                    "natural",
                    "no --natural folder: natural-bracket quality is unqualified",
                )]);
            }
            let mut rows = Vec::new();
            for (index, folder) in options.natural.iter().enumerate() {
                let result = spawn(&Job {
                    natural: Some(folder.canonicalize()?),
                    ..Job {
                        mode: Mode::Natural,
                        workload: format!("natural-{index}"),
                        inputs: Vec::new(),
                        dir: workers.join(format!("natural-{index}")),
                        warm: 0,
                        cancellations: 0,
                        contention: None,
                        catalog: None,
                        merge_id: None,
                        stop: None,
                        natural: None,
                    }
                })?;
                rows.push(natural_row(index, &result));
            }
            Ok(rows)
        });
    }

    let rows = report.value["rows"].as_array().cloned().unwrap_or_default();
    report.value["status"] = json!(status(&rows));
    report.write()?;
    println!(
        "hdr-merge-measure: {}; see {} and {}",
        report.value["status"].as_str().unwrap_or_default(),
        out.join(JSON).display(),
        out.join(MARKDOWN).display()
    );
    ensure(
        report.value["status"] != "failed",
        "an hdr-merge-measure step failed; its row names why",
    )
}

/// Remove what a workload's workers saved once its steps are done — every catalog's artifacts
/// (each a result of up to 720 MB at 60 MP) and the moved copies of its inputs — keeping their
/// reports, logs and exports: a run of every workload would otherwise hold tens of gigabytes.
fn prune(workers: &Path, workload: &str) {
    let Ok(entries) = fs::read_dir(workers) else {
        return;
    };
    for worker in entries.flatten() {
        if !worker
            .file_name()
            .to_string_lossy()
            .starts_with(&format!("{workload}-"))
        {
            continue;
        }
        let _ = fs::remove_dir_all(worker.path().join("inputs-moved-away"));
        let Ok(catalogs) = fs::read_dir(worker.path()) else {
            continue;
        };
        for catalog in catalogs.flatten() {
            let _ = fs::remove_dir_all(catalog.path().join("catalog.artifacts"));
        }
    }
}

/// A folder of 1,000 copies of a small JPEG to browse beside a merge.
fn browse_folder(dir: &Path, source: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    for index in 0..1000 {
        let path = dir.join(format!("BROWSE_{index:04}.jpg"));
        if !path.exists() {
            fs::copy(source, &path)?;
        }
    }
    Ok(dir.canonicalize()?)
}

/// Write `workload`'s bracket into `dir` (the test size at `--scale tiny`): its files and the
/// record of what was written.
pub fn write_bracket(
    dir: &Path,
    workload: Workload,
    scale: Scale,
) -> Result<(Vec<PathBuf>, Value)> {
    let started = Instant::now();
    let bracket = Bracket::new(&workload.spec());
    let paths = match scale {
        Scale::Tiny => bracket.write(dir)?,
        Scale::Full => {
            fs::create_dir_all(dir)?;
            let mut paths = Vec::new();
            for (at, frame) in bracket.frames().iter().enumerate() {
                let samples = photo_samples(&bracket, at, workload.size);
                let path = dir.join(&frame.file_name);
                fs::write(&path, bracket.photo_dng(at, workload.size, samples))?;
                paths.push(path);
            }
            paths
        }
    };
    let paths: Vec<PathBuf> = paths
        .iter()
        .map(|path| path.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let files: Vec<Value> = paths
        .iter()
        .map(|path| -> Result<Value> {
            Ok(json!({
                "name": path.file_name().map(|name| name.to_string_lossy().into_owned()),
                "bytes": fs::metadata(path)?.len(),
                "sha256": hash(path)?,
            }))
        })
        .collect::<Result<_>>()?;
    let (width, height) = match scale {
        Scale::Tiny => (
            luxforge_testkit::hdr_bracket::WIDTH,
            luxforge_testkit::hdr_bracket::HEIGHT,
        ),
        Scale::Full => workload.size.frame(),
    };
    let spec = workload.spec();
    Ok((
        paths,
        json!({
            "workload": workload.name(),
            "width": width,
            "height": height,
            "frames": spec.frames,
            "step_ev": spec.step_ev,
            "seed": spec.seed,
            "handheld": spec.handheld,
            "moving_subject": spec.moving_subject,
            "noise": spec.noise,
            "files": files,
            "written_ms": started.elapsed().as_secs_f64() * 1000.0,
        }),
    ))
}

/// One photo-sized frame's sensor samples, its bands made on every core.
fn photo_samples(bracket: &Bracket, at: usize, size: PhotoSize) -> Vec<u8> {
    const BAND: u32 = 64;
    let height = size.sensor().1;
    let bands = height.div_ceil(BAND) as usize;
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |count| count.get());
    let mut made: Vec<(usize, Vec<u8>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let band = next.fetch_add(1, Ordering::Relaxed);
                        if band >= bands {
                            return done;
                        }
                        let top = band as u32 * BAND;
                        let rows = top..(top + BAND).min(height);
                        done.push((band, bracket.photo_samples(at, size, rows)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("a band's thread panicked"))
            .collect()
    });
    made.sort_by_key(|(band, _)| *band);
    made.into_iter().flat_map(|(_, samples)| samples).collect()
}

/// Run `job` in a worker process of this program and answer its result. The worker's output goes
/// to `worker.log` in its folder.
fn spawn(job: &Job) -> Result<Value> {
    fs::create_dir_all(&job.dir)?;
    let file = job.dir.join("job.json");
    write_json(&file, &serde_json::to_value(job)?)?;
    let log = fs::File::create(job.dir.join("worker.log"))?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            OsStr::new("hdr-merge-measure"),
            OsStr::new("--worker"),
            file.as_os_str(),
        ])
        // Admits the synthetic test body where the camera catalog keeps it apart.
        .env("LUXFORGE_TEST_CAMERAS", "1")
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()?;
    ensure(
        status.success(),
        format!(
            "the {:?} worker failed ({status}); see {}",
            job.mode,
            job.dir.join("worker.log").display()
        ),
    )?;
    read_json(&job.dir.join(worker::RESULT))
}

fn mib(bytes: &Value) -> Option<f64> {
    bytes.as_u64().map(|bytes| bytes as f64 / MIB)
}

/// Samples of `field` over `runs`, each through `read`.
fn over(runs: &[&Value], read: impl Fn(&Value) -> Option<f64>) -> Vec<f64> {
    runs.iter().filter_map(|run| read(run)).collect()
}

/// The rows every set of completed merges has: the total and each stage, each input's read, and
/// the job's accounting, the process's sampled memory and the saved source.
fn merge_rows(prefix: &str, runs: &[&Value], scope: &str) -> Vec<Value> {
    let mut rows = vec![row(
        &format!("{prefix}.total"),
        "ms",
        over(runs, |run| run["total_ms"].as_f64()),
        scope,
    )];
    for stage in CancelAt::ALL {
        rows.push(row(
            &format!("{prefix}.{}", stage.name()),
            "ms",
            over(runs, |run| run["stages"][stage.name()].as_f64()),
            &format!("{scope}; the stage from its phase on the activity board to the next"),
        ));
    }
    rows.push(row(
        &format!("{prefix}.read_per_input"),
        "ms",
        runs.iter()
            .flat_map(|run| run["reads_ms"].as_array().cloned().unwrap_or_default())
            .filter_map(|value| value.as_f64()),
        &format!("{scope}; one input read, hashed, developed and spooled, from its Reading k of n to the next"),
    ));
    let memory = [
        (
            "scratch_peak",
            "accounting.scratch_peak_bytes",
            "the job's accounted scratch at its peak, against its 256 MiB limit",
        ),
        (
            "development",
            "accounting.development_bytes",
            "the one development the job held (its float planes), outside the scratch",
        ),
        (
            "temp_max",
            "accounting.temp_bytes_max",
            "the temporary bytes the job had spooled at most",
        ),
        (
            "footprint_sampled_max",
            "footprint_max_bytes",
            "the process's physical footprint, the largest of resources.read every 100 ms while the merge ran",
        ),
        (
            "footprint_growth",
            "",
            "the largest sampled footprint less the footprint just before the request",
        ),
        (
            "retained_source",
            "result.source_bytes",
            "the saved result's objects under the catalog's artifacts sources/: its chunks and manifest",
        ),
    ];
    for (name, path, what) in memory {
        let samples = over(runs, |run| {
            if name == "footprint_growth" {
                let (high, low) = (
                    run["footprint_max_bytes"].as_f64()?,
                    run["footprint_before_bytes"].as_f64()?,
                );
                return Some((high - low) / MIB);
            }
            let mut value: &Value = run;
            for key in path.split('.') {
                value = &value[key];
            }
            mib(value)
        });
        rows.push(row(
            &format!("{prefix}.{name}"),
            "MiB",
            samples,
            &format!("{scope}; {what}"),
        ));
    }
    rows
}

fn cold_rows(name: &str, results: &[Value]) -> Vec<Value> {
    let runs: Vec<&Value> = results.iter().map(|result| &result["merge"]).collect();
    let scope = format!(
        "{name}: the first merge of each of {} new worker processes, each into a new catalog",
        results.len()
    );
    let mut rows = merge_rows(&format!("{name}.cold"), &runs, &scope);
    rows.push(row(
        &format!("{name}.cold.process_peak"),
        "MiB",
        results.iter().filter_map(|result| mib(&result["process"]["peak_bytes"])),
        &format!("{scope}; the process's peak physical footprint as macOS records it, over the whole process: the owner, indexing the bracket and the one merge"),
    ));
    rows.push(spool_row(&format!("{name}.cold"), &runs));
    rows
}

/// Every merge's spool files must be gone as it ends, however it ends.
fn spool_row(prefix: &str, runs: &[&Value]) -> Value {
    let left: u64 = runs
        .iter()
        .map(|run| run["spool_files_left"].as_u64().unwrap_or(u64::MAX))
        .sum();
    if left == 0 {
        row(
            &format!("{prefix}.spool_files_left"),
            "files",
            runs.iter().map(|_| 0.0),
            "spool files left under the catalog's merges/ once each merge ended: none",
        )
    } else {
        failed(
            &format!("{prefix}.spool_files_left"),
            &format!("{left} spool files outlived their merges"),
        )
    }
}

fn warm_rows(name: &str, result: &Value) -> Result<Vec<Value>> {
    let warm: Vec<&Value> = result["warm"]
        .as_array()
        .map(|runs| runs.iter().collect())
        .unwrap_or_default();
    let scope = format!(
        "{name}: {} merges in one worker process after a first, each into a new catalog",
        warm.len()
    );
    let mut rows = merge_rows(&format!("{name}.warm"), &warm, &scope);
    rows.push(row(
        &format!("{name}.warm.process_peak"),
        "MiB",
        mib(&result["process"]["peak_bytes"]),
        &format!("{name}: the warm worker's peak physical footprint over its whole life: every merge, cancellation and the editor work beside merges"),
    ));
    let mut all: Vec<&Value> = warm.clone();
    all.push(&result["first"]);
    for stage in CancelAt::ALL {
        let runs: Vec<&Value> = result["cancellations"][stage.name()]
            .as_array()
            .map(|runs| runs.iter().collect())
            .unwrap_or_default();
        all.extend(runs.iter().copied());
        let outcomes: BTreeMap<String, usize> =
            runs.iter().fold(BTreeMap::new(), |mut counts, run| {
                *counts
                    .entry(run["status"].as_str().unwrap_or("unknown").to_owned())
                    .or_default() += 1;
                counts
            });
        let mut latency = row(
            &format!("{name}.cancel.{}", stage.name()),
            "ms",
            over(&runs, |run| run["cancel"]["latency_ms"].as_f64()),
            &format!(
                "{name}: job.cancel sent when the board showed the merge {} until the job ended, {} merges in the warm process",
                match stage {
                    CancelAt::Reading => "reading its second input",
                    CancelAt::Aligning => "aligning",
                    CancelAt::Merging => "half way through its chunks",
                    CancelAt::Saving => "saving its last chunk",
                },
                runs.len()
            ),
        );
        latency["detail"] = json!({
            "outcomes": outcomes,
            "phases_at_cancel": runs.iter().map(|run| run["cancel"]["phase"].clone()).collect::<Vec<_>>(),
            "requested_ms": runs.iter().map(|run| run["cancel"]["requested_ms"].clone()).collect::<Vec<_>>(),
        });
        rows.push(latency);
    }
    rows.push(spool_row(&format!("{name}.warm"), &all));
    if let Some(contention) = result["contention"].as_object() {
        for operation in worker::OPERATIONS {
            for (when, key) in [("idle", "idle"), ("beside_merge", "during")] {
                rows.push(row(
                    &format!("{name}.editor.{operation}.{when}"),
                    "ms",
                    contention[key][operation]
                        .as_array()
                        .map(|samples| samples.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
                        .unwrap_or_default(),
                    &format!("{name}: {} through the same owner as the merge, the reference renderer (no GPU in the worker); {}", match operation {
                        "browse_rows" => "browse.rows of 200 over a view of 1,000 JPEGs",
                        "render_sample" => "render.sample of one pixel of a prepared 24 MP JPEG photograph",
                        "commit" => "edit.set-basic of its exposure, the request to its answer",
                        _ => "export.jpeg of it, the request to the job's end",
                    }, if when == "idle" { "no merge running" } else { "only the samples that ended while a merge still ran" }),
                ));
            }
        }
        let merges: Vec<&Value> = contention["merges"]
            .as_array()
            .map(|runs| runs.iter().collect())
            .unwrap_or_default();
        rows.push(row(
            &format!("{name}.editor.merge_total_beside_editor"),
            "ms",
            over(&merges, |run| run["total_ms"].as_f64()),
            &format!("{name}: the merges the editor work ran beside, request to end"),
        ));
    }
    Ok(rows)
}

fn reopen_rows(name: &str, created: &Value, reopened: &Value) -> Vec<Value> {
    let scope = format!(
        "{name}: a merge of copies of the bracket; undo and redo in its process; then, the copies moved away, the catalog reopened in a new process: merge.inspect reports every input missing, undo and redo again, source.prepare and export.jpeg of the result"
    );
    let mut checked = row(
        &format!("{name}.reopen.merge_activity"),
        "changes",
        [
            created["merge_activity"].as_f64().unwrap_or(f64::NAN),
            reopened["merge_activity"].as_f64().unwrap_or(f64::NAN),
        ],
        &format!(
            "{scope}. Asserted zero: no HDR merge began, progressed or ended on the activity board through undo, redo, reopening, preparation and export, and the saved objects under sources/ and the merges' working folders were unchanged; with the inputs moved away no step could read one"
        ),
    );
    checked["detail"] =
        json!({"inputs_on_reopen": reopened["inputs"], "export": reopened["export"]});
    vec![
        checked,
        row(
            &format!("{name}.reopen.open"),
            "ms",
            reopened["open_ms"].as_f64(),
            &format!("{scope}; the owner's start over the catalog to merge.inspect's answer"),
        ),
        row(
            &format!("{name}.reopen.undo"),
            "ms",
            [
                created["journal"]["undo_ms"].as_f64(),
                reopened["journal"]["undo_ms"].as_f64(),
            ]
            .into_iter()
            .flatten(),
            &format!("{scope}; library.undo of the merge's creation"),
        ),
        row(
            &format!("{name}.reopen.redo"),
            "ms",
            [
                created["journal"]["redo_ms"].as_f64(),
                reopened["journal"]["redo_ms"].as_f64(),
            ]
            .into_iter()
            .flatten(),
            &format!("{scope}; library.redo of it"),
        ),
        row(
            &format!("{name}.reopen.prepare"),
            "ms",
            reopened["prepare_ms"].as_f64(),
            &format!(
                "{scope}; source.prepare of the result: its chunks read and verified into one frame"
            ),
        ),
        row(
            &format!("{name}.reopen.prepared_footprint_growth"),
            "MiB",
            reopened["footprint_prepared_bytes"]
                .as_f64()
                .zip(reopened["footprint_before_prepare_bytes"].as_f64())
                .map(|(after, before)| (after - before) / MIB),
            &format!("{scope}; the process's footprint after preparation less before"),
        ),
        row(
            &format!("{name}.reopen.export"),
            "ms",
            reopened["export_ms"].as_f64(),
            &format!("{scope}; export.jpeg of the result, the reference renderer"),
        ),
    ]
}

fn natural_row(index: usize, result: &Value) -> Value {
    let brackets = result["brackets"].as_u64().unwrap_or(0);
    let mut row = if brackets == 0 {
        skipped(
            &format!("natural.{index}"),
            &format!(
                "Select found no bracket among {} files: natural-bracket quality is unqualified",
                result["files"]
            ),
        )
    } else {
        row(
            &format!("natural.{index}.total"),
            "ms",
            result["cases"]
                .as_array()
                .map(|cases| {
                    cases
                        .iter()
                        .filter_map(|case| case["outcome"]["total_ms"].as_f64())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            "a person's brackets, read only, merged into a new catalog",
        )
    };
    row["detail"] = result.clone();
    row
}

/// `editor-latency`'s Basic drag on the 24 MP JPEG alone, then while a worker merges `workload`
/// back to back.
fn editor_drag(
    root: &Path,
    out: &Path,
    binary: &Path,
    photo: &Path,
    busy_dir: &Path,
    workload: Workload,
    inputs: &[PathBuf],
) -> Result<Vec<Value>> {
    let drag = |dir: &Path| -> Result<Vec<f64>> {
        editor_latency::run(
            root,
            dir,
            binary,
            editor_latency::Options {
                source: photo,
                samples: 30,
                mode: editor_latency::Mode::Drag,
                control: editor_latency::Control::Slider,
                action: None,
                parameter: None,
                crop: None,
                idle: false,
                basic: false,
                presence: false,
                curve_layer: false,
                detail: false,
                lens: false,
                perspective: false,
                mask: false,
                zoom: None,
                moving_pan: false,
                mask_overlay: false,
                contend: None,
                warm_ms: None,
                no_gpu_render: false,
                masks: 1,
                mask_presence: false,
                window: None,
                usage: None,
            },
        )?;
        let latency = read_json(&dir.join("latency.json"))?;
        Ok(stats::distribution(&latency, "input_to_presented_frame")
            .and_then(|distribution| distribution["samples"].as_array())
            .map(|samples| samples.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default())
    };
    let idle = drag(&out.join("idle"))?;
    fs::create_dir_all(busy_dir)?;
    let stop = busy_dir.join("stop");
    let job = Job {
        mode: Mode::Busy,
        workload: workload.name(),
        inputs: inputs.to_vec(),
        dir: busy_dir.to_path_buf(),
        warm: 0,
        cancellations: 0,
        contention: None,
        catalog: None,
        merge_id: None,
        stop: Some(stop.clone()),
        natural: None,
    };
    let file = busy_dir.join("job.json");
    write_json(&file, &serde_json::to_value(&job)?)?;
    let log = fs::File::create(busy_dir.join("worker.log"))?;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([
            OsStr::new("hdr-merge-measure"),
            OsStr::new("--worker"),
            file.as_os_str(),
        ])
        .env("LUXFORGE_TEST_CAMERAS", "1")
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    // Drag once the first merge has begun.
    let started = Instant::now();
    while !busy_dir.join("merging").exists() {
        ensure(
            started.elapsed() < std::time::Duration::from_secs(300),
            "the busy worker began no merge",
        )?;
        ensure(child.try_wait()?.is_none(), "the busy worker stopped")?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let beside = drag(&out.join("beside-merge"));
    fs::write(&stop, "stop")?;
    let status = child.wait()?;
    let beside = beside?;
    ensure(
        status.success(),
        "the busy worker failed; see its worker.log",
    )?;
    let merges = read_json(&busy_dir.join(worker::RESULT))?;
    let scope = format!(
        "editor-latency --mode drag (Basic exposure at Fit) on the 24 MP JPEG, 30 inputs, release editor in a background launch; beside: while another process merged {} back to back ({} merges)",
        workload.name(),
        merges["merges"].as_array().map_or(0, Vec::len)
    );
    Ok(vec![
        row("editor.drag.idle", "ms", idle, &scope),
        row("editor.drag.beside_merge", "ms", beside, &scope),
    ])
}

fn figure(distribution: &Value, key: &str) -> String {
    distribution[key]
        .as_f64()
        .map_or_else(|| "–".into(), |value| format!("{value:.1}"))
}

/// The summary beside the report: one table row per report row, then why each row that was not
/// measured was not, and each row's scope.
pub fn markdown(report: &Value) -> String {
    let text = |value: &Value| value.as_str().unwrap_or("unknown").to_owned();
    let mut out = format!(
        "# HDR merge measurements\n\nStatus: {}. Scale: {}. Commit {}{}. Host: {} ({}), {}. Load threshold {}.\n\n",
        text(&report["status"]),
        text(&report["scale"]),
        text(&report["git"]["sha"]),
        if report["git"]["dirty"] == true {
            " with uncommitted changes"
        } else {
            ""
        },
        text(&report["host"]["model"]),
        text(&report["host"]["cpu"]),
        text(&report["host"]["os"]),
        launch::LOAD_THRESHOLD,
    );
    out.push_str("| Metric | Status | p50 | p95 | max | n | Unit | Load start → end |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    let rows = stats::rows(report);
    let load = |value: &Value| {
        value["load_average_1m"]
            .as_f64()
            .map_or_else(|| "–".into(), |load| format!("{load:.1}"))
    };
    for row in rows {
        let distribution = &row["distribution"];
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} → {} |\n",
            text(&row["metric"]),
            text(&row["status"]),
            figure(distribution, "p50"),
            figure(distribution, "p95"),
            figure(distribution, "max"),
            distribution["count"].as_u64().unwrap_or(0),
            row["unit"].as_str().unwrap_or(""),
            load(&row["load"]["start"]),
            load(&row["load"]["end"]),
        ));
    }
    let unmeasured: Vec<&Value> = rows
        .iter()
        .filter(|row| row["status"] != MEASURED)
        .collect();
    if !unmeasured.is_empty() {
        out.push_str("\n## Not measured\n\n");
        for row in unmeasured {
            out.push_str(&format!(
                "- `{}` ({}): {}\n",
                text(&row["metric"]),
                text(&row["status"]),
                text(&row["reason"])
            ));
        }
    }
    out.push_str("\n## Scope\n\n");
    for row in rows.iter().filter(|row| row["scope"].is_string()) {
        out.push_str(&format!(
            "- `{}`: {}\n",
            text(&row["metric"]),
            text(&row["scope"])
        ));
    }
    out
}
