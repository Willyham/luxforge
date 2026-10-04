//! One command for a whole verification tier.
//!
//! Every component is one of the harness's own commands, run as a child process of the release
//! `xtask` executable (in the quick tier, of the one running `verify`, since quick builds nothing in
//! release) with its console output redirected to a file. Child processes rather than
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

/// The release gate's bound: `gpu-qualification` renders every stack of the corpus on the reference
/// renderer and on the GPU at four views, which takes far longer than any other component, so it
/// alone is killed only after three hours.
const GATE_DEADLINE: Duration = Duration::from_secs(3 * 60 * 60);

/// The 24 MP generated workload's path, relative to the repository root, that the timing
/// components read as their `--source`. `fixtures::TABLE` is the one table of what `generate`
/// writes and what a directory needs to hold; this just names its first entry's location so the
/// path is built from the same table rather than repeated as a second literal.
fn twenty_four_mp() -> String {
    format!("fixtures/generated/{}", fixtures::TABLE[0].file)
}

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
    /// The component ran and exited with [`INCOMPLETE_EXIT_CODE`]: nothing it ran failed, but it
    /// could not run all it lists, as the release gate cannot without a source or an adapter.
    Incomplete,
    TimedOut,
    NotRun,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Incomplete => "incomplete",
            Self::TimedOut => "timed_out",
            Self::NotRun => "not_run",
        }
    }
    /// A skip and a component that never ran are both unproven; only `passed` is a pass.
    fn failure(self) -> bool {
        matches!(self, Self::Failed | Self::TimedOut)
    }
}

/// How many editor processes a component started, from its own `run/result.json`: a run of the
/// scenario library's launch envelope — every smoke scenario, `editor-latency`, `measure` and
/// `hardening` — lists every editor process it started under `launches`, so a run that stopped
/// early reports the launches it actually made. A component that starts no editor (`check`, the
/// core acceptance journey, core timing diagnostics, fixture generation and `raw-authentic`'s
/// `cargo test` processes) keeps no such list, and counts none.
fn launches(result: Option<&Value>) -> u64 {
    result.map_or(0, |result| {
        result["launches"].as_array().map_or(0, Vec::len) as u64
    })
}

/// One planned component: the arguments it gets and where its result lives.
struct Spec {
    name: String,
    tier: &'static str,
    args: Vec<String>,
    /// The result file inside the component's own `run/` directory, when it writes one.
    result: Option<&'static str>,
    /// Whether the component takes `--output`, `--binary` and `--manifest`.
    output: bool,
    binary: bool,
    manifest: bool,
    /// Why this component cannot run, when it cannot.
    skip: Option<&'static str>,
    /// How long it may run before it is killed and recorded as `timed_out`.
    deadline: Duration,
}

fn spec(name: &str, tier: &'static str, args: &[&str]) -> Spec {
    Spec {
        name: name.into(),
        tier,
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        result: None,
        output: true,
        binary: false,
        manifest: false,
        skip: None,
        deadline: DEADLINE,
    }
}

/// One `cargo test` invocation `raw-authentic` runs: the package and integration-test binary that
/// hold the `#[ignore]`d authentic-file tests, and the filters that select the ones it can answer:
/// the module that holds them in a binary of several, or, where more than one test in a binary is
/// `#[ignore]`d for a reason `raw-authentic` cannot answer (a separate CC0 public-fixture directory
/// no manifest here names), the exact names of the ones it can.
struct Authentic {
    package: &'static str,
    test: &'static str,
    filters: &'static [&'static str],
}

/// The authentic RAW tests `raw-authentic` can run from a manifest alone: every ignored test in
/// `luxforge-raw`'s `real_files` and in the `raw` module of `luxforge-cli`'s `json_cli` that needs
/// nothing beyond one `LUXFORGE_RAW_OWNER_DIR` directory of exactly-named files
/// (`docs/engineering/development.md`). `real_files` also holds `authentic_public_modes_preserve_sources_and_develop_float`
/// and `nikon_high_efficiency_is_refused_and_lossless_controls_decode`, which need their own
/// separate `LUXFORGE_RAW_PUBLIC_DIR` and `LUXFORGE_RAW_POPULAR_DIR` of CC0 fixtures no manifest here
/// names, so they are filtered out rather than left to fail on a missing environment variable.
const AUTHENTIC: [Authentic; 2] = [
    Authentic {
        package: "luxforge-raw",
        test: "real_files",
        filters: &[
            "authentic_owner_modes_preserve_sources_and_develop_float",
            "required_dji_opcodes_are_applied_to_fc3411",
            "malformed_or_unknown_dji_opcode_fails_explicitly",
        ],
    },
    Authentic {
        package: "luxforge-cli",
        test: "json_cli",
        filters: &["raw::"],
    },
];

fn authentic_args(invocation: &Authentic) -> Vec<String> {
    let mut args: Vec<String> = ["test", "--release", "--locked", "--package"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.push(invocation.package.into());
    args.extend(["--test".into(), invocation.test.into()]);
    args.extend(["--".into(), "--ignored".into(), "--nocapture".into()]);
    args.extend(invocation.filters.iter().map(|f| (*f).to_owned()));
    args
}

/// The manifest's own RAW directory: the parent of its sources, which is where every filename the
/// authentic tests read (`nikon_z6.NEF`, `fujifilm_x100vi.RAF`, `mavic_air_2s.DNG`, …) lives, since
/// the manifest's sources are exactly the owner's qualified files in that one directory.
fn owner_dir(sources: &[(String, PathBuf)]) -> Result<PathBuf> {
    sources
        .first()
        .and_then(|(_, path)| path.parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| "RAW manifest has no source to read an owner directory from".into())
}

/// Run the authentic RAW tests a manifest makes possible: the owner-supplied native and JSON tests
/// that are otherwise `#[ignore]`d and sit in no tier at all. Every invocation is logged to its own
/// file inside `out`, and `result.json` records what was run, against which directory, and whether
/// it all passed.
pub fn authentic(root: &Path, manifest_path: &Path, out: &Path) -> Result {
    ensure(!out.exists(), "RAW authentic output must be new")?;
    fs::create_dir_all(out)?;
    let sources = raw::sources(manifest_path)?;
    let dir = owner_dir(&sources)?;
    let mut result = json!({
        "status":"failed",
        "manifest":manifest_path,
        "owner_dir":dir,
        "invocations":AUTHENTIC.iter().map(|i| json!({"package":i.package,"test":i.test,"filters":i.filters})).collect::<Vec<_>>(),
    });
    let checked = (|| -> Result {
        for (index, invocation) in AUTHENTIC.iter().enumerate() {
            let log = out.join(format!("{}-{}.log", invocation.package, invocation.test));
            let file = fs::File::create(&log)?;
            let args = authentic_args(invocation);
            let status = cargo_command()
                .current_dir(root)
                .env("LUXFORGE_RAW_OWNER_DIR", &dir)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(file.try_clone()?)
                .stderr(file)
                .status()?;
            ensure(
                status.success(),
                format!(
                    "Authentic RAW test {} of {} ({} --test {}) failed; see {}",
                    index + 1,
                    AUTHENTIC.len(),
                    invocation.package,
                    invocation.test,
                    log.display()
                ),
            )?;
        }
        Ok(())
    })();
    match &checked {
        Ok(()) => result["status"] = json!("passed"),
        Err(e) => result["error"] = json!(e.to_string()),
    }
    write_json(&out.join("result.json"), &result)?;
    checked
}

/// What each tier runs, in order. Every tier includes the ones below it. The rendered scenarios are
/// the one block that runs through a pool; the timing components run strictly serially, in this
/// order, after everything else in the tier and behind the host-wide timing lock, so nothing else
/// on the machine is competing with them from this command.
///
/// `manifest` is the manifest's own sources, `(id, absolute path)`, read once by the caller through
/// the one RAW manifest reader; `None` when `--manifest` was not given. Building one `raw-editor`
/// and one `raw-panel` component per source and one RAW `performance` component needs the source
/// list itself, not just whether a manifest was given.
fn plan(tier: Tier, manifest: Option<&[(String, PathBuf)]>, fixtures: bool) -> Vec<Spec> {
    let mut specs = Vec::new();
    if !fixtures && tier != Tier::Quick {
        specs.push(Spec {
            output: false,
            ..spec("generate-fixtures", "setup", &["generate-fixtures"])
        });
    }
    // Quick is the fast headless subset: `check --quick` leaves out the slow tests and the
    // doctests. Every other tier runs the whole `check` and the release acceptance journey.
    if tier == Tier::Quick {
        specs.push(Spec {
            output: false,
            ..spec("check", "quick", &["check", "--quick"])
        });
    } else {
        specs.push(Spec {
            output: false,
            ..spec("check", "headless", &["check"])
        });
        specs.push(Spec {
            result: Some("result.json"),
            ..spec("editor-acceptance", "headless", &["editor-acceptance"])
        });
    }
    if tier.rendered() {
        for scenario in smoke::SCENARIOS
            .iter()
            .filter(|scenario| scenario.rendered())
        {
            specs.push(Spec {
                result: Some("result.json"),
                binary: true,
                ..spec(
                    &format!("smoke-{}", scenario.name),
                    "rendered",
                    &["smoke", "--scenario", scenario.name],
                )
            });
        }
        // The release gate: every stack of the corpus on the reference renderer and on the GPU,
        // each output kind the GPU renders held to its recorded limit. It runs alone, after the
        // pool, since it fills the host's cores and the device; given the RAW manifest when the run
        // has one, and incomplete without it.
        specs.push(Spec {
            result: Some("report.json"),
            manifest: true,
            deadline: GATE_DEADLINE,
            ..spec("gpu-qualification", "gate", &["gpu-qualification"])
        });
    }
    if tier == Tier::Full {
        let sources = manifest.unwrap_or(&[]);
        // The RAW editor journey once per manifest source, each run given the manifest that lists
        // it. Without a manifest there is nothing to run it over, and the skip says so.
        if manifest.is_none() {
            specs.push(Spec {
                skip: Some("no --manifest"),
                ..spec("raw-editor", "full", &[])
            });
        }
        for (id, path) in sources {
            let path = path.to_string_lossy().into_owned();
            specs.push(Spec {
                result: Some("result.json"),
                binary: true,
                manifest: true,
                ..spec(
                    &format!("raw-editor-{id}"),
                    "full",
                    &["smoke", "--scenario", "raw-editor", "--source", &path],
                )
            });
        }
        for (id, path) in sources {
            let path = path.to_string_lossy().into_owned();
            specs.push(Spec {
                result: Some("result.json"),
                binary: true,
                manifest: true,
                ..spec(
                    &format!("raw-detail-{id}"),
                    "full",
                    &["smoke", "--scenario", "raw-detail", "--source", &path],
                )
            });
        }
        specs.push(Spec {
            result: Some("result.json"),
            manifest: true,
            skip: manifest.is_none().then_some("no --manifest"),
            ..spec("raw-authentic", "full", &["raw-authentic"])
        });
        for (id, path) in sources {
            let path = path.to_string_lossy().into_owned();
            specs.push(Spec {
                result: Some("result.json"),
                binary: true,
                ..spec(
                    &format!("raw-panel-{id}"),
                    "full",
                    &["smoke", "--scenario", "raw-panel", "--source", &path],
                )
            });
        }
        if let Some((_, path)) = sources.first() {
            let path = path.to_string_lossy().into_owned();
            specs.push(Spec {
                result: Some("result.json"),
                binary: true,
                ..spec(
                    "raw-performance",
                    "full",
                    &["smoke", "--scenario", "performance", "--source", &path],
                )
            });
        }
        specs.push(Spec {
            result: Some("result.json"),
            binary: true,
            ..spec("hardening", "full", &["hardening"])
        });
    }
    if tier.timing() {
        specs.push(Spec {
            args: vec![
                "editor-performance".into(),
                "--source".into(),
                twenty_four_mp(),
            ],
            result: Some("result.json"),
            ..spec("editor-performance", "timing", &[])
        });
        specs.push(Spec {
            args: vec!["editor-latency".into(), "--source".into(), twenty_four_mp()],
            result: Some("latency.json"),
            binary: true,
            ..spec("editor-latency", "timing", &[])
        });
        specs.push(Spec {
            args: vec![
                "editor-latency".into(),
                "--source".into(),
                twenty_four_mp(),
                "--mode".into(),
                "burst".into(),
            ],
            result: Some("latency.json"),
            binary: true,
            ..spec("editor-latency-burst", "timing", &[])
        });
        specs.push(Spec {
            result: Some("measurements.json"),
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
    /// The component's own report inside its `run/` directory, when it writes one: for a timing
    /// component, the file whose `rows` the summary collects.
    result: Option<&'static str>,
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
            "reliability":self.load.map(|load| launch::reliability(Some(load))),
        })
    }
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
    deadline: Duration,
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
        if started.elapsed() >= deadline {
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

/// Why a figure cannot be compared against its target.
fn too_loaded(load: Option<f64>) -> String {
    format!(
        "one-minute load average {} at the start of the component exceeds the {} threshold; this figure is neither a pass nor a miss",
        number(load.unwrap_or_default()),
        number(launch::LOAD_THRESHOLD)
    )
}

/// One summary row from one row of a timing component's report: its metric, unit, p50, p95 and
/// sample count, where it came from, and the one load record ([`launch::load`]) of the load its
/// component started at. A row the run never reached has no distribution, so it reads as a
/// zero-sample row with no figures.
fn summary_row(source: &str, row: &Value, load: Option<f64>) -> Value {
    let distribution = &row["distribution"];
    let mut summary = json!({
        "metric":row["metric"],
        "unit":row["unit"],
        "p50":distribution["p50"],
        "p95":distribution["p95"],
        "count":distribution["count"].as_u64().unwrap_or(0),
        "source":source,
    });
    scenario::launch::stamp(&mut summary, &launch::load(load));
    summary
}

/// Which statistic of a row's distribution answers a target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Statistic {
    P50,
    P95,
}

impl Statistic {
    fn name(self) -> &'static str {
        match self {
            Self::P50 => "p50",
            Self::P95 => "p95",
        }
    }
}

/// Which side of `limit` is a pass. Every target before the instant-preview design was an upper
/// bound; the burst design adds a lower bound (frames per second), so the comparison is explicit
/// rather than assumed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    AtMost,
    AtLeast,
}

/// A provisional target from the performance specification with the report row that answers it:
/// the `metric` row of `source`'s `rows`, read at `statistic`. `strict` is the comparison the
/// specification wrote (`<`/`>` rather than `<=`/`>=`), and `scale` converts the stored figure into
/// `unit`. The sample count is always the row's own distribution count.
struct Target {
    text: &'static str,
    /// The report, relative to the verification output: `<component>/run/<file>`.
    source: &'static str,
    metric: &'static str,
    statistic: Statistic,
    unit: &'static str,
    limit: f64,
    /// A second, looser bound in the same direction: a figure past `limit` but inside this one is
    /// `acceptable` rather than a miss. The owner's slider target is the one target that has one.
    acceptable: Option<f64>,
    direction: Direction,
    strict: bool,
    scale: f64,
    note: &'static str,
}

const LATENCY: &str = "editor-latency/run/latency.json";
const MEASURE: &str = "measure/run/measurements.json";
/// `editor-latency --mode burst`'s own report: a wild, undrained drag.
const BURST: &str = "editor-latency-burst/run/latency.json";

const TARGETS: [Target; 10] = [
    Target {
        text: "Warm 24 MP slider-to-presented-frame p95 < 16 ms, acceptable below 32 ms",
        source: LATENCY,
        metric: "input_to_presented_frame",
        statistic: Statistic::P95,
        unit: "ms",
        limit: 16.0,
        acceptable: Some(32.0),
        direction: Direction::AtMost,
        strict: true,
        scale: 1.0,
        note: "",
    },
    Target {
        text: "Settled exact histogram p95 < 200 ms after the final input, 24 MP",
        source: LATENCY,
        metric: "final_input_to_settled_histogram",
        statistic: Statistic::P95,
        unit: "ms",
        limit: 200.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: true,
        scale: 1.0,
        note: "",
    },
    Target {
        text: "Scratch aggregate at most 64 MiB",
        source: LATENCY,
        metric: "scratch_peak_bytes",
        statistic: Statistic::P50,
        unit: "MiB",
        limit: 64.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: false,
        scale: 1.0 / (1024.0 * 1024.0),
        note: "High-water mark of the owner render context's colour budget over the gesture run",
    },
    Target {
        text: "24 MP single-image edit working set <= 600 MiB CPU-resident",
        source: MEASURE,
        metric: "24mp.sampled_peak_rss_mib",
        statistic: Statistic::P50,
        unit: "MiB",
        limit: 600.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: false,
        scale: 1.0,
        note: "Sampled process RSS includes capture readbacks and GPU resources",
    },
    Target {
        text: "60 MP peak <= 1 GiB process RSS",
        source: MEASURE,
        metric: "60mp.sampled_peak_rss_mib",
        statistic: Statistic::P50,
        unit: "MiB",
        limit: 1024.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: false,
        scale: 1.0,
        note: "One 60 MP open; the sixteen-load workload is a separate row in the measurements",
    },
    Target {
        text: "Idle CPU < 1% of one core over 30 s",
        source: MEASURE,
        metric: "idle.cpu_percent_one_core",
        statistic: Statistic::P50,
        unit: "% of one core",
        limit: 1.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: true,
        scale: 1.0,
        note: "One 30 s window after readiness and a one-second settle",
    },
    Target {
        text: "Launch to usable empty shell p95 < 1 s warm",
        source: MEASURE,
        metric: "empty.launch_to_observed_frame_ms",
        statistic: Statistic::P95,
        unit: "ms",
        limit: 1000.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: true,
        scale: 1.0,
        note: "Upper bound: includes copying the executable into the temporary background bundle",
    },
    Target {
        text: "Uncached 24 MP JPEG to Fit preview p95 < 750 ms",
        source: MEASURE,
        metric: "24mp.open_to_raster_ms",
        statistic: Statistic::P95,
        unit: "ms",
        limit: 750.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: true,
        scale: 1.0,
        note: "Warm filesystem cache, CPU raster; import, refresh and render",
    },
    // The instant-preview design's provisional burst target (docs/design/instant-preview.md,
    // "Goal"): a wild, undrained drag the drained-drag report above cannot answer at all. The
    // design's own drained-drag figure read the same drag report and row as the warm 24 MP row
    // above, so it never had a target of its own here; only the owner's 16/32 ms row does.
    Target {
        text: "Instant preview: burst presented frames per second >= 30",
        source: BURST,
        metric: "presented_fps",
        statistic: Statistic::P50,
        unit: "fps",
        limit: 30.0,
        acceptable: None,
        direction: Direction::AtLeast,
        strict: false,
        scale: 1.0,
        note: "editor-latency --mode burst: 120 inputs/s for 3 s, alternating direction",
    },
    Target {
        text: "Instant preview: burst presented-frame staleness p95 <= 50 ms",
        source: BURST,
        metric: "staleness_ms",
        statistic: Statistic::P95,
        unit: "ms",
        limit: 50.0,
        acceptable: None,
        direction: Direction::AtMost,
        strict: false,
        scale: 1.0,
        note: "Each presented frame's own input time (slider_draft_set) to its presentation",
    },
];

impl Target {
    /// The component whose report answers this target.
    fn component(&self) -> &'static str {
        self.source.split('/').next().unwrap_or(self.source)
    }
    fn verdict(&self, result: Option<&Value>, timing: bool, load: Option<f64>) -> Value {
        let source = format!("{} {} {}", self.source, self.metric, self.statistic.name());
        let unmeasured = |why: String| {
            let mut verdict = json!({"target":self.text,"unit":self.unit,"limit":self.limit,"source":source,"measured":Value::Null,"samples":0,"verdict":"not_measured","reason":why,"note":self.note});
            scenario::launch::stamp(&mut verdict, &launch::load(load));
            verdict
        };
        let Some(result) = result else {
            return unmeasured(if timing {
                format!("{} was not written", self.source)
            } else {
                "timing tier did not run".into()
            });
        };
        let Some((raw, samples)) = stats::distribution(result, self.metric).and_then(|d| {
            Some((
                d[self.statistic.name()].as_f64()?,
                d["count"].as_u64().unwrap_or(0),
            ))
        }) else {
            return unmeasured(format!("{} holds no {} row", self.source, self.metric));
        };
        let value = raw * self.scale;
        let within = |limit: f64| match (self.direction, self.strict) {
            (Direction::AtMost, true) => value < limit,
            (Direction::AtMost, false) => value <= limit,
            (Direction::AtLeast, true) => value > limit,
            (Direction::AtLeast, false) => value >= limit,
        };
        let ok = within(self.limit);
        let acceptable = self.acceptable.is_some_and(within);
        // A figure taken while the host was busy is recorded with everything it came from, and is
        // not turned into a verdict: the target is unanswered, not met and not missed.
        let over = launch::unreliable(load);
        let verdict = if over {
            "unreliable"
        } else if ok {
            "pass"
        } else if acceptable {
            "acceptable"
        } else {
            "miss"
        };
        let mut verdict = json!({"target":self.text,"unit":self.unit,"limit":self.limit,"acceptable_limit":self.acceptable,"source":source,"measured":value,"samples":samples,"verdict":verdict,"reason":over.then(|| too_loaded(load)),"note":self.note});
        scenario::launch::stamp(&mut verdict, &launch::load(load));
        verdict
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

/// Every timing row and target verdict the directory can answer so far: one loop over every timing
/// component's report, each written in the one `rows` shape, and each target read from its row.
/// Both are recomputed on each write, so a run that stops early still leaves the rows of the
/// components that finished.
fn collect(out: &Path, tier: Tier, entries: &[Entry]) -> (Vec<Value>, Vec<Value>) {
    let mut rows = Vec::new();
    for entry in entries.iter().filter(|e| e.tier == "timing") {
        let Some(file) = entry.result else { continue };
        let source = format!("{}/run/{file}", entry.component);
        let Some(report) = optional(&out.join(&source)) else {
            continue;
        };
        for row in stats::rows(&report) {
            rows.push(summary_row(&source, row, entry.load));
        }
    }
    let targets = TARGETS
        .iter()
        .map(|target| {
            let result = optional(&out.join(target.source));
            target.verdict(
                result.as_ref(),
                tier.timing(),
                load_of(entries, target.component()),
            )
        })
        .collect();
    (rows, targets)
}

/// Every component that did not prove all it lists: `skipped` (a prerequisite this run does not
/// have, such as no `--manifest`), `incomplete` (it ran and could not run everything, such as the
/// release gate without a source) or `not_run` (never reached, usually because an earlier setup
/// step failed). A skip is not a pass, so a tier with one of these and no outright failure is
/// `incomplete` rather than `passed`.
fn incomplete(entries: &[Entry]) -> Vec<&Entry> {
    entries
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                Status::Skipped | Status::Incomplete | Status::NotRun
            )
        })
        .collect()
}

fn markdown(header: &Value, entries: &[Entry], rows: &[Value], targets: &[Value]) -> String {
    let failed: Vec<_> = entries
        .iter()
        .filter(|e| e.status.failure())
        .map(|e| e.component.clone())
        .collect();
    let incomplete: Vec<String> = incomplete(entries)
        .into_iter()
        .map(|e| {
            format!(
                "{} ({}{})",
                e.component,
                e.status.name(),
                e.error
                    .as_deref()
                    .map(|why| format!(": {why}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    let mut text = String::from("# Verification summary\n\n");
    let verdict = match header["refused"].as_str() {
        Some(why) => format!("REFUSED ({why})"),
        None if !failed.is_empty() => format!("FAILED ({})", failed.join(", ")),
        None if !incomplete.is_empty() => format!("INCOMPLETE ({})", incomplete.join(", ")),
        None => "passed".to_owned(),
    };
    text.push_str(&format!(
        "Tier {}: {}. Host {}. {}. Cargo.lock SHA-256 {}. Total {} s. Output {}.\n\n",
        header["tier"].as_str().unwrap_or("?"),
        verdict,
        header["host"].as_str().unwrap_or("unknown"),
        match header["binary"].as_str() {
            Some(binary) => format!(
                "Binary SHA-256 {} ({binary})",
                header["binary_sha256"].as_str().unwrap_or("unavailable")
            ),
            None => "No editor binary".to_owned(),
        },
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
    } else if !incomplete(entries).is_empty() {
        "incomplete"
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
    summary["incomplete"] = json!(
        incomplete(entries)
            .iter()
            .map(|e| json!({"component":e.component,"status":e.status.name(),"reason":e.error}))
            .collect::<Vec<_>>()
    );
    summary["components"] = json!(entries.iter().map(Entry::value).collect::<Vec<_>>());
    summary["timing_rows"] = json!(rows);
    summary["targets"] = json!(targets);
    write_json(&out.join("summary.json"), &summary)?;
    fs::write(out.join("summary.md"), &text)?;
    Ok(text)
}

/// A tier that ran to completion but proved less than a full pass: nothing failed outright, but
/// some component was `skipped` or `not_run`. Its own error type, distinct from an ordinary
/// failure, so `main` can give it its own exit code: a skip is not a pass, but it is not the same
/// as a defect either, and a script needs to be able to tell the two apart.
#[derive(Debug)]
pub struct Incomplete(pub String);
impl std::fmt::Display for Incomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for Incomplete {}

/// The process exit code `main` gives an `incomplete` tier: distinct from `0` (passed) and `1`
/// (an ordinary failure, `ExitCode::FAILURE`), so a script can tell "nothing failed but the tier
/// did not prove everything it lists" apart from both.
pub const INCOMPLETE_EXIT_CODE: u8 = 3;

/// Name every component that failed, timed out, was skipped or never ran, so the exit status says
/// what to look at. A failure anywhere makes the whole tier an ordinary failure; short of that, any
/// skip or non-run makes it `incomplete` rather than a silent pass.
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
    )?;
    let incomplete: Vec<_> = incomplete(entries)
        .into_iter()
        .map(|e| {
            format!(
                "{} ({}{})",
                e.component,
                e.status.name(),
                e.error
                    .as_deref()
                    .map(|why| format!(": {why}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    if incomplete.is_empty() {
        Ok(())
    } else {
        Err(Box::new(Incomplete(format!(
            "Verification incomplete: {}. Summary: {}",
            incomplete.join(", "),
            out.join("summary.md").display()
        ))))
    }
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
        result: s.result,
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
        s.deadline,
    );
    entry.elapsed_s = clock.elapsed().as_secs_f64();
    match ran {
        Ok((code, timed_out)) => {
            let result = s
                .result
                .and_then(|name| optional(&dir.join("run").join(name)));
            entry.exit_code = code;
            entry.launches = launches(optional(&dir.join("run/result.json")).as_ref());
            entry.status = match (timed_out, code) {
                (true, _) => Status::TimedOut,
                (false, Some(0)) => Status::Passed,
                (false, Some(code)) if code == i32::from(INCOMPLETE_EXIT_CODE) => {
                    Status::Incomplete
                }
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
            result: s.result,
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

    let fixtures = fixtures::present(&root.join("fixtures/generated"));
    let manifest = manifest.map(|path| absolute(root, &path));
    let raw_sources = manifest
        .as_deref()
        .map(raw::sources)
        .transpose()?
        .unwrap_or_default();
    let specs = plan(
        tier,
        manifest.is_some().then_some(raw_sources.as_slice()),
        fixtures,
    );

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
    // Quick launches no editor and times nothing, so it builds nothing in release: its one
    // component, `check --quick`, builds what it tests and runs through this executable.
    let build_started = Instant::now();
    let built = if tier == Tier::Quick {
        None
    } else {
        let log = fs::File::create(out.join("build.log"))?;
        Some(
            cargo_command()
                .current_dir(root)
                .args([
                    "build",
                    "--release",
                    "--locked",
                    "--package",
                    "luxforge-app",
                    "--package",
                    "xtask",
                ])
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .status()?
                .success(),
        )
    };

    let release = binary(root)?;
    let xtask = if built.is_some() {
        release.with_file_name(format!("xtask{}", std::env::consts::EXE_SUFFIX))
    } else {
        std::env::current_exe()?
    };
    // Without `--binary` the built release executable is passed explicitly, so every component that
    // launches the editor measures the same file rather than whatever it would resolve itself.
    let bin = selected_binary
        .map(|path| absolute(root, &path))
        .or_else(|| built.is_some().then(|| release.clone()));

    let header = json!({
        "format":1,
        "tier":tier.name(),
        "host":host(root).unwrap_or_else(|_| "unknown".into()),
        "binary":bin,
        "binary_sha256":bin.as_deref().and_then(|bin| hash(bin).ok()),
        "lockfile_sha256":hash(&root.join("Cargo.lock"))?,
        "output":out,
        "manifest":manifest,
        "elapsed_s":0.0,
        "jobs":jobs,
        "load_threshold":tier.timing().then_some(launch::LOAD_THRESHOLD),
        "build":match built {
            Some(passed) => json!({"status":if passed {"passed"} else {"failed"},"elapsed_s":tenths(build_started.elapsed().as_secs_f64()),"log":"build.log"}),
            None => json!({"status":"not_needed"}),
        },
        "note":"Components run as child processes of the xtask executable, the release one above the quick tier, with their console output in <component>/console.log. The rendered scenarios run through a bounded pool; every other component, and every timing component in particular, runs serially. A skip is not a pass.",
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
            result: s.result,
        })
        .collect();

    if built == Some(false) {
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
        bin: bin.unwrap_or_default(),
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
        let load = (s.tier == "timing")
            .then(|| launch::load_average(root))
            .flatten();
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
    fn names(tier: Tier, manifest: Option<&[(String, PathBuf)]>, fixtures: bool) -> Vec<String> {
        plan(tier, manifest, fixtures)
            .into_iter()
            .map(|s| s.name)
            .collect()
    }
    #[test]
    fn tiers_compose_in_order() {
        assert_eq!(names(Tier::Quick, None, true), ["check"]);
        assert_eq!(plan(Tier::Quick, None, true)[0].args, ["check", "--quick"]);
        assert_eq!(plan(Tier::Rendered, None, true)[0].args, ["check"]);
        let rendered = names(Tier::Rendered, None, true);
        assert_eq!(&rendered[..2], ["check", "editor-acceptance"]);
        assert_eq!(
            rendered.len(),
            2 + smoke::SCENARIOS.iter().filter(|s| s.rendered()).count() + 1
        );
        assert_eq!(
            &rendered[2..6],
            [
                "smoke-detail",
                "smoke-detail-fit",
                "smoke-detail-zoom",
                "smoke-empty"
            ]
        );
        // The release gate last, alone after the pool, given the manifest when there is one and
        // the longest bound.
        assert_eq!(rendered[rendered.len() - 2], "smoke-unavailable");
        assert_eq!(rendered.last().unwrap(), "gpu-qualification");
        let gate = plan(Tier::Rendered, None, true).pop().unwrap();
        assert_eq!(gate.args, ["gpu-qualification"]);
        assert_eq!(gate.tier, "gate");
        assert!(gate.output && gate.manifest && !gate.binary);
        assert_eq!(gate.deadline, GATE_DEADLINE);
        assert_eq!(gate.result, Some("report.json"));
        assert_eq!(
            names(Tier::Timing, None, true),
            [
                "check",
                "editor-acceptance",
                "editor-performance",
                "editor-latency",
                "editor-latency-burst",
                "measure"
            ]
        );
        // Without a manifest, `full` lists `raw-editor` and `raw-authentic`, skipped, and has no
        // source to build a `raw-panel` or RAW `performance` component from.
        let full = names(Tier::Full, None, true);
        assert_eq!(&full[..2], ["check", "editor-acceptance"]);
        // Every rendered scenario, then the RAW components and hardening, then the timing
        // components last.
        assert_eq!(
            &full[full.len() - 7..],
            [
                "raw-editor",
                "raw-authentic",
                "hardening",
                "editor-performance",
                "editor-latency",
                "editor-latency-burst",
                "measure"
            ]
        );
        assert_eq!(
            full.len(),
            2 + smoke::SCENARIOS.iter().filter(|s| s.rendered()).count() + 1 + 7
        );
        // Missing generated fixtures are produced first, and only where a tier needs them.
        assert_eq!(names(Tier::Quick, None, false)[0], "check");
        assert_eq!(names(Tier::Rendered, None, false)[0], "generate-fixtures");
        assert_eq!(names(Tier::Timing, None, false)[0], "generate-fixtures");
    }
    #[test]
    fn full_adds_raw_editor_detail_and_panel_per_source_and_one_raw_performance_run() {
        let sources = [
            ("z6".to_owned(), PathBuf::from("/tmp/z6.nef")),
            ("x100vi".to_owned(), PathBuf::from("/tmp/x100vi.raf")),
        ];
        let full = plan(Tier::Full, Some(&sources), true);
        let names: Vec<&str> = full.iter().map(|s| s.name.as_str()).collect();
        // One `raw-editor`, `raw-detail` and `raw-panel` component per manifest source, named by source id,
        // around `raw-authentic`, and one RAW `performance` run over the first source, all before
        // the timing tier, with `hardening` last in `full`'s own part of the plan.
        assert_eq!(
            &names[names.len() - 13..],
            [
                "raw-editor-z6",
                "raw-editor-x100vi",
                "raw-detail-z6",
                "raw-detail-x100vi",
                "raw-authentic",
                "raw-panel-z6",
                "raw-panel-x100vi",
                "raw-performance",
                "hardening",
                "editor-performance",
                "editor-latency",
                "editor-latency-burst",
                "measure",
            ]
        );
        let detail_z6 = full.iter().find(|s| s.name == "raw-detail-z6").unwrap();
        assert_eq!(
            detail_z6.args,
            [
                "smoke",
                "--scenario",
                "raw-detail",
                "--source",
                "/tmp/z6.nef"
            ]
        );
        assert!(detail_z6.binary && detail_z6.output && detail_z6.manifest);
        assert_eq!(detail_z6.skip, None);
        let panel_z6 = full.iter().find(|s| s.name == "raw-panel-z6").unwrap();
        assert_eq!(
            panel_z6.args,
            [
                "smoke",
                "--scenario",
                "raw-panel",
                "--source",
                "/tmp/z6.nef"
            ]
        );
        assert!(panel_z6.binary && panel_z6.output && !panel_z6.manifest);
        assert_eq!(panel_z6.skip, None);
        // The journey is the `raw-editor` smoke row, given the manifest that lists its source.
        let editor_z6 = full.iter().find(|s| s.name == "raw-editor-z6").unwrap();
        assert_eq!(
            arguments(
                editor_z6,
                Path::new("/v/raw-editor-z6"),
                Path::new("/bin/luxforge"),
                Some(Path::new("/m.json"))
            ),
            [
                "smoke",
                "--scenario",
                "raw-editor",
                "--source",
                "/tmp/z6.nef",
                "--output",
                "/v/raw-editor-z6/run",
                "--binary",
                "/bin/luxforge",
                "--manifest",
                "/m.json"
            ]
        );
        assert_eq!(editor_z6.result, Some("result.json"));
        assert!(smoke::find("raw-editor").is_ok_and(|row| row.listed()));
        let performance = full.iter().find(|s| s.name == "raw-performance").unwrap();
        assert_eq!(
            performance.args,
            [
                "smoke",
                "--scenario",
                "performance",
                "--source",
                "/tmp/z6.nef"
            ]
        );
        // Without a manifest there is no source to run any of them over.
        let without = plan(Tier::Full, None, true);
        assert!(without.iter().all(|s| !s.name.starts_with("raw-panel")
            && !s.name.starts_with("raw-editor-")
            && s.name != "raw-performance"));
    }
    #[test]
    fn a_missing_manifest_skips_raw_editor_instead_of_passing_it() {
        let without = plan(Tier::Full, None, true);
        for name in ["raw-editor", "raw-authentic"] {
            let s = without.iter().find(|s| s.name == name).unwrap();
            assert_eq!(s.skip, Some("no --manifest"), "{name}");
        }
        let with = plan(
            Tier::Full,
            Some(&[("z6".to_owned(), PathBuf::from("/raw/z6.nef"))]),
            true,
        );
        assert!(with.iter().all(|s| s.name != "raw-editor"));
        for name in ["raw-editor-z6", "raw-authentic"] {
            assert!(
                with.iter().find(|s| s.name == name).unwrap().skip.is_none(),
                "{name}"
            );
        }
    }
    #[test]
    fn hardening_is_scheduled_only_in_full() {
        for tier in [Tier::Quick, Tier::Rendered, Tier::Timing] {
            assert!(!names(tier, None, true).contains(&"hardening".to_owned()));
        }
        let full = plan(Tier::Full, None, true);
        let hardening = full.iter().find(|s| s.name == "hardening").unwrap();
        assert_eq!(hardening.args, ["hardening"]);
        assert!(hardening.binary && hardening.output && !hardening.manifest);
        assert_eq!(hardening.skip, None, "hardening needs no --manifest");
    }
    #[test]
    fn raw_authentic_runs_both_ignored_test_binaries_with_the_owner_directory() {
        let sources = [
            ("z6".to_owned(), PathBuf::from("/tmp/raw/nikon_z6.NEF")),
            (
                "x100vi".to_owned(),
                PathBuf::from("/tmp/raw/fujifilm_x100vi.RAF"),
            ),
        ];
        assert_eq!(
            owner_dir(&sources).unwrap(),
            PathBuf::from("/tmp/raw"),
            "the manifest's own sources' shared directory"
        );
        assert!(owner_dir(&[]).is_err(), "no source, no directory to read");
        let full = plan(Tier::Full, Some(&sources), true);
        let authentic = full.iter().find(|s| s.name == "raw-authentic").unwrap();
        assert_eq!(authentic.args, ["raw-authentic"]);
        assert!(authentic.manifest && authentic.output && !authentic.binary);
        assert_eq!(AUTHENTIC.len(), 2);
        assert_eq!(
            authentic_args(&AUTHENTIC[0]),
            [
                "test",
                "--release",
                "--locked",
                "--package",
                "luxforge-raw",
                "--test",
                "real_files",
                "--",
                "--ignored",
                "--nocapture",
                "authentic_owner_modes_preserve_sources_and_develop_float",
                "required_dji_opcodes_are_applied_to_fc3411",
                "malformed_or_unknown_dji_opcode_fails_explicitly",
            ]
        );
        assert_eq!(
            authentic_args(&AUTHENTIC[1]),
            [
                "test",
                "--release",
                "--locked",
                "--package",
                "luxforge-cli",
                "--test",
                "json_cli",
                "--",
                "--ignored",
                "--nocapture",
                "raw::",
            ]
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
            result: Some("result.json"),
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
        let header = json!({"tier":"full","host":"aarch64-apple-darwin","binary":"/tmp/luxforge","binary_sha256":"abc","lockfile_sha256":"def","output":"/tmp/out","elapsed_s":61.5});
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
        let latency = json!({"rows":[
            stats::row("input_to_presented_frame", "ms", [74.8, 83.4].repeat(15)),
            stats::row("final_input_to_settled_histogram", "ms", [99.7, 250.0].repeat(15)),
            stats::scalar("scratch_peak_bytes", "bytes", Some(14_116_000.0)),
        ]});
        let verdicts: Vec<Value> = TARGETS
            .iter()
            .map(|t| t.verdict((t.source == LATENCY).then_some(&latency), true, None))
            .collect();
        assert_eq!(
            verdicts[0]["verdict"], "miss",
            "83.4 ms is past the 32 ms acceptable bound"
        );
        assert_eq!(verdicts[0]["acceptable_limit"], 32.0);
        assert_eq!(verdicts[0]["measured"], 83.4);
        assert_eq!(
            verdicts[0]["source"],
            "editor-latency/run/latency.json input_to_presented_frame p95"
        );
        for (p95, expected) in [(12.0, "pass"), (20.0, "acceptable"), (32.0, "miss")] {
            let banded =
                json!({"rows":[stats::row("input_to_presented_frame", "ms", [p95 - 1.0, p95])]});
            assert_eq!(
                TARGETS[0].verdict(Some(&banded), true, None)["verdict"],
                expected,
                "{p95} ms"
            );
        }
        assert_eq!(verdicts[0]["samples"], 30);
        assert_eq!(verdicts[1]["verdict"], "miss");
        assert_eq!(verdicts[2]["verdict"], "pass");
        assert!(verdicts[2]["measured"].as_f64().unwrap() < 64.0);
        assert_eq!(verdicts[2]["samples"], 1);
        // The measure file was never written, so its targets are unmeasured, never passes.
        assert_eq!(verdicts[3]["verdict"], "not_measured");
        assert_eq!(
            verdicts[3]["reason"],
            "measure/run/measurements.json was not written"
        );
        // The burst targets read a different file, never written in this test, so they too are
        // unmeasured rather than failed.
        assert_eq!(verdicts[8]["verdict"], "not_measured");
        assert_eq!(
            verdicts[8]["reason"],
            "editor-latency-burst/run/latency.json was not written"
        );
        assert_eq!(verdicts[9]["verdict"], "not_measured");
        // A report that exists but holds no such row, or a row with no samples, is also
        // unmeasured, with the row named.
        for empty in [
            json!({"rows":[]}),
            json!({"rows":[stats::row("input_to_presented_frame", "ms", Vec::new())]}),
        ] {
            let missing = TARGETS[0].verdict(Some(&empty), true, None);
            assert_eq!(missing["verdict"], "not_measured");
            assert_eq!(
                missing["reason"],
                "editor-latency/run/latency.json holds no input_to_presented_frame row"
            );
        }
        // Outside the timing tier the reason is the tier, not a missing file.
        assert_eq!(
            TARGETS[0].verdict(None, false, None)["reason"],
            "timing tier did not run"
        );
    }
    #[test]
    fn measure_targets_read_their_own_rows_and_sample_counts() {
        let mut rows = vec![
            stats::row("24mp.sampled_peak_rss_mib", "MiB", [500.0, 520.0]),
            stats::row("24mp.open_to_raster_ms", "ms", [100.0, 110.0, 120.0]),
            stats::row("60mp.sampled_peak_rss_mib", "MiB", [1100.0]),
            stats::row("empty.launch_to_observed_frame_ms", "ms", [900.0, 1200.0]),
        ];
        rows.extend(
            stats::IdleWindow {
                duration_s: 30.1,
                cpu_percent_one_core: 0.93,
                rss_mib_start: 300.0,
                rss_mib_end: 301.0,
                rss_mib_peak: 302.0,
            }
            .rows(),
        );
        let measure = json!({ "rows": rows });
        let verdict = |index: usize| TARGETS[index].verdict(Some(&measure), true, Some(2.5));
        // Nearest-rank p50 of two samples is the smaller one.
        assert_eq!(verdict(3)["measured"], 500.0);
        assert_eq!(verdict(3)["verdict"], "pass");
        assert_eq!(verdict(3)["samples"], 2);
        assert_eq!(verdict(3)["load_average_1m"], 2.5);
        assert_eq!(verdict(3)["reliability"], "reliable");
        assert_eq!(verdict(4)["verdict"], "miss");
        assert_eq!(verdict(5)["verdict"], "pass");
        assert_eq!(verdict(5)["samples"], 1);
        assert_eq!(verdict(6)["verdict"], "miss");
        assert_eq!(verdict(7)["verdict"], "pass");
        assert_eq!(verdict(7)["samples"], 3);
    }
    #[test]
    fn burst_targets_read_their_own_report_and_the_fps_row_is_a_lower_bound() {
        let report = |fps: f64, staleness: f64| {
            json!({"rows":[
                stats::scalar("presented_fps", "fps", Some(fps)),
                stats::row("staleness_ms", "ms", std::iter::repeat_n(staleness, 300)),
            ]})
        };
        let passing = report(42.0, 41.0);
        let verdict =
            |index: usize, result: &Value| TARGETS[index].verdict(Some(result), true, Some(2.5));
        // 42 fps clears the >= 30 lower bound; a figure below it misses instead of passing, which
        // proves the direction is not silently inverted into an upper bound.
        assert_eq!(verdict(8, &passing)["verdict"], "pass");
        assert_eq!(verdict(8, &passing)["measured"], 42.0);
        assert_eq!(verdict(9, &passing)["verdict"], "pass");
        assert_eq!(verdict(9, &passing)["samples"], 300);
        let failing = report(18.0, 61.0);
        assert_eq!(verdict(8, &failing)["verdict"], "miss");
        assert_eq!(verdict(9, &failing)["verdict"], "miss");
    }
    /// The collector is one loop over every timing component's `rows`: whatever rows a report
    /// holds become summary rows with their own source, sample count and the one load record of
    /// the load their component started at, and a component that wrote nothing adds none.
    #[test]
    fn the_collector_reads_every_timing_report_through_one_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path();
        let write = |source: &str, report: Value| {
            let path = out.join(source);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_json(&path, &report).unwrap();
        };
        write(
            "editor-performance/run/result.json",
            json!({"rows":[stats::scalar("import", "ms", Some(12.0))]}),
        );
        write(
            "editor-latency/run/latency.json",
            json!({"rows":[
                stats::row("input_to_presented_frame", "ms", (1..=20).map(f64::from)),
                stats::row("press_to_first_draft_set", "ms", Vec::new()),
            ]}),
        );
        write(
            "editor-latency-burst/run/latency.json",
            json!({"rows":[stats::scalar("photo_upload_bytes", "bytes", Some(1024.0))]}),
        );
        let timing = |component: &str, file: &'static str, load: Option<f64>| {
            let mut entry = entry(component, Status::Passed);
            entry.tier = "timing".into();
            entry.result = Some(file);
            entry.load = load;
            entry
        };
        let entries = [
            // A rendered scenario's result is never read for rows.
            entry("smoke-load", Status::Passed),
            timing("editor-performance", "result.json", Some(3.0)),
            timing("editor-latency", "latency.json", Some(9.0)),
            timing("editor-latency-burst", "latency.json", None),
            timing("measure", "measurements.json", Some(3.0)),
        ];
        let (rows, targets) = collect(out, Tier::Timing, &entries);
        let metrics: Vec<&str> = rows.iter().map(|r| r["metric"].as_str().unwrap()).collect();
        assert_eq!(
            metrics,
            [
                "import",
                "input_to_presented_frame",
                "press_to_first_draft_set",
                "photo_upload_bytes"
            ]
        );
        assert_eq!(
            rows[1],
            json!({"metric":"input_to_presented_frame","unit":"ms","p50":10.0,"p95":19.0,"count":20,
                "source":"editor-latency/run/latency.json","load_average_1m":9.0,"load_threshold":8.0,
                "reliability":"unreliable"})
        );
        assert_eq!(rows[0]["reliability"], "reliable");
        assert_eq!(rows[2]["count"], 0);
        assert_eq!(rows[2]["p95"], Value::Null);
        assert_eq!(rows[3]["unit"], "bytes");
        assert_eq!(rows[3]["load_average_1m"], Value::Null);
        assert_eq!(targets.len(), TARGETS.len());
        assert_eq!(targets[0]["verdict"], "unreliable");
        assert_eq!(targets[3]["verdict"], "not_measured");
    }
    #[test]
    fn launch_counts_come_from_what_each_component_recorded() {
        // A component that starts no editor keeps no list, and one that never wrote a result
        // started none it can prove.
        assert_eq!(launches(Some(&json!({}))), 0);
        assert_eq!(launches(None), 0);
        assert_eq!(launches(Some(&json!({"exit_code":0}))), 0);
        assert_eq!(launches(Some(&json!({"launches":[{"exit_code":0}]}))), 1);
        // A launch its watcher stopped, as `measure`'s idle process and `hardening`'s ordinary
        // launches are, is listed with no exit code and counted like any other; a `raw-editor`
        // run lists its edit and its reopen.
        let four = json!({"launches":[{"exit_code":0},{"exit_code":0},{"exit_code":0},{"exit_code":null,"stopped":true}]});
        assert_eq!(launches(Some(&four)), 4);
        let raw_editor = json!({"scenario":"raw-editor","launches":[{"evidence":"edit","exit_code":0},{"evidence":"reopen","exit_code":0}]});
        assert_eq!(launches(Some(&raw_editor)), 2);
    }
    #[test]
    fn an_all_passed_plan_is_the_only_ok_outcome() {
        assert!(outcome(&[entry("check", Status::Passed)], Path::new("/tmp/verify")).is_ok());
    }
    #[test]
    fn a_skipped_or_not_run_component_is_incomplete_not_a_silent_pass() {
        let out = Path::new("/tmp/verify");
        let error = outcome(
            &[
                entry("check", Status::Passed),
                entry("raw-editor", Status::Skipped),
                entry("measure", Status::NotRun),
            ],
            out,
        )
        .unwrap_err();
        // Its own error type, never an ordinary failure's, so `main` can give it its own exit code.
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error}");
        let text = error.to_string();
        assert!(text.contains("raw-editor (skipped"), "{text}");
        assert!(text.contains("measure (not_run"), "{text}");
        assert!(text.contains("summary.md"), "{text}");
    }
    #[test]
    fn a_component_that_ran_but_could_not_run_everything_makes_the_tier_incomplete() {
        let out = Path::new("/tmp/verify");
        let error = outcome(
            &[
                entry("check", Status::Passed),
                entry("gpu-qualification", Status::Incomplete),
            ],
            out,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error}");
        assert!(
            error.to_string().contains("gpu-qualification (incomplete"),
            "{error}"
        );
        assert!(!Status::Incomplete.failure());
    }
    #[test]
    fn any_failure_names_its_components_in_the_exit_error() {
        let out = Path::new("/tmp/verify");
        let error = outcome(
            &[
                entry("smoke-crop", Status::Failed),
                entry("check", Status::Passed),
                entry("measure", Status::TimedOut),
            ],
            out,
        )
        .unwrap_err();
        // A real failure is an ordinary failure, never `Incomplete`, even alongside a skip.
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error}");
        let error = error.to_string();
        assert!(error.contains("smoke-crop (failed)"), "{error}");
        assert!(error.contains("measure (timed_out)"), "{error}");
        assert!(error.contains("summary.md"), "{error}");
    }
    #[test]
    fn an_incomplete_tier_is_reported_incomplete_not_passed_in_both_summaries() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = [
            entry("check", Status::Passed),
            entry("raw-editor", Status::Skipped),
        ];
        let header = json!({
            "tier":"full","host":"x","binary":"/tmp/b","binary_sha256":"a",
            "lockfile_sha256":"b","output":"/tmp/o","elapsed_s":1.0,
        });
        let text = write(tmp.path(), &header, &entries, &[], &[]).unwrap();
        assert!(
            text.contains("Tier full: INCOMPLETE (raw-editor (skipped"),
            "{text}"
        );
        let summary: Value = read_json(&tmp.path().join("summary.json")).unwrap();
        assert_eq!(summary["status"], "incomplete");
        assert_eq!(summary["incomplete"][0]["component"], "raw-editor");
        assert_eq!(summary["incomplete"][0]["status"], "skipped");
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
    /// The ignored tests a pooled "scenario" runs in place of an editor: this test binary, told to
    /// run one of them, so the pool tests need no program of the host's own (`/bin/sleep` is not on
    /// Windows). The long one outlasts the short ones, so completion order cannot be list order.
    #[test]
    #[ignore]
    fn nap_long() {
        std::thread::sleep(Duration::from_millis(800));
    }

    #[test]
    #[ignore]
    fn nap_short() {
        std::thread::sleep(Duration::from_millis(100));
    }

    #[test]
    #[ignore]
    fn nap_medium() {
        std::thread::sleep(Duration::from_millis(200));
    }

    /// A block of "scenarios" that are nothing but naps, so the pool's ordering and placement can
    /// be tested without launching an editor.
    fn sleeps(naps: &[&str]) -> Vec<Spec> {
        naps.iter()
            .enumerate()
            .map(|(index, nap)| Spec {
                output: false,
                ..spec(
                    &format!("smoke-{index}"),
                    "rendered",
                    &["--ignored", "--exact", &format!("verify::tests::{nap}")],
                )
            })
            .collect()
    }

    fn pooled(specs: &[Spec], jobs: usize, out: &Path) -> Vec<Entry> {
        let started = Instant::now();
        let ctx = Ctx {
            root: out,
            out,
            xtask: std::env::current_exe().unwrap(),
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
                result: s.result,
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
        let specs = sleeps(&[
            "nap_long",
            "nap_short",
            "nap_short",
            "nap_short",
            "nap_short",
            "nap_short",
        ]);
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
        let specs = sleeps(&["nap_medium", "nap_medium", "nap_medium"]);
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
        let bin = Path::new("/tmp/luxforge");
        let manifest = PathBuf::from("/tmp/raw.json");
        let sources = [
            ("z6".to_owned(), PathBuf::from("/tmp/z6.nef")),
            ("x100vi".to_owned(), PathBuf::from("/tmp/x100vi.raf")),
        ];
        let specs = plan(Tier::Full, Some(&sources), true);

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
            smoke::SCENARIOS.iter().filter(|s| s.rendered()).count()
        );
    }

    #[test]
    fn load_over_the_threshold_marks_rows_and_verdicts_unreliable() {
        let row = stats::row("24mp.open_to_raster_ms", "ms", [120.0, 140.0]);
        let quiet = summary_row("measure/run/measurements.json", &row, Some(5.7));
        assert_eq!(quiet["reliability"], "reliable");
        assert_eq!(quiet["load_threshold"], 8.0);
        let busy = summary_row("measure/run/measurements.json", &row, Some(19.4));
        assert_eq!(busy["reliability"], "unreliable");
        assert_eq!(busy["load_average_1m"], 19.4);
        assert_eq!(busy["load_threshold"], 8.0);

        // The same figure is a pass below the threshold and neither a pass nor a miss above it.
        let latency = json!({"rows":[stats::row(
            "input_to_presented_frame",
            "ms",
            [9.5, 12.4].repeat(15),
        )]});
        let below = TARGETS[0].verdict(Some(&latency), true, Some(5.7));
        assert_eq!(below["verdict"], "pass");
        assert_eq!(below["reason"], Value::Null);
        assert_eq!(below["reliability"], "reliable");
        let above = TARGETS[0].verdict(Some(&latency), true, Some(19.4));
        assert_eq!(above["verdict"], "unreliable");
        assert_eq!(above["reliability"], "unreliable");
        assert_eq!(above["measured"], 12.4);
        assert_eq!(above["samples"], 30);
        assert!(
            above["reason"].as_str().unwrap().contains("19.40")
                && above["reason"].as_str().unwrap().contains("8.00"),
            "{}",
            above["reason"]
        );
        // A miss is withheld the same way.
        let missing = json!({"rows":[stats::row(
            "input_to_presented_frame",
            "ms",
            [180.0, 220.0].repeat(15),
        )]});
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
