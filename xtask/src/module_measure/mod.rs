//! `module-measure`: what external adjustment modules cost (`docs/design/module-manager.md`,
//! "Evidence and measurement › Measurement"), in one report, `module-measure.json` with
//! `module-measure.md` beside it. Every row carries its scope and the one-minute load at its
//! step's start and end; the header records the machine, the build and the editor binary.
//!
//! The packages are the example's: Split toning built by `module-package` from
//! `examples/modules/split-toning`, and four more packaged from its component and program under
//! other identities (`org.example.splittoning2` to `5`), so five distinct modules can be installed
//! side by side without touching the example's source. Every data root is the output's own and
//! every install goes through the API's `module.install` on a catalog owner with the store a
//! launch reads ([`store`]); the next launch's `module.package.list` must answer each loaded.
//!
//! Steps, in the order they run (`--only` takes a comma-separated list):
//!
//! - **size**: the release editor binary's size and the share of its text attributed to the
//!   packages only `luxforge-wasm` brings into the editor's dependency graph (Wasmtime, Cranelift
//!   and the parsers), by the symbols' crate paths ([`build`]).
//! - **first-use**: in this process, what a module's first use costs on its parts: the engine and
//!   the component's compilation with no cache, the compiled code read from the cache, the first
//!   call that instantiates it against a warm call, the GPU program's registration (naga) and the
//!   probe harness's pipeline on the local adapter, and the probe itself, every value set
//!   ([`in_process`]).
//! - **reference**: in this process, the whole-frame reference render of the 24 MP and 60 MP
//!   JPEGs with no layer, with the native vignette and with Split toning, alternating sample by
//!   sample, and the first render of each, whose Split toning includes instantiating an instance on
//!   each worker ([`in_process`]).
//! - **launch**: ordinary background launches of the release editor opening the 24 MP JPEG with no
//!   external module, with Split toning installed and unused, and with five installed: launch to
//!   its first `render_ready`, the peak RSS until then, and a one-second settle then thirty-second
//!   idle window's RSS and CPU ([`desktop`]).
//! - **desktop-first-use**: an ordinary background launch opening the 24 MP JPEG whose recipe holds
//!   a Split toning layer, the program unprobed: the editor's own clock from launch to its first
//!   `render_ready` (the reference's picture), to the view's probe ask and answer, and to the GPU's
//!   picture at rest drawn after the answer, with the peak RSS ([`desktop`]).
//! - **build**: a clean release build of `luxforge-wasm` alone in a new target directory with the
//!   compiler cache off, and Cargo's own timings of the units only `luxforge-wasm` brings in
//!   ([`build`]).
//!
//! The in-process steps time this process, so the command needs a release xtask:
//! `cargo run --release --locked -p xtask -- module-measure ...`. It sets no budget and is not
//! part of `verify`'s timing tier.
mod build;
mod desktop;
mod in_process;
mod store;

use crate::*;
use std::time::Instant;

pub(crate) const TOOL: &str = "module-measure";
pub(crate) const ACTOR: &str = "xtask-module-measure";
/// The steps, in the order they run.
pub const STEPS: [&str; 6] = [
    "size",
    "first-use",
    "reference",
    "launch",
    "desktop-first-use",
    "build",
];
/// The installed modules of the "five" launch: the example and four of its repackagings.
pub(crate) const FIVE: usize = 5;

pub struct Options {
    /// The release editor the desktop steps launch, and whose size `size` reads.
    pub binary: PathBuf,
    /// Where the generated 24 MP and 60 MP JPEGs are (`generate-fixtures`).
    pub fixtures: PathBuf,
    /// Launches per workload in `launch` and `desktop-first-use`.
    pub launches: usize,
    /// Alternating rounds per size in `reference`, and repetitions in `first-use`.
    pub samples: usize,
    pub only: Option<Vec<String>>,
}

impl Options {
    pub(crate) fn wanted(&self, step: &str) -> bool {
        self.only
            .as_ref()
            .is_none_or(|only| only.iter().any(|name| name == step))
    }

    pub(crate) fn source(&self, megapixels: u32) -> PathBuf {
        self.fixtures.join(format!("{megapixels}mp.jpg"))
    }
}

/// Parse `--only`'s list, each a step of [`STEPS`].
pub fn parse_only(list: &str) -> Result<Vec<String>> {
    list.split(',')
        .map(|step| {
            ensure(
                STEPS.contains(&step),
                format!("--only takes {}, not {step}", STEPS.join(", ")),
            )?;
            Ok(step.to_owned())
        })
        .collect()
}

/// The report, written again after every step so a partial run keeps what it has.
pub(crate) struct Report {
    out: PathBuf,
    root: PathBuf,
    pub rows: Vec<Value>,
    steps: Vec<Value>,
    pub header: Value,
    /// What each step recorded beside its rows: methods, recipes, adapters, the store's answers.
    pub details: serde_json::Map<String, Value>,
}

impl Report {
    /// One row of the one shape, with its scope and the load at its step's start and end.
    pub fn row(&mut self, metric: &str, unit: &str, samples: Vec<f64>, scope: &str, load: &Value) {
        let mut row = stats::row(metric, unit, samples);
        row["scope"] = json!(scope);
        row["load"] = load.clone();
        self.rows.push(row);
    }

    /// The one-minute load now, where the host reports one.
    pub fn load(&self) -> Option<f64> {
        launch::load_average(&self.root)
    }

    /// The load at a step's start and end, as every row carries it.
    pub fn span(&self, start: Option<f64>) -> Value {
        json!({"start": launch::load(start), "end": launch::load(self.load())})
    }

    fn write(&self) -> Result {
        let report = json!({
            "tool": TOOL,
            "header": self.header,
            "steps": self.steps,
            "rows": self.rows,
            "details": self.details,
        });
        write_json(&self.out.join("module-measure.json"), &report)?;
        fs::write(self.out.join("module-measure.md"), markdown(&report))?;
        Ok(())
    }
}

/// The report's rows as a table, each with its scope, and the steps' outcomes.
pub(crate) fn markdown(report: &Value) -> String {
    let mut md = String::from(
        "# module-measure\n\nOne-minute load at each step's start and end; a figure taken above \
         the threshold is not evidence. A row with no samples was not measured.\n\n| Metric | \
         Unit | n | p50 | p95 | min | max | Load start | Load end | Scope |\n| --- | --- | ---: | \
         ---: | ---: | ---: | ---: | ---: | ---: | --- |\n",
    );
    for row in stats::rows(report) {
        let d = &row["distribution"];
        let figure = |key: &str| d[key].as_f64().map_or("-".into(), |v| format!("{v:.3}"));
        let load = |end: &str| {
            row["load"][end]["load_average_1m"]
                .as_f64()
                .map_or("-".into(), |v| format!("{v:.2}"))
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            row["metric"].as_str().unwrap_or(""),
            row["unit"].as_str().unwrap_or(""),
            d["count"].as_u64().unwrap_or(0),
            figure("p50"),
            figure("p95"),
            figure("min"),
            figure("max"),
            load("start"),
            load("end"),
            row["scope"].as_str().unwrap_or(""),
        ));
    }
    md.push_str("\n## Steps\n\n");
    for step in report["steps"].as_array().into_iter().flatten() {
        md.push_str(&format!(
            "- {}: {}{}\n",
            step["name"].as_str().unwrap_or(""),
            step["status"].as_str().unwrap_or(""),
            step["error"]
                .as_str()
                .map_or(String::new(), |error| format!(" ({error})"))
        ));
    }
    md
}

/// The machine the run measured on, as the host describes it, for the report's header.
fn machine(root: &Path) -> Value {
    let read = |program: &str, args: &[&str]| {
        output(root, program, args)
            .ok()
            .map(|text| text.trim().to_owned())
    };
    if cfg!(target_os = "macos") {
        json!({
            "model": read("sysctl", &["-n", "hw.model"]),
            "cpu": read("sysctl", &["-n", "machdep.cpu.brand_string"]),
            "logical_cpus": read("sysctl", &["-n", "hw.logicalcpu"]),
            "memory_bytes": read("sysctl", &["-n", "hw.memsize"]),
            "os": read("sw_vers", &["-productVersion"]),
            "power": read("pmset", &["-g", "batt"]),
        })
    } else {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        json!({
            "cpu": cpuinfo.lines().find_map(|line| {
                line.strip_prefix("model name").map(|rest| rest.trim_start_matches([' ', '\t', ':']))
            }),
            "logical_cpus": std::thread::available_parallelism().ok().map(usize::from),
            "memory": meminfo.lines().find(|line| line.starts_with("MemTotal")),
            "os": read("uname", &["-sr"]),
        })
    }
}

/// The checkout the run was built from: its commit and whether the tree differs from it.
fn checkout(root: &Path) -> Value {
    let read = |program: &str, args: &[&str]| {
        output(root, program, args)
            .ok()
            .map(|text| text.trim().to_owned())
    };
    json!({
        "commit": read("git", &["rev-parse", "HEAD"]),
        "dirty": read("git", &["status", "--porcelain"]).map(|status| !status.is_empty()),
        "rustc": read("rustc", &["-V"]),
    })
}

pub fn run(root: &Path, out: &Path, options: Options) -> Result {
    ensure(!out.exists(), "module-measure output must be new")?;
    ensure(
        (1..=100).contains(&options.launches),
        "--launches must be 1..100",
    )?;
    ensure(
        (1..=200).contains(&options.samples),
        "--samples must be 1..200",
    )?;
    let in_process = ["first-use", "reference"];
    ensure(
        !cfg!(debug_assertions) || in_process.iter().all(|step| !options.wanted(step)),
        "the first-use and reference steps time this process: run a release xtask \
         (`cargo run --release --locked -p xtask -- module-measure ...`)",
    )?;
    let desktop = ["launch", "desktop-first-use", "size"];
    if desktop.iter().any(|step| options.wanted(step)) {
        ensure(
            options.binary.is_file(),
            format!(
                "no editor at {}: build it with `cargo build --release --locked -p luxforge-app`",
                options.binary.display()
            ),
        )?;
    }
    let sources = [options.source(24), options.source(60)];
    for source in &sources {
        ensure(
            source.is_file(),
            format!(
                "no {}: run `cargo xtask generate-fixtures` first",
                source.display()
            ),
        )?;
    }
    fs::create_dir_all(out)?;
    let hashes: Vec<String> = sources.iter().map(|s| hash(s)).collect::<Result<_>>()?;
    let mut report = Report {
        out: out.to_path_buf(),
        root: root.to_path_buf(),
        rows: Vec::new(),
        steps: Vec::new(),
        header: json!({
            "binary": options.binary,
            "binary_sha256": hash(&options.binary).ok(),
            "lockfile_sha256": hash(&root.join("Cargo.lock")).ok(),
            "target": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "machine": machine(root),
            "checkout": checkout(root),
            "sources": sources,
            "sources_sha256": hashes,
            "launches": options.launches,
            "samples": options.samples,
            "load_threshold": launch::LOAD_THRESHOLD,
            "xtask_profile": if cfg!(debug_assertions) { "dev" } else { "release" },
        }),
        details: serde_json::Map::new(),
    };
    report.write()?;
    let started = Instant::now();
    let packages = store::Packages::build(root, &out.join("packages"))?;
    report.details.insert(
        "packages".into(),
        json!({"built_s": started.elapsed().as_secs_f64(), "packages": packages.describe()}),
    );
    report.write()?;
    let mut failed = Vec::new();
    for name in STEPS {
        if !options.wanted(name) {
            report
                .steps
                .push(json!({"name": name, "status": "not_run"}));
            continue;
        }
        println!("module-measure: {name}");
        let started = Instant::now();
        let outcome = match name {
            "size" => build::size(&mut report, root, &options),
            "first-use" => in_process::first_use(&mut report, out, &packages, &options),
            "reference" => in_process::reference(&mut report, out, &packages, &options),
            "launch" => desktop::launch(&mut report, root, out, &packages, &options),
            "desktop-first-use" => desktop::first_use(&mut report, root, out, &packages, &options),
            _ => build::build(&mut report, root, out),
        };
        let status = match &outcome {
            Ok(()) => json!({"name": name, "status": "passed",
                "elapsed_s": started.elapsed().as_secs_f64()}),
            Err(error) => {
                eprintln!("module-measure: {name} failed: {error}");
                failed.push(name);
                json!({"name": name, "status": "failed", "error": error.to_string(),
                    "elapsed_s": started.elapsed().as_secs_f64()})
            }
        };
        report.steps.push(status);
        report.write()?;
    }
    for (source, before) in sources.iter().zip(&hashes) {
        ensure(
            hash(source)? == *before,
            format!("{} changed", source.display()),
        )?;
    }
    ensure(
        failed.is_empty(),
        format!("module-measure steps failed: {failed:?}"),
    )?;
    println!("PASS module-measure: {}", out.display());
    Ok(())
}

/// Milliseconds since `started`.
pub(crate) fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Copy the directory tree at `from` to `to`, which must not exist.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_takes_the_steps_by_name() {
        assert_eq!(
            parse_only("size,build").unwrap(),
            vec!["size".to_owned(), "build".to_owned()]
        );
        let error = parse_only("size,probe").unwrap_err().to_string();
        assert!(error.contains("not probe"), "{error}");
    }

    /// A row with samples reads its figures; one with none reads dashes, never a figure; the
    /// steps' outcomes follow with a failure's reason.
    #[test]
    fn the_markdown_lists_every_row_with_its_scope() {
        let mut measured = stats::row("launch.none.rss_mib", "MiB", [300.0, 310.0]);
        measured["scope"] = json!("five launches");
        let mut unmeasured = stats::row("build.wasmtime_units_s", "s", Vec::new());
        unmeasured["scope"] = json!("clean build");
        let report = json!({
            "rows": [measured, unmeasured],
            "steps": [{"name":"launch","status":"passed"},
                {"name":"build","status":"failed","error":"cargo refused"}],
        });
        let md = markdown(&report);
        assert!(
            md.contains("| launch.none.rss_mib | MiB | 2 | 300.000 | 310.000 |"),
            "{md}"
        );
        assert!(
            md.contains("| build.wasmtime_units_s | s | 0 | - | - |"),
            "{md}"
        );
        assert!(md.contains("- build: failed (cargo refused)"), "{md}");
    }

    #[test]
    fn copy_tree_copies_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir_all(from.join("a/b")).unwrap();
        fs::write(from.join("a/b/c.txt"), "c").unwrap();
        fs::write(from.join("top.txt"), "top").unwrap();
        let to = dir.path().join("to");
        copy_tree(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("a/b/c.txt")).unwrap(), "c");
        assert_eq!(fs::read_to_string(to.join("top.txt")).unwrap(), "top");
    }
}
