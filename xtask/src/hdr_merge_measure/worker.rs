//! The worker: one process the run starts for each job ([`Job`]), so a cold merge is the first in
//! a new process and the process's peak memory belongs to the work the job names. It reads its
//! job from a file and writes what it observed beside it as `result.json`; the parent turns that
//! into rows.
//!
//! Every merge goes into a new catalog in the job's folder, so no merge is answered from an earlier
//! one's exact match; the catalog's preparation (indexing the bracket's folder, its catalog folder,
//! anything else the job needs) is done and settled before the clock starts.
use super::{session::*, *};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// What a worker does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// One merge, the first in the process.
    Cold,
    /// A first merge, then `warm` more, each cancellation and the contention rounds.
    Warm,
    /// Merge copies of the inputs, then undo and redo the merge.
    ReopenCreate,
    /// Reopen that catalog with its inputs gone: inspect, undo, redo, prepare and export.
    Reopen,
    /// Merge again and again until the stop file appears.
    Busy,
    /// Look for brackets in a folder of a person's files and merge up to three.
    Natural,
}

/// Where a merge is cancelled: once the board shows it there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CancelAt {
    /// As the second input begins.
    Reading,
    /// As alignment begins.
    Aligning,
    /// Half way through the chunks.
    Merging,
    /// As the last chunk is written.
    Saving,
}

impl CancelAt {
    pub const ALL: [Self; 4] = [Self::Reading, Self::Aligning, Self::Merging, Self::Saving];

    pub fn name(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Aligning => "aligning",
            Self::Merging => "merging",
            Self::Saving => "saving",
        }
    }

    /// Whether a merge whose board entry shows `phase` and `message` has reached this point.
    pub fn due(self, phase: &str, message: &str) -> bool {
        match self {
            Self::Reading => message.starts_with("Reading 2 of"),
            Self::Aligning => phase == "aligning",
            Self::Merging => {
                progress_of(message, "Merging").is_some_and(|(done, of)| 2 * done >= of)
            }
            Self::Saving => phase == "saving",
        }
    }
}

/// `(k, n)` of a progress message `"{verb} k of n"`.
pub fn progress_of(message: &str, verb: &str) -> Option<(u32, u32)> {
    let rest = message.strip_prefix(verb)?.trim();
    let (done, of) = rest.split_once(" of ")?;
    Some((done.trim().parse().ok()?, of.trim().parse().ok()?))
}

/// The editor work a contention job times, idle and beside a merge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contention {
    /// The photograph developed, prepared, sampled, edited and exported.
    pub photo: PathBuf,
    /// A folder of files to browse.
    pub browse: PathBuf,
    /// Samples of each operation idle, and at least as many beside merges.
    pub rounds: usize,
    /// The most merges run to collect them.
    pub merges: usize,
}

/// One worker's job.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub mode: Mode,
    pub workload: String,
    /// The bracket's files, in capture order.
    pub inputs: Vec<PathBuf>,
    /// The worker's folder: its catalogs, its log and its result.
    pub dir: PathBuf,
    #[serde(default)]
    pub warm: usize,
    #[serde(default)]
    pub cancellations: usize,
    #[serde(default)]
    pub contention: Option<Contention>,
    /// The catalog [`Mode::Reopen`] reopens.
    #[serde(default)]
    pub catalog: Option<PathBuf>,
    /// The merge [`Mode::Reopen`] inspects.
    #[serde(default)]
    pub merge_id: Option<String>,
    /// The file whose appearance stops [`Mode::Busy`].
    #[serde(default)]
    pub stop: Option<PathBuf>,
    /// The folder [`Mode::Natural`] looks in.
    #[serde(default)]
    pub natural: Option<PathBuf>,
}

pub const RESULT: &str = "result.json";

/// Run the job in `path` and write its result beside it.
pub fn run(path: &Path) -> Result {
    let job: Job = serde_json::from_value(read_json(path)?)?;
    fs::create_dir_all(&job.dir)?;
    // The synthetic brackets' body is a RAW test camera; a natural folder is read as shipped.
    if job.mode != Mode::Natural {
        luxforge_raw::admit_test_cameras();
    }
    let result = match job.mode {
        Mode::Cold => cold(&job),
        Mode::Warm => warm(&job),
        Mode::ReopenCreate => reopen_create(&job),
        Mode::Reopen => reopen(&job),
        Mode::Busy => busy(&job),
        Mode::Natural => natural(&job),
    }?;
    write_json(&job.dir.join(RESULT), &result)
}

/// A new catalog `name` in the job's folder with `folder` indexed and a catalog folder for results:
/// the session and the folder's id, every job it started settled.
fn fresh(job: &Job, name: &str, folder: &Path) -> Result<(Session, Value)> {
    let dir = job.dir.join(name);
    ensure(!dir.exists(), format!("{} exists", dir.display()))?;
    fs::create_dir_all(&dir)?;
    let session = Session::open(&dir.join("catalog.sqlite"))?;
    let listed = session.ask(
        "index.refresh",
        json!({"source": {"kind": "folder", "path": folder}}),
    )?;
    session.finished(&listed)?;
    let created = session.ask(
        "folder.create",
        json!({"name": "Merges", "mutation": session.mutation("folder")}),
    )?;
    let folder_id = created["folder"]["id"].clone();
    session.settle()?;
    Ok((session, folder_id))
}

fn bracket_folder(inputs: &[PathBuf]) -> Result<PathBuf> {
    Ok(inputs
        .first()
        .and_then(|path| path.parent())
        .ok_or("a bracket has no folder")?
        .to_path_buf())
}

/// What the work beside a merge does on each call: one operation, timed, while the merge runs.
type Between<'a> = &'a mut dyn FnMut(&Session, &str) -> Result;

/// Plan and merge `inputs` into `folder_id`, following the job on the board until it ends:
/// cancelled at `cancel` when named, with `between` called instead of waiting while it runs.
pub fn merge(
    session: &Session,
    inputs: &[PathBuf],
    folder_id: &Value,
    cancel: Option<CancelAt>,
    mut between: Option<Between<'_>>,
) -> Result<Value> {
    let targets = json!({"kind": "paths", "paths": inputs});
    let plan = session.ask("merge.plan", json!({"targets": targets}))?;
    ensure(
        plan["suitable"] == true,
        format!("the plan refused: {plan}"),
    )?;
    let footprint_before = footprint(session)?;
    let requested = Instant::now();
    let answer = session.ask(
        "merge.create",
        json!({
            "kind": "hdr",
            "targets": targets,
            "plan_id": plan["plan_id"],
            "folder_id": folder_id,
            "mutation": session.mutation("merge"),
        }),
    )?;
    let job = job_id(&answer)?;
    let mut phases: Vec<(String, f64)> = Vec::new();
    let mut messages: Vec<(String, f64)> = Vec::new();
    let mut footprints = Vec::new();
    let mut accounting = Accounting::default();
    let mut sampled = None::<Instant>;
    let mut cancelled = None::<(Instant, String, String)>;
    loop {
        let board = session.board();
        let Some(entry) = board
            .active
            .iter()
            .find(|entry| entry.entry.job_id.as_deref() == Some(job.as_str()))
        else {
            break;
        };
        let at = ms(requested.elapsed());
        let phase = entry.entry.phase.as_deref().unwrap_or("").to_owned();
        let message = entry
            .entry
            .progress
            .as_ref()
            .and_then(|progress| progress.message.clone())
            .unwrap_or_default();
        if !phase.is_empty() && phases.last().is_none_or(|(last, _)| *last != phase) {
            phases.push((phase.clone(), at));
        }
        if !message.is_empty() && messages.last().is_none_or(|(last, _)| *last != message) {
            messages.push((message.clone(), at));
        }
        if let Some(stage) = cancel
            && cancelled.is_none()
            && stage.due(&phase, &message)
        {
            let now = Instant::now();
            session.ask("job.cancel", json!({"job_id": job}))?;
            cancelled = Some((now, phase.clone(), message.clone()));
        }
        if sampled.is_none_or(|last| last.elapsed() >= SAMPLE_EVERY) {
            sampled = Some(Instant::now());
            footprints.push(footprint(session)?);
            let read = session.ask("job.read", json!({"job_id": job}))?;
            accounting.add(&read["result"]["accounting"]);
        }
        match between.as_mut() {
            Some(work) => work(session, &job)?,
            None => session.pause(SAMPLE_EVERY),
        }
    }
    let (record, ended) = session.wait_job(&job)?;
    let total_ms = ms(ended.saturating_duration_since(requested));
    accounting.add(&record["result"]["accounting"]);
    footprints.push(footprint(session)?);
    let artifacts = session.artifact_root();
    let spool_left = spool_files(&artifacts.join("merges"))?;
    let status = record["status"].as_str().unwrap_or_default().to_owned();
    let mut run = json!({
        "status": status,
        "total_ms": total_ms,
        "stages": stages(&phases, total_ms),
        "reads_ms": reads(&messages, &phases),
        "phases": phases,
        "messages": messages.len(),
        "chunks": messages.iter().rev().find_map(|(message, _)| progress_of(message, "Merging").map(|(_, of)| of)),
        "footprint_before_bytes": footprint_before,
        "footprint_max_bytes": footprints.iter().copied().max(),
        "footprint_samples": footprints.len(),
        "accounting": accounting.value(),
        "spool_files_left": spool_left,
        "error": record["error"],
    });
    if let Some((at, phase, message)) = cancelled {
        run["cancel"] = json!({
            "requested_ms": ms(at.saturating_duration_since(requested)),
            "latency_ms": ms(ended.saturating_duration_since(at)),
            "phase": phase,
            "message": message,
        });
    }
    if status == "ready" {
        let result = &record["result"];
        let inspected = session.ask("merge.inspect", json!({"merge_id": result["merge_id"]}))?;
        let (files, bytes) = tree(&artifacts.join("sources"))?;
        run["result"] = json!({
            "merge_id": result["merge_id"],
            "result_asset_id": result["result_asset_id"],
            "source_id": result["source_id"],
            "source_files": files,
            "source_bytes": bytes,
            "resolved": inspected["provenance"]["resolved"],
        });
    }
    Ok(run)
}

/// The process's memory in the platform monitor's measure, now.
fn footprint(session: &Session) -> Result<Option<u64>> {
    Ok(session.ask("resources.read", json!({}))?["memory"]["bytes"].as_u64())
}

/// The process's peak memory so far, as the platform records it.
fn process_peak(session: &Session) -> Result<Value> {
    let read = session.ask("resources.read", json!({}))?;
    Ok(json!({
        "kind": read["memory"]["kind"],
        "peak_bytes": read["memory"]["peak_bytes"],
        "bytes": read["memory"]["bytes"],
        "gpu": read["gpu"],
    }))
}

/// The largest of each figure a merge's accounting reported.
#[derive(Default)]
struct Accounting {
    scratch_peak: u64,
    scratch_limit: u64,
    temp: u64,
    development: u64,
}

impl Accounting {
    fn add(&mut self, accounting: &Value) {
        let read = |key: &str| accounting[key].as_u64().unwrap_or(0);
        self.scratch_peak = self
            .scratch_peak
            .max(read("scratch_peak_bytes"))
            .max(read("scratch_bytes"));
        self.scratch_limit = self.scratch_limit.max(read("scratch_limit_bytes"));
        self.temp = self.temp.max(read("temp_bytes"));
        self.development = self.development.max(read("development_bytes"));
    }

    fn value(&self) -> Value {
        json!({
            "scratch_peak_bytes": self.scratch_peak,
            "scratch_limit_bytes": self.scratch_limit,
            "temp_bytes_max": self.temp,
            "development_bytes": self.development,
        })
    }
}

/// Each phase's duration in ms from the order the phases were first seen in and the merge's total:
/// `reading` from the request (its queueing included) to `aligning`, each later one to the next
/// seen, `saving` to the end. A phase never seen (it began and ended between two looks) is null,
/// and the one before it holds its time.
pub fn stages(phases: &[(String, f64)], total_ms: f64) -> Value {
    let order = ["reading", "aligning", "merging", "saving"];
    let seen = |name: &str| {
        phases
            .iter()
            .find(|(phase, _)| phase == name)
            .map(|(_, at)| *at)
    };
    let mut stages = serde_json::Map::new();
    for (i, name) in order.iter().enumerate() {
        let start = if i == 0 { Some(0.0) } else { seen(name) };
        let value = start.map(|start| {
            let end = order[i + 1..]
                .iter()
                .find_map(|next| seen(next))
                .unwrap_or(total_ms);
            (end - start).max(0.0)
        });
        stages.insert((*name).into(), json!(value));
    }
    Value::Object(stages)
}

/// Each input's read and development in ms, in reading order: from its `Reading k of n` to the
/// next, the last to `aligning`. Empty when the board showed too few of them to tell.
pub fn reads(messages: &[(String, f64)], phases: &[(String, f64)]) -> Vec<f64> {
    let mut starts: Vec<(u32, f64)> = messages
        .iter()
        .filter_map(|(message, at)| progress_of(message, "Reading").map(|(k, _)| (k, *at)))
        .collect();
    starts.dedup_by_key(|(k, _)| *k);
    let total = messages
        .iter()
        .find_map(|(message, _)| progress_of(message, "Reading").map(|(_, n)| n));
    let aligning = phases
        .iter()
        .find(|(phase, _)| phase == "aligning")
        .map(|(_, at)| *at);
    if total != Some(starts.len() as u32) || aligning.is_none() {
        return Vec::new();
    }
    let ends = starts.iter().skip(1).map(|(_, at)| *at).chain(aligning);
    starts
        .iter()
        .zip(ends)
        .map(|((_, start), end)| (end - start).max(0.0))
        .collect()
}

/// How many spool files are left under the merges' folder.
fn spool_files(merges: &Path) -> Result<usize> {
    let Ok(folders) = fs::read_dir(merges) else {
        return Ok(0);
    };
    let mut count = 0;
    for folder in folders.flatten() {
        if let Ok(entries) = fs::read_dir(folder.path()) {
            count += entries
                .flatten()
                .filter(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.starts_with("input-") && name.ends_with(".spool")
                })
                .count();
        }
    }
    Ok(count)
}

/// The files under `dir`, recursively, and their total bytes.
pub fn tree(dir: &Path) -> Result<(usize, u64)> {
    let mut files = 0;
    let mut bytes = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok((0, 0));
    };
    for entry in entries.flatten() {
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            let (more, size) = tree(&entry.path())?;
            files += more;
            bytes += size;
        } else {
            files += 1;
            bytes += metadata.len();
        }
    }
    Ok((files, bytes))
}

/// Every file name under `dir` with its length, sorted: what a step must leave as it was.
fn listing(dir: &Path) -> Result<Vec<(String, u64)>> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(found);
    };
    for entry in entries.flatten() {
        let metadata = entry.metadata()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if metadata.is_dir() {
            for (inner, size) in listing(&entry.path())? {
                found.push((format!("{name}/{inner}"), size));
            }
        } else {
            found.push((name, metadata.len()));
        }
    }
    found.sort();
    Ok(found)
}

fn cold(job: &Job) -> Result<Value> {
    let folder = bracket_folder(&job.inputs)?;
    let (session, folder_id) = fresh(job, "catalog", &folder)?;
    let idle = process_peak(&session)?;
    let run = merge(&session, &job.inputs, &folder_id, None, None)?;
    let peak = process_peak(&session)?;
    session.close()?;
    Ok(json!({"merge": run, "before": idle, "process": peak}))
}

fn warm(job: &Job) -> Result<Value> {
    let folder = bracket_folder(&job.inputs)?;
    let mut samples = 0;
    let mut next = || {
        samples += 1;
        format!("catalog-{samples}")
    };
    let (session, folder_id) = fresh(job, &next(), &folder)?;
    let first = merge(&session, &job.inputs, &folder_id, None, None)?;
    session.close()?;
    let mut runs = Vec::new();
    for _ in 0..job.warm {
        let (session, folder_id) = fresh(job, &next(), &folder)?;
        runs.push(merge(&session, &job.inputs, &folder_id, None, None)?);
        session.close()?;
    }
    let mut cancellations = serde_json::Map::new();
    for stage in CancelAt::ALL {
        let mut observed = Vec::new();
        for _ in 0..job.cancellations {
            let (session, folder_id) = fresh(job, &next(), &folder)?;
            observed.push(merge(&session, &job.inputs, &folder_id, Some(stage), None)?);
            session.close()?;
        }
        cancellations.insert(stage.name().into(), json!(observed));
    }
    let contention = match &job.contention {
        Some(contention) => Some(contend(job, contention, &folder, &mut next)?),
        None => None,
    };
    let peak = {
        let (session, _) = fresh(job, &next(), &folder)?;
        let peak = process_peak(&session)?;
        session.close()?;
        peak
    };
    Ok(json!({
        "first": first,
        "warm": runs,
        "cancellations": cancellations,
        "contention": contention,
        "process": peak,
    }))
}

/// The editor work a contention job times: one operation of each kind in turn.
pub const OPERATIONS: [&str; 4] = ["browse_rows", "render_sample", "commit", "export"];

/// What editor work needs in a catalog: the photograph developed and prepared, and how many rows
/// the browse folder's view holds.
struct Editing {
    asset: Value,
    rows: u64,
    exports: PathBuf,
    turn: usize,
}

impl Editing {
    /// A new catalog ready for editor work beside a merge: the bracket indexed, the browse folder
    /// in a view with its previews read, the photograph developed and prepared, all settled.
    fn open(
        job: &Job,
        contention: &Contention,
        folder: &Path,
        name: &str,
    ) -> Result<(Session, Value, Self)> {
        let (session, folder_id) = fresh(job, name, folder)?;
        let started = session.ask(
            "pick.develop",
            json!({"targets": {"kind": "paths", "paths": [contention.photo]}, "into": [],
                   "confirm_removable": true, "mutation": session.mutation("develop")}),
        )?;
        let developed = session.finished(&started)?;
        let asset = developed["developed"][0]["asset_id"].clone();
        ensure(asset.is_string(), format!("nothing developed: {developed}"))?;
        let (owner, client) = session.owner();
        luxforge_testkit::client::prepare(owner, client, &asset)?;
        let listed = session.ask(
            "index.refresh",
            json!({"source": {"kind": "folder", "path": contention.browse}}),
        )?;
        session.finished(&listed)?;
        let view = session.ask(
            "browse.view",
            json!({"source": {"kind": "folder", "path": contention.browse}}),
        )?;
        let rows = view["count"].as_u64().unwrap_or(0);
        ensure(rows >= 200, format!("the browse view holds {rows} rows"))?;
        session.settle()?;
        let exports = session.catalog.with_file_name("exports");
        fs::create_dir_all(&exports)?;
        Ok((
            session,
            folder_id,
            Self {
                asset,
                rows,
                exports,
                turn: 0,
            },
        ))
    }

    /// The next operation through `session`, timed: its name and ms.
    fn next(&mut self, session: &Session) -> Result<(&'static str, f64)> {
        let operation = OPERATIONS[self.turn % OPERATIONS.len()];
        let turn = self.turn as u64;
        self.turn += 1;
        let (owner, client) = session.owner();
        let started = Instant::now();
        match operation {
            "browse_rows" => {
                let from = turn * 7919 % (self.rows - 199);
                let rows = session.ask("browse.rows", json!({"from": from, "count": 200}))?;
                ensure(
                    rows["rows"].as_array().map_or(0, Vec::len) == 200,
                    "a window of rows came back short",
                )?;
            }
            "render_sample" => {
                session.ask(
                    "render.sample",
                    json!({"asset_id": self.asset, "x": turn * 811 % 6000, "y": turn * 409 % 4000}),
                )?;
            }
            "commit" => {
                let revision = luxforge_testkit::client::revision(owner, client, &self.asset)?;
                let started = Instant::now();
                session.ask(
                    "edit.set-basic",
                    json!({"asset_id": self.asset, "exposure": 0.05 * (turn % 20) as f64 - 0.5,
                           "mutation": luxforge_testkit::client::mutation(
                               revision, &format!("commit-{turn}-{}", std::process::id()), ACTOR)}),
                )?;
                return Ok((operation, ms(started.elapsed())));
            }
            _ => {
                let destination = self.exports.join(format!("export-{turn}.jpg"));
                let accepted = session.ask(
                    "export.jpeg",
                    json!({"asset_id": self.asset, "destination": destination,
                           "mutation": session.mutation("export")}),
                )?;
                let job = job_id(&accepted)?;
                let (record, ended) = session.wait_job(&job)?;
                ensure(
                    record["status"] == "ready",
                    format!("export ended {record}"),
                )?;
                let _ = fs::remove_file(&destination);
                return Ok((operation, ms(ended.saturating_duration_since(started))));
            }
        }
        Ok((operation, ms(started.elapsed())))
    }
}

/// Editor work alone, then beside merges in the same owner: each operation's samples idle and
/// those that ended while a merge still ran, with the merges themselves.
fn contend(
    job: &Job,
    contention: &Contention,
    folder: &Path,
    next: &mut dyn FnMut() -> String,
) -> Result<Value> {
    let mut idle: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    {
        let (session, _, mut editing) = Editing::open(job, contention, folder, &next())?;
        for _ in 0..contention.rounds * OPERATIONS.len() {
            let (operation, took) = editing.next(&session)?;
            idle.entry(operation).or_default().push(took);
        }
        session.close()?;
    }
    let mut during: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut merges = Vec::new();
    for _ in 0..contention.merges.max(1) {
        let (session, folder_id, mut editing) = Editing::open(job, contention, folder, &next())?;
        let mut taken: Vec<(&'static str, f64)> = Vec::new();
        let mut work = |session: &Session, merge_job: &str| -> Result {
            let (operation, took) = editing.next(session)?;
            let running = session
                .board()
                .active
                .iter()
                .any(|entry| entry.entry.job_id.as_deref() == Some(merge_job));
            if running {
                taken.push((operation, took));
            }
            Ok(())
        };
        let run = merge(&session, &job.inputs, &folder_id, None, Some(&mut work))?;
        session.close()?;
        for (operation, took) in taken {
            during.entry(operation).or_default().push(took);
        }
        merges.push(run);
        if during
            .get("export")
            .is_some_and(|samples| samples.len() >= contention.rounds)
        {
            break;
        }
    }
    Ok(json!({"idle": idle, "during": during, "merges": merges}))
}

/// The objects a saved result rests on, and the merges' working folders: what undo, redo,
/// reopening and rendering must leave exactly as they were.
fn closure(session: &Session) -> Result<Value> {
    let root = session.artifact_root();
    let merges: Vec<String> = fs::read_dir(root.join("merges"))
        .map(|entries| {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        })
        .unwrap_or_default();
    Ok(json!({"sources": listing(&root.join("sources"))?, "merges": merges}))
}

/// Undo and redo the merge's creation through the library journal, each timed, checking the
/// merge's state after each.
fn undo_redo(session: &Session, merge_id: &Value) -> Result<Value> {
    let state = || -> Result<Value> {
        Ok(session.ask("merge.inspect", json!({"merge_id": merge_id}))?["state"].clone())
    };
    let started = Instant::now();
    let undone = session.ask(
        "library.undo",
        json!({"mutation": session.mutation("undo")}),
    )?;
    let undo_ms = ms(started.elapsed());
    ensure(undone["outcome"] == "applied", format!("undo: {undone}"))?;
    let after_undo = state()?;
    let started = Instant::now();
    let redone = session.ask(
        "library.redo",
        json!({"mutation": session.mutation("redo")}),
    )?;
    let redo_ms = ms(started.elapsed());
    ensure(redone["outcome"] == "applied", format!("redo: {redone}"))?;
    let after_redo = state()?;
    ensure(
        after_undo == "undone" && after_redo == "active",
        format!("undo left {after_undo}, redo {after_redo}"),
    )?;
    Ok(json!({"undo_ms": undo_ms, "redo_ms": redo_ms}))
}

/// Merge copies of the inputs into a catalog of their own, then undo and redo the merge: no merge
/// activity on the board and no saved object changed.
fn reopen_create(job: &Job) -> Result<Value> {
    let inputs_dir = job.dir.join("inputs");
    fs::create_dir_all(&inputs_dir)?;
    let mut inputs = Vec::new();
    for input in &job.inputs {
        let copy = inputs_dir.join(input.file_name().ok_or("an input has no name")?);
        fs::copy(input, &copy)?;
        inputs.push(copy.canonicalize()?);
    }
    let (session, folder_id) = fresh(job, "catalog", &inputs_dir.canonicalize()?)?;
    let run = merge(&session, &inputs, &folder_id, None, None)?;
    ensure(run["status"] == "ready", format!("the merge ended {run}"))?;
    let merge_id = run["result"]["merge_id"].clone();
    let before = closure(&session)?;
    let activity = session.merge_activity();
    let journal = undo_redo(&session, &merge_id)?;
    let merge_changes = session.merge_activity() - activity;
    let after = closure(&session)?;
    ensure(
        merge_changes == 0,
        format!("undo and redo changed {merge_changes} merges on the board"),
    )?;
    ensure(before == after, "undo and redo changed the saved objects")?;
    let catalog = session.catalog.clone();
    session.close()?;
    Ok(json!({
        "merge": run,
        "catalog": catalog,
        "merge_id": merge_id,
        "inputs": inputs,
        "journal": journal,
        "merge_activity": merge_changes,
    }))
}

/// Reopen the catalog [`reopen_create`] made, in a new process, with its inputs moved away:
/// every input `missing` and the source `available`; undo and redo again; prepare and export the
/// result; no merge activity on the board and no saved object or working folder changed
/// throughout. With the inputs gone, none of this can have read one.
fn reopen(job: &Job) -> Result<Value> {
    let catalog = job.catalog.as_ref().ok_or("reopen needs a catalog")?;
    let merge_id = json!(job.merge_id.as_ref().ok_or("reopen needs a merge")?);
    for input in &job.inputs {
        ensure(
            !input.exists(),
            format!(
                "{} is still there; reopening must not reach it",
                input.display()
            ),
        )?;
    }
    let started = Instant::now();
    let session = Session::open(catalog)?;
    let inspected = session.ask("merge.inspect", json!({"merge_id": merge_id}))?;
    let open_ms = ms(started.elapsed());
    let before = closure(&session)?;
    let activity = session.merge_activity();
    let files: Vec<Value> = inspected["inputs"]
        .as_array()
        .map(|inputs| inputs.iter().map(|input| input["file"].clone()).collect())
        .unwrap_or_default();
    ensure(
        files.len() == job.inputs.len() && files.iter().all(|file| file == "missing"),
        format!("the inputs read {files:?}, not every one missing"),
    )?;
    ensure(
        inspected["source"]["status"] == "available",
        format!("the source is {}", inspected["source"]),
    )?;
    let journal = undo_redo(&session, &merge_id)?;
    let asset = inspected["result_asset_id"].clone();
    let footprint_before = footprint(&session)?;
    let started = Instant::now();
    let prepared = session.ask("source.prepare", json!({"asset_id": asset}))?;
    if prepared["job_id"].is_string() {
        session.finished(&prepared)?;
    }
    let prepare_ms = ms(started.elapsed());
    let footprint_prepared = footprint(&session)?;
    let destination = job.dir.join("result.jpg");
    let started = Instant::now();
    let accepted = session.ask(
        "export.jpeg",
        json!({"asset_id": asset, "destination": destination, "mutation": session.mutation("export")}),
    )?;
    let exported = session.finished(&accepted)?;
    let export_ms = ms(started.elapsed());
    let merge_changes = session.merge_activity() - activity;
    let after = closure(&session)?;
    ensure(
        merge_changes == 0,
        format!("reopening changed {merge_changes} merges on the board"),
    )?;
    ensure(before == after, "reopening changed the saved objects")?;
    let peak = process_peak(&session)?;
    session.close()?;
    Ok(json!({
        "open_ms": open_ms,
        "journal": journal,
        "prepare_ms": prepare_ms,
        "export_ms": export_ms,
        "export": {"width": exported["width"], "height": exported["height"],
                   "renderer": exported["renderer"], "path": destination},
        "footprint_before_prepare_bytes": footprint_before,
        "footprint_prepared_bytes": footprint_prepared,
        "inputs": files,
        "merge_activity": merge_changes,
        "process": peak,
    }))
}

/// Merge into one new catalog after another until the stop file appears.
fn busy(job: &Job) -> Result<Value> {
    let folder = bracket_folder(&job.inputs)?;
    let stop = job.stop.as_ref().ok_or("busy needs a stop file")?;
    let mut runs = Vec::new();
    let mut count = 0;
    while !stop.exists() {
        count += 1;
        let (session, folder_id) = fresh(job, &format!("catalog-{count}"), &folder)?;
        fs::write(job.dir.join("merging"), count.to_string())?;
        runs.push(merge(&session, &job.inputs, &folder_id, None, None)?);
        session.close()?;
    }
    Ok(json!({"merges": runs}))
}

/// Look for brackets among a person's files, read only, in a new catalog: the brackets Select
/// finds there, and up to three of them merged, with each one's outcome and diagnostics and the
/// result exported for a look.
fn natural(job: &Job) -> Result<Value> {
    let folder = job.natural.as_ref().ok_or("natural needs a folder")?;
    let (session, folder_id) = fresh(job, "catalog", folder)?;
    let view = session.ask(
        "browse.view",
        json!({"source": {"kind": "folder", "path": folder}}),
    )?;
    session.settle()?;
    let brackets: Vec<Value> = view["groups"]["moments"]
        .as_array()
        .map(|moments| {
            moments
                .iter()
                .filter(|moment| moment["kind"] == "bracket")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut cases = Vec::new();
    for (at, moment) in brackets.iter().take(3).enumerate() {
        let rows = session.ask(
            "browse.rows",
            json!({"from": moment["start"], "count": moment["len"]}),
        )?;
        let paths: Vec<PathBuf> = rows["rows"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["path"].as_str().map(PathBuf::from))
                    .collect()
            })
            .unwrap_or_default();
        let outcome = (|| -> Result<Value> {
            let mut run = merge(&session, &paths, &folder_id, None, None)?;
            if run["status"] == "ready" {
                let asset = run["result"]["result_asset_id"].clone();
                let destination = job.dir.join(format!("natural-{at}.jpg"));
                let accepted = session.ask(
                    "export.jpeg",
                    json!({"asset_id": asset, "destination": destination,
                           "mutation": session.mutation("export")}),
                )?;
                session.finished(&accepted)?;
                run["export"] = json!(destination);
            }
            Ok(run)
        })();
        cases.push(json!({
            "moment": moment,
            "inputs": paths,
            "outcome": outcome
                .unwrap_or_else(|error| json!({"status": "failed", "error": error.to_string()})),
        }));
    }
    session.close()?;
    Ok(json!({
        "folder": folder,
        "files": view["count"],
        "brackets": brackets.len(),
        "cases": cases,
    }))
}
