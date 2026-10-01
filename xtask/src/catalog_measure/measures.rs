//! Each step's figures. The core runs in the harness's own process over scratch catalogs
//! ([`Core`]), built with the harness's own profile; the editor runs as a background launch of the
//! release binary, through the scenario library and `editor-latency`.
use super::{
    client::{Core, job_id, ms, timed},
    data::{DataSet, Tree, Trip},
    report::Row,
};
use crate::{
    editor_latency,
    scenario::{
        self, Launch, Run,
        launch::{Poll, Watched, watch},
    },
    stats, *,
};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// The longest any job the harness waits for may take: a first index of 200,000 files reads
/// headers for a minute or two.
const JOB_DEADLINE: Duration = Duration::from_secs(30 * 60);
/// How long the indexed folders may take to be watched once listed.
const WATCH_DEADLINE: Duration = Duration::from_secs(60);
/// How often an owner round trip is sampled while a first index runs.
const ROUND_TRIP_EVERY: Duration = Duration::from_millis(100);
/// How long the editor settles after its startup before its idle window's own settle: long
/// enough for its owner to open the catalog and watch its indexed folders.
const EDITOR_SETTLE: Duration = Duration::from_secs(5);
/// One screen of the grid: the block of rows the desktop reads at a time.
const SCREEN: u64 = 200;
const MIB: f64 = 1024.0 * 1024.0;

/// The bench `bracket` runs, in `luxforge-core`'s `organize` tests, and the environment it reads.
pub const BENCH: &str = "organize::tests::bracket_timing::bracket_probe_per_run";
const BENCH_CATALOG: &str = "LUXFORGE_BRACKET_CATALOG";
const BENCH_FOLDER: &str = "LUXFORGE_BRACKET_FOLDER";
const BENCH_EVALUATIONS: &str = "LUXFORGE_BRACKET_EVALUATIONS";
const BENCH_OUT: &str = "LUXFORGE_BRACKET_OUT";

/// What every step reads.
pub struct Context<'a> {
    pub root: &'a Path,
    pub out: &'a Path,
    /// The release editor.
    pub binary: &'a Path,
    /// Samples a figure.
    pub samples: usize,
    /// Samples a journey: a first browse.
    pub journeys: usize,
    pub data: &'a DataSet,
    /// The JPEG the drags run over.
    pub fixture: PathBuf,
}

impl Context<'_> {
    /// A catalog of its own for `name`, which does not exist yet.
    fn catalog(&self, name: &str) -> Result<PathBuf> {
        let dir = self.data.scratch.join("catalogs").join(name);
        ensure(!dir.exists(), format!("{} already exists", dir.display()))?;
        Ok(dir.join("catalog.sqlite"))
    }
}

/// One figure of a step: its metric's last part, unit, the design's target and what it times.
pub struct Figure {
    pub name: &'static str,
    pub unit: &'static str,
    pub target: &'static str,
    pub scope: &'static str,
}

impl Figure {
    fn metric(&self, prefix: &str) -> String {
        format!("{prefix}.{}", self.name)
    }

    fn row(&self, prefix: &str, samples: Vec<f64>) -> Row {
        Row::measured(&self.metric(prefix), self.unit, samples)
            .target(self.target)
            .scope(self.scope)
    }
}

/// Every figure of `figures` skipped, for `reason`.
pub fn skipped(prefix: &str, figures: &[Figure], reason: &str) -> Vec<Row> {
    figures
        .iter()
        .map(|figure| {
            Row::skipped(&figure.metric(prefix), figure.unit, reason)
                .target(figure.target)
                .scope(figure.scope)
        })
        .collect()
}

fn folder_source(path: &Path) -> Value {
    json!({"kind": "folder", "path": path})
}

/// A view of a listed folder with its subfolders, as the desktop views a folder it browses.
fn view_of(listed: &Value) -> Value {
    json!({"source": {"kind": "folder", "path": listed, "subfolders": true}})
}

/// Wait for the listing job `job` a request began: its record and when it ended.
fn listing(core: &Core, job: &str, what: &str) -> Result<(Value, Instant)> {
    let ended = core.wait_job(job, JOB_DEADLINE)?;
    ensure(
        ended.record["status"] == "ready",
        format!("the index could not list {what}: {}", ended.record),
    )?;
    Ok((ended.record, ended.at))
}

/// List `source` (an `index.refresh` source) and wait for the job.
fn refresh(core: &Core, source: Value) -> Result<(Value, Instant)> {
    let job = job_id(&core.ask("index.refresh", json!({"source": source}))?)?;
    listing(core, &job, &source.to_string())
}

/// The root the index listed a folder as.
fn listed_root(record: &Value) -> Result<Value> {
    let root = record["result"]["roots"][0].clone();
    ensure(
        root.is_string(),
        format!("the listing names no root: {record}"),
    )?;
    Ok(root)
}

/// The volume of the mounted camera card `path` is on, from `card.list`'s answer: the card whose
/// mount point is the longest that holds `path`.
pub fn card_volume(cards: &Value, path: &Path) -> Result<String> {
    cards["cards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|card| {
            let mount = card["volume"]["mount_point"].as_str()?;
            let id = card["volume"]["id"].as_str()?;
            path.starts_with(mount).then_some((mount.len(), id))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, id)| id.to_owned())
        .ok_or_else(|| {
            format!(
                "{} is on no mounted camera card (card.list: {cards})",
                path.display()
            )
            .into()
        })
}

// ── Browsing a folder or a card for the first time, and returning to it ─────────────────────

/// What a first browse lists and views.
#[derive(Clone, Copy)]
pub enum Browsed<'a> {
    /// A folder on disk, added as an indexed folder (`index.add-folder`): listed with its headers
    /// read, organized into events, then watched.
    Folder(&'a Path),
    /// The mounted camera card this path is on, listed and viewed as a card (`{kind: card}`).
    Card(&'a Path),
}

/// A first browse's target from a card reader, in place of the internal SSD's.
pub const CARD_TARGET: &str =
    "Browsing a 1,000-frame card for the first time from a card reader: reported";

pub const FIRST_BROWSE: [Figure; 4] = [
    Figure {
        name: "listed",
        unit: "ms",
        target: "Every file within 5 s (internal SSD)",
        scope: "index.add-folder of the folder (a card: index.refresh of the card), from the request to its listing job's end on the activity board: every file listed and its header read, into a new catalog each sample",
    },
    Figure {
        name: "first_screen",
        unit: "ms",
        target: "The first screen of the grid within 1 s (internal SSD)",
        scope: "from the request to browse.view of the listed folder with its subfolders (a card: of the card) and browse.rows of its first 200 rows answered, as the desktop views a folder once its listing ends",
    },
    Figure {
        name: "known",
        unit: "ms",
        target: "Every file, event and moment within 5 s (internal SSD)",
        scope: "from the request, after the first screen, to event.list answered over the index the listing left: every file, event and moment known",
    },
    Figure {
        name: "grid_previews",
        unit: "ms",
        target: "Grid previews for all: reported",
        scope: "from the request to the end, on the activity board, of the preview lane's view job browse.view began: a grid tier for every file the view lacked",
    },
];

/// Browse `browsed` for the first time, `journeys` times, each into a new catalog; the last
/// catalog's core is left in `kept` for the return and the Develop.
pub fn first_browse(
    cx: &Context,
    prefix: &str,
    browsed: Browsed,
    kept: &mut Option<Core>,
) -> Result<Vec<Row>> {
    let mut figures: [Vec<f64>; 4] = Default::default();
    let mut details = Vec::new();
    for sample in 0..cx.journeys {
        if let Some(core) = kept.take() {
            core.close()?;
        }
        let core = Core::open(&cx.catalog(&format!("{prefix}-{sample}"))?)?;
        // A card is in the sources panel before it is browsed, so finding it is not timed.
        let card = match browsed {
            Browsed::Folder(_) => None,
            Browsed::Card(path) => Some(card_volume(&core.ask("card.list", json!({}))?, path)?),
        };
        let started = Instant::now();
        let (record, listed) = match (&card, browsed) {
            (Some(volume), _) => refresh(&core, json!({"kind": "card", "volume_id": volume}))?,
            (None, Browsed::Folder(folder) | Browsed::Card(folder)) => {
                let answer = core.ask(
                    "index.add-folder",
                    json!({"path": folder, "mutation": core.mutation()}),
                )?;
                listing(&core, &job_id(&answer)?, &folder.display().to_string())?
            }
        };
        figures[0].push(ms(listed.saturating_duration_since(started)));
        let view = match &card {
            Some(volume) => json!({"source": {"kind": "card", "volume_id": volume}}),
            None => view_of(&listed_root(&record)?),
        };
        let before = core.newest_entry();
        let summary = core.ask("browse.view", view)?;
        let count = summary["count"].as_u64().ok_or("the view has no count")?;
        ensure(count > 0, "the view holds nothing")?;
        let rows = core.ask(
            "browse.rows",
            json!({"from": 0, "count": count.min(SCREEN)}),
        )?;
        figures[1].push(ms(started.elapsed()));
        ensure(
            rows["rows"].as_array().map_or(0, Vec::len) as u64 == count.min(SCREEN),
            "the first screen's rows are missing",
        )?;
        let events = core.ask("event.list", json!({}))?;
        figures[2].push(ms(started.elapsed()));
        let previews = match core.view_job_since(before) {
            Some(view) => {
                let ended = core.wait_job(&view, JOB_DEADLINE)?;
                figures[3].push(ms(ended.at.saturating_duration_since(started)));
                ended.record["result"].clone()
            }
            None => {
                figures[3].push(ms(started.elapsed()));
                json!("the view lacked no grid preview")
            }
        };
        details.push(json!({
            "sample": sample,
            "listing": record["result"],
            "view": {
                "count": count,
                "moments": summary["groups"]["moments"].as_array().map_or(0, Vec::len),
            },
            "events": events["events"].as_array().map_or(0, Vec::len),
            "previews": previews,
        }));
        *kept = Some(core);
    }
    let (what, card) = match browsed {
        Browsed::Folder(path) => (path, false),
        Browsed::Card(path) => (path, true),
    };
    let detail = json!({"browsed": what, "samples": details});
    Ok(FIRST_BROWSE
        .iter()
        .zip(figures)
        .map(|(figure, samples)| {
            let row = figure
                .row(prefix, samples)
                .cache("the first sample reads files the set-up had just written (a card: whatever the OS still holds of it); later samples read the same files again; the OS file cache is never purged")
                .detail(detail.clone());
            if card { row.target(CARD_TARGET) } else { row }
        })
        .collect())
}

pub const RETURN: [Figure; 2] = [
    Figure {
        name: "listed",
        unit: "ms",
        target: "Returning to a known 1,000-file folder: reconciled within 1 s",
        scope: "index.refresh of the indexed folder the first browse left, from the request to its job's end: every file reconciled by signature, as a card mounted again or a folder browsed again is, and no header read",
    },
    Figure {
        name: "drawn",
        unit: "ms",
        target: "Returning to a known 1,000-file folder: reconciled and drawn within 1 s",
        scope: "from the index.refresh request to browse.view of the folder and browse.rows of its first 200 rows answered",
    },
];

/// Return to the indexed folder the last first browse left in `core`, `samples` times.
pub fn returning(cx: &Context, core: &Core, folder: &Path) -> Result<Vec<Row>> {
    let mut figures: [Vec<f64>; 2] = Default::default();
    let mut last = Value::Null;
    for _ in 0..cx.samples {
        let started = Instant::now();
        let (record, listed) = refresh(core, json!({"kind": "indexed-folder", "path": folder}))?;
        figures[0].push(ms(listed.saturating_duration_since(started)));
        let summary = core.ask("browse.view", view_of(&listed_root(&record)?))?;
        let count = summary["count"].as_u64().unwrap_or(0);
        core.ask(
            "browse.rows",
            json!({"from": 0, "count": count.clamp(1, SCREEN)}),
        )?;
        figures[1].push(ms(started.elapsed()));
        last = record["result"].clone();
    }
    Ok(RETURN
        .iter()
        .zip(figures)
        .map(|(figure, samples)| {
            figure
                .row("return", samples)
                .cache("warm: the folder, its rows and its previews were read by the first browse")
                .detail(json!({"folder": folder, "last_listing": last}))
        })
        .collect())
}

// ── Developing picks ──────────────────────────────────────────────────────────────────────────

pub const DEVELOP: [Figure; 2] = [
    Figure {
        name: "first_committed",
        unit: "ms",
        target: "Develop shows the first photograph's preview as soon as it is committed",
        scope: "from the pick.develop request to the first library change its job recorded (events.wait): the first photograph committed",
    },
    Figure {
        name: "picks",
        unit: "ms",
        target: "Developing 20 RAW picks: reported",
        scope: "from the pick.develop request to its job's end on the activity board: every pick read, fingerprinted and committed",
    },
];

/// Pick `picks` frames of the trip, each a different corpus file, and develop them in `core`,
/// which has browsed the trip.
pub fn develop(core: &Core, trip: &Trip, picks: usize) -> Result<Vec<Row>> {
    let mut sources = HashSet::new();
    let paths: Vec<&Path> = trip
        .frames
        .iter()
        .filter(|frame| sources.insert(frame.source.clone()))
        .map(|frame| frame.path.as_path())
        .take(picks)
        .collect();
    ensure(!paths.is_empty(), "the trip has no frame to pick")?;
    let targets = json!({"kind": "paths", "paths": paths});
    core.ask(
        "pick.set",
        json!({"targets": targets, "picked": true, "mutation": core.mutation()}),
    )?;
    let mut after =
        core.ask("events.wait", json!({"after": 0, "timeout_ms": 0}))?["current_sequence"]
            .as_u64()
            .ok_or("events.wait names no sequence")?;
    let started = Instant::now();
    let job = job_id(&core.ask(
        "pick.develop",
        json!({"into": [], "mutation": core.mutation(), "targets": targets}),
    )?)?;
    let mut first = None;
    while first.is_none() {
        let answer = core.ask("events.wait", json!({"after": after, "timeout_ms": 1000}))?;
        let arrived = started.elapsed();
        let committed = answer["events"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|event| {
                (event["job_id"] == job.as_str() || event["method"] == "pick.develop")
                    && event["library_sequence"].is_u64()
            });
        if committed {
            first = Some(ms(arrived));
        }
        after = answer["current_sequence"].as_u64().unwrap_or(after);
        if first.is_none() && !core.running(&job) {
            let read = core.ask("job.read", json!({"job_id": job}))?;
            if !matches!(read["status"].as_str(), Some("queued" | "running")) {
                break;
            }
        }
        ensure(
            started.elapsed() < JOB_DEADLINE,
            "the Develop committed nothing in time",
        )?;
    }
    let ended = core.wait_job(&job, JOB_DEADLINE)?;
    ensure(
        ended.record["status"] == "ready",
        format!("the Develop did not finish: {}", ended.record),
    )?;
    let report = &ended.record["result"];
    let detail = json!({
        "picks": paths.len(),
        "developed": report["developed"].as_array().map_or(0, Vec::len),
        "failed": report["failed"],
        "files": paths,
    });
    let cache = "the picks' files were read by the first browse's header reads and grid previews (the OS file cache is never purged)";
    Ok(vec![
        DEVELOP[0]
            .row("develop", first.into_iter().collect())
            .cache(cache)
            .detail(detail.clone()),
        DEVELOP[1]
            .row(
                "develop",
                vec![ms(ended.at.saturating_duration_since(started))],
            )
            .cache(cache)
            .detail(detail),
    ])
}

// ── The first index of a large tree ────────────────────────────────────────────────────────────

/// A first index's target, of the tree of copies and of the tree of hard links alike.
const FIRST_INDEX_TARGET: &str = "First index of a 200,000-file folder: reported, in the background, with the editor responsive throughout";

/// A first index's two rows, its duration and the owner round trips sampled while it ran, with
/// their target and their scope over the tree `what` names.
fn first_index_rows(what: &str, duration: Row, round_trips: Row) -> [Row; 2] {
    [
        duration.target(FIRST_INDEX_TARGET).scope(format!(
            "index.add-folder of {what}, from the request to its listing job's end on the activity board, into a new catalog"
        )),
        round_trips.target(FIRST_INDEX_TARGET).scope(format!(
            "a session.state round trip of a second client through the owner every {} ms while the first index of {what} ran: how long any client, the desktop included, waited on the owner",
            ROUND_TRIP_EVERY.as_millis()
        )),
    ]
}

/// The first index of the tree of copies, each file its own file identity.
pub fn first_index(cx: &Context) -> Result<Vec<Row>> {
    let tree = &cx.data.tree;
    index_tree(
        cx,
        "first_index",
        tree,
        &format!(
            "a tree of {} copies of the generated JPEGs in {} folders, each file its own file identity",
            tree.files, tree.folders
        ),
    )
}

/// The metric prefix of the first index of the tree of hard links.
const HARD_LINKS: &str = "first_index_hard_links";

/// The first index of the tree of hard links' rows, skipped for `reason`.
pub fn first_index_hard_links_skipped(reason: &str) -> Vec<Row> {
    first_index_rows(
        "a tree of hard links to the generated JPEGs, each JPEG's links sharing its file identity",
        Row::skipped(&format!("{HARD_LINKS}.duration"), "s", reason),
        Row::skipped(&format!("{HARD_LINKS}.owner_round_trip"), "ms", reason),
    )
    .into()
}

/// The first index of the tree of hard links: every generated JPEG's links share its file
/// identity, which the index looks up for every new file to tell a moved file from a new one.
/// Skipped, with the reason, when the file system refused the links.
pub fn first_index_hard_links(cx: &Context) -> Result<Vec<Row>> {
    let tree = match &cx.data.links {
        Ok(tree) => tree,
        Err(reason) => return Ok(first_index_hard_links_skipped(reason)),
    };
    index_tree(
        cx,
        HARD_LINKS,
        tree,
        &format!(
            "a tree of {} hard links to the {} generated JPEGs in turn in {} folders, the {} links of each JPEG sharing its file identity",
            tree.files,
            tree.sources,
            tree.folders,
            tree.files.div_ceil(tree.sources)
        ),
    )
}

/// Index `tree`, described by `what`, for the first time into a new catalog, sampling an owner
/// round trip every [`ROUND_TRIP_EVERY`] until its listing job ends: its two rows as
/// `<prefix>.duration` and `<prefix>.owner_round_trip`.
fn index_tree(cx: &Context, prefix: &str, tree: &Tree, what: &str) -> Result<Vec<Row>> {
    let core = Core::open(&cx.catalog(&prefix.replace('_', "-"))?)?;
    let other = core.register();
    let started = Instant::now();
    let job = job_id(&core.ask(
        "index.add-folder",
        json!({"path": tree.dir, "mutation": core.mutation()}),
    )?)?;
    let mut trips = Vec::new();
    loop {
        trips.push(timed(|| core.ask_as(other, "session.state", json!({})))?.0);
        if !core.running(&job) {
            let read = core.ask("job.read", json!({"job_id": job}))?;
            if !matches!(read["status"].as_str(), Some("queued" | "running")) {
                break;
            }
        }
        ensure(
            started.elapsed() < JOB_DEADLINE,
            "the first index did not end in time",
        )?;
        std::thread::sleep(ROUND_TRIP_EVERY);
    }
    let ended = core.wait_job(&job, JOB_DEADLINE)?;
    ensure(
        ended.record["status"] == "ready",
        format!("the first index did not finish: {}", ended.record),
    )?;
    core.close()?;
    let seconds = ended.at.saturating_duration_since(started).as_secs_f64();
    let detail = json!({
        "tree": tree.dir,
        "files": tree.files,
        "folders": tree.folders,
        "listing": ended.record["result"],
    });
    Ok(first_index_rows(
        what,
        Row::measured(&format!("{prefix}.duration"), "s", [seconds])
            .cache("the tree was written by the set-up (the OS file cache is never purged)")
            .detail(detail.clone()),
        Row::measured(&format!("{prefix}.owner_round_trip"), "ms", trips).detail(detail),
    )
    .into())
}

// ── Browse views at the design's scale ─────────────────────────────────────────────────────────

/// How long the owner's footprint is watched for the allocator to hand back what an evaluation
/// freed, and how often it is read meanwhile.
const SETTLE_WITHIN: Duration = Duration::from_secs(5);
const SETTLE_EVERY: Duration = Duration::from_millis(100);
/// A footprint that has not fallen by more than this over five reads in a row has settled.
const SETTLED_MIB: f64 = 0.5;

/// The process's footprint (`resources.read`, `memory.bytes`) once it has settled: an evaluation
/// frees a working set of a few hundred bytes an item that the allocator hands back to the system
/// over the following second or so, and only what is left is what the owner holds. Read every
/// [`SETTLE_EVERY`] until five reads in a row fall by less than [`SETTLED_MIB`] in all, or for
/// [`SETTLE_WITHIN`]; answers the lowest read.
fn settled_memory(core: &Core) -> Result<f64> {
    let read = || -> Result<f64> {
        Ok(core.ask("resources.read", json!({}))?["memory"]["bytes"]
            .as_f64()
            .ok_or("resources.read reports no memory")?
            / MIB)
    };
    let started = Instant::now();
    let mut reads = vec![read()?];
    while started.elapsed() < SETTLE_WITHIN {
        std::thread::sleep(SETTLE_EVERY);
        reads.push(read()?);
        if let [.., first, _, _, _, last] = reads.as_slice()
            && first - last < SETTLED_MIB
        {
            break;
        }
    }
    Ok(reads.iter().copied().fold(f64::INFINITY, f64::min))
}

pub fn browse_generated(cx: &Context) -> Result<Vec<Row>> {
    let core = Core::open(&cx.data.catalog)?;
    let mut rows = Vec::new();
    for (name, what, source) in [
        (
            "files",
            "every file of the generated index, as one folder view of / with its subfolders",
            json!({"kind": "folder", "path": "/", "subfolders": true}),
        ),
        (
            "photos",
            "every photograph of the generated catalog (all-photographs)",
            json!({"kind": "all-photographs"}),
        ),
    ] {
        let query = json!({"source": source});
        let before = settled_memory(&core)?;
        let (first, summary) = timed(|| core.ask("browse.view", query.clone()))?;
        let count = summary["count"].as_u64().ok_or("the view has no count")?;
        ensure(count > 0, format!("the view of {what} holds nothing"))?;
        let moments = summary["groups"]["moments"].as_array().map_or(0, Vec::len);
        // The answer is the harness's, not the owner's: dropped before the owner is measured.
        drop(summary);
        let after = settled_memory(&core)?;
        let mut views = Vec::with_capacity(cx.samples);
        for _ in 0..cx.samples {
            views.push(timed(|| core.ask("browse.view", query.clone()))?.0);
        }
        let again = settled_memory(&core)?;
        let window = count.min(SCREEN);
        let mut windows = Vec::with_capacity(cx.samples);
        for sample in 0..cx.samples as u64 {
            let from = sample * 7919 % (count - window + 1);
            let (took, answer) =
                timed(|| core.ask("browse.rows", json!({"from": from, "count": window})))?;
            ensure(
                answer["rows"].as_array().map_or(0, Vec::len) as u64 == window,
                "a window of rows came back short",
            )?;
            windows.push(took);
        }
        let detail = json!({
            "count": count,
            "moments": moments,
        });
        let queued = if name == "files" {
            " Each evaluation also queues the view's missing grid previews, which the preview lane's workers fail in the background, since the generated files do not exist on disk."
        } else {
            ""
        };
        rows.push(
            Row::measured(&format!("browse_view.{name}"), "ms", views)
                .target("browse.view over 10,000 files or 100,000 photographs: p95 under 50 ms")
                .scope(format!(
                    "browse.view of {what}, {count} items, evaluated again with the same query after a first evaluation, through the owner in the harness's process.{queued}"
                ))
                .cache("warm: the first evaluation, its own row, read the catalog and index first")
                .detail(detail.clone()),
        );
        rows.push(
            Row::measured(&format!("browse_view.{name}.first"), "ms", [first])
                .target("browse.view over 10,000 files or 100,000 photographs: p95 under 50 ms")
                .scope(format!(
                    "the first browse.view of {what}, {count} items, after the owner opened the catalog{queued}"
                ))
                .cache("the catalog and index were written by the set-up; nothing of them was read before")
                .detail(detail.clone()),
        );
        rows.push(
            Row::measured(&format!("browse_rows.{name}"), "ms", windows)
                .target("browse.rows of 200 under 2 ms")
                .scope(format!(
                    "browse.rows of {window} rows at positions spread over the view of {what}"
                ))
                .detail(detail.clone()),
        );
        if name == "photos" {
            rows.push(
                Row::measured("memory.owner_view_photos", "MiB", [after - before])
                    .target("The owner grows by the view's id list")
                    .scope(format!(
                        "the harness process's footprint (resources.read, memory.bytes) once settled after the first browse.view of {count} photographs, less the settled footprint before it: the view's id list, its layout, the preview lane's queued tasks for it (at most its queue's capacity) and anything else the evaluation keeps, not the working set it freed"
                    ))
                    .detail(json!({"before_mib": before, "after_mib": after, "count": count})),
            );
            rows.push(
                Row::measured("memory.owner_view_photos_growth", "MiB", [again - after])
                    .target("The same view evaluated again holds nothing more")
                    .scope(format!(
                        "the settled footprint after {} more browse.view of the same {count} photographs, less the settled footprint after the first",
                        cx.samples
                    ))
                    .detail(json!({"after_first_mib": after, "after_again_mib": again, "views": cx.samples})),
            );
        }
    }
    core.close()?;
    Ok(rows)
}

// ── Brackets from previews ─────────────────────────────────────────────────────────────────────

fn bench_command(root: &Path) -> Command {
    let mut command = cargo_command();
    command
        .current_dir(root)
        .args(["test", "--locked", "--package", "luxforge-core", "--lib"]);
    // The bench is built with the harness's own profile: a release harness times a release bench.
    if !cfg!(debug_assertions) {
        command.arg("--release");
    }
    command
}

/// Build the bracket bench before anything is timed, so no step compiles while it measures.
pub fn build_bench(root: &Path, log: &Path) -> Result {
    let file = fs::File::create(log)?;
    let status = bench_command(root)
        .arg("--no-run")
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()?;
    ensure(
        status.success(),
        format!("the bracket bench did not build; see {}", log.display()),
    )
}

pub fn bracket(cx: &Context) -> Result<Vec<Row>> {
    let catalog = cx.catalog("bracket")?;
    let core = Core::open(&catalog)?;
    let (record, _) = refresh(&core, folder_source(&cx.data.images))?;
    let root = listed_root(&record)?;
    let before = core.newest_entry();
    core.ask("browse.view", view_of(&root))?;
    let previews = match core.view_job_since(before) {
        Some(view) => core.wait_job(&view, JOB_DEADLINE)?.record["result"].clone(),
        None => Value::Null,
    };
    core.close()?;
    let out = cx.out.join("bracket-bench.json");
    let log = cx.out.join("bracket-bench.log");
    let file = fs::File::create(&log)?;
    let status = bench_command(cx.root)
        .args([BENCH, "--", "--ignored", "--exact", "--nocapture"])
        .env(BENCH_CATALOG, &catalog)
        .env(BENCH_FOLDER, root.as_str().unwrap_or_default())
        .env(BENCH_EVALUATIONS, cx.samples.to_string())
        .env(BENCH_OUT, &out)
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()?;
    ensure(
        status.success(),
        format!("the bracket bench failed; see {}", log.display()),
    )?;
    let bench = read_json(&out)?;
    let runs = bench["runs"].as_array().cloned().unwrap_or_default();
    let samples: Vec<f64> = runs.iter().filter_map(|run| run["ms"].as_f64()).collect();
    let target =
        "Bracket detection from previews: under 1 ms a run, from the grid tiers' fingerprints";
    let scope = format!(
        "the core's ignored bench {BENCH} (in process, since the probe is internal to the core), built with the harness's profile: the generated JPEGs' folder, its grid tiers and fingerprints written by the preview lane, viewed {} times through the browse lane's evaluation; each run organizing asked the preview lane's probe about, timed from the probe's call to its answer (one indexed query for the run's fingerprints and the measure)",
        cx.samples
    );
    let detail = json!({
        "evaluations": cx.samples,
        "items": bench["items"],
        "runs_asked": runs.len(),
        "measured_as_brackets": runs.iter().filter(|run| run["bracket"] == true).count(),
        "previews": previews,
    });
    let row = if samples.is_empty() {
        Row::not_measured(
            "bracket.probe_per_run",
            "ms",
            "organizing asked the probe about no run of this folder",
        )
    } else {
        Row::measured("bracket.probe_per_run", "ms", samples)
    };
    Ok(vec![row.target(target).scope(scope).detail(detail)])
}

// ── Idle with the watchers armed ───────────────────────────────────────────────────────────────

/// An idle window's rows, `idle.<figure>`, as `<prefix>.<figure>`.
fn idle_rows(prefix: &str, rows: &[Value], scope: &str) -> Vec<Row> {
    rows.iter()
        .map(|row| {
            let figure = row["metric"]
                .as_str()
                .unwrap_or_default()
                .trim_start_matches("idle.");
            let row = Row::from_report(&format!("{prefix}.{figure}"), row).scope(scope);
            if figure == "cpu_percent_one_core" {
                row.target("Idle CPU < 1% of one core over 30 s after background work settles")
            } else {
                row
            }
        })
        .collect()
}

pub fn idle_watchers(cx: &Context) -> Result<Vec<Row>> {
    let catalog = cx.catalog("idle")?;
    let folders = [&cx.data.folder.dir, &cx.data.images];
    let core = Core::open(&catalog)?;
    for folder in folders {
        let job = job_id(&core.ask(
            "index.add-folder",
            json!({"path": folder, "mutation": core.mutation()}),
        )?)?;
        let ended = core.wait_job(&job, JOB_DEADLINE)?;
        ensure(
            ended.record["status"] == "ready",
            format!("{} was not listed: {}", folder.display(), ended.record),
        )?;
    }
    // Each folder is watched once its listing ends; `index.folders` says when.
    let started = Instant::now();
    let indexed = loop {
        let answer = core.ask("index.folders", json!({}))?;
        let listed = answer["folders"].as_array().cloned().unwrap_or_default();
        if listed.len() == folders.len() && listed.iter().all(|folder| folder["watching"] == true) {
            break listed;
        }
        ensure(
            started.elapsed() < WATCH_DEADLINE,
            format!("the indexed folders were not all watched: {answer}"),
        )?;
        core.pause();
    };
    let window = stats::idle_window(cx.root, std::process::id())?;
    core.close()?;
    let detail = json!({"indexed_folders": indexed});
    let mut rows: Vec<Row> = idle_rows(
        "idle_watchers.core",
        &window.rows(),
        "the core in the harness's own process — the catalog owner and its index lane watching 2 indexed folders, both answered watching by index.folders — sampled with ps: one second of settling, then 30 s (stats::idle_window); no desktop",
    )
    .into_iter()
    .map(|row| row.detail(detail.clone()))
    .collect();
    rows.extend(idle_editor(
        cx,
        &catalog,
        "idle-editor",
        "idle_watchers.editor",
        "over the same catalog with nothing open (Develop, the Performance section at its default): its owner watches both indexed folders from their cursors as it opens",
    )?);
    rows.extend(idle_editor(
        cx,
        &cx.catalog("idle-baseline")?,
        "idle-editor-baseline",
        "idle_watchers.editor_baseline",
        "over a new catalog with no indexed folder and nothing open, so no index lane or watcher starts: the editor's own idle, the figure the watched run compares with",
    )?);
    Ok(rows)
}

/// The editor over `catalog`, with nothing open: its idle window once it has started, into
/// `<out>/<dir>` and as `<prefix>.<figure>` rows.
fn idle_editor(
    cx: &Context,
    catalog: &Path,
    dir: &str,
    prefix: &str,
    over: &str,
) -> Result<Vec<Row>> {
    let out = cx.out.join(dir);
    let data = out.join("data");
    let events = data.join("logs/events.jsonl");
    let root = cx.root.to_path_buf();
    let mut window = Value::Null;
    let run = Run::tool(
        cx.root,
        &out,
        "catalog-measure",
        cx.binary,
        Duration::from_secs(120),
    )?;
    run.check(|run| {
        let log = events.clone();
        let launched = run.launch(
            Launch::ordinary("idle")
                .catalog(catalog)
                .data_root(&data)
                .deadline(Duration::from_secs(120))
                .watch(Box::new(move |child, _, deadline| {
                    let poll = Poll {
                        every: Duration::from_millis(50),
                        from: Instant::now(),
                        deadline,
                        late: "The editor never logged its startup",
                    };
                    let started = || {
                        Ok(fs::read_to_string(&log)
                            .unwrap_or_default()
                            .contains("\"event\":\"startup\"")
                            .then_some(()))
                    };
                    match watch(child, poll, started, |_| Ok(()))? {
                        Watched::Exited(status) => {
                            Err(format!("The editor exited before its startup: {status}").into())
                        }
                        Watched::Reached(()) => {
                            std::thread::sleep(EDITOR_SETTLE);
                            let window = stats::idle_window(&root, child.child.id())?;
                            Ok((None, json!(window.rows())))
                        }
                    }
                })),
        )?;
        run.provenance(&scenario::events(&events)?)?;
        window = launched.watched;
        Ok(())
    })?;
    let rows = window.as_array().cloned().unwrap_or_default();
    Ok(idle_rows(
        prefix,
        &rows,
        &format!(
            "the release editor, a background launch with its window hidden, {over}; {} s after its startup event, then stats::idle_window (one second of settling, then 30 s)",
            EDITOR_SETTLE.as_secs()
        ),
    ))
}

// ── A Basic drag, alone and under the catalog's background work ────────────────────────────────

/// A Basic exposure drag through `editor-latency` into `<out>/drag-<name>`, its rows renamed
/// `drag.<name>.<metric>`.
fn drag(cx: &Context, name: &str, scope: &str) -> Result<Vec<Row>> {
    let dir = cx.out.join(format!("drag-{}", name.replace('_', "-")));
    editor_latency::run(
        cx.root,
        &dir,
        cx.binary,
        editor_latency::Options {
            source: &cx.fixture,
            samples: cx.samples.clamp(1, 60),
            mode: editor_latency::Mode::Drag,
            control: editor_latency::Control::Slider,
            action: None,
            parameter: None,
            crop: None,
            idle: false,
            basic: false,
            presence: false,
            mask: false,
            zoom: None,
            moving_pan: false,
            mask_overlay: false,
        },
    )?;
    let report = read_json(&dir.join("latency.json"))?;
    let scope = format!(
        "editor-latency --mode drag over {} with {} inputs, Basic exposure at Fit, the release editor in a background launch (report in {}); {scope}",
        cx.fixture.display(),
        cx.samples.clamp(1, 60),
        dir.display()
    );
    Ok(stats::rows(&report)
        .iter()
        .map(|row| {
            let metric = row["metric"].as_str().unwrap_or_default();
            let row = Row::from_report(&format!("drag.{name}.{metric}"), row).scope(&scope);
            if metric == "input_to_presented_frame" {
                row.target("Slider input to presented frame at Fit, warm 24 MP: p95 < 16 ms; acceptable below 32 ms")
            } else {
                row
            }
        })
        .collect())
}

pub fn drag_baseline(cx: &Context) -> Result<Vec<Row>> {
    drag(
        cx,
        "baseline",
        "no catalog work in the background: the figure the loaded drags compare with",
    )
}

/// What a drag under background work says about that work: whether it outlasted the drag.
fn throughout(running: bool, work: &str) -> String {
    if running {
        format!("{work} was still running when the drag's run ended, and was then cancelled")
    } else {
        format!("{work} had ended before the drag's run did, so part of the drag ran without it")
    }
}

pub fn drag_during_indexing(cx: &Context) -> Result<Vec<Row>> {
    let tree = &cx.data.tree;
    let core = Core::open(&cx.catalog("drag-indexing")?)?;
    let job = job_id(&core.ask(
        "index.add-folder",
        json!({"path": tree.dir, "mutation": core.mutation()}),
    )?)?;
    let work = format!(
        "a first index of the {}-file tree, by a core in the harness's own process (the editor's own owner is not the one indexing; the drag competes for the CPU and the disk)",
        tree.files
    );
    let dragged = drag(cx, "during_indexing", &format!("in the background, {work}"));
    let running = core.running(&job);
    if running {
        core.ask("job.cancel", json!({"job_id": job}))?;
    }
    core.wait_job(&job, JOB_DEADLINE)?;
    core.close()?;
    let note = throughout(running, "The index");
    Ok(dragged?
        .into_iter()
        .map(|row| row.detail(json!({"background": note})))
        .collect())
}

pub fn drag_during_preview_backlog(cx: &Context) -> Result<Vec<Row>> {
    let (folder, what) = match &cx.data.trip {
        Ok(trip) => (&trip.dir, "the RAW trip"),
        Err(_) => (
            &cx.data.folder.dir,
            "the folder of JPEG copies (no RAW corpus)",
        ),
    };
    let core = Core::open(&cx.catalog("drag-backlog")?)?;
    let (record, _) = refresh(&core, folder_source(folder))?;
    let before = core.newest_entry();
    core.ask("browse.view", view_of(&listed_root(&record)?))?;
    let view = core
        .view_job_since(before)
        .ok_or("the view lacked no grid preview, so there is no backlog")?;
    let work = format!(
        "the preview lane reading every grid preview of {what} for a view (a view job), by a core in the harness's own process"
    );
    let dragged = drag(
        cx,
        "during_preview_backlog",
        &format!("in the background, {work}"),
    );
    let running = core.running(&view);
    if running {
        core.ask("job.cancel", json!({"job_id": view}))?;
    }
    core.wait_job(&view, JOB_DEADLINE)?;
    core.close()?;
    let note = throughout(running, "The preview backlog");
    Ok(dragged?
        .into_iter()
        .map(|row| row.detail(json!({"background": note})))
        .collect())
}
