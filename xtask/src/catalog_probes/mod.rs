//! The catalog measurement's desktop probes ([catalog design](../../../docs/design/catalog.md#performance)):
//! grid scroll ([`grid`]), loupe stepping with the look-ahead warm and a held arrow
//! ([`loupe`]), the 100% focus check ([`focus`]), and the decoded previews the grid and the loupe
//! hold meanwhile. `cargo xtask catalog-measure` runs them in its desktop step
//! (`catalog_measure/desktop.rs`), which turns each [`Figure`] into a row of its report under the
//! metric names and targets its `PROBES` fix.
//!
//! Every probe drives the release editor through the background evidence launch every timing tool
//! makes ([`Run`], a hidden window in a background-only bundle on macOS), over a folder read into a
//! catalog of its own before the editor opens it ([`prepare`]), and times what the editor itself
//! records in its `events.jsonl`: never a wall clock around the process, a sleep or a screenshot.
//!
//! **Presented** means what it means for the photograph's `preview_displayed` ([performance
//! spec](../../../docs/specs/performance.md)): the update in which the picture became the view's
//! source, drawn by the redraw that update requests. For the grid that is the update that adopts a
//! new offset (`select_scrolled`); for the loupe the update whose derived model draws the active
//! frame's own picture (`loupe_presented`, under its own item and preview key; a stand-in drawn
//! while the tier is read is never counted) or the inset's region (`loupe_region_presented`). It is
//! not display scanout, which the harness cannot observe, and the window is invisible, so nothing
//! is composited: each figure is an upper bound on the editor's own work and a lower bound on what
//! an eye sees.
//!
//! Each figure carries what was timed over what (`scope`), what was warm (`cache`), the design's
//! target it answers and the counts it rests on (`detail`), and never a verdict. Each probe run —
//! `grid`, `loupe`, `loupe-raw`, `focus`, `focus-raw` — runs and answers on its own: one that cannot
//! run answers one failed figure of its own (`desktop.probe.<run>`, [`Outcome::Failed`]) naming its
//! run directory, whose `result.json` lists every launch, and why; the other runs' figures stand.
mod focus;
mod grid;
mod loupe;

use crate::{
    catalog_measure::desktop::PROBES,
    generate_catalog,
    scenario::{Launch, Run},
    smoke::PANELLED,
    *,
};
use luxforge_core::{ApiRequest, ClientId, OwnerHandle};
use luxforge_evidence::{self as script, SelectStep};
use std::{fmt, time::Duration};

/// What the probes run with: exactly the measuring command's `DesktopContext`.
pub(crate) struct ProbeContext {
    /// The release editor.
    pub binary: PathBuf,
    /// A directory the probes create and write into: each probe's run, its catalogs and the images
    /// it generates, each in a new directory of its own.
    pub scratch: PathBuf,
    /// Measured samples per figure.
    pub samples: usize,
    /// The generated folder grid scroll is measured over (10,000 files).
    pub folder_10k: PathBuf,
    /// A folder of RAW files from real cameras, for the focus check from a development per camera
    /// and loupe stepping over RAW previews.
    pub raw_trip: Option<PathBuf>,
}

/// The measuring command's figures these probes answer, each named, with its unit and target, in
/// its `PROBES`.
const GRID_FRAME_TIME: &str = "desktop.grid_scroll_10k.frame_time";
const LOUPE_STEP: &str = "desktop.loupe_step.key_to_presented";
const FOCUS_EMBEDDED: &str = "desktop.focus_check.embedded";
const FOCUS_DEVELOPMENT: &str = "desktop.focus_check.development";
const MEMORY_GRID: &str = "desktop.memory.decoded_grid";
const MEMORY_LOUPE: &str = "desktop.memory.decoded_loupe";

/// The one tool name every probe run records itself under.
const TOOL: &str = "catalog-probes";
/// The seed and size of the folder of generated JPEGs the loupe and the focus check run over.
const SEED: u64 = 1;
const IMAGES: u32 = 120;
/// Each launch's own deadline: the editor's evidence deadline for a script is 60 seconds, and the
/// background bundle is copied first.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(120);
const MIB: f64 = 1024.0 * 1024.0;

/// What a figure came to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    Measured(Vec<f64>),
    /// Nothing this run did could take it, and why.
    NotMeasured(String),
    /// Its data is absent from this host, and why.
    Skipped(String),
    /// Its probe run returned an error, which says why.
    Failed(String),
}

/// One figure for the measuring command's report: its metric and unit, its samples or why it has
/// none, what was timed over what, what was warm, the design's target it answers and the counts
/// it rests on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Figure {
    pub metric: String,
    pub unit: &'static str,
    pub outcome: Outcome,
    pub scope: String,
    pub cache: String,
    pub target: Option<&'static str>,
    pub detail: Value,
}

impl Figure {
    fn new(metric: impl Into<String>, unit: &'static str, outcome: Outcome) -> Self {
        Self {
            metric: metric.into(),
            unit,
            outcome,
            scope: String::new(),
            cache: String::new(),
            target: None,
            detail: Value::Null,
        }
    }

    fn measured(metric: impl Into<String>, unit: &'static str, samples: Vec<f64>) -> Self {
        Self::new(metric, unit, Outcome::Measured(samples))
    }

    fn not_measured(
        metric: impl Into<String>,
        unit: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self::new(metric, unit, Outcome::NotMeasured(reason.into()))
    }

    fn skipped(metric: impl Into<String>, unit: &'static str, reason: impl Into<String>) -> Self {
        Self::new(metric, unit, Outcome::Skipped(reason.into()))
    }

    /// The probe run `run`, which writes into its directory under the probes' scratch, failed:
    /// one figure of its own in place of the figures it would have taken.
    fn failed(context: &ProbeContext, run: &str, error: impl fmt::Display) -> Self {
        let dir = context.scratch.join(run);
        Self::new(
            format!("desktop.probe.{run}"),
            "",
            Outcome::Failed(format!("The {run} probe ({}): {error}", dir.display())),
        )
        .scope(format!(
            "the {run} probe's run: every launch, its evidence and its result.json in {}",
            dir.display()
        ))
        .detail(json!({"run": run, "dir": dir}))
    }

    fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    fn cache(mut self, cache: impl Into<String>) -> Self {
        self.cache = cache.into();
        self
    }

    /// The target of the command's figure `of`, which this one answers or stands beside.
    fn target(mut self, of: &str) -> Self {
        self.target = PROBES
            .iter()
            .find(|(metric, ..)| *metric == of)
            .map(|(.., target)| *target);
        self
    }

    fn detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }
}

/// `bytes` in MiB.
fn mib(bytes: &Value) -> Option<f64> {
    bytes.as_f64().map(|bytes| bytes / MIB)
}

/// The largest of `values`.
fn largest(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    values.into_iter().reduce(f64::max)
}

/// The decoded previews the desktop held during and after the runs, as its evidence reports them.
#[derive(Debug, Default)]
struct Memory {
    /// The grid's decoded bytes at each frame of the scroll and each captured frame of its run, in
    /// MiB, and its budget.
    grid: Vec<f64>,
    grid_budget: Option<f64>,
    /// Each editor run that opened the loupe: the most decoded bytes its loupe held at once.
    loupe: Vec<Value>,
}

impl Memory {
    /// The loupe's high-water mark over every frame `launched` captured, beside its budget.
    fn loupe_run(&mut self, run: &str, launched: &Launched, held_keys: Option<u32>) {
        let frames = || {
            launched
                .frames
                .iter()
                .map(|frame| &frame["state"]["select"]["loupe"]["frames"])
        };
        self.loupe.push(json!({
            "run": run,
            "launch": launched.dir.file_name().map(|name| name.to_string_lossy().into_owned()),
            "peak_mib": largest(frames().filter_map(|frames| mib(&frames["peak_bytes"]))),
            "budget_mib": frames().find_map(|frames| mib(&frames["budget"])),
            "held_arrow_keys": held_keys,
        }));
    }

    fn figures(self) -> Vec<Figure> {
        let grid = Figure::measured(MEMORY_GRID, "MiB", self.grid.clone())
            .scope(
                "The decoded grid previews the desktop holds, by the grid cache's own count of \
                 their bytes: at every display frame of the grid scroll and at each frame its run \
                 captured",
            )
            .cache("the grid scroll's run: previews read and decoded as the grid asked for them")
            .target(MEMORY_GRID)
            .detail(json!({
                "peak_mib": largest(self.grid.iter().copied()),
                "budget_mib": self.grid_budget,
                "samples": self.grid.len(),
            }));
        let peaks: Vec<f64> = self
            .loupe
            .iter()
            .filter_map(|run| run["peak_mib"].as_f64())
            .collect();
        let loupe = Figure::measured(MEMORY_LOUPE, "MiB", peaks)
            .scope(
                "The most decoded loupe frames each editor run held at once, by the loupe cache's \
                 own high-water mark (peak_bytes): one sample per run, over the loupe runs with \
                 their held arrow and the focus-check runs",
            )
            .cache("each run's loupe tiers read and decoded in that run")
            .target(MEMORY_LOUPE)
            .detail(json!({"runs": self.loupe}));
        vec![grid, loupe]
    }
}

/// Every desktop figure: grid scroll first, whose frame clock gives the display frame interval the
/// loupe's stepping is counted in, then the loupe, the focus check, and the decoded previews held
/// meanwhile. Each probe run answers on its own: a run that fails is one failed figure among the
/// others' ([`Figure::failed`]). Only a context no probe can run with is an error.
pub(crate) fn desktop_probes(context: &ProbeContext) -> Result<Vec<Figure>> {
    ensure(
        (1..=10_000).contains(&context.samples),
        "The desktop probes take 1 to 10,000 samples",
    )?;
    let root = root()?;
    fs::create_dir_all(&context.scratch)?;
    let mut memory = Memory::default();
    let mut figures = Vec::new();
    let frame_ms = match grid::probe(&root, context, &mut memory) {
        Ok((grid, frame_ms)) => {
            figures.extend(grid);
            frame_ms
        }
        Err(error) => {
            figures.push(Figure::failed(context, "grid", error));
            None
        }
    };
    let generated = context.scratch.join("generated");
    let images = generate_catalog::run(
        &generated,
        &generate_catalog::Options {
            seed: SEED,
            files: None,
            assets: None,
            images: Some(IMAGES),
        },
    )
    .map(|()| Source {
        label: format!(
            "{IMAGES} generated JPEGs (generate-catalog --images {IMAGES} --seed {SEED})"
        ),
        folder: generated.join("images"),
    })
    .map_err(|error| figures.push(Figure::failed(context, "generated", error)))
    .ok();
    let raw = context.raw_trip.as_ref().map(|folder| Source {
        label: "the RAW trip".into(),
        folder: folder.clone(),
    });
    figures.extend(loupe::probe(
        &root,
        context,
        images.as_ref(),
        raw.as_ref(),
        frame_ms,
        &mut memory,
    ));
    figures.extend(focus::probe(
        &root,
        context,
        images.as_ref(),
        raw.as_ref(),
        &mut memory,
    ));
    figures.extend(memory.figures());
    Ok(figures)
}

/// A folder a probe browses, and what its figures call it.
struct Source {
    label: String,
    folder: PathBuf,
}

/// A folder read by the index lane into a new catalog before the editor opens it, and the view the
/// core answers for it as the desktop first views it (the folder, its subfolders included, in the
/// default order and grouping), so a launch's `folder` step only reconciles it.
struct Prepared {
    catalog: PathBuf,
    folder: PathBuf,
    count: u32,
    moments: Vec<Value>,
    /// Every row: `position`, `file_name`, `kind`, `camera` and `dimensions`.
    rows: Vec<Value>,
}

/// One request through the probe's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("catalog-probes-{method}"),
                method: method.into(),
                params,
                token: None,
            },
        )
        .map_err(|error| format!("{method}: {error}"))?;
    match response.error {
        Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

/// Read `folder` into a new catalog in `dir` and answer its view and rows.
fn prepare(dir: &Path, folder: &Path) -> Result<Prepared> {
    ensure(!dir.exists(), format!("{} must be new", dir.display()))?;
    fs::create_dir_all(dir)?;
    let catalog = dir.join("catalog.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog)
        .map_err(|error| format!("the core cannot create a catalog: {error}"))?;
    let client = owner.register();
    let answers = (|| -> Result<Prepared> {
        let started = ask(
            &owner,
            client,
            "index.refresh",
            json!({"source": {"kind": "folder", "path": folder}}),
        )?;
        let job = json!({"job_id": started["job_id"]});
        let read = loop {
            let read = ask(&owner, client, "job.read", job.clone())?;
            if !matches!(read["status"].as_str(), Some("queued" | "running")) {
                break read;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        ensure(
            read["status"] == "ready",
            format!("the core could not read {}: {read}", folder.display()),
        )?;
        let listed = read["result"]["roots"][0].clone();
        let view = ask(
            &owner,
            client,
            "browse.view",
            json!({"source": {"kind": "folder", "path": listed, "subfolders": true}}),
        )?;
        let count = view["count"]
            .as_u64()
            .ok_or("The folder view has no count")?;
        let mut rows = Vec::new();
        while (rows.len() as u64) < count {
            let block = ask(
                &owner,
                client,
                "browse.rows",
                json!({"from": rows.len(), "count": (count - rows.len() as u64).min(1000)}),
            )?;
            let block = block["rows"]
                .as_array()
                .filter(|block| !block.is_empty())
                .ok_or("The folder view answered no rows")?;
            rows.extend(block.iter().map(|row| {
                json!({
                    "position": row["position"],
                    "file_name": row["file_name"],
                    "kind": row["kind"],
                    "camera": row["camera"],
                    "dimensions": row["dimensions"],
                })
            }));
        }
        Ok(Prepared {
            catalog: catalog.clone(),
            folder: folder.to_path_buf(),
            count: u32::try_from(count)?,
            moments: view["groups"]["moments"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            rows,
        })
    })();
    owner.stop();
    let _ = join.join();
    answers
}

/// The steps that bring a launch to the loupe open on `position`: `G`, the folder browsed, the
/// cell at `position` clicked and `E`.
fn open_loupe(prepared: &Prepared, position: u32) -> Vec<script::Step> {
    vec![
        script::Step::key("g"),
        script::Step::Select(SelectStep::Folder(
            prepared.folder.to_string_lossy().into_owned(),
        )),
        script::Step::Select(SelectStep::Click {
            position,
            shift: false,
            command: false,
        }),
        script::Step::key("e"),
    ]
}

/// What one launch left: its evidence directory, its events, every frame it captured with its
/// state, the last of them, and the build profile the editor reported.
struct Launched {
    dir: PathBuf,
    events: Vec<Value>,
    frames: Vec<Value>,
    last: Value,
    profile: Value,
}

/// One background evidence launch named `name` over `catalog`, running `steps` at the smoke
/// scenarios' panelled window size. Every step must have run to its captured frame and the log
/// must be whole.
fn launch(run: &mut Run, name: &str, catalog: &Path, steps: &[script::Step]) -> Result<Launched> {
    ensure(
        steps.len() <= script::MAX_SCRIPT_STEPS,
        format!("The {name} script exceeds the evidence step bound"),
    )?;
    let launched = run.launch(
        Launch::named(name)
            .catalog(catalog)
            .script(&format!("{name}-script.json"), script::write(steps))
            .window(PANELLED),
    )?;
    let dir = launched.dir;
    let app = read_json(&dir.join("result.json"))?;
    ensure(
        app["status"] == "captured"
            && app["had_input_errors"] == false
            && app["script"]
                .as_array()
                .is_some_and(|recorded| recorded.len() == steps.len()),
        format!(
            "A {name} step failed or never ran ({}): {}",
            dir.display(),
            app["script"]
        ),
    )?;
    let events = scenario::events(&dir.join("events.jsonl"))?;
    ensure(
        !events
            .iter()
            .any(|event| event["event"] == "diagnostics_truncated"),
        format!("The {name} launch's log was truncated ({})", dir.display()),
    )?;
    let header = run.provenance(&events)?;
    let frames = app["frames"].as_array().cloned().unwrap_or_default();
    let last = frames.last().cloned().ok_or("No frame was captured")?;
    Ok(Launched {
        dir,
        events,
        frames,
        last,
        profile: header["profile"].clone(),
    })
}

impl Launched {
    /// `result`, an error naming this launch's evidence directory.
    fn read<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|error| format!("{}: {error}", self.dir.display()).into())
    }
}

/// A new probe run into `out`.
fn start(root: &Path, out: &Path, binary: &Path) -> Result<Run> {
    let mut run = Run::tool(root, out, TOOL, binary, LAUNCH_DEADLINE)?;
    run.hash(&[])?;
    Ok(run)
}

/// The facts every figure of one launch shares: the window, its scale, the backend and the
/// profile.
fn launch_detail(launched: &Launched) -> Value {
    json!({
        "window": PANELLED.map(|side| side.parse::<u32>().ok()),
        "physical_size": launched.last["physical_size"],
        "scale": launched.last["scale"],
        "backend": launched.last["state"]["backend"]["backend"],
        "profile": launched.profile,
        "launch": "background evidence launch, hidden window",
    })
}

/// `extra`'s fields added to `detail`.
fn merge(detail: &mut Value, extra: &Value) {
    for (key, value) in extra.as_object().into_iter().flatten() {
        detail[key] = value.clone();
    }
}

/// The detail of each event named `name`, with its log time, in order, beside the script step it
/// happened in: the `step` of the last `script_step` before it (0 before the first).
fn stepped<'a>(events: &'a [Value], name: &str) -> Vec<(u64, f64, &'a Value)> {
    let mut step = 0;
    let mut found = Vec::new();
    for event in events {
        if event["event"] == "script_step" {
            step = event["detail"]["step"].as_u64().unwrap_or(step);
        } else if event["event"] == name {
            found.push((
                step,
                event["elapsed_ms"].as_f64().unwrap_or(f64::NAN),
                &event["detail"],
            ));
        }
    }
    found
}

#[cfg(test)]
mod tests;
