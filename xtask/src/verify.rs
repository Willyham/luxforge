//! One command for a whole verification tier.
//!
//! Every component is one of the harness's own commands, run as a child process of the release
//! `xtask` executable with its console output redirected to a file. Child processes rather than
//! in-process calls: each runner prints its result JSON to stdout and `check` runs Cargo, so the
//! terminal would drown out the table this command exists to print; a panic or an abort inside one
//! component must not take the summary down; and running the documented entry points is itself a
//! check that they still work as documented.
use crate::*;
use std::{
    process::Stdio,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// How many rendered scenarios run at once by default. Each one is its own child process with its
/// own output directory, catalog and evidence directory, so the only thing they share is the host;
/// three keeps the fourteen-core machine busy without making every scenario's own deadlines a race.
pub const JOBS: usize = 3;

/// How long any one component may run before it is killed and reaped. Every component already
/// bounds its own editor launches, so this only catches a component that has stopped making
/// progress. A killed child cannot run its own cleanup guards, so a component recorded as
/// `timed_out` may have left an editor process behind: that is a failure to investigate, never a
/// normal outcome.
const DEADLINE: Duration = Duration::from_secs(20 * 60);

/// The JPEG workloads the rendered and timing tiers need before they can run. The hue wheel is the
/// `mixer` scenario's own fixture; timing components still read `GENERATED[0]`, the 24 MP workload.
const GENERATED: [&str; 3] = [
    "fixtures/generated/24mp.jpg",
    "fixtures/generated/60mp.jpg",
    "fixtures/generated/hue-wheel.jpg",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Quick,
    Rendered,
    Timing,
    Full,
}

impl Tier {
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "quick" => Self::Quick,
            "rendered" => Self::Rendered,
            "timing" => Self::Timing,
            "full" => Self::Full,
            other => {
                return Err(
                    format!("--tier is quick, rendered, timing or full, not {other}").into(),
                );
            }
        })
    }
    fn name(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Rendered => "rendered",
            Self::Timing => "timing",
            Self::Full => "full",
        }
    }
    fn rendered(self) -> bool {
        matches!(self, Self::Rendered | Self::Full)
    }
    fn timing(self) -> bool {
        matches!(self, Self::Timing | Self::Full)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Passed,
    Failed,
    Skipped,
    TimedOut,
    NotRun,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
            Self::NotRun => "not_run",
        }
    }
    /// A skip and a component that never ran are both unproven; only `passed` is a pass.
    fn failure(self) -> bool {
        matches!(self, Self::Failed | Self::TimedOut)
    }
}

/// How to count the editor processes a component started. The count is always read from what the
/// component itself recorded, so a run that stopped early reports the launches it actually made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Launches {
    /// No editor process at all: `check`, the core acceptance journey, core timing diagnostics,
    /// the numerical RAW reference and fixture generation.
    None,
    Smoke,
    Measure,
    Latency,
    RawEditor,
}

fn runs(result: &Value) -> usize {
    result["runs"].as_array().map_or(0, Vec::len)
}

impl Launches {
    fn count(self, dir: &Path, result: Option<&Value>) -> u64 {
        let Some(result) = result else { return 0 };
        match self {
            Self::None => 0,
            // A scenario records one exit code per launch it made: the two-launch scenarios
            // (`unavailable`, `basic-restart`) record `launch1_exit_code` and `launch2_exit_code`,
            // every other one a single `exit_code`.
            Self::Smoke => ["exit_code", "launch1_exit_code", "launch2_exit_code"]
                .iter()
                .filter(|key| !result[**key].is_null())
                .count() as u64,
            // One launch per measured run, plus the idle process, which is recorded separately.
            Self::Measure => runs(result) as u64 + u64::from(!result["idle"].is_null()),
            // One scripted gesture launch; `--idle` adds the hold and idle pair, and that pair is
            // the only thing that writes `resources.json`.
            Self::Latency => {
                1 + if dir.join("run/resources.json").is_file() {
                    2
                } else {
                    0
                }
            }
            // Each trial is a scripted edit launch and a reopen launch.
            Self::RawEditor => 2 * runs(result) as u64,
        }
    }
}

/// One planned component: the arguments it gets, where its result lives and how to count its
/// launches.
struct Spec {
    name: String,
    tier: &'static str,
    args: Vec<String>,
    /// The result file inside the component's own `run/` directory, when it writes one.
    result: Option<&'static str>,
    launches: Launches,
    /// Whether the component takes `--output`, `--binary` and `--manifest`.
    output: bool,
    binary: bool,
    manifest: bool,
    /// Why this component cannot run, when it cannot.
    skip: Option<&'static str>,
}

fn spec(name: &str, tier: &'static str, args: &[&str]) -> Spec {
    Spec {
        name: name.into(),
        tier,
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        result: None,
        launches: Launches::None,
        output: true,
        binary: false,
        manifest: false,
        skip: None,
    }
}

/// What each tier runs, in order. Every tier includes the ones below it. The rendered scenarios are
/// the one block that runs through a pool; the timing components run strictly serially, in this
/// order, after everything else in the tier and behind the host-wide timing lock, so nothing else
/// on the machine is competing with them from this command.
fn plan(tier: Tier, manifest: bool, fixtures: bool) -> Vec<Spec> {
    let mut specs = Vec::new();
    if !fixtures && tier != Tier::Quick {
        specs.push(Spec {
            output: false,
            ..spec("generate-fixtures", "setup", &["generate-fixtures"])
        });
    }
    specs.push(Spec {
        output: false,
        ..spec("check", "quick", &["check"])
    });
    specs.push(Spec {
        result: Some("result.json"),
        ..spec("editor-acceptance", "quick", &["editor-acceptance"])
    });
    if tier.rendered() {
        for scenario in smoke::SCENARIOS {
            specs.push(Spec {
                result: Some("result.json"),
                launches: Launches::Smoke,
                binary: true,
                ..spec(
                    &format!("smoke-{scenario}"),
                    "rendered",
                    &["smoke", "--scenario", scenario],
                )
            });
        }
    }
    if tier == Tier::Full {
        specs.push(Spec {
            result: Some("result.json"),
            ..spec("raw-reference", "full", &["raw-reference"])
        });
        specs.push(Spec {
            result: Some("result.json"),
            launches: Launches::RawEditor,
            binary: true,
            manifest: true,
            skip: (!manifest).then_some("no --manifest"),
            ..spec("raw-editor", "full", &["raw-editor"])
        });
    }
    if tier.timing() {
        specs.push(Spec {
            result: Some("result.json"),
            ..spec(
                "editor-performance",
                "timing",
                &["editor-performance", "--source", GENERATED[0]],
            )
        });
        specs.push(Spec {
            result: Some("latency.json"),
            launches: Launches::Latency,
            binary: true,
            ..spec(
                "editor-latency",
                "timing",
                &["editor-latency", "--source", GENERATED[0]],
            )
        });
        specs.push(Spec {
            result: Some("measurements.json"),
            launches: Launches::Measure,
            binary: true,
            ..spec("measure", "timing", &["measure"])
        });
    }
    specs
}

fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// One component's outcome, as the summary records it.
struct Entry {
    component: String,
    tier: String,
    status: Status,
    /// Seconds from the start of the run to the start of this component. With the pool this is how
    /// the summary shows which scenarios overlapped and which waited for a worker.
    started_at_s: f64,
    elapsed_s: f64,
    exit_code: Option<i32>,
    error: Option<String>,
    artifacts: Vec<String>,
    launches: u64,
    /// The one-minute load average when this component started, for timing components: a figure is
    /// only as good as the host was.
    load: Option<f64>,
}

impl Entry {
    fn value(&self) -> Value {
        json!({
            "component":self.component,
            "tier":self.tier,
            "status":self.status.name(),
            "started_at_s":tenths(self.started_at_s),
            "elapsed_s":tenths(self.elapsed_s),
            "exit_code":self.exit_code,
            "error":self.error,
            "artifacts":self.artifacts,
            "launches":self.launches,
            "load_average_1m":self.load,
            "load_threshold":self.load.map(|_| launch::LOAD_THRESHOLD),
            "unreliable":self.load.map(|load| launch::unreliable(Some(load))),
        })
    }
}

/// The one-minute load average, or nothing where `sysctl` cannot report it.
fn load_average(root: &Path) -> Option<f64> {
    let text = output(root, "sysctl", &["-n", "vm.loadavg"]).ok()?;
    text.trim()
        .trim_matches(['{', '}'])
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The first line of the failure: the component's own `error` field when it wrote one, otherwise
/// the last meaningful line of its console log.
fn reason(dir: &Path, result: Option<&Value>) -> Option<String> {
    let recorded = result
        .and_then(|value| value["error"].as_str())
        .and_then(|text| text.lines().next())
        .map(str::to_owned);
    recorded
        .or_else(|| {
            fs::read_to_string(dir.join("console.log"))
                .ok()?
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(str::to_owned)
        })
        .map(|line| line.chars().take(400).collect())
}

fn execute(
    program: &Path,
    root: &Path,
    args: &[OsString],
    log: &Path,
    env: Option<(&str, String)>,
) -> Result<(Option<i32>, bool)> {
    let file = fs::File::create(log)?;
    let mut command = Command::new(program);
    if let Some((key, value)) = env {
        command.env(key, value);
    }
    let mut child = command
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code(), false));
        }
        if started.elapsed() >= DEADLINE {
            child.kill()?;
            child.wait()?;
            return Ok((None, true));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn number(value: f64) -> String {
    if value.abs() >= 100.0 {
        format!("{value:.1}")
    } else if value.abs() >= 1.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.3}")
    }
}

fn cell(value: &Value) -> String {
    value.as_f64().map_or_else(|| "—".into(), number)
}

/// Keep a failure message inside one Markdown table cell.
fn escape(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn unit(metric: &str) -> &'static str {
    if metric.contains("_mib") {
        "MiB"
    } else if metric.contains("cpu_percent") {
        "% of one core"
    } else if metric.ends_with("_s") {
        "s"
    } else {
        "ms"
    }
}

/// How a figure taken at this load is labelled. Every timing row and verdict carries the load and
/// the threshold, whichever side of it the host was on, so a summary always says what it was
/// measured against.
fn reliability(load: Option<f64>) -> &'static str {
    if launch::unreliable(load) {
        "unreliable"
    } else {
        "ok"
    }
}

/// Why a figure cannot be compared against its target.
fn too_loaded(load: Option<f64>) -> String {
    format!(
        "one-minute load average {} at the start of the component exceeds the {} threshold; this figure is neither a pass nor a miss",
        number(load.unwrap_or_default()),
        number(launch::LOAD_THRESHOLD)
    )
}

fn row(
    source: &str,
    metric: &str,
    p50: Value,
    p95: Value,
    count: usize,
    load: Option<f64>,
) -> Value {
    json!({"metric":metric,"unit":unit(metric),"p50":p50,"p95":p95,"count":count,"source":source,"load_average_1m":load,"load_threshold":launch::LOAD_THRESHOLD,"reliability":reliability(load)})
}

/// How many values fed one `measure` summary statistic: the same collection the runner itself does,
/// counted rather than assumed from the sample argument.
fn measured(result: &Value, workload: &str, metric: &str) -> usize {
    result["runs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|run| run["workload"] == workload)
        .map(|run| match &run[metric] {
            Value::Array(values) => values.iter().filter(|v| v.is_f64() || v.is_i64()).count(),
            Value::Null => 0,
            _ => 1,
        })
        .sum()
}

/// Which file answers a provisional target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum From {
    Latency,
    Measure,
}

/// A provisional target from the performance specification with the exact JSON path that answers
/// it. `strict` is the comparison the specification wrote, and `scale` converts the stored figure
/// into `unit`.
struct Target {
    text: &'static str,
    from: From,
    path: &'static str,
    count: Option<&'static str>,
    unit: &'static str,
    limit: f64,
    strict: bool,
    scale: f64,
    note: &'static str,
}

const TARGETS: [Target; 8] = [
    Target {
        text: "Warm 24 MP slider-to-presented-frame p95 < 100 ms",
        from: From::Latency,
        path: "/timings_ms/input_to_presented_frame/p95_ms",
        count: Some("/timings_ms/input_to_presented_frame/count"),
        unit: "ms",
        limit: 100.0,
        strict: true,
        scale: 1.0,
        note: "",
    },
    Target {
        text: "Settled exact histogram p95 < 200 ms after the final input, 24 MP",
        from: From::Latency,
        path: "/timings_ms/final_input_to_settled_histogram/p95_ms",
        count: Some("/timings_ms/final_input_to_settled_histogram/count"),
        unit: "ms",
        limit: 200.0,
        strict: true,
        scale: 1.0,
        note: "",
    },
    Target {
        text: "Scratch aggregate at most 64 MiB",
        from: From::Latency,
        path: "/resources/scratch/peak_bytes",
        count: None,
        unit: "MiB",
        limit: 64.0,
        strict: false,
        scale: 1.0 / (1024.0 * 1024.0),
        note: "High-water mark of the process-wide colour budget over the gesture run",
    },
    Target {
        text: "24 MP single-image edit working set <= 600 MiB CPU-resident",
        from: From::Measure,
        path: "/summary/24mp/sampled_peak_rss_mib/median",
        count: None,
        unit: "MiB",
        limit: 600.0,
        strict: false,
        scale: 1.0,
        note: "Sampled process RSS includes capture readbacks and GPU resources",
    },
    Target {
        text: "60 MP peak <= 1 GiB process RSS",
        from: From::Measure,
        path: "/summary/60mp/sampled_peak_rss_mib/median",
        count: None,
        unit: "MiB",
        limit: 1024.0,
        strict: false,
        scale: 1.0,
        note: "One 60 MP open; the sixteen-load workload is a separate row in the measurements",
    },
    Target {
        text: "Idle CPU < 1% of one core over 30 s",
        from: From::Measure,
        path: "/idle/cpu_percent_one_core",
        count: None,
        unit: "% of one core",
        limit: 1.0,
        strict: true,
        scale: 1.0,
        note: "One 30 s window after readiness and a one-second settle",
    },
    Target {
        text: "Launch to usable empty shell p95 < 1 s warm",
        from: From::Measure,
        path: "/summary/empty/launch_to_observed_frame_ms/p95",
        count: None,
        unit: "ms",
        limit: 1000.0,
        strict: true,
        scale: 1.0,
        note: "Upper bound: includes copying the executable into the temporary background bundle",
    },
    Target {
        text: "Uncached 24 MP JPEG to Fit preview p95 < 750 ms",
        from: From::Measure,
        path: "/summary/24mp/open_to_raster_ms/p95",
        count: None,
        unit: "ms",
        limit: 750.0,
        strict: true,
        scale: 1.0,
        note: "Warm filesystem cache, CPU raster; import, refresh and render",
    },
];

impl Target {
    fn file(&self) -> &'static str {
        match self.from {
            From::Latency => "editor-latency/run/latency.json",
            From::Measure => "measure/run/measurements.json",
        }
    }
    /// Sample count for the figure: the distribution's own count where the file records one, the
    /// number of collected values for a `measure` statistic, and one for a single observation.
    fn samples(&self, result: &Value) -> usize {
        if let Some(pointer) = self.count {
            return result.pointer(pointer).and_then(Value::as_u64).unwrap_or(0) as usize;
        }
        match self.path.split('/').collect::<Vec<_>>()[..] {
            ["", "summary", workload, metric, _] => measured(result, workload, metric),
            _ => 1,
        }
    }
    fn verdict(&self, result: Option<&Value>, timing: bool, load: Option<f64>) -> Value {
        let Some(result) = result else {
            let why = if timing {
                format!("{} was not written", self.file())
            } else {
                "timing tier did not run".into()
            };
            return json!({"target":self.text,"unit":self.unit,"limit":self.limit,"source":format!("{} {}",self.file(),self.path),"measured":Value::Null,"samples":0,"verdict":"not_measured","reason":why,"note":self.note,"load_average_1m":load,"load_threshold":launch::LOAD_THRESHOLD});
        };
        let Some(raw) = result.pointer(self.path).and_then(Value::as_f64) else {
            return json!({"target":self.text,"unit":self.unit,"limit":self.limit,"source":format!("{} {}",self.file(),self.path),"measured":Value::Null,"samples":0,"verdict":"not_measured","reason":format!("{} holds no {}",self.file(),self.path),"note":self.note,"load_average_1m":load,"load_threshold":launch::LOAD_THRESHOLD});
        };
        let value = raw * self.scale;
        let ok = if self.strict {
            value < self.limit
        } else {
            value <= self.limit
        };
        // A figure taken while the host was busy is recorded with everything it came from, and is
        // not turned into a verdict: the target is unanswered, not met and not missed.
        let over = launch::unreliable(load);
        let verdict = if over {
            "unreliable"
        } else if ok {
            "pass"
        } else {
            "miss"
        };
        json!({"target":self.text,"unit":self.unit,"limit":self.limit,"source":format!("{} {}",self.file(),self.path),"measured":value,"samples":self.samples(result),"verdict":verdict,"reason":over.then(|| too_loaded(load)),"note":self.note,"load_average_1m":load,"load_threshold":launch::LOAD_THRESHOLD})
    }
}

fn optional(path: &Path) -> Option<Value> {
    read_json(path).ok()
}

fn load_of(entries: &[Entry], component: &str) -> Option<f64> {
    entries
        .iter()
        .find(|e| e.component == component)
        .and_then(|e| e.load)
}

/// Every timing row and target verdict the directory can answer so far. Both are recomputed on each
/// write, so a run that stops early still leaves the rows of the components that finished.
fn collect(out: &Path, tier: Tier, entries: &[Entry]) -> (Vec<Value>, Vec<Value>) {
    let mut rows = Vec::new();
    let performance = optional(&out.join("editor-performance/run/result.json"));
    let latency = optional(&out.join("editor-latency/run/latency.json"));
    let measure = optional(&out.join("measure/run/measurements.json"));
    if let Some(result) = &performance {
        let load = load_of(entries, "editor-performance");
        for (metric, value) in result["timings_ms"].as_object().into_iter().flatten() {
            // `import` and the other one-shot core steps are single values, not distributions; they
            // are listed with that single value rather than a percentile they cannot support.
            let (p50, p95, count) = if value.is_object() {
                (
                    value["p50_ms"].clone(),
                    value["p95_ms"].clone(),
                    value["samples_ms"].as_array().map_or(0, Vec::len),
                )
            } else {
                (value.clone(), Value::Null, usize::from(!value.is_null()))
            };
            rows.push(row(
                "editor-performance/run/result.json",
                metric,
                p50,
                p95,
                count,
                load,
            ));
        }
    }
    if let Some(result) = &latency {
        let load = load_of(entries, "editor-latency");
        for (metric, value) in result["timings_ms"].as_object().into_iter().flatten() {
            rows.push(row(
                "editor-latency/run/latency.json",
                metric,
                value["p50_ms"].clone(),
                value["p95_ms"].clone(),
                value["count"].as_u64().unwrap_or(0) as usize,
                load,
            ));
        }
    }
    if let Some(result) = &measure {
        let load = load_of(entries, "measure");
        for workload in ["empty", "24mp", "60mp"] {
            for metric in [
                "launch_to_observed_frame_ms",
                "sampled_peak_rss_mib",
                "open_to_raster_ms",
                "upload_ms",
                "request_to_capture_ms",
            ] {
                let stat = &result["summary"][workload][metric];
                rows.push(row(
                    "measure/run/measurements.json",
                    &format!("{workload}.{metric}"),
                    stat["median"].clone(),
                    stat["p95"].clone(),
                    measured(result, workload, metric),
                    load,
                ));
            }
        }
        // The idle block is one observation, not a distribution, and it is absent from a run that
        // never reached it.
        for metric in ["cpu_percent_one_core", "rss_mib_peak", "duration_s"] {
            let value = result["idle"][metric].clone();
            let count = usize::from(!value.is_null());
            rows.push(row(
                "measure/run/measurements.json",
                &format!("idle.{metric}"),
                value,
                Value::Null,
                count,
                load,
            ));
        }
    }
    let targets = TARGETS
        .iter()
        .map(|target| {
            let (result, component) = match target.from {
                From::Latency => (latency.as_ref(), "editor-latency"),
                From::Measure => (measure.as_ref(), "measure"),
            };
            target.verdict(result, tier.timing(), load_of(entries, component))
        })
        .collect();
    (rows, targets)
}

fn markdown(header: &Value, entries: &[Entry], rows: &[Value], targets: &[Value]) -> String {
    let failed: Vec<_> = entries
        .iter()
        .filter(|e| e.status.failure())
        .map(|e| e.component.clone())
        .collect();
    let mut text = String::from("# Verification summary\n\n");
    let verdict = match header["refused"].as_str() {
        Some(why) => format!("REFUSED ({why})"),
        None if failed.is_empty() => "passed".to_owned(),
        None => format!("FAILED ({})", failed.join(", ")),
    };
    text.push_str(&format!(
        "Tier {}: {}. Host {}. Binary SHA-256 {} ({}). Cargo.lock SHA-256 {}. Total {} s. Output {}.\n\n",
        header["tier"].as_str().unwrap_or("?"),
        verdict,
        header["host"].as_str().unwrap_or("unknown"),
        header["binary_sha256"].as_str().unwrap_or("unavailable"),
        header["binary"].as_str().unwrap_or("unavailable"),
        header["lockfile_sha256"].as_str().unwrap_or("unavailable"),
        cell(&header["elapsed_s"]),
        header["output"].as_str().unwrap_or("."),
    ));
    if let Some(pool) = header["pool"].as_object() {
        text.push_str(&format!(
            "Rendered pool: {} {}, {} at a time, {} s of wall clock against {} s of scenario time. Each scenario is a separate process with its own output directory.\n\n",
            pool["scenarios"],
            if pool["scenarios"] == json!(1) { "scenario" } else { "scenarios" },
            pool["jobs"],
            cell(&pool["wall_clock_s"]),
            cell(&pool["serial_equivalent_s"]),
        ));
    }
    if let Some(threshold) = header["load_threshold"].as_f64() {
        let over: Vec<String> = entries
            .iter()
            .filter(|e| launch::unreliable(e.load))
            .map(|e| format!("{} ({})", e.component, number(e.load.unwrap_or_default())))
            .collect();
        text.push_str(&format!(
            "Timing load threshold {} (one-minute average, read at the start of each timing component). {}\n\n",
            number(threshold),
            if header["refused"].is_string() {
                "No timing component started.".to_owned()
            } else if over.is_empty() {
                "Every timing component started below it.".to_owned()
            } else {
                format!(
                    "Above it: {}. Every timing row and target verdict from {} is marked unreliable rather than pass or miss.",
                    over.join(", "),
                    if over.len() == 1 { "it" } else { "them" }
                )
            },
        ));
    }
    text.push_str("## Components\n\n| Component | Tier | Status | Started s | Elapsed s | Launches | Artifact | Error |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for entry in entries {
        text.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            entry.component,
            entry.tier,
            entry.status.name(),
            number(entry.started_at_s),
            number(entry.elapsed_s),
            entry.launches,
            entry.artifacts.last().cloned().unwrap_or_default(),
            entry.error.as_deref().map(escape).unwrap_or_default(),
        ));
    }
    if !rows.is_empty() {
        text.push_str("\n## Timing rows\n\n| Metric | Unit | p50 | p95 | Samples | Load 1m | Reliability | Source |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n");
        for r in rows {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r["metric"].as_str().unwrap_or_default(),
                r["unit"].as_str().unwrap_or_default(),
                cell(&r["p50"]),
                cell(&r["p95"]),
                r["count"],
                cell(&r["load_average_1m"]),
                r["reliability"].as_str().unwrap_or_default(),
                r["source"].as_str().unwrap_or_default(),
            ));
        }
    }
    if !targets.is_empty() {
        text.push_str("\n## Provisional targets\n\n| Target | Measured | Samples | Verdict | Source | Note |\n| --- | --- | --- | --- | --- | --- |\n");
        for t in targets {
            let measured = match t["measured"].as_f64() {
                Some(value) => format!(
                    "{} {}",
                    number(value),
                    t["unit"].as_str().unwrap_or_default()
                ),
                None => t["reason"].as_str().unwrap_or("not measured").to_owned(),
            };
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                t["target"].as_str().unwrap_or_default(),
                escape(&measured),
                t["samples"],
                t["verdict"].as_str().unwrap_or_default(),
                t["source"].as_str().unwrap_or_default(),
                escape(t["note"].as_str().unwrap_or_default()),
            ));
        }
    }
    text.push_str(
        "\nNo frame is opened by this command. Read a capture only for a failed scenario or a design review. A verdict from the default sample counts is a functional check, not a baseline, and a figure marked unreliable is not a baseline at all.\n",
    );
    text
}

/// Write both summaries and return the Markdown, so the caller can print exactly what it wrote.
fn write(
    out: &Path,
    header: &Value,
    entries: &[Entry],
    rows: &[Value],
    targets: &[Value],
) -> Result<String> {
    let text = markdown(header, entries, rows, targets);
    let mut summary = header.clone();
    summary["status"] = json!(if header["refused"].is_string() {
        "refused"
    } else if entries.iter().any(|e| e.status.failure()) {
        "failed"
    } else {
        "passed"
    });
    summary["failed"] = json!(
        entries
            .iter()
            .filter(|e| e.status.failure())
            .map(|e| e.component.clone())
            .collect::<Vec<_>>()
    );
    summary["components"] = json!(entries.iter().map(Entry::value).collect::<Vec<_>>());
    summary["timing_rows"] = json!(rows);
    summary["targets"] = json!(targets);
    write_json(&out.join("summary.json"), &summary)?;
    fs::write(out.join("summary.md"), &text)?;
    Ok(text)
}

/// Name every component that failed or timed out, so the exit status says what to look at.
fn outcome(entries: &[Entry], out: &Path) -> Result {
    let failed: Vec<_> = entries
        .iter()
        .filter(|e| e.status.failure())
        .map(|e| format!("{} ({})", e.component, e.status.name()))
        .collect();
    ensure(
        failed.is_empty(),
        format!(
            "Verification failed: {}. Summary: {}",
            failed.join(", "),
            out.join("summary.md").display()
        ),
    )
}

/// Everything a component needs to run, shared unchanged by the serial phases and the pool.
struct Ctx<'a> {
    root: &'a Path,
    out: &'a Path,
    xtask: PathBuf,
    bin: PathBuf,
    manifest: Option<PathBuf>,
    started: Instant,
}

/// The exact argument array one component runs with. One place, so a scenario started by the pool
/// and one started serially cannot differ, and so a test can check the whole plan's arrays at once.
fn arguments(s: &Spec, dir: &Path, bin: &Path, manifest: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = s.args.iter().map(OsString::from).collect();
    if s.output {
        args.extend(["--output".into(), dir.join("run").into_os_string()]);
    }
    if s.binary {
        args.extend(["--binary".into(), bin.to_path_buf().into_os_string()]);
    }
    if let (true, Some(path)) = (s.manifest, manifest) {
        args.extend(["--manifest".into(), path.to_path_buf().into_os_string()]);
    }
    args
}

/// Every component's own directory, which is what makes running two of them at once safe: the
/// output directory carries the component's catalog, evidence directory, captures and result file.
fn directories(specs: &[Spec], out: &Path) -> Vec<PathBuf> {
    specs.iter().map(|s| out.join(&s.name)).collect()
}

/// Run one component to completion and describe it. A component that cannot even be started is
/// recorded as that component's failure rather than aborting the run, so the summary still names
/// what went wrong, and so one scenario in the pool can never take the others down with it.
fn component(
    ctx: &Ctx,
    s: &Spec,
    started_at: f64,
    load: Option<f64>,
    env: Option<(&str, String)>,
) -> Entry {
    let dir = ctx.out.join(&s.name);
    let mut entry = Entry {
        component: s.name.clone(),
        tier: s.tier.into(),
        status: Status::NotRun,
        started_at_s: started_at,
        elapsed_s: 0.0,
        exit_code: None,
        error: None,
        artifacts: vec![s.name.clone()],
        launches: 0,
        load,
    };
    if let Err(error) = fs::create_dir_all(&dir) {
        entry.status = Status::Failed;
        entry.error = Some(error.to_string());
        return entry;
    }
    if let Some(why) = s.skip {
        entry.status = Status::Skipped;
        entry.error = Some(why.into());
        return entry;
    }
    entry.artifacts = vec![format!("{}/console.log", s.name)];
    if let Some(name) = s.result {
        entry.artifacts.push(format!("{}/run/{name}", s.name));
    }
    let clock = Instant::now();
    let ran = execute(
        &ctx.xtask,
        ctx.root,
        &arguments(s, &dir, &ctx.bin, ctx.manifest.as_deref()),
        &dir.join("console.log"),
        env,
    );
    entry.elapsed_s = clock.elapsed().as_secs_f64();
    match ran {
        Ok((code, timed_out)) => {
            let result = s
                .result
                .and_then(|name| optional(&dir.join("run").join(name)));
            entry.exit_code = code;
            entry.launches = s.launches.count(&dir, result.as_ref());
            entry.status = match (timed_out, code) {
                (true, _) => Status::TimedOut,
                (false, Some(0)) => Status::Passed,
                _ => Status::Failed,
            };
            if entry.status != Status::Passed {
                entry.error = reason(&dir, result.as_ref());
            }
        }
        Err(error) => {
            entry.status = Status::Failed;
            entry.error = Some(error.to_string());
        }
    }
    entry
}

/// The summary as it stands. Both files are rewritten after every component, from whichever thread
/// finished it, so a run that stops early still reports everything that finished.
struct Progress<'a> {
    out: &'a Path,
    tier: Tier,
    started: Instant,
    header: Value,
    entries: Vec<Entry>,
}

impl Progress<'_> {
    fn write(&mut self) -> Result<String> {
        self.header["elapsed_s"] = json!(tenths(self.started.elapsed().as_secs_f64()));
        if self.tier.timing() {
            self.header["unreliable_components"] = json!(
                self.entries
                    .iter()
                    .filter(|e| launch::unreliable(e.load))
                    .map(|e| e.component.clone())
                    .collect::<Vec<_>>()
            );
        }
        let (rows, targets) = collect(self.out, self.tier, &self.entries);
        write(self.out, &self.header, &self.entries, &rows, &targets)
    }
}

/// Run one block of scenarios through a bounded pool of `jobs` workers.
///
/// Every scenario is already a separate child process with its own output directory, catalog and
/// evidence directory, which is the whole reason they can overlap; that separation is asserted
/// before anything starts rather than assumed. Workers take the next scenario in list order, and
/// each result is stored at its own index, so the summary reads in list order however the
/// completions interleave. `jobs` of 1 is the serial run through the same path.
fn pool(
    ctx: &Ctx,
    specs: &[Spec],
    offset: usize,
    jobs: usize,
    progress: &Mutex<Progress>,
) -> Result {
    let next = AtomicUsize::new(0);
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let wall = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, specs.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(s) = specs.get(index) else { return };
                    let entry = component(ctx, s, ctx.started.elapsed().as_secs_f64(), None, None);
                    let mut state = progress.lock().expect("summary lock");
                    state.entries[offset + index] = entry;
                    if let Err(error) = state.write() {
                        failures
                            .lock()
                            .expect("failure lock")
                            .push(error.to_string());
                    }
                }
            });
        }
    });
    let elapsed = wall.elapsed().as_secs_f64();
    let mut state = progress.lock().expect("summary lock");
    let serial: f64 = (0..specs.len())
        .map(|index| state.entries[offset + index].elapsed_s)
        .sum();
    state.header["pool"] = json!({
        "jobs":jobs,
        "scenarios":specs.len(),
        "wall_clock_s":tenths(elapsed),
        "serial_equivalent_s":tenths(serial),
        "output_directories_distinct":true,
    });
    state.write()?;
    drop(state);
    let failures = failures.into_inner().expect("failure lock");
    ensure(
        failures.is_empty(),
        format!("The summary could not be written: {}", failures.join("; ")),
    )
}

/// Refuse a run outright, with the refusal in both summary files and in the exit error.
fn refuse(out: &Path, tier: Tier, specs: &[Spec], why: &str) -> Result {
    let entries: Vec<Entry> = specs
        .iter()
        .map(|s| Entry {
            component: s.name.clone(),
            tier: s.tier.into(),
            status: Status::NotRun,
            started_at_s: 0.0,
            elapsed_s: 0.0,
            exit_code: None,
            error: Some(why.to_owned()),
            artifacts: vec![s.name.clone()],
            launches: 0,
            load: None,
        })
        .collect();
    let header = json!({
        "format":1,
        "tier":tier.name(),
        "output":out,
        "elapsed_s":0.0,
        "refused":why,
        "load_threshold":launch::LOAD_THRESHOLD,
        "note":"Timing runs hold a host-wide lock so two of them can never overlap. Nothing was run.",
    });
    println!("{}", write(out, &header, &entries, &[], &[])?);
    Err(format!("Verification refused: {why}").into())
}

pub fn run(
    root: &Path,
    out: &Path,
    tier: Tier,
    selected_binary: Option<PathBuf>,
    manifest: Option<PathBuf>,
    jobs: usize,
) -> Result {
    ensure(!out.exists(), "Verification output must be new")?;
    ensure(
        (1..=16).contains(&jobs),
        "--jobs is 1 to 16 rendered scenarios at a time",
    )?;
    fs::create_dir_all(out)?;
    let started = Instant::now();

    let fixtures = GENERATED.iter().all(|p| root.join(p).is_file());
    let specs = plan(tier, manifest.is_some(), fixtures);

    // A timing tier that cannot have the host to itself is refused before it spends minutes on the
    // rest of the tier. The lock is still taken for real before the first timing component, because
    // another run can take it in between.
    if tier.timing()
        && let Some(pid) = launch::TimingGate::holder()?
    {
        return refuse(out, tier, &specs, &launch::TimingGate::refusal(pid));
    }

    // Build once up front so every component measures the same executable and no component's own
    // build time lands in its elapsed figure. Cargo's output goes to a file, never the terminal.
    let build_started = Instant::now();
    let log = fs::File::create(out.join("build.log"))?;
    let built = Command::new("cargo")
        .current_dir(root)
        .args([
            "build",
            "--release",
            "--locked",
            "--package",
            "lightwell-app",
            "--package",
            "xtask",
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()?;

    let release = binary(root)?;
    let xtask = release.with_file_name(format!("xtask{}", std::env::consts::EXE_SUFFIX));
    // Without `--binary` the built release executable is passed explicitly, so every component that
    // launches the editor measures the same file rather than whatever it would resolve itself.
    let bin = selected_binary
        .map(|path| absolute(root, &path))
        .unwrap_or_else(|| release.clone());
    let manifest = manifest.map(|path| absolute(root, &path));

    let header = json!({
        "format":1,
        "tier":tier.name(),
        "host":host(root).unwrap_or_else(|_| "unknown".into()),
        "binary":bin,
        "binary_sha256":hash(&bin).ok(),
        "lockfile_sha256":hash(&root.join("Cargo.lock"))?,
        "output":out,
        "manifest":manifest,
        "elapsed_s":0.0,
        "jobs":jobs,
        "load_threshold":tier.timing().then_some(launch::LOAD_THRESHOLD),
        "build":{"status":if built.success() {"passed"} else {"failed"},"elapsed_s":tenths(build_started.elapsed().as_secs_f64()),"log":"build.log"},
        "note":"Components run as child processes of the release xtask executable, with their console output in <component>/console.log. The rendered scenarios run through a bounded pool; every other component, and every timing component in particular, runs serially. A skip is not a pass.",
    });

    let mut entries: Vec<Entry> = specs
        .iter()
        .map(|s| Entry {
            component: s.name.clone(),
            tier: s.tier.into(),
            status: Status::NotRun,
            started_at_s: 0.0,
            elapsed_s: 0.0,
            exit_code: None,
            error: None,
            artifacts: vec![s.name.clone()],
            launches: 0,
            load: None,
        })
        .collect();

    if !built.success() {
        for entry in &mut entries {
            entry.error = Some("the release build failed; see build.log".into());
        }
        let (rows, targets) = collect(out, tier, &entries);
        println!("{}", write(out, &header, &entries, &rows, &targets)?);
        return Err("Verification failed: the release build failed. See build.log".into());
    }

    // Two components writing into one directory would make the pool unsafe and every result
    // ambiguous. Checked here, over the plan that is about to run, rather than trusted to the names.
    let dirs = directories(&specs, out);
    let mut distinct = dirs.clone();
    distinct.sort();
    distinct.dedup();
    ensure(
        distinct.len() == dirs.len(),
        "Two components would share an output directory",
    )?;

    let ctx = Ctx {
        root,
        out,
        xtask,
        bin,
        manifest,
        started,
    };
    let progress = Mutex::new(Progress {
        out,
        tier,
        started,
        header,
        entries,
    });
    // The host-wide timing lock, held from the first timing component to the end of the run and
    // released by its own drop, including on failure.
    let mut gate: Option<launch::TimingGate> = None;

    let mut index = 0;
    while index < specs.len() {
        // The rendered scenarios are the one block that overlaps; everything else, and every timing
        // component in particular, runs alone.
        if specs[index].tier == "rendered" {
            let end = index
                + specs[index..]
                    .iter()
                    .take_while(|s| s.tier == "rendered")
                    .count();
            pool(&ctx, &specs[index..end], index, jobs, &progress)?;
            index = end;
            continue;
        }
        let s = &specs[index];
        if s.tier == "timing" && gate.is_none() {
            match launch::TimingGate::take()? {
                Ok(taken) => gate = Some(taken),
                Err(pid) => {
                    let why = launch::TimingGate::refusal(pid);
                    let mut state = progress.lock().expect("summary lock");
                    state.header["refused"] = json!(why);
                    for entry in state.entries.iter_mut().filter(|e| e.tier == "timing") {
                        entry.error = Some(why.clone());
                    }
                    println!("{}", state.write()?);
                    return Err(format!("Verification refused: {why}").into());
                }
            }
        }
        let load = (s.tier == "timing").then(|| load_average(root)).flatten();
        let env = (s.tier == "timing")
            .then(|| gate.as_ref().map(launch::TimingGate::child_env))
            .flatten();
        let at = started.elapsed().as_secs_f64();
        let entry = component(&ctx, s, at, load, env);
        let status = entry.status;
        let mut state = progress.lock().expect("summary lock");
        state.entries[index] = entry;
        state.write()?;
        drop(state);
        // A prerequisite that fails leaves the rest unprovable, so the remaining components stay
        // `not_run` rather than failing for a reason that is already recorded.
        if status != Status::Passed && s.tier == "setup" {
            break;
        }
        index += 1;
    }

    let mut state = progress.lock().expect("summary lock");
    println!("{}", state.write()?);
    outcome(&state.entries, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn names(tier: Tier, manifest: bool, fixtures: bool) -> Vec<String> {
        plan(tier, manifest, fixtures)
            .into_iter()
            .map(|s| s.name)
            .collect()
    }
    #[test]
    fn tiers_compose_in_order() {
        assert_eq!(
            names(Tier::Quick, false, true),
            ["check", "editor-acceptance"]
        );
        let rendered = names(Tier::Rendered, false, true);
        assert_eq!(&rendered[..2], ["check", "editor-acceptance"]);
        assert_eq!(rendered.len(), 2 + smoke::SCENARIOS.len());
        assert_eq!(rendered[2], "smoke-empty");
        assert_eq!(rendered.last().unwrap(), "smoke-unavailable");
        assert_eq!(
            names(Tier::Timing, false, true),
            [
                "check",
                "editor-acceptance",
                "editor-performance",
                "editor-latency",
                "measure"
            ]
        );
        let full = names(Tier::Full, true, true);
        assert_eq!(&full[..2], ["check", "editor-acceptance"]);
        // Every rendered scenario, then the RAW components, then the timing components last.
        assert_eq!(
            &full[full.len() - 5..],
            [
                "raw-reference",
                "raw-editor",
                "editor-performance",
                "editor-latency",
                "measure"
            ]
        );
        assert_eq!(full.len(), 2 + smoke::SCENARIOS.len() + 5);
        // Missing generated fixtures are produced first, and only where a tier needs them.
        assert_eq!(names(Tier::Quick, false, false)[0], "check");
        assert_eq!(names(Tier::Rendered, false, false)[0], "generate-fixtures");
        assert_eq!(names(Tier::Timing, false, false)[0], "generate-fixtures");
    }
    #[test]
    fn a_missing_manifest_skips_raw_editor_instead_of_passing_it() {
        let without = plan(Tier::Full, false, true);
        let raw = without.iter().find(|s| s.name == "raw-editor").unwrap();
        assert_eq!(raw.skip, Some("no --manifest"));
        let with = plan(Tier::Full, true, true);
        assert!(
            with.iter()
                .find(|s| s.name == "raw-editor")
                .unwrap()
                .skip
                .is_none()
        );
    }
    fn entry(component: &str, status: Status) -> Entry {
        Entry {
            component: component.into(),
            tier: "rendered".into(),
            status,
            started_at_s: 0.5,
            elapsed_s: 12.25,
            exit_code: (status == Status::Failed).then_some(1),
            error: (status != Status::Passed).then(|| "Application exit code 1".into()),
            artifacts: vec![format!("{component}/run/result.json")],
            launches: 1,
            load: Some(3.5),
        }
    }
    #[test]
    fn the_table_shows_every_status_and_its_artifact() {
        let entries = [
            entry("smoke-load", Status::Passed),
            entry("smoke-crop", Status::Failed),
            entry("raw-editor", Status::Skipped),
            entry("measure", Status::NotRun),
            entry("editor-latency", Status::TimedOut),
        ];
        let header = json!({"tier":"full","host":"aarch64-apple-darwin","binary":"/tmp/lightwell","binary_sha256":"abc","lockfile_sha256":"def","output":"/tmp/out","elapsed_s":61.5});
        let text = markdown(&header, &entries, &[], &[]);
        assert!(text.contains("Tier full: FAILED (smoke-crop, editor-latency)."));
        assert!(text.contains("Host aarch64-apple-darwin"));
        assert!(text.contains("Total 61.50 s"));
        assert!(text.contains(
            "| smoke-load | rendered | passed | 0.500 | 12.25 | 1 | smoke-load/run/result.json |  |"
        ));
        assert!(text.contains("| smoke-crop | rendered | failed | 0.500 | 12.25 | 1 | smoke-crop/run/result.json | Application exit code 1 |"));
        assert!(text.contains("| raw-editor | rendered | skipped |"));
        assert!(text.contains("| measure | rendered | not_run |"));
        assert!(text.contains("| editor-latency | rendered | timed_out |"));
        // A skip is never shown as a pass.
        assert!(!text.contains("| raw-editor | rendered | passed"));
    }
    #[test]
    fn a_pipe_in_a_failure_cannot_break_the_table() {
        let mut broken = entry("smoke-load", Status::Failed);
        broken.error = Some("a | b\nc".into());
        let text = markdown(&json!({"tier":"rendered"}), &[broken], &[], &[]);
        let line = text
            .lines()
            .find(|l| l.starts_with("| smoke-load"))
            .unwrap();
        assert!(line.contains("a \\| b c"), "{line}");
        assert_eq!(line.matches(" | ").count(), 7);
    }
    #[test]
    fn target_verdicts_follow_the_measured_figure() {
        let latency = json!({
            "timings_ms":{
                "input_to_presented_frame":{"count":30,"p50_ms":74.8,"p95_ms":83.4},
                "final_input_to_settled_histogram":{"count":30,"p50_ms":99.7,"p95_ms":250.0},
            },
            "resources":{"scratch":{"peak_bytes":14_116_000}},
        });
        let verdicts: Vec<Value> = TARGETS
            .iter()
            .map(|t| {
                t.verdict(
                    matches!(t.from, From::Latency).then_some(&latency),
                    true,
                    None,
                )
            })
            .collect();
        assert_eq!(verdicts[0]["verdict"], "pass");
        assert_eq!(verdicts[0]["measured"], 83.4);
        assert_eq!(verdicts[0]["samples"], 30);
        assert_eq!(verdicts[1]["verdict"], "miss");
        assert_eq!(verdicts[2]["verdict"], "pass");
        assert!(verdicts[2]["measured"].as_f64().unwrap() < 64.0);
        // The measure file was never written, so its targets are unmeasured, never passes.
        assert_eq!(verdicts[3]["verdict"], "not_measured");
        assert_eq!(
            verdicts[3]["reason"],
            "measure/run/measurements.json was not written"
        );
        // A file that exists but holds no such path is also unmeasured, with the path named.
        let empty = json!({"timings_ms":{}});
        let missing = TARGETS[0].verdict(Some(&empty), true, None);
        assert_eq!(missing["verdict"], "not_measured");
        assert!(
            missing["reason"]
                .as_str()
                .unwrap()
                .contains("/timings_ms/input_to_presented_frame/p95_ms")
        );
        // Outside the timing tier the reason is the tier, not a missing file.
        assert_eq!(
            TARGETS[0].verdict(None, false, None)["reason"],
            "timing tier did not run"
        );
    }
    #[test]
    fn measure_targets_read_their_own_summary_and_sample_counts() {
        let measure = json!({
            "runs":[
                {"workload":"24mp","sampled_peak_rss_mib":500.0,"open_to_raster_ms":[100.0,110.0]},
                {"workload":"24mp","sampled_peak_rss_mib":520.0,"open_to_raster_ms":[120.0]},
                {"workload":"60mp","sampled_peak_rss_mib":975.0},
            ],
            "summary":{
                "24mp":{"sampled_peak_rss_mib":{"median":510.0},"open_to_raster_ms":{"p95":120.0}},
                "60mp":{"sampled_peak_rss_mib":{"median":1100.0}},
                "empty":{"launch_to_observed_frame_ms":{"p95":1200.0}},
            },
            "idle":{"cpu_percent_one_core":0.93},
        });
        let verdict = |index: usize| TARGETS[index].verdict(Some(&measure), true, Some(2.5));
        assert_eq!(verdict(3)["verdict"], "pass");
        assert_eq!(verdict(3)["samples"], 2);
        assert_eq!(verdict(3)["load_average_1m"], 2.5);
        assert_eq!(verdict(4)["verdict"], "miss");
        assert_eq!(verdict(5)["verdict"], "pass");
        assert_eq!(verdict(6)["verdict"], "miss");
        assert_eq!(verdict(7)["verdict"], "pass");
        assert_eq!(verdict(7)["samples"], 3);
    }
    #[test]
    fn launch_counts_come_from_what_each_component_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(Launches::None.count(dir, Some(&json!({}))), 0);
        assert_eq!(Launches::Smoke.count(dir, None), 0);
        assert_eq!(Launches::Smoke.count(dir, Some(&json!({"exit_code":0}))), 1);
        assert_eq!(
            Launches::Smoke.count(
                dir,
                Some(&json!({"launch1_exit_code":0,"launch2_exit_code":0}))
            ),
            2
        );
        let measure = json!({"runs":[{},{},{}],"idle":{"duration_s":30.0}});
        assert_eq!(Launches::Measure.count(dir, Some(&measure)), 4);
        assert_eq!(
            Launches::Measure.count(dir, Some(&json!({"runs":[{},{}]}))),
            2
        );
        assert_eq!(
            Launches::RawEditor.count(dir, Some(&json!({"runs":[{},{},{}]}))),
            6
        );
        assert_eq!(Launches::Latency.count(dir, Some(&json!({}))), 1);
        fs::create_dir_all(dir.join("run")).unwrap();
        fs::write(dir.join("run/resources.json"), "{}").unwrap();
        assert_eq!(Launches::Latency.count(dir, Some(&json!({}))), 3);
    }
    #[test]
    fn any_failure_names_its_components_in_the_exit_error() {
        let out = Path::new("/tmp/verify");
        assert!(
            outcome(
                &[
                    entry("check", Status::Passed),
                    entry("raw-editor", Status::Skipped),
                    entry("measure", Status::NotRun)
                ],
                out
            )
            .is_ok()
        );
        let error = outcome(
            &[
                entry("smoke-crop", Status::Failed),
                entry("check", Status::Passed),
                entry("measure", Status::TimedOut),
            ],
            out,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("smoke-crop (failed)"), "{error}");
        assert!(error.contains("measure (timed_out)"), "{error}");
        assert!(error.contains("summary.md"), "{error}");
    }
    #[test]
    fn an_existing_output_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            run(&root().unwrap(), tmp.path(), Tier::Quick, None, None, JOBS)
                .unwrap_err()
                .to_string()
                .contains("must be new")
        );
    }
    #[test]
    fn a_rows_unit_comes_from_the_metric_the_runner_named() {
        assert_eq!(unit("input_to_presented_frame"), "ms");
        assert_eq!(unit("24mp.open_to_raster_ms"), "ms");
        assert_eq!(unit("24mp.sampled_peak_rss_mib"), "MiB");
        assert_eq!(unit("idle.rss_mib_peak"), "MiB");
        assert_eq!(unit("idle.cpu_percent_one_core"), "% of one core");
        assert_eq!(unit("idle.duration_s"), "s");
    }
    /// A block of "scenarios" that are nothing but sleeps, so the pool's ordering and placement can
    /// be tested without launching an editor. The first one outlasts all the others, so completion
    /// order cannot be list order.
    fn sleeps(seconds: &[&str]) -> Vec<Spec> {
        seconds
            .iter()
            .enumerate()
            .map(|(index, duration)| Spec {
                output: false,
                ..spec(&format!("smoke-{index}"), "rendered", &[duration])
            })
            .collect()
    }

    fn pooled(specs: &[Spec], jobs: usize, out: &Path) -> Vec<Entry> {
        let started = Instant::now();
        let ctx = Ctx {
            root: out,
            out,
            xtask: "/bin/sleep".into(),
            bin: PathBuf::new(),
            manifest: None,
            started,
        };
        let entries = specs
            .iter()
            .map(|s| Entry {
                component: s.name.clone(),
                tier: s.tier.into(),
                status: Status::NotRun,
                started_at_s: 0.0,
                elapsed_s: 0.0,
                exit_code: None,
                error: None,
                artifacts: vec![s.name.clone()],
                launches: 0,
                load: None,
            })
            .collect();
        let progress = Mutex::new(Progress {
            out,
            tier: Tier::Rendered,
            started,
            header: json!({"tier":"rendered","output":out}),
            entries,
        });
        pool(&ctx, specs, 0, jobs, &progress).unwrap();
        progress.into_inner().unwrap().entries
    }

    #[test]
    fn the_pool_overlaps_scenarios_but_keeps_every_result_at_its_own_index() {
        let specs = sleeps(&["0.8", "0.1", "0.1", "0.1", "0.1", "0.1"]);
        let tmp = tempfile::tempdir().unwrap();
        let entries = pooled(&specs, 3, tmp.path());

        // Every result is at its own index, in list order, however the completions interleaved.
        assert_eq!(
            entries
                .iter()
                .map(|e| e.component.as_str())
                .collect::<Vec<_>>(),
            [
                "smoke-0", "smoke-1", "smoke-2", "smoke-3", "smoke-4", "smoke-5"
            ]
        );
        assert!(
            entries.iter().all(|e| e.status == Status::Passed),
            "{:?}",
            entries
                .iter()
                .map(|e| (e.component.clone(), e.status))
                .collect::<Vec<_>>()
        );
        // The summary table is written in list order too, not completion order.
        let text = fs::read_to_string(tmp.path().join("summary.md")).unwrap();
        let listed: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("| smoke-"))
            .filter_map(|l| l.split(' ').next())
            .collect();
        assert_eq!(listed, ["0", "1", "2", "3", "4", "5"]);

        // The long first scenario really did overlap the ones after it.
        let first_ends = entries[0].started_at_s + entries[0].elapsed_s;
        assert!(
            entries[1..].iter().any(|e| e.started_at_s < first_ends),
            "nothing overlapped the first scenario"
        );
        let summary: Value = read_json(&tmp.path().join("summary.json")).unwrap();
        let wall = summary["pool"]["wall_clock_s"].as_f64().unwrap();
        let serial = summary["pool"]["serial_equivalent_s"].as_f64().unwrap();
        assert_eq!(summary["pool"]["jobs"], 3);
        assert_eq!(summary["pool"]["scenarios"], 6);
        assert!(wall < serial, "pool {wall} s against {serial} s serial");
    }

    #[test]
    fn one_job_is_the_serial_run_through_the_same_path() {
        let specs = sleeps(&["0.2", "0.2", "0.2"]);
        let tmp = tempfile::tempdir().unwrap();
        let entries = pooled(&specs, 1, tmp.path());
        assert!(entries.iter().all(|e| e.status == Status::Passed));
        // Each scenario starts only after the one before it has finished.
        for pair in entries.windows(2) {
            assert!(
                pair[1].started_at_s >= pair[0].started_at_s + pair[0].elapsed_s,
                "{} overlapped {}",
                pair[1].component,
                pair[0].component
            );
        }
        let summary: Value = read_json(&tmp.path().join("summary.json")).unwrap();
        assert_eq!(summary["pool"]["jobs"], 1);
    }

    #[test]
    fn no_two_components_share_an_output_directory() {
        let out = Path::new("/tmp/verify");
        let bin = Path::new("/tmp/lightwell");
        let manifest = PathBuf::from("/tmp/raw.json");
        let specs = plan(Tier::Full, true, true);

        let dirs = directories(&specs, out);
        let mut distinct = dirs.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), specs.len(), "{dirs:?}");

        // The same holds of what each component is actually told on its command line: the argument
        // arrays, not just the directory names.
        let mut outputs = Vec::new();
        for (s, dir) in specs.iter().zip(&dirs) {
            let args = arguments(s, dir, bin, Some(&manifest));
            if let Some(index) = args.iter().position(|a| a == "--output") {
                outputs.push(args[index + 1].clone());
            } else {
                assert!(!s.output, "{} takes --output", s.name);
            }
            assert_eq!(
                args.iter().filter(|a| *a == "--binary").count(),
                usize::from(s.binary)
            );
            assert_eq!(
                args.iter().filter(|a| *a == "--manifest").count(),
                usize::from(s.manifest)
            );
        }
        let mut distinct_outputs = outputs.clone();
        distinct_outputs.sort();
        distinct_outputs.dedup();
        assert_eq!(distinct_outputs.len(), outputs.len(), "{outputs:?}");
        // Every rendered scenario is one of them, each under its own directory.
        assert_eq!(
            outputs
                .iter()
                .filter(|o| o.to_string_lossy().contains("/smoke-"))
                .count(),
            smoke::SCENARIOS.len()
        );
    }

    #[test]
    fn load_over_the_threshold_marks_rows_and_verdicts_unreliable() {
        let quiet = row(
            "measure/run/measurements.json",
            "24mp.open_to_raster_ms",
            json!(120.0),
            json!(140.0),
            5,
            Some(5.7),
        );
        assert_eq!(quiet["reliability"], "ok");
        assert_eq!(quiet["load_threshold"], 8.0);
        let busy = row(
            "measure/run/measurements.json",
            "24mp.open_to_raster_ms",
            json!(120.0),
            json!(140.0),
            5,
            Some(19.4),
        );
        assert_eq!(busy["reliability"], "unreliable");
        assert_eq!(busy["load_average_1m"], 19.4);
        assert_eq!(busy["load_threshold"], 8.0);

        // The same figure is a pass below the threshold and neither a pass nor a miss above it.
        let latency = json!({"timings_ms":{"input_to_presented_frame":{"count":30,"p50_ms":74.8,"p95_ms":83.4}}});
        let below = TARGETS[0].verdict(Some(&latency), true, Some(5.7));
        assert_eq!(below["verdict"], "pass");
        assert_eq!(below["reason"], Value::Null);
        let above = TARGETS[0].verdict(Some(&latency), true, Some(19.4));
        assert_eq!(above["verdict"], "unreliable");
        assert_eq!(above["measured"], 83.4);
        assert_eq!(above["samples"], 30);
        assert!(
            above["reason"].as_str().unwrap().contains("19.40")
                && above["reason"].as_str().unwrap().contains("8.00"),
            "{}",
            above["reason"]
        );
        // A miss is withheld the same way.
        let missing = json!({"timings_ms":{"input_to_presented_frame":{"count":30,"p50_ms":180.0,"p95_ms":220.0}}});
        assert_eq!(
            TARGETS[0].verdict(Some(&missing), true, Some(5.7))["verdict"],
            "miss"
        );
        assert_eq!(
            TARGETS[0].verdict(Some(&missing), true, Some(8.01))["verdict"],
            "unreliable"
        );
        // Exactly at the threshold is still a verdict.
        assert_eq!(
            TARGETS[0].verdict(Some(&missing), true, Some(8.0))["verdict"],
            "miss"
        );
        // And the threshold is recorded even where nothing was measured.
        assert_eq!(TARGETS[0].verdict(None, true, None)["load_threshold"], 8.0);
    }

    #[test]
    fn the_summary_states_the_threshold_and_names_what_exceeded_it() {
        let mut busy = entry("measure", Status::Passed);
        busy.tier = "timing".into();
        busy.load = Some(19.4);
        let header = json!({"tier":"timing","load_threshold":launch::LOAD_THRESHOLD});
        let text = markdown(&header, &[busy], &[], &[]);
        assert!(text.contains("Timing load threshold 8.00"), "{text}");
        assert!(text.contains("Above it: measure (19.40)"), "{text}");
        assert!(
            text.contains("marked unreliable rather than pass or miss"),
            "{text}"
        );

        let mut quiet = entry("measure", Status::Passed);
        quiet.tier = "timing".into();
        quiet.load = Some(4.1);
        let calm = markdown(&header, &[quiet], &[], &[]);
        assert!(
            calm.contains("Every timing component started below it."),
            "{calm}"
        );

        // A refusal says so instead of reporting a pass, and the summary status follows.
        let tmp = tempfile::tempdir().unwrap();
        let refused = json!({"tier":"timing","load_threshold":launch::LOAD_THRESHOLD,"refused":launch::TimingGate::refusal(4242)});
        let entries = [entry("measure", Status::NotRun)];
        let text = write(tmp.path(), &refused, &entries, &[], &[]).unwrap();
        assert!(
            text.contains("REFUSED (another timing run (pid 4242) is alive)"),
            "{text}"
        );
        assert!(text.contains("No timing component started."), "{text}");
        let summary: Value = read_json(&tmp.path().join("summary.json")).unwrap();
        assert_eq!(summary["status"], "refused");
        assert_eq!(summary["refused"], "another timing run (pid 4242) is alive");
    }

    #[test]
    fn the_pool_block_reports_what_it_ran() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = pooled(&sleeps(&["0.05"]), 3, tmp.path());
        assert_eq!(entries.len(), 1);
        let summary: Value = read_json(&tmp.path().join("summary.json")).unwrap();
        assert_eq!(summary["pool"]["output_directories_distinct"], true);
        let text = fs::read_to_string(tmp.path().join("summary.md")).unwrap();
        assert!(
            text.contains("Rendered pool: 1 scenario, 3 at a time"),
            "{text}"
        );
    }

    #[test]
    fn tier_names_round_trip_and_reject_anything_else() {
        for tier in [Tier::Quick, Tier::Rendered, Tier::Timing, Tier::Full] {
            assert_eq!(Tier::parse(tier.name()).unwrap(), tier);
        }
        assert!(Tier::parse("fast").is_err());
    }
}
