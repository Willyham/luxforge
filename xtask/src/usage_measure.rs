//! `usage-measure`: what optional usage statistics cost, opted out against opted in
//! (`docs/design/usage-statistics.md`, "Acceptance and evidence"), in one report,
//! `usage-measure.json` with `usage-measure.md` beside it. Every row carries its scope and the
//! one-minute load average at its step's start and end.
//!
//! - **producer**: one event recorded by the collector, in this process: while collection is off
//!   (the gate alone), into a bank with room, into a full bank (dropped), and from four threads at
//!   once while another detaches windows, beside `Supervisor::interacted` (each input event's call)
//!   and the keyed image ID (one HMAC per open). Each sample is one call timed with the monotonic
//!   clock, so it includes one clock read.
//! - **sampler**: one fresh read of the process counters the statistics sampler takes at most once
//!   a minute (CPU time and memory; the GPU counters the desktop also enables are not opened here).
//! - **owner**: a catalog owner's import (`pick.develop`), preparation, Basic commits and exports
//!   of each source, with an in-memory collector started (opted in) or not (opted out), alternating.
//! - **drag**: `editor-latency --mode drag --usage MODE` for each source, each mode (`off`, `fast`,
//!   `slow`, `offline`) in a rotating order each repeat: input to presented frame, peak memory and
//!   the close. Idleness is the presence component's.
//! - **presence**: one launch per mode and repeat over the first source: the Performance section
//!   closed, the statistics clock moved past the first window, two ten-second idle windows while
//!   presented and two observation windows while the window is hidden through the native evidence
//!   step, counting process CPU, frames, views and update-loop wakes.
//! - **quit**: one launch per mode and repeat that moves the statistics clock and quits five
//!   seconds later, opted out, with the fast receiver, and while the slow receiver holds the request: the
//!   close from the editor's shutdown event to the process's exit.
//!
//! The generated inputs are local verification inputs; nothing about them enters a statistics
//! request. The producer, sampler and owner components time this process, so they need a release
//! xtask: `cargo run --release --locked -p xtask -- usage-measure ...`. Not part of `verify`'s
//! timing tier.
use crate::{
    editor_latency,
    scenario::{Launch, Run},
    usage_receiver::{UsageMode, UsageSetup},
    *,
};
use luxforge_core::{
    Error, HostConfig, ModuleRegistry, OwnerHandle,
    capabilities::transport::{SendOptions, Transport, TransportRequest, TransportResponse},
    usage::{
        Collector, DeviceFn, DurationMs, EventKind, Feature, ImageKey, NoSampling, OperationEnd,
        Outcome, Release, SessionClock, SessionStart, Supervisor, SupervisorConfig, SystemClock,
        SystemRandomness, TestReceiver, Tool, Trigger, Usage, eligibility::Eligibility,
    },
};
use luxforge_evidence as script;
use luxforge_testkit::client::{self, call};
use std::{
    collections::BTreeMap,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const TOOL: &str = "usage-measure";
const ACTOR: &str = "xtask-usage-measure";
/// The calls each single-threaded producer row times.
const PRODUCER_CALLS: usize = 200_000;
/// The producer threads of the contended row, beside one thread detaching windows.
const PRODUCER_THREADS: usize = 4;
/// The process counter reads the sampler row times.
const SAMPLER_READS: usize = 200;
/// Exports per owner run.
const EXPORTS: usize = 3;
/// How far the presence launch moves the statistics clock before its windows.
const ADVANCE_S: u64 = 660;
/// Each presence window.
const WINDOW: script::IdleStep = script::IdleStep {
    settle_ms: 2_000,
    ms: script::MAX_WAIT_MS,
};
/// The components, in the order they run.
pub const COMPONENTS: [&str; 6] = ["producer", "sampler", "owner", "drag", "presence", "quit"];

pub struct Options {
    pub sources: Vec<PathBuf>,
    pub samples: usize,
    pub repeats: usize,
    /// Run only these components.
    pub only: Option<Vec<String>>,
}

/// The report, written again after every component so a partial run keeps what it has.
struct Report {
    out: PathBuf,
    root: PathBuf,
    rows: Vec<Value>,
    components: Vec<Value>,
    header: Value,
}

impl Report {
    fn row(&mut self, metric: &str, unit: &str, samples: Vec<f64>, scope: &str, load: &Value) {
        let mut row = stats::row(metric, unit, samples);
        row["scope"] = json!(scope);
        row["load"] = load.clone();
        self.rows.push(row);
    }

    fn write(&self) -> Result {
        let report = json!({
            "tool": TOOL,
            "header": self.header,
            "components": self.components,
            "rows": self.rows,
        });
        write_json(&self.out.join("usage-measure.json"), &report)?;
        let mut md = String::from(
            "# usage-measure\n\nOne-minute load at each step's start and end; a figure taken above the threshold is not evidence of a target.\n\n| Metric | Unit | n | p50 | p95 | Load start | Load end |\n| --- | --- | ---: | ---: | ---: | ---: | ---: |\n",
        );
        for row in &self.rows {
            let d = &row["distribution"];
            let figure = |key: &str| d[key].as_f64().map_or("-".into(), |v| format!("{v:.4}"));
            let load = |end: &str| {
                row["load"][end]["load_average_1m"]
                    .as_f64()
                    .map_or("-".into(), |v| format!("{v:.2}"))
            };
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                row["metric"].as_str().unwrap_or(""),
                row["unit"].as_str().unwrap_or(""),
                d["count"].as_u64().unwrap_or(0),
                figure("p50"),
                figure("p95"),
                load("start"),
                load("end"),
            ));
        }
        md.push_str("\n## Components\n\n");
        for component in &self.components {
            md.push_str(&format!(
                "- {}: {}\n",
                component["name"].as_str().unwrap_or(""),
                component["status"].as_str().unwrap_or("")
            ));
        }
        fs::write(self.out.join("usage-measure.md"), md)?;
        Ok(())
    }

    fn load(&self) -> Option<f64> {
        launch::load_average(&self.root)
    }
}

/// Microseconds of a duration.
fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// The events the producer rows record, in turn: the largest kinds a producer writes.
fn events() -> [EventKind; 3] {
    let image = ImageKey::from_bytes([3; 32]).image_id(&[4; 32]);
    [
        EventKind::ToolCommit {
            tool: Tool::Basic,
            image,
        },
        EventKind::Interaction {
            feature: Feature::Zoom,
            image: Some(image),
        },
        EventKind::OperationEnd(OperationEnd::export(
            Outcome::Completed,
            DurationMs::from_millis(812),
            Some(1),
            Some(image),
        )),
    ]
}

fn start_session(collector: &Collector) {
    collector.start(
        Trigger::Launch,
        SessionStart::parse("2026-10-11T00:00:00Z").expect("a valid start"),
        SessionClock::monotonic(),
        ImageKey::from_bytes([1; 32]),
    );
}

/// A transport that refuses every request; the producer rows never reach a send.
#[derive(Debug)]
struct Refusing;

impl Transport for Refusing {
    fn send(
        &self,
        _: &TransportRequest,
        _: SendOptions<'_>,
        _: &mut dyn io::Write,
    ) -> std::result::Result<TransportResponse, Error> {
        Err(Error::not_ready("usage-measure sends nothing"))
    }
}

fn producer(report: &mut Report) -> Result {
    let kinds = events();
    let scope = "one call in this process (release xtask), timed with the monotonic clock around it, so each sample includes one clock read";
    let load_start = report.load();

    // Off: the gate's one atomic load.
    let collector = Collector::new();
    let mut off = Vec::with_capacity(PRODUCER_CALLS);
    for index in 0..PRODUCER_CALLS {
        let started = Instant::now();
        collector.record(kinds[index % kinds.len()]);
        off.push(micros(started.elapsed()));
    }

    // On, into a bank with room: the window is detached (untimed) every 500 events.
    start_session(&collector);
    let session = collector.session().ok_or("no session")?;
    let mut stored = Vec::with_capacity(PRODUCER_CALLS);
    let mut batch = Duration::ZERO;
    for index in 0..PRODUCER_CALLS {
        if index % 500 == 0 {
            drop(session.detach());
        }
        let started = Instant::now();
        collector.record(kinds[index % kinds.len()]);
        let took = started.elapsed();
        batch += took;
        stored.push(micros(took));
    }

    // Into a full bank: dropped and counted.
    drop(session.detach());
    for index in 0..600 {
        collector.record(kinds[index % kinds.len()]);
    }
    let mut dropped = Vec::with_capacity(PRODUCER_CALLS);
    for index in 0..PRODUCER_CALLS {
        let started = Instant::now();
        collector.record(kinds[index % kinds.len()]);
        dropped.push(micros(started.elapsed()));
    }

    // Four producers at once while a fifth thread detaches a window every millisecond.
    drop(session.detach());
    let collector = Arc::new(collector);
    let done = Arc::new(AtomicBool::new(false));
    let detacher = {
        let (collector, done) = (collector.clone(), done.clone());
        thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if let Some(session) = collector.session() {
                    drop(session.detach());
                }
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let producers: Vec<_> = (0..PRODUCER_THREADS)
        .map(|_| {
            let collector = collector.clone();
            thread::spawn(move || {
                let kinds = events();
                let mut samples = Vec::with_capacity(PRODUCER_CALLS / PRODUCER_THREADS);
                for index in 0..PRODUCER_CALLS / PRODUCER_THREADS {
                    let started = Instant::now();
                    collector.record(kinds[index % kinds.len()]);
                    samples.push(micros(started.elapsed()));
                }
                samples
            })
        })
        .collect();
    let mut contended = Vec::with_capacity(PRODUCER_CALLS);
    for producer in producers {
        contended.extend(producer.join().map_err(|_| "a producer thread panicked")?);
    }
    done.store(true, Ordering::Relaxed);
    detacher.join().map_err(|_| "the detaching thread panicked")?;

    // The keyed image ID: one HMAC over the catalog fingerprint, once per open.
    let fingerprint = "ab".repeat(32);
    let mut image_ids = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let started = Instant::now();
        let id = collector.image_id(&fingerprint);
        image_ids.push(micros(started.elapsed()));
        ensure(id.is_some(), "the started collector computed no image ID")?;
        std::hint::black_box(id);
    }

    // `Supervisor::interacted`, as each key press, click and scroll calls it: an opted-in
    // supervisor on the platform clock with test eligibility, its worker parked.
    let dir = tempfile::tempdir()?;
    write_json(
        &dir.path().join("preferences.json"),
        &json!({"format": 1, "usage_statistics": {"prompt_shown": true, "enabled": true,
            "disclosure_version": 1, "installation_id": "c9a10716-1b78-4cbe-8d1c-b8309e5e7247",
            "image_key": "ab".repeat(32)}}),
    )?;
    let usage = Arc::new(Usage::new(Eligibility::test(
        TestReceiver::loopback("http://127.0.0.1:9/v1/usage/batches")
            .map_err(|error| error.detail)?,
    )));
    usage.launch(Some(dir.path().to_path_buf()));
    let supervised = Arc::new(Collector::new());
    let supervisor = Supervisor::start(SupervisorConfig {
        usage,
        collector: supervised.clone(),
        preferences_dir: Some(dir.path().to_path_buf()),
        transport: Arc::new(Refusing),
        release: Release::parse("0.0.0+000000e71de5"),
        device: Box::new(DeviceFn(|| {
            Err(luxforge_core::usage::DeviceUnavailable::Pending)
        })),
        sampler: Box::new(NoSampling),
        clock: Arc::new(SystemClock::new()),
        randomness: Box::new(SystemRandomness::new()),
    });
    supervisor.set_presented(true);
    let mut interacted = Vec::with_capacity(PRODUCER_CALLS);
    for _ in 0..PRODUCER_CALLS {
        let started = Instant::now();
        supervisor.interacted();
        interacted.push(micros(started.elapsed()));
    }
    supervisor.shutdown();
    drop(supervisor);

    let load = json!({"start": launch::load(load_start), "end": launch::load(report.load())});
    let mean = |samples: &[f64]| vec![samples.iter().sum::<f64>() / samples.len().max(1) as f64];
    let mean_scope = "the mean of the row's samples: the monotonic clock ticks every 41.7 ns on Apple silicon, so a p50 or p95 below a few ticks is the clock's resolution, not the call's";
    for (metric, samples) in [
        ("producer.record_off_mean", mean(&off)),
        ("producer.record_full_bank_mean", mean(&dropped)),
        ("producer.image_id_mean", mean(&image_ids)),
        ("producer.interacted_mean", mean(&interacted)),
    ] {
        report.row(metric, "us", samples, mean_scope, &load);
    }
    report.row("producer.record_off", "us", off, scope, &load);
    report.row("producer.record_stored", "us", stored, scope, &load);
    report.row(
        "producer.record_stored_mean",
        "us",
        vec![micros(batch) / PRODUCER_CALLS as f64],
        mean_scope,
        &load,
    );
    report.row("producer.record_full_bank", "us", dropped, scope, &load);
    report.row(
        "producer.record_contended",
        "us",
        contended,
        "four producer threads at once while a fifth detaches a window every millisecond; each call timed on its own thread",
        &load,
    );
    report.row("producer.image_id", "us", image_ids, scope, &load);
    report.row(
        "producer.interacted",
        "us",
        interacted,
        "Supervisor::interacted on an opted-in supervisor with the platform clock, its worker parked: a lock and a clock read",
        &load,
    );
    Ok(())
}

fn sampler(report: &mut Report) -> Result {
    let load_start = report.load();
    let mut reads = Vec::with_capacity(SAMPLER_READS);
    for _ in 0..SAMPLER_READS {
        let started = Instant::now();
        let fetched = luxforge_core::resources::reading(Duration::ZERO);
        reads.push(micros(started.elapsed()));
        ensure(!fetched.reused, "a zero-age reading reused an earlier one")?;
        std::hint::black_box(fetched);
    }
    let load = json!({"start": launch::load(load_start), "end": launch::load(report.load())});
    report.row(
        "sampler.fresh_reading",
        "us",
        reads,
        "one fresh read of this process's CPU time and memory through the core's process sampler, the read the statistics sampler takes at most once a minute of presented activity; the GPU counters the desktop also enables are not opened here",
        &load,
    );
    Ok(())
}

/// One owner run over `source`: import, preparation, `samples` Basic commits and the exports, each
/// timed in milliseconds, with collection on or off.
fn owner_run(dir: &Path, source: &Path, samples: usize, collecting: bool) -> Result<[Vec<f64>; 4]> {
    fs::create_dir_all(dir)?;
    let collector = Arc::new(Collector::new());
    if collecting {
        start_session(&collector);
    }
    let (owner, join) = OwnerHandle::start_with_host(
        &dir.join("catalog.sqlite"),
        Arc::new(ModuleRegistry::builtin()),
        HostConfig {
            usage: collector.clone(),
            ..HostConfig::unconfigured()
        },
    )?;
    let outcome = (|| -> Result<[Vec<f64>; 4]> {
        let editor = owner.register();
        let started = Instant::now();
        let accepted = call(
            &owner,
            editor,
            "pick.develop",
            json!({"targets": {"kind": "paths", "paths": [source]}, "into": [],
                "confirm_removable": true,
                "mutation": {"request_id": client::request_id("develop"), "actor": ACTOR}}),
        )?;
        let developed = client::settle(&owner, editor, &accepted["job_id"])?;
        let develop_ms = milliseconds(started);
        ensure(developed["status"] == "ready", format!("{developed}"))?;
        let asset = developed["result"]["developed"][0]["asset_id"].clone();
        let started = Instant::now();
        let prepared = call(&owner, editor, "source.prepare", json!({"asset_id": asset}))?;
        let status = client::settle(&owner, editor, &prepared["job_id"])?;
        let prepare_ms = milliseconds(started);
        ensure(status["status"] == "ready", format!("{status}"))?;
        let mut commits = Vec::with_capacity(samples);
        for index in 0..samples {
            let revision = client::revision(&owner, editor, &asset)?;
            let started = Instant::now();
            call(
                &owner,
                editor,
                "edit.set-basic",
                json!({"asset_id": asset, "exposure": 0.1 + index as f64 * 0.05,
                    "mutation": client::mutation(revision, &client::request_id("commit"), ACTOR)}),
            )?;
            commits.push(milliseconds(started));
        }
        let mut exports = Vec::with_capacity(EXPORTS);
        for index in 0..EXPORTS {
            let destination = dir.join(format!("export-{index}.jpg"));
            let started = Instant::now();
            let accepted = call(
                &owner,
                editor,
                "export.jpeg",
                json!({"asset_id": asset, "destination": destination,
                    "mutation": {"request_id": client::request_id("export"), "actor": ACTOR}}),
            )?;
            let record = client::settle(&owner, editor, &accepted["job_id"])?;
            exports.push(milliseconds(started));
            ensure(record["status"] == "ready", format!("{record}"))?;
            fs::remove_file(&destination)?;
        }
        Ok([vec![develop_ms, prepare_ms], commits, exports, Vec::new()])
    })();
    owner.stop();
    join.join().map_err(|_| "the owner thread panicked")?;
    if collecting {
        let recorded = collector
            .session()
            .map(|session| session.diagnostics().recorded);
        ensure(
            recorded.is_some_and(|recorded| recorded > 0),
            "the opted-in owner recorded no event",
        )?;
    }
    let _ = fs::remove_dir_all(dir);
    outcome
}

fn owner(report: &mut Report, options: &Options) -> Result {
    for source in &options.sources {
        let name = source_name(source);
        let load_start = report.load();
        let mut figures: [[Vec<f64>; 4]; 2] = Default::default();
        for repeat in 0..options.repeats {
            let order = if repeat % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for collecting in order {
                let mode = if collecting { "on" } else { "off" };
                let [opening, commits, exports, _] = owner_run(
                    &report.out.join(format!("owner-{name}-{mode}-{repeat}")),
                    source,
                    options.samples,
                    collecting,
                )?;
                let held = &mut figures[usize::from(collecting)];
                held[0].push(opening[0]);
                held[1].push(opening[1]);
                held[2].extend(commits);
                held[3].extend(exports);
            }
        }
        let load = json!({"start": launch::load(load_start), "end": launch::load(report.load())});
        for (collecting, held) in figures.into_iter().enumerate() {
            let mode = if collecting == 1 { "on" } else { "off" };
            let scope = format!(
                "a catalog owner in this process over {} (reference renderer, no GPU), an in-memory collector {}; {} runs alternating on and off; each figure from the request to its settled job (polled every millisecond) or its answer",
                source.display(),
                if collecting == 1 {
                    "started (opted in)"
                } else {
                    "never started (opted out)"
                },
                options.repeats
            );
            let [develop, prepare, commit, export] = held;
            report.row(&format!("owner.{name}.{mode}.develop"), "ms", develop, &scope, &load);
            report.row(&format!("owner.{name}.{mode}.prepare"), "ms", prepare, &scope, &load);
            report.row(&format!("owner.{name}.{mode}.commit"), "ms", commit, &scope, &load);
            report.row(&format!("owner.{name}.{mode}.export"), "ms", export, &scope, &load);
        }
        report.write()?;
    }
    Ok(())
}

fn source_name(source: &Path) -> String {
    source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".into())
}

/// The drag rows kept from each `editor-latency` report.
const DRAG_METRICS: [(&str, &str); 4] = [
    ("input_to_presented_frame", "ms"),
    ("sampled_peak_rss_mib", "MiB"),
    ("process_peak_footprint_mib", "MiB"),
    ("close_after_shutdown_event", "ms"),
];

fn drag(report: &mut Report, options: &Options, bin: &Path) -> Result {
    for source in &options.sources {
        let name = source_name(source);
        let load_start = report.load();
        let mut figures: BTreeMap<(usize, &str), Vec<f64>> = BTreeMap::new();
        let mut loads = Vec::new();
        for repeat in 0..options.repeats {
            for offset in 0..UsageMode::ALL.len() {
                let index = (offset + repeat) % UsageMode::ALL.len();
                let mode = UsageMode::ALL[index];
                let dir = report
                    .out
                    .join(format!("drag-{name}-{}-{repeat}", mode.name()));
                let before = report.load();
                editor_latency::run(
                    &report.root,
                    &dir,
                    bin,
                    editor_latency::Options {
                        source,
                        samples: options.samples.clamp(1, 60),
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
                        usage: Some(mode),
                    },
                )?;
                loads.push(json!({"run": dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                    "start": before, "end": report.load()}));
                let latency = read_json(&dir.join("latency.json"))?;
                for (metric, _) in DRAG_METRICS {
                    if let Some(samples) = stats::distribution(&latency, metric)
                        .and_then(|distribution| distribution["samples"].as_array())
                    {
                        figures
                            .entry((index, metric))
                            .or_default()
                            .extend(samples.iter().filter_map(Value::as_f64));
                    }
                }
            }
        }
        let load = json!({"start": launch::load(load_start), "end": launch::load(report.load()),
            "runs": loads});
        for (index, mode) in UsageMode::ALL.iter().enumerate() {
            let scope = format!(
                "editor-latency --mode drag --usage {} over {}, {} inputs, Basic exposure at Fit, release editor in a background launch; {} runs per mode in rotating order; the statistics clock moved past the first window just before the drag",
                mode.name(),
                source.display(),
                options.samples.clamp(1, 60),
                options.repeats
            );
            for (metric, unit) in DRAG_METRICS {
                let samples = figures.remove(&(index, metric)).unwrap_or_default();
                let scope = if metric == "close_after_shutdown_event" {
                    format!("{scope}. {}", editor_latency::CLOSE_NOTE)
                } else {
                    scope.clone()
                };
                report.row(
                    &format!("drag.{name}.{}.{metric}", mode.name()),
                    unit,
                    samples,
                    &scope,
                    &load,
                );
            }
        }
        report.write()?;
    }
    Ok(())
}

/// The presence launch's script: the native window shown, the Performance section closed, the
/// statistics clock moved past the first window, two idle windows presented, the window hidden,
/// two observation windows, shown again, and the attempt's end (opted in) or collection read off
/// (opted out), so every mode runs the same steps.
fn presence_script(mode: UsageMode) -> Vec<script::Step> {
    let mut steps = vec![
        script::Step::WindowVisibility {
            action: "show_window".into(),
        },
        script::Step::Performance { expanded: false },
        script::Step::Usage(script::UsageStep::Advance(ADVANCE_S)),
        script::Step::Idle(WINDOW),
        script::Step::Idle(WINDOW),
        script::Step::WindowVisibility {
            action: "hide_window".into(),
        },
        script::Step::Observe(WINDOW),
        script::Step::Observe(WINDOW),
        script::Step::WindowVisibility {
            action: "show_window".into(),
        },
    ];
    steps.push(script::Step::Usage(script::UsageStep::Await(
        editor_latency::usage_await(mode),
    )));
    steps
}

/// The windows a presence launch recorded: `(presented, cpu %, drawn frames, views, updates)`.
fn presence_windows(events: &[Value]) -> Vec<(bool, f64, f64, f64, f64)> {
    events
        .iter()
        .filter_map(|event| {
            let presented = match event["event"].as_str()? {
                "idle_check" => true,
                "presentation_observation_ended" => false,
                _ => return None,
            };
            let detail = &event["detail"];
            let updates = detail["after"]["updates"]
                .as_f64()
                .zip(detail["before"]["updates"].as_f64())
                .map_or(f64::NAN, |(after, before)| after - before);
            Some((
                presented,
                detail["process_cpu_percent_one_core"]
                    .as_f64()
                    .unwrap_or(f64::NAN),
                detail["drawn_frames_delta"].as_f64().unwrap_or(f64::NAN),
                detail["views_delta"].as_f64().unwrap_or(f64::NAN),
                updates,
            ))
        })
        .collect()
}

fn presence(report: &mut Report, options: &Options, bin: &Path) -> Result {
    let source = options.sources.first().ok_or("no source")?.canonicalize()?;
    let modes = [UsageMode::Off, UsageMode::Fast, UsageMode::Offline];
    let load_start = report.load();
    let mut figures: BTreeMap<(usize, &str), Vec<f64>> = BTreeMap::new();
    let dir = report.out.join("presence");
    let mut run = Run::tool(&report.root, &dir, TOOL, bin, Duration::from_secs(150))?;
    let outcome = (|| -> Result {
        for repeat in 0..options.repeats {
            for offset in 0..modes.len() {
                let index = (offset + repeat) % modes.len();
                let mode = modes[index];
                let name = format!("presence-{}-{repeat}", mode.name());
                let setup = UsageSetup::start(&dir.join(format!("{name}-usage")), mode)?;
                let steps = presence_script(mode);
                let launch = Launch::named(&name)
                    .script(&format!("{name}-script.json"), script::write(&steps))
                    .open_all(std::slice::from_ref(&source))
                    .usage_receiver(&setup.url)
                    .evidence_config(&setup.config)
                    .watch(editor_latency::sampled(&report.root, &name));
                let launched = run.launch(launch)?;
                let events = scenario::events(&launched.dir.join("events.jsonl"))?;
                let windows = presence_windows(&events);
                ensure(
                    windows.len() == 4,
                    format!("{name}: {} windows recorded, not 4", windows.len()),
                )?;
                for (presented, cpu, drawn, views, updates) in windows {
                    let state = if presented { "presented" } else { "hidden" };
                    for (figure, value) in [
                        ("process_cpu_percent_one_core", cpu),
                        ("drawn_frames", drawn),
                        ("views", views),
                        ("updates", updates),
                    ] {
                        let key: &'static str = match (state, figure) {
                            ("presented", "process_cpu_percent_one_core") => "presented.cpu",
                            ("presented", "drawn_frames") => "presented.frames",
                            ("presented", "views") => "presented.views",
                            ("presented", _) => "presented.updates",
                            (_, "process_cpu_percent_one_core") => "hidden.cpu",
                            (_, "drawn_frames") => "hidden.frames",
                            (_, "views") => "hidden.views",
                            _ => "hidden.updates",
                        };
                        if value.is_finite() {
                            figures.entry((index, key)).or_default().push(value);
                        }
                    }
                }
                if let Some(close) = editor_latency::close_after_shutdown(&events, &launched.watched) {
                    figures.entry((index, "close")).or_default().push(close);
                }
                let requests = setup.requests();
                match mode {
                    UsageMode::Off | UsageMode::Offline => ensure(
                        requests.is_empty(),
                        format!("{name}: the receiver saw {} requests", requests.len()),
                    )?,
                    _ => ensure(
                        requests.len() == 1,
                        format!("{name}: {} requests, not one", requests.len()),
                    )?,
                }
                write_json(&dir.join(format!("{name}-receiver.json")), &setup.record())?;
            }
        }
        Ok(())
    })();
    run.finish(outcome, |_| Ok(()))?;
    let load = json!({"start": launch::load(load_start), "end": launch::load(report.load())});
    for (index, mode) in modes.iter().enumerate() {
        let scope = format!(
            "one background launch per repeat over {} with --usage {} ({} repeats, modes rotating): the Performance section closed and the statistics clock moved past the first window, then two {} ms windows presented and idle (the editor's own idle check) and two while the window is hidden through the native evidence step; per window, from the editor's own counters",
            source.display(),
            mode.name(),
            options.repeats,
            WINDOW.ms
        );
        for (key, metric, unit) in [
            ("presented.cpu", "presented_idle_process_cpu_percent_one_core", "%"),
            ("presented.frames", "presented_idle_drawn_frames", "frames"),
            ("presented.views", "presented_idle_views", "views"),
            ("presented.updates", "presented_idle_updates", "updates"),
            ("hidden.cpu", "hidden_process_cpu_percent_one_core", "%"),
            ("hidden.frames", "hidden_drawn_frames", "frames"),
            ("hidden.views", "hidden_views", "views"),
            ("hidden.updates", "hidden_updates", "updates"),
            ("close", "close_after_shutdown_event", "ms"),
        ] {
            let samples = figures.remove(&(index, key)).unwrap_or_default();
            let scope = if key == "close" {
                format!("{scope}. {}", editor_latency::CLOSE_NOTE)
            } else {
                scope.clone()
            };
            report.row(
                &format!("presence.{}.{metric}", mode.name()),
                unit,
                samples,
                &scope,
                &load,
            );
        }
    }
    Ok(())
}

/// How long a quit launch runs after its clock move before its script ends and it quits.
const QUIT_AFTER: script::IdleStep = script::IdleStep {
    settle_ms: 1,
    ms: 5_000,
};

/// The close with a request in flight: one launch per mode and repeat over the first source that
/// moves the statistics clock past the first window and quits five seconds later, while the slow
/// receiver still holds that window's request (it holds each for ten seconds), against the same
/// launch opted out and with the fast receiver.
fn quit(report: &mut Report, options: &Options, bin: &Path) -> Result {
    let source = options.sources.first().ok_or("no source")?.canonicalize()?;
    let modes = [UsageMode::Off, UsageMode::Fast, UsageMode::Slow];
    let load_start = report.load();
    let mut closes: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    let dir = report.out.join("quit");
    let mut run = Run::tool(&report.root, &dir, TOOL, bin, Duration::from_secs(60))?;
    let outcome = (|| -> Result {
        for repeat in 0..options.repeats {
            for offset in 0..modes.len() {
                let index = (offset + repeat) % modes.len();
                let mode = modes[index];
                let name = format!("quit-{}-{repeat}", mode.name());
                let setup = UsageSetup::start(&dir.join(format!("{name}-usage")), mode)?;
                let steps = [
                    script::Step::Usage(script::UsageStep::Advance(ADVANCE_S)),
                    script::Step::Observe(QUIT_AFTER),
                ];
                let launch = Launch::named(&name)
                    .script(&format!("{name}-script.json"), script::write(&steps))
                    .open_all(std::slice::from_ref(&source))
                    .usage_receiver(&setup.url)
                    .evidence_config(&setup.config)
                    .watch(editor_latency::sampled(&report.root, &name));
                let launched = run.launch(launch)?;
                let events = scenario::events(&launched.dir.join("events.jsonl"))?;
                let close = editor_latency::close_after_shutdown(&events, &launched.watched)
                    .ok_or(format!("{name}: no close measured"))?;
                closes.entry(index).or_default().push(close);
                let requests = setup.requests();
                let expected = usize::from(mode.collects());
                ensure(
                    requests.len() == expected,
                    format!("{name}: {} requests, not {expected}", requests.len()),
                )?;
                if mode == UsageMode::Slow {
                    // The request was still held when the editor's script ended.
                    let received = requests[0]["received_ms"].as_f64().unwrap_or(f64::NAN);
                    ensure(
                        received.is_finite(),
                        format!("{name}: the request has no receipt time"),
                    )?;
                }
                write_json(&dir.join(format!("{name}-receiver.json")), &setup.record())?;
            }
        }
        Ok(())
    })();
    run.finish(outcome, |_| Ok(()))?;
    let load = json!({"start": launch::load(load_start), "end": launch::load(report.load())});
    for (index, mode) in modes.iter().enumerate() {
        let scope = format!(
            "one background launch per repeat over {} with --usage {} ({} repeats, modes rotating): the statistics clock moved past the first window, then a {} ms observation and the script's end, so the editor quits{}. {}",
            source.display(),
            mode.name(),
            options.repeats,
            QUIT_AFTER.ms,
            if *mode == UsageMode::Slow {
                " while the slow receiver holds the window's request"
            } else {
                ""
            },
            editor_latency::CLOSE_NOTE
        );
        report.row(
            &format!("quit.{}.close_after_shutdown_event", mode.name()),
            "ms",
            closes.remove(&index).unwrap_or_default(),
            &scope,
            &load,
        );
    }
    Ok(())
}

pub fn run(root: &Path, out: &Path, bin: &Path, options: Options) -> Result {
    ensure(!out.exists(), "usage-measure output must be new")?;
    ensure(options.repeats > 0, "--repeats must be positive")?;
    ensure(!options.sources.is_empty(), "no --source given")?;
    if let Some(only) = &options.only {
        for name in only {
            ensure(
                COMPONENTS.contains(&name.as_str()),
                format!("--only names one of {COMPONENTS:?}, not {name}"),
            )?;
        }
    }
    let in_process = ["producer", "sampler", "owner"];
    ensure(
        !cfg!(debug_assertions)
            || options
                .only
                .as_ref()
                .is_some_and(|only| only.iter().all(|name| !in_process.contains(&name.as_str()))),
        "the producer, sampler and owner components time this process: run a release xtask \
         (`cargo run --release --locked -p xtask -- usage-measure ...`)",
    )?;
    fs::create_dir_all(out)?;
    let hashes: Vec<String> = options
        .sources
        .iter()
        .map(|source| hash(source))
        .collect::<Result<_>>()?;
    let mut report = Report {
        out: out.to_path_buf(),
        root: root.to_path_buf(),
        rows: Vec::new(),
        components: Vec::new(),
        header: json!({
            "binary": bin,
            "binary_sha256": hash(bin).ok(),
            "lockfile_sha256": hash(&root.join("Cargo.lock")).ok(),
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "sources": options.sources,
            "samples": options.samples,
            "repeats": options.repeats,
            "load_threshold": launch::LOAD_THRESHOLD,
            "xtask_profile": if cfg!(debug_assertions) { "dev" } else { "release" },
        }),
    };
    let wanted =
        |name: &str| options.only.as_ref().is_none_or(|only| only.iter().any(|n| n == name));
    let mut failed = Vec::new();
    for name in COMPONENTS {
        if !wanted(name) {
            report
                .components
                .push(json!({"name": name, "status": "not_run"}));
            continue;
        }
        let started = Instant::now();
        let outcome = match name {
            "producer" => producer(&mut report),
            "sampler" => sampler(&mut report),
            "owner" => owner(&mut report, &options),
            "drag" => drag(&mut report, &options, bin),
            "presence" => presence(&mut report, &options, bin),
            _ => quit(&mut report, &options, bin),
        };
        let status = match &outcome {
            Ok(()) => json!({"name": name, "status": "passed",
                "elapsed_s": started.elapsed().as_secs_f64()}),
            Err(error) => {
                failed.push(name);
                json!({"name": name, "status": "failed", "error": error.to_string(),
                    "elapsed_s": started.elapsed().as_secs_f64()})
            }
        };
        report.components.push(status);
        report.write()?;
    }
    for (source, before) in options.sources.iter().zip(&hashes) {
        ensure(
            hash(source)? == *before,
            format!("{} changed", source.display()),
        )?;
    }
    ensure(
        failed.is_empty(),
        format!("usage-measure components failed: {failed:?}"),
    )?;
    println!("PASS usage-measure: {}", out.display());
    Ok(())
}
