//! The catalog measurement's desktop probes ([catalog design](../../../docs/design/catalog.md#performance),
//! TASK-025): loupe stepping with the look-ahead warm and a held arrow ([`loupe`]), the 100% focus
//! check ([`focus`]) and grid scroll ([`grid`]).
//!
//! Every probe drives the release editor through the background evidence launch every timing tool
//! makes ([`Run`], a hidden window in a background-only bundle on macOS), over a folder read into a
//! catalog of its own before the editor opens it ([`prepare`]), and times what the editor itself
//! records in its `events.jsonl`: never a wall clock around the process, a sleep or a screenshot.
//!
//! **Presented** means what it means for the photograph's `preview_displayed` ([performance
//! spec](../../../docs/specs/performance.md)): the update in which the picture became the view's
//! source, drawn by the redraw that update requests. For the loupe that is the update whose
//! derived model draws the active frame's own picture (`loupe_presented`, under its own item and
//! preview key; a stand-in drawn while the tier is read is never counted) or the inset's region
//! (`loupe_region_presented`). It is not display scanout, which the harness cannot observe, and the
//! window is invisible, so nothing is composited: each figure is an upper bound on the editor's own
//! work and a lower bound on what an eye sees.
//!
//! Each probe answers rows of the one report shape ([`stats::row`]) with a `scope` of the facts
//! that bound them — samples, files, the preview's origin and size, warm or cold, the key interval —
//! and never a verdict, or `{"metric", "status": "not_measured", "reason"}`. A probe that cannot
//! run fails with an error naming its run directory, whose `result.json` lists every launch.
mod focus;
mod grid;
mod loupe;

use crate::{
    generate_catalog,
    scenario::{Launch, Run},
    smoke::PANELLED,
    stats, *,
};
use luxforge_core::{ApiRequest, ClientId, OwnerHandle};
use luxforge_evidence::{self as script, SelectStep};
use std::time::Duration;

/// What the probes run with: exactly the measuring command's `DesktopContext`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the catalog measurement's desktop stub (lane A) delegates here once it lands"
    )
)]
pub(crate) struct ProbeContext {
    /// The release editor.
    pub binary: PathBuf,
    /// A directory the probes write into: each probe's run, its catalogs and the images it
    /// generates, each in a new directory of its own.
    pub scratch: PathBuf,
    /// Measured samples per figure.
    pub samples: usize,
    /// The generated folder grid scroll is measured over (10,000 files).
    pub folder_10k: PathBuf,
    /// A folder of RAW files from real cameras, for the focus check from a development per camera
    /// and loupe stepping over RAW previews.
    pub raw_trip: Option<PathBuf>,
}

/// The one tool name every probe run records itself under.
const TOOL: &str = "catalog-probes";
/// The seed and size of the folder of generated JPEGs the loupe and the focus check run over.
const SEED: u64 = 1;
const IMAGES: u32 = 120;
/// Each launch's own deadline: the editor's evidence deadline for a script is 60 seconds, and the
/// background bundle is copied first.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(120);

/// Every desktop probe, in order: the loupe, the focus check, grid scroll.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the catalog measurement's desktop stub (lane A) delegates here once it lands"
    )
)]
pub(crate) fn desktop_probes(context: &ProbeContext) -> Result<Vec<Value>> {
    ensure(
        (1..=10_000).contains(&context.samples),
        "The desktop probes take 1 to 10,000 samples",
    )?;
    let root = root()?;
    let generated = context.scratch.join("generated");
    generate_catalog::run(
        &generated,
        &generate_catalog::Options {
            seed: SEED,
            files: None,
            assets: None,
            images: Some(IMAGES),
        },
    )?;
    let images = Source {
        label: format!(
            "{IMAGES} generated JPEGs (generate-catalog --images {IMAGES} --seed {SEED})"
        ),
        folder: generated.join("images"),
    };
    let raw = context.raw_trip.as_ref().map(|folder| Source {
        label: "the RAW trip".into(),
        folder: folder.clone(),
    });
    let mut rows = loupe::probe(&root, context, &images, raw.as_ref())?;
    rows.extend(focus::probe(&root, context, &images, raw.as_ref())?);
    rows.extend(grid::probe(context));
    Ok(rows)
}

/// A folder a probe browses, and what its rows call it.
struct Source {
    label: String,
    folder: PathBuf,
}

/// A measured row: the one report shape with the facts that bound it.
fn measured(metric: &str, unit: &str, samples: Vec<f64>, scope: Value) -> Value {
    let mut row = stats::row(metric, unit, samples);
    row["scope"] = scope;
    row
}

/// A row for a figure this run could not take, and why.
fn not_measured(metric: &str, reason: impl Into<String>) -> Value {
    json!({"metric": metric, "status": "not_measured", "reason": reason.into()})
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

/// What one launch left: its evidence directory, its events, the record of its last captured frame
/// and the build profile the editor reported.
struct Launched {
    dir: PathBuf,
    events: Vec<Value>,
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
    let last = app["frames"]
        .as_array()
        .and_then(|frames| frames.last())
        .cloned()
        .ok_or("No frame was captured")?;
    Ok(Launched {
        dir,
        events,
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

/// The facts every row of one launch shares: the window, its scale, the backend and the profile.
fn launch_scope(launched: &Launched) -> Value {
    json!({
        "window": PANELLED.map(|side| side.parse::<u32>().ok()),
        "physical_size": launched.last["physical_size"],
        "scale": launched.last["scale"],
        "backend": launched.last["state"]["backend"]["backend"],
        "profile": launched.profile,
        "launch": "background evidence launch, hidden window",
    })
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
