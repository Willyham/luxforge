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
    time::{Duration, Instant},
};

/// How long any one component may run before it is killed and reaped. Every component already
/// bounds its own editor launches, so this only catches a component that has stopped making
/// progress. A killed child cannot run its own cleanup guards, so a component recorded as
/// `timed_out` may have left an editor process behind: that is a failure to investigate, never a
/// normal outcome.
const DEADLINE: Duration = Duration::from_secs(20 * 60);

/// The JPEG workloads the rendered and timing tiers need before they can run.
const GENERATED: [&str; 2] = ["fixtures/generated/24mp.jpg", "fixtures/generated/60mp.jpg"];

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

/// What each tier runs, in order. Every tier includes the ones below it. The timing components run
/// strictly serially, in this order, after everything else in the tier, so nothing else on the
/// machine is competing with them from this command.
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

/// One component's outcome, as the summary records it.
struct Entry {
    component: String,
    tier: String,
    status: Status,
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
            "elapsed_s":(self.elapsed_s*10.0).round()/10.0,
            "exit_code":self.exit_code,
            "error":self.error,
            "artifacts":self.artifacts,
            "launches":self.launches,
            "load_average_1m":self.load,
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
) -> Result<(Option<i32>, bool)> {
    let file = fs::File::create(log)?;
    let mut child = Command::new(program)
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

fn row(
    source: &str,
    metric: &str,
    p50: Value,
    p95: Value,
    count: usize,
    load: Option<f64>,
) -> Value {
    json!({"metric":metric,"unit":unit(metric),"p50":p50,"p95":p95,"count":count,"source":source,"load_average_1m":load})
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
            return json!({"target":self.text,"unit":self.unit,"limit":self.limit,"source":format!("{} {}",self.file(),self.path),"measured":Value::Null,"samples":0,"verdict":"not_measured","reason":why,"note":self.note,"load_average_1m":load});
        };
        let Some(raw) = result.pointer(self.path).and_then(Value::as_f64) else {
            return json!({"target":self.text,"unit":self.unit,"limit":self.limit,"source":format!("{} {}",self.file(),self.path),"measured":Value::Null,"samples":0,"verdict":"not_measured","reason":format!("{} holds no {}",self.file(),self.path),"note":self.note,"load_average_1m":load});
        };
        let value = raw * self.scale;
        let ok = if self.strict {
            value < self.limit
        } else {
            value <= self.limit
        };
        json!({"target":self.text,"unit":self.unit,"limit":self.limit,"source":format!("{} {}",self.file(),self.path),"measured":value,"samples":self.samples(result),"verdict":if ok {"pass"} else {"miss"},"note":self.note,"load_average_1m":load})
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
    text.push_str(&format!(
        "Tier {}: {}. Host {}. Binary SHA-256 {} ({}). Cargo.lock SHA-256 {}. Total {} s. Output {}.\n\n",
        header["tier"].as_str().unwrap_or("?"),
        if failed.is_empty() { "passed".to_owned() } else { format!("FAILED ({})", failed.join(", ")) },
        header["host"].as_str().unwrap_or("unknown"),
        header["binary_sha256"].as_str().unwrap_or("unavailable"),
        header["binary"].as_str().unwrap_or("unavailable"),
        header["lockfile_sha256"].as_str().unwrap_or("unavailable"),
        cell(&header["elapsed_s"]),
        header["output"].as_str().unwrap_or("."),
    ));
    text.push_str("## Components\n\n| Component | Tier | Status | Elapsed s | Launches | Artifact | Error |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    for entry in entries {
        text.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            entry.component,
            entry.tier,
            entry.status.name(),
            number(entry.elapsed_s),
            entry.launches,
            entry.artifacts.last().cloned().unwrap_or_default(),
            entry.error.as_deref().map(escape).unwrap_or_default(),
        ));
    }
    if !rows.is_empty() {
        text.push_str("\n## Timing rows\n\n| Metric | Unit | p50 | p95 | Samples | Load 1m | Source |\n| --- | --- | --- | --- | --- | --- | --- |\n");
        for r in rows {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                r["metric"].as_str().unwrap_or_default(),
                r["unit"].as_str().unwrap_or_default(),
                cell(&r["p50"]),
                cell(&r["p95"]),
                r["count"],
                cell(&r["load_average_1m"]),
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
        "\nNo frame is opened by this command. Read a capture only for a failed scenario or a design review. A verdict from the default sample counts is a functional check, not a baseline.\n",
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
    summary["status"] = json!(if entries.iter().any(|e| e.status.failure()) {
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

pub fn run(
    root: &Path,
    out: &Path,
    tier: Tier,
    selected_binary: Option<PathBuf>,
    manifest: Option<PathBuf>,
) -> Result {
    ensure(!out.exists(), "Verification output must be new")?;
    fs::create_dir_all(out)?;
    let started = Instant::now();

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

    let mut header = json!({
        "format":1,
        "tier":tier.name(),
        "host":host(root).unwrap_or_else(|_| "unknown".into()),
        "binary":bin,
        "binary_sha256":hash(&bin).ok(),
        "lockfile_sha256":hash(&root.join("Cargo.lock"))?,
        "output":out,
        "manifest":manifest,
        "elapsed_s":0.0,
        "build":{"status":if built.success() {"passed"} else {"failed"},"elapsed_s":(build_started.elapsed().as_secs_f64()*10.0).round()/10.0,"log":"build.log"},
        "note":"Components run as child processes of the release xtask executable, serially, with their console output in <component>/console.log. A skip is not a pass.",
    });

    let fixtures = GENERATED.iter().all(|p| root.join(p).is_file());
    let specs = plan(tier, manifest.is_some(), fixtures);
    let mut entries: Vec<Entry> = specs
        .iter()
        .map(|s| Entry {
            component: s.name.clone(),
            tier: s.tier.into(),
            status: Status::NotRun,
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

    for (index, s) in specs.iter().enumerate() {
        let dir = out.join(&s.name);
        fs::create_dir_all(&dir)?;
        if let Some(why) = s.skip {
            entries[index].status = Status::Skipped;
            entries[index].error = Some(why.into());
            let (rows, targets) = collect(out, tier, &entries);
            header["elapsed_s"] = json!((started.elapsed().as_secs_f64() * 10.0).round() / 10.0);
            write(out, &header, &entries, &rows, &targets)?;
            continue;
        }
        let mut args: Vec<OsString> = s.args.iter().map(OsString::from).collect();
        if s.output {
            args.extend(["--output".into(), dir.join("run").into_os_string()]);
        }
        if s.binary {
            args.extend(["--binary".into(), bin.clone().into_os_string()]);
        }
        if let (true, Some(path)) = (s.manifest, &manifest) {
            args.extend(["--manifest".into(), path.clone().into_os_string()]);
        }
        if s.tier == "timing" {
            entries[index].load = load_average(root);
        }
        let component = Instant::now();
        let (code, timed_out) = execute(&xtask, root, &args, &dir.join("console.log"))?;
        let result = s
            .result
            .and_then(|name| optional(&dir.join("run").join(name)));
        let entry = &mut entries[index];
        entry.elapsed_s = component.elapsed().as_secs_f64();
        entry.exit_code = code;
        entry.launches = s.launches.count(&dir, result.as_ref());
        entry.artifacts = vec![format!("{}/console.log", s.name)];
        if let Some(name) = s.result {
            entry.artifacts.push(format!("{}/run/{name}", s.name));
        }
        entry.status = match (timed_out, code) {
            (true, _) => Status::TimedOut,
            (false, Some(0)) => Status::Passed,
            _ => Status::Failed,
        };
        if entry.status != Status::Passed {
            entry.error = reason(&dir, result.as_ref());
        }
        let status = entry.status;
        header["elapsed_s"] = json!((started.elapsed().as_secs_f64() * 10.0).round() / 10.0);
        let (rows, targets) = collect(out, tier, &entries);
        write(out, &header, &entries, &rows, &targets)?;
        // A prerequisite that fails leaves the rest unprovable, so the remaining components stay
        // `not_run` rather than failing for a reason that is already recorded.
        if status != Status::Passed && s.tier == "setup" {
            break;
        }
    }

    header["elapsed_s"] = json!((started.elapsed().as_secs_f64() * 10.0).round() / 10.0);
    let (rows, targets) = collect(out, tier, &entries);
    println!("{}", write(out, &header, &entries, &rows, &targets)?);
    outcome(&entries, out)
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
            "| smoke-load | rendered | passed | 12.25 | 1 | smoke-load/run/result.json |  |"
        ));
        assert!(text.contains("| smoke-crop | rendered | failed | 12.25 | 1 | smoke-crop/run/result.json | Application exit code 1 |"));
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
        assert_eq!(line.matches(" | ").count(), 6);
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
            run(&root().unwrap(), tmp.path(), Tier::Quick, None, None)
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
    #[test]
    fn tier_names_round_trip_and_reject_anything_else() {
        for tier in [Tier::Quick, Tier::Rendered, Tier::Timing, Tier::Full] {
            assert_eq!(Tier::parse(tier.name()).unwrap(), tier);
        }
        assert!(Tier::parse("fast").is_err());
    }
}
