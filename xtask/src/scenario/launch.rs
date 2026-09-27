//! The launch envelope every smoke runner and every timing or diagnostic tool (`editor-latency`,
//! `measure`, `hardening`) shares: a [`Run`] owns one output directory, hashes what the run opens
//! and the binary that opens it, makes each editor [`Launch`] the run asks for, records every
//! launch in `result.json`'s `launches` list, checks the sources are unchanged and writes
//! `result.json` and `reproduce.md`. A launch is waited for through the one [`watch`] loop, and a
//! tool's report carries the one [`Run::provenance`] header.
//!
//! A run can also be a **replay**: the same scenario code over a copy of a recorded run, where each
//! launch is the one recorded there rather than a new editor process. Everything after a launch —
//! the preamble, the verifier, the checks files — runs exactly as it does after a real one, which
//! is what makes `smoke --verify-only` a replay of the checks and not a second implementation of
//! them.
use crate::launch::{Background, MODE, editor_args};
use crate::*;
use std::{
    convert::Infallible,
    process::{Child, ExitStatus, Stdio},
    time::{Duration, Instant},
};

/// An editor process and the temporary bundle it runs from, killed and reaped when dropped.
pub struct Guard {
    pub child: Child,
    _launch: Background,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// Start `bin` with `args` from a background bundle, its output to `log`.
pub fn spawn(root: &Path, bin: &Path, args: &[OsString], log: &Path) -> Result<Guard> {
    let launch = Background::new(bin)?;
    let f = fs::File::create(log)?;
    Ok(Guard {
        child: Command::new(&launch.executable)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(f.try_clone()?)
            .stderr(f)
            .spawn()?,
        _launch: launch,
    })
}

/// What ended a [`watch`]: the editor's exit, or what the watch was waiting for.
pub enum Watched<T> {
    Exited(ExitStatus),
    Reached(T),
}

/// How a [`watch`] polls: every `every`, failing with `late` once `deadline` has passed since
/// `from`.
pub struct Poll<'a> {
    pub every: Duration,
    pub from: Instant,
    pub deadline: Duration,
    pub late: &'a str,
}

/// The one loop that watches a launched editor. Each poll, in order: `reached` may end the watch
/// with what it found; the editor's exit ends it with its status; past the deadline it fails;
/// otherwise `sample` reads what it reads, and the loop sleeps. Each caller keeps its own poll
/// rate, deadline and samples, so a figure it takes is taken at the rate it always was.
pub fn watch<T>(
    child: &mut Guard,
    poll: Poll,
    mut reached: impl FnMut() -> Result<Option<T>>,
    mut sample: impl FnMut(&mut Guard) -> Result,
) -> Result<Watched<T>> {
    loop {
        if let Some(found) = reached()? {
            return Ok(Watched::Reached(found));
        }
        if let Some(status) = child.child.try_wait()? {
            return Ok(Watched::Exited(status));
        }
        ensure(poll.from.elapsed() < poll.deadline, poll.late)?;
        sample(child)?;
        std::thread::sleep(poll.every);
    }
}

/// A [`watch`] that ends only with the editor's exit.
pub fn until_exit(
    child: &mut Guard,
    poll: Poll,
    sample: impl FnMut(&mut Guard) -> Result,
) -> Result<ExitStatus> {
    match watch(child, poll, || Ok(None::<Infallible>), sample)? {
        Watched::Exited(status) => Ok(status),
        Watched::Reached(never) => match never {},
    }
}

/// Wait for the child to exit, polling every 10 ms.
pub fn wait(child: &mut Guard, timeout: Duration) -> Result<ExitStatus> {
    let poll = Poll {
        every: Duration::from_millis(10),
        from: Instant::now(),
        deadline: timeout,
        late: "Child timed out; killed and reaped",
    };
    until_exit(child, poll, |_| Ok(()))
}

/// Waits for a launched editor in place of [`wait`], given the moment just before it was spawned
/// and the launch's deadline, and returns what it recorded while it did: the editor's exit status,
/// or `None` when the watcher is done with an editor that is still running, which the launch then
/// ends.
pub type Watcher =
    Box<dyn FnOnce(&mut Guard, Instant, Duration) -> Result<(Option<ExitStatus>, Value)>>;

enum ScriptFile {
    /// Written into the output directory under this name.
    Kept(String),
    /// Written to a temporary file that goes with the launch, and this copy kept as `script.json`.
    Secret(Value),
}

/// One flag an editor launch can pass, for the order its arguments are assembled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    DataRoot,
    Catalog,
    Disable,
    Evidence,
    Developer,
    Endpoint,
    Open,
    Script,
    Window,
}

/// The order a launch assembles its flags in unless it names another with [`Launch::order`].
pub const ORDER: [Flag; 9] = [
    Flag::DataRoot,
    Flag::Catalog,
    Flag::Disable,
    Flag::Evidence,
    Flag::Developer,
    Flag::Endpoint,
    Flag::Open,
    Flag::Script,
    Flag::Window,
];

/// Where a launch's evidence goes.
enum Evidence {
    /// `<out>/<name>`.
    Named(String),
    /// A directory the caller names.
    At(PathBuf),
    /// Nowhere: an ordinary launch, not an evidence run.
    None,
}

/// One editor launch: what it opens and with which flags, where its evidence, data and log go, the
/// evidence script it runs, how long it may take and how it is watched. Its arguments are
/// assembled in [`ORDER`] — `--data-root`, `--catalog`, each `--disable-module`,
/// `--evidence-dir`, `--developer`, `--proof-endpoint`, each `--open`, `--evidence-script` and
/// `--window-size` — unless it names another order.
pub struct Launch {
    evidence: Evidence,
    log: String,
    data_root: Option<PathBuf>,
    catalog: Option<PathBuf>,
    disabled: Vec<String>,
    developer: bool,
    endpoint: Option<String>,
    sources: Vec<PathBuf>,
    script: Option<(Value, ScriptFile)>,
    window: Option<[String; 2]>,
    order: [Flag; 9],
    deadline: Option<Duration>,
    exit: Option<i32>,
    watch: Option<Watcher>,
    keep: Option<String>,
}

impl Launch {
    fn new(evidence: Evidence, log: &str) -> Self {
        Self {
            evidence,
            log: log.into(),
            data_root: None,
            catalog: None,
            disabled: Vec::new(),
            developer: false,
            endpoint: None,
            sources: Vec::new(),
            script: None,
            window: None,
            order: ORDER,
            deadline: None,
            exit: None,
            watch: None,
            keep: None,
        }
    }

    /// A scenario's one ordinary launch: its evidence in `app/`, its console in `subprocess.log`.
    pub fn app() -> Self {
        Self::new(Evidence::Named("app".into()), "subprocess.log")
    }

    /// One of several launches: its evidence in `<name>/`, its console in `<name>.log`.
    pub fn named(name: &str) -> Self {
        Self::new(Evidence::Named(name.into()), &format!("{name}.log"))
    }

    /// A launch that is not an evidence run: the editor as a person starts it, with its data in the
    /// [`data_root`](Self::data_root) it must be given and its console in `<name>.log`.
    pub fn ordinary(name: &str) -> Self {
        Self::new(Evidence::None, &format!("{name}.log"))
    }

    /// Keep the evidence in `dir` rather than in the output directory under the launch's name.
    pub fn evidence_dir(mut self, dir: &Path) -> Self {
        self.evidence = Evidence::At(dir.into());
        self
    }

    /// Keep the editor's data, its diagnostics log among it, under `dir`.
    pub fn data_root(mut self, dir: &Path) -> Self {
        self.data_root = Some(dir.into());
        self
    }

    /// Open an existing catalog rather than the evidence directory's own.
    pub fn catalog(mut self, catalog: &Path) -> Self {
        self.catalog = Some(catalog.into());
        self
    }

    /// Register a built-in module as unavailable.
    pub fn disable(mut self, module: &str) -> Self {
        self.disabled.push(module.into());
        self
    }

    pub fn developer(mut self) -> Self {
        self.developer = true;
        self
    }

    /// Register the capability proof module against this endpoint.
    pub fn proof_endpoint(mut self, url: &str) -> Self {
        self.endpoint = Some(url.into());
        self
    }

    pub fn open_all(mut self, sources: &[PathBuf]) -> Self {
        self.sources.extend(sources.iter().cloned());
        self
    }

    /// Run this evidence script, written into the output directory as `file`.
    pub fn script(mut self, file: &str, script: Value) -> Self {
        self.script = Some((script, ScriptFile::Kept(file.into())));
        self
    }

    /// Run a script that carries a secret: it is written outside the output directory and removed
    /// once the launch has exited, and `kept` — the script with its secrets redacted — is what the
    /// output directory keeps, as `script.json`.
    pub fn secret_script(mut self, script: Value, kept: Value) -> Self {
        self.script = Some((script, ScriptFile::Secret(kept)));
        self
    }

    pub fn window(mut self, size: [&str; 2]) -> Self {
        self.window = Some(size.map(str::to_owned));
        self
    }

    /// Assemble the arguments in `order`, every flag named once, instead of [`ORDER`].
    pub fn order(mut self, order: [Flag; 9]) -> Self {
        assert!(
            ORDER.iter().all(|flag| order.contains(flag)),
            "A launch order names every flag once: {order:?}"
        );
        self.order = order;
        self
    }

    /// Give the launch `deadline` rather than the run's.
    pub fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// The launch must exit with `code` rather than succeed: an editor that is expected to refuse.
    pub fn exits(mut self, code: i32) -> Self {
        self.exit = Some(code);
        self
    }

    /// Wait for the editor with `watcher` instead of [`wait`]. What it returns comes back from
    /// [`Run::launch`].
    pub fn watch(mut self, watcher: Watcher) -> Self {
        self.watch = Some(watcher);
        self
    }

    /// Also write what the watcher returns into the output directory as `file`.
    pub fn keep(mut self, file: &str) -> Self {
        self.keep = Some(file.into());
        self
    }

    /// Where the launch's own record goes in a run written to `out`: its evidence directory, or an
    /// ordinary launch's data root.
    fn dir(&self, out: &Path) -> Result<PathBuf> {
        match &self.evidence {
            Evidence::Named(name) => Ok(out.join(name)),
            Evidence::At(dir) => Ok(dir.clone()),
            Evidence::None => self
                .data_root
                .clone()
                .ok_or_else(|| "An ordinary launch needs a data root".into()),
        }
    }

    /// The launch's evidence as `result.json` lists it: its directory inside the output directory,
    /// or the whole path of one outside it, or null for an ordinary launch.
    fn listed(&self, out: &Path) -> Value {
        match &self.evidence {
            Evidence::Named(name) => json!(name),
            Evidence::At(dir) => json!(
                dir.strip_prefix(out)
                    .unwrap_or(dir)
                    .to_string_lossy()
                    .into_owned()
            ),
            Evidence::None => Value::Null,
        }
    }

    /// The editor's arguments, hidden-window flag included, as [`Run::launch`] passes them in a run
    /// whose output directory is `out`, with a secret script shown at its kept copy: what the dump
    /// tests write.
    #[cfg(test)]
    pub fn command(&self, out: &Path) -> Vec<String> {
        let script = self.script.as_ref().map(|(_, file)| match file {
            ScriptFile::Kept(name) => out.join(name),
            ScriptFile::Secret(_) => out.join("script.json"),
        });
        editor_args(&self.arguments(out, script.as_deref()))
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// The editor's arguments for this launch in a run written to `out`, before the hidden-window
    /// flag every harness launch adds, with its script, if it has one, at `script`.
    fn arguments(&self, out: &Path, script: Option<&Path>) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        for flag in self.order {
            match flag {
                Flag::DataRoot => {
                    if let Some(dir) = &self.data_root {
                        args.extend(["--data-root".into(), dir.into()]);
                    }
                }
                Flag::Catalog => {
                    if let Some(catalog) = &self.catalog {
                        args.extend(["--catalog".into(), catalog.into()]);
                    }
                }
                Flag::Disable => {
                    for module in &self.disabled {
                        args.extend(["--disable-module".into(), module.into()]);
                    }
                }
                Flag::Evidence => match &self.evidence {
                    Evidence::Named(name) => {
                        args.extend(["--evidence-dir".into(), out.join(name).into()]);
                    }
                    Evidence::At(dir) => args.extend(["--evidence-dir".into(), dir.into()]),
                    Evidence::None => {}
                },
                Flag::Developer => {
                    if self.developer {
                        args.push("--developer".into());
                    }
                }
                Flag::Endpoint => {
                    if let Some(url) = &self.endpoint {
                        args.extend(["--proof-endpoint".into(), url.into()]);
                    }
                }
                Flag::Open => {
                    for source in &self.sources {
                        args.extend(["--open".into(), source.into()]);
                    }
                }
                Flag::Script => {
                    if let Some(script) = script {
                        args.extend(["--evidence-script".into(), script.into()]);
                    }
                }
                Flag::Window => {
                    if let Some([width, height]) = &self.window {
                        args.extend(["--window-size".into(), width.into(), height.into()]);
                    }
                }
            }
        }
        args
    }
}

/// What a launch left: where it recorded itself — its evidence directory, or an ordinary launch's
/// data root — and what its watcher returned, `Null` without one or in a replay.
#[derive(Debug)]
pub struct Launched {
    pub dir: PathBuf,
    pub watched: Value,
}

enum Mode {
    Launch {
        bin: PathBuf,
        timeout: Duration,
    },
    /// Over a copy of the run recorded in this directory.
    Replay {
        recorded: PathBuf,
    },
}

/// One scenario run and its output directory. See the module documentation.
pub struct Run {
    root: PathBuf,
    out: PathBuf,
    scenario: String,
    /// Whether this is a run of a timing or diagnostic tool rather than of a smoke scenario.
    tool: bool,
    mode: Mode,
    result: Value,
    sources: Vec<PathBuf>,
    launches: Vec<Value>,
    note: Option<String>,
}

/// What a run's checks write rather than what its launches recorded: a replay leaves these out of
/// its copy, so every one in the replay's output is the replay's own.
fn written_by_checks(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.ends_with("-checks.json") || matches!(name, "render-times.json" | "replay.json")
    })
}

fn copy_evidence(from: &Path, to: &Path) -> Result {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            if !written_by_checks(&entry.file_name()) {
                copy_evidence(&entry.path(), &to.join(entry.file_name()))?;
            }
        }
    } else {
        fs::copy(from, to)?;
    }
    Ok(())
}

impl Run {
    fn new(root: &Path, out: &Path, scenario: &str, mode: Mode) -> Self {
        Self {
            root: root.into(),
            out: out.into(),
            scenario: scenario.into(),
            tool: false,
            mode,
            result: json!({"scenario":scenario,"status":"failed","launch_mode":MODE,"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH)}),
            sources: Vec::new(),
            launches: Vec::new(),
            note: None,
        }
    }

    /// A new run of `scenario` into `out`, which must not exist yet, launching `bin`.
    pub fn start(
        root: &Path,
        out: &Path,
        scenario: &str,
        bin: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        ensure(!out.exists(), "Smoke output must be new")?;
        fs::create_dir_all(out)?;
        Ok(Self::new(
            root,
            out,
            scenario,
            Mode::Launch {
                bin: bin.into(),
                timeout,
            },
        ))
    }

    /// A new run of the timing or diagnostic tool `cargo xtask <tool>` into `out`, which must not
    /// exist yet, launching `bin`, each launch within `timeout` unless it names its own deadline.
    /// Its `result.json` names the tool, records every launch and the [`provenance`](Self::provenance)
    /// the tool reads, and says whether the tool passed; the tool's own report sits beside it.
    pub fn tool(
        root: &Path,
        out: &Path,
        tool: &str,
        bin: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        ensure(!out.exists(), format!("{tool} output must be new"))?;
        fs::create_dir_all(out)?;
        let mut run = Self::new(
            root,
            out,
            tool,
            Mode::Launch {
                bin: bin.into(),
                timeout,
            },
        );
        run.tool = true;
        run.result = json!({"tool":tool,"status":"failed","launch_mode":MODE});
        Ok(run)
    }

    /// The provenance header every tool report carries, in one shape: how the editor was launched,
    /// the host it ran on, the build profile the binary itself reported in `events`' `startup`
    /// event, and the hashes of the binary and the lockfile. It is recorded in `result.json` too.
    pub fn provenance(&mut self, events: &[Value]) -> Result<Value> {
        let Mode::Launch { bin, .. } = &self.mode else {
            return Err("A replay launches no binary to describe".into());
        };
        let debug = events
            .iter()
            .find(|event| event["event"] == "startup")
            .and_then(|event| event["detail"]["debug_assertions"].as_bool())
            .ok_or("The editor reported no build profile at startup")?;
        let header = json!({
            "launch_mode":MODE,
            "platform":host(&self.root)?,
            "profile":if debug { "debug" } else { "release" },
            "binary_sha256":hash(bin)?,
            "lockfile_sha256":hash(&self.root.join("Cargo.lock"))?,
        });
        stamp(&mut self.result, &header);
        Ok(header)
    }

    /// A replay of the run recorded in `recorded`: its evidence is copied into `out`, which must
    /// not exist yet, and the scenario's checks run over the copy, so the recorded evidence is never
    /// rewritten. The recorded checks files are left out of the copy: every one the replay's output
    /// holds, the replay wrote.
    pub fn replay(root: &Path, recorded: &Path, out: &Path, scenario: &str) -> Result<Self> {
        ensure(!out.exists(), "Replay output must be new")?;
        ensure(
            recorded.join("result.json").is_file(),
            format!("{} holds no smoke run", recorded.display()),
        )?;
        copy_evidence(recorded, out)?;
        let recorded = recorded.into();
        Ok(Self::new(root, out, scenario, Mode::Replay { recorded }))
    }

    /// A run of its own in `<out>/<name>`, in the same mode: a scenario made of whole smoke runs.
    pub fn child(&self, name: &str) -> Result<Self> {
        let out = self.out.join(name);
        match &self.mode {
            Mode::Launch { bin, timeout } => {
                Self::start(&self.root, &out, &self.scenario, bin, *timeout)
            }
            Mode::Replay { recorded } => {
                ensure(
                    out.join("result.json").is_file(),
                    format!("The recorded run has no {name} run"),
                )?;
                let recorded = recorded.join(name);
                Ok(Self::new(
                    &self.root,
                    &out,
                    &self.scenario,
                    Mode::Replay { recorded },
                ))
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn out(&self) -> &Path {
        &self.out
    }

    pub fn scenario(&self) -> &str {
        &self.scenario
    }

    /// The sources [`Run::hash`] hashed: what the run opens.
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    /// Whether each launch is the recorded one rather than a new process.
    pub fn replaying(&self) -> bool {
        matches!(self.mode, Mode::Replay { .. })
    }

    /// In a replay, what the recorded run holds at `path`, a path inside this run's output: a
    /// check the replay cannot take again reads what the recorded run found.
    pub fn recorded(&self, path: &Path) -> Option<PathBuf> {
        let Mode::Replay { recorded } = &self.mode else {
            return None;
        };
        Some(recorded.join(path.strip_prefix(&self.out).ok()?))
    }

    /// The paragraph `reproduce.md` gives this scenario.
    pub fn note(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
    }

    /// Add one record to `result.json`.
    pub fn record(&mut self, key: &str, value: Value) {
        self.result[key] = value;
    }

    /// What this run has recorded under `key` so far; `Null` when nothing has.
    pub fn recorded_value(&self, key: &str) -> &Value {
        &self.result[key]
    }

    /// Record the hashes of the sources the run opens, of the binary it launches and of the
    /// lockfile it was built from. [`Run::sources_unchanged`] checks the sources against them.
    pub fn hash(&mut self, sources: &[PathBuf]) -> Result {
        let mut hashes = serde_json::Map::new();
        for source in sources {
            hashes.insert(file_name(source)?, json!(hash(source)?));
        }
        self.result["fixture_hashes"] = json!(hashes);
        if let Mode::Launch { bin, .. } = &self.mode {
            self.result["binary_sha256"] = json!(hash(bin)?);
        }
        self.result["lockfile_sha256"] = json!(hash(&self.root.join("Cargo.lock"))?);
        self.sources = sources.to_vec();
        Ok(())
    }

    /// Every hashed source still has the hash it had before the run: the originals are read-only.
    pub fn sources_unchanged(&self) -> Result {
        for source in &self.sources {
            ensure(
                json!(hash(source)?) == self.result["fixture_hashes"][file_name(source)?],
                "Source changed",
            )?;
        }
        Ok(())
    }

    /// Make one launch and return where it recorded itself and what its watcher returned. The
    /// launch is recorded before the editor is waited for, with its exit code once it has one, and a
    /// launch that does not exit as it must — successfully, or with the code it [`exits`] with — is
    /// an error. A launch its watcher is done with while the editor still runs is ended here and
    /// listed as `stopped`. A replay makes no process: the evidence directory is the recorded one.
    ///
    /// [`exits`]: Launch::exits
    pub fn launch(&mut self, launch: Launch) -> Result<Launched> {
        let dir = launch.dir(&self.out)?;
        let listed = launch.listed(&self.out);
        let Mode::Launch { bin, timeout } = &self.mode else {
            ensure(
                dir.is_dir(),
                format!("The recorded run has no {} directory", dir.display()),
            )?;
            return Ok(Launched {
                dir,
                watched: Value::Null,
            });
        };
        let (bin, deadline) = (bin.clone(), launch.deadline.unwrap_or(*timeout));
        // A script with a secret lives in this directory until the launch has exited.
        let mut scratch = None;
        let script = match &launch.script {
            Some((script, file)) => {
                let path = match file {
                    ScriptFile::Kept(name) => self.out.join(name),
                    ScriptFile::Secret(kept) => {
                        write_json(&self.out.join("script.json"), kept)?;
                        scratch
                            .insert(tempfile::tempdir()?)
                            .path()
                            .join("script.json")
                    }
                };
                write_json(&path, script)?;
                Some(path)
            }
            None => None,
        };
        // The recorded command is what actually runs, hidden-window flag included.
        let args = editor_args(&launch.arguments(&self.out, script.as_deref()));
        let command = std::iter::once(bin.as_os_str())
            .chain(args.iter().map(OsString::as_os_str))
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let started = Instant::now();
        let mut child = spawn(&self.root, &bin, &args, &self.out.join(&launch.log))?;
        let index = self.launches.len();
        self.launches
            .push(json!({"evidence":listed,"log":launch.log,"command":command,"exit_code":null}));
        let (status, watched) = match launch.watch {
            Some(watcher) => watcher(&mut child, started, deadline)?,
            None => (Some(wait(&mut child, deadline)?), Value::Null),
        };
        if let Some(file) = &launch.keep {
            write_json(&self.out.join(file), &watched)?;
        }
        // An editor the watcher is done with is ended before the launch counts as over.
        drop(child);
        drop(scratch);
        let Some(status) = status else {
            self.launches[index]["stopped"] = json!(true);
            return Ok(Launched { dir, watched });
        };
        self.launches[index]["exit_code"] = json!(status.code());
        let who = if listed == "app" {
            "Application".to_owned()
        } else {
            format!("Launch {}", index + 1)
        };
        match launch.exit {
            None => ensure(
                status.success(),
                format!("{who} exit {status}; see {}", launch.log),
            )?,
            Some(code) => ensure(
                status.code() == Some(code),
                format!("{who} exit {status}, not {code}; see {}", launch.log),
            )?,
        }
        Ok(Launched { dir, watched })
    }

    /// Record a launch made by a child run, with what that run recorded about itself.
    pub fn record_launches(&mut self, child: &Path, extra: Value) -> Result {
        let recorded = read_json(&child.join("result.json")).unwrap_or(Value::Null);
        let directory = child
            .strip_prefix(&self.out)
            .map_err(|_| "A child run outside its parent")?
            .to_string_lossy()
            .into_owned();
        for launch in recorded["launches"].as_array().into_iter().flatten() {
            let mut launch = launch.clone();
            launch["directory"] = json!(directory);
            launch["status"] = recorded["status"].clone();
            launch["error"] = recorded["error"].clone();
            for (key, value) in extra.as_object().into_iter().flatten() {
                launch[key] = value.clone();
            }
            self.launches.push(launch);
        }
        Ok(())
    }

    /// Run the scenario's body and finish with its outcome.
    pub fn check(mut self, body: impl FnOnce(&mut Run) -> Result) -> Result {
        let outcome = body(&mut self);
        self.finish(outcome, |_| Ok(()))
    }

    /// Write the outcome. A launched run writes `result.json` and `reproduce.md`, then runs
    /// `after` over the output directory, and a failure `after` returns replaces the outcome in
    /// `result.json`. A replay writes `replay.json`, its verdict over the copy, and nothing else.
    pub fn finish(mut self, outcome: Result, after: impl FnOnce(&Path) -> Result) -> Result {
        if self.replaying() {
            let mut replay = json!({"scenario":self.scenario,"status":"passed"});
            if let Err(error) = &outcome {
                replay["status"] = json!("failed");
                replay["error"] = json!(error.to_string());
            }
            write_json(&self.out.join("replay.json"), &replay)?;
            println!("{}", serde_json::to_string_pretty(&replay)?);
            return outcome;
        }
        self.result["launches"] = json!(self.launches);
        match &outcome {
            Ok(()) => self.result["status"] = json!("passed"),
            Err(error) => self.result["error"] = json!(error.to_string()),
        }
        write_json(&self.out.join("result.json"), &self.result)?;
        fs::write(self.out.join("reproduce.md"), self.reproduce()?)?;
        if let Err(error) = after(&self.out) {
            self.result["status"] = json!("failed");
            self.result["error"] = json!(error.to_string());
            write_json(&self.out.join("result.json"), &self.result)?;
            return Err(error);
        }
        println!("{}", serde_json::to_string_pretty(&self.result)?);
        outcome
    }

    fn reproduce(&self) -> Result<String> {
        let scenario = &self.scenario;
        let status = self.result["status"].as_str().unwrap_or_default();
        let focus = "on macOS each launch copies the binary into a temporary background-only bundle and the editor runs with `--hidden-window`, so no window is ever placed on the desktop. Running an argument array below directly bypasses that focus protection.";
        let mut text = if self.tool {
            format!(
                "# {scenario} run\n\nStatus: {status}.\n\nLaunch mode: {MODE}. Reproduce with `cargo xtask {scenario} --output NEW_DIR --binary PATH` and this run's own options; {focus}\n\n"
            )
        } else {
            format!(
                "# Smoke run\n\nScenario: {scenario}. Status: {status}.\n\nLaunch mode: {MODE}. Reproduce with `cargo xtask smoke --scenario {scenario} --output NEW_DIR --binary PATH`; {focus} `cargo xtask smoke --verify-only DIR --output NEW_DIR` reruns this run's checks over a copy of it without launching anything.\n\n"
            )
        };
        if let Some(note) = &self.note {
            text.push_str(note);
            text.push_str("\n\n");
        }
        for launch in &self.launches {
            text.push_str(&format!(
                "Launch `{}`:\n\n```json\n{}\n```\n\n",
                launch["evidence"]
                    .as_str()
                    .or(launch["log"].as_str())
                    .unwrap_or_default(),
                serde_json::to_string_pretty(&launch["command"])?
            ));
        }
        if !self.tool {
            text.push_str("Actual renderer readback.\n");
        }
        Ok(text)
    }
}

/// Write a [`Run::provenance`] header's fields into a report.
pub fn stamp(report: &mut Value, header: &Value) {
    for (key, value) in header.as_object().into_iter().flatten() {
        report[key] = value.clone();
    }
}

fn file_name(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .ok_or("A source has no file name")?
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn sleeping_child() {
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn timeout_reaps_child() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = spawn(
            &root().unwrap(),
            &std::env::current_exe().unwrap(),
            &[
                "--ignored".into(),
                "--exact".into(),
                "scenario::launch::tests::sleeping_child".into(),
            ],
            &tmp.path().join("child.log"),
        )
        .unwrap();
        assert!(
            wait(&mut child, Duration::from_millis(50))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        child.child.kill().unwrap();
        assert!(child.child.wait().is_ok());
    }

    /// Every runner's launches, built from their parts in any order, give the argument arrays the
    /// runners wrote out by hand before they shared this builder.
    #[test]
    fn a_launch_gives_each_runners_own_argument_array() {
        let strings = |args: Vec<OsString>| -> Vec<String> {
            args.into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let (out, fixture, script) = (
            Path::new("/out"),
            Path::new("/f/orientation-1.jpg"),
            Path::new("/out/script.json"),
        );
        // A plain developer scenario, as `smoke::run_sources` wrote it for `gallery`.
        let gallery = Launch::app()
            .window(["1440", "1000"])
            .open_all(&[fixture.into()])
            .developer()
            .script("script.json", json!([]));
        assert_eq!(
            strings(gallery.arguments(out, Some(script))),
            [
                "--evidence-dir",
                "/out/app",
                "--developer",
                "--open",
                "/f/orientation-1.jpg",
                "--evidence-script",
                "/out/script.json",
                "--window-size",
                "1440",
                "1000"
            ]
        );
        // A plain scenario with no script opens every source in order and sets no window.
        let repeated = Launch::app().open_all(&[fixture.into(), fixture.into()]);
        assert_eq!(
            strings(repeated.arguments(out, None)),
            [
                "--evidence-dir",
                "/out/app",
                "--open",
                "/f/orientation-1.jpg",
                "--open",
                "/f/orientation-1.jpg"
            ]
        );
        // `capabilities`: the proof endpoint between the developer flag and the source.
        let capabilities = Launch::app()
            .developer()
            .proof_endpoint("http://127.0.0.1:1")
            .open_all(&[fixture.into()])
            .window(["1440", "900"]);
        assert_eq!(
            strings(capabilities.arguments(out, Some(Path::new("/tmp/s.json")))),
            [
                "--evidence-dir",
                "/out/app",
                "--developer",
                "--proof-endpoint",
                "http://127.0.0.1:1",
                "--open",
                "/f/orientation-1.jpg",
                "--evidence-script",
                "/tmp/s.json",
                "--window-size",
                "1440",
                "900"
            ]
        );
        // `unavailable`'s second launch: an existing catalog and a disabled module come first,
        // and a launch with no script still sets its window.
        let reopened = Launch::named("launch2")
            .open_all(&[fixture.into()])
            .window(["1440", "900"])
            .disable("luxforge.crop")
            .catalog(Path::new("/out/launch1/catalog.sqlite"));
        assert_eq!(
            strings(reopened.arguments(out, None)),
            [
                "--catalog",
                "/out/launch1/catalog.sqlite",
                "--disable-module",
                "luxforge.crop",
                "--evidence-dir",
                "/out/launch2",
                "--open",
                "/f/orientation-1.jpg",
                "--window-size",
                "1440",
                "900"
            ]
        );
        assert_eq!(
            (reopened.listed(out), reopened.log.as_str()),
            (json!("launch2"), "launch2.log")
        );
        let app = Launch::app();
        assert_eq!(
            (app.listed(out), app.log.as_str()),
            (json!("app"), "subprocess.log")
        );
    }

    /// The timing tools' launches: a data root takes its place in the order, an evidence
    /// directory can be named, an ordinary launch passes none and records itself by its data root,
    /// and a tool's own order puts the same flags where its launches always put them.
    #[test]
    fn a_launch_takes_a_data_root_an_evidence_dir_and_an_order() {
        let strings = |args: Vec<OsString>| -> Vec<String> {
            args.into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let (out, fixture) = (Path::new("/out"), PathBuf::from("/f/photo.jpg"));
        let degraded = Launch::ordinary("diagnostics-unavailable")
            .open_all(std::slice::from_ref(&fixture))
            .catalog(Path::new("/out/degraded.sqlite"))
            .data_root(Path::new("/out/not-a-directory"));
        assert_eq!(
            strings(degraded.arguments(out, None)),
            [
                "--data-root",
                "/out/not-a-directory",
                "--catalog",
                "/out/degraded.sqlite",
                "--open",
                "/f/photo.jpg"
            ]
        );
        assert_eq!(degraded.listed(out), Value::Null);
        assert_eq!(
            degraded.dir(out).unwrap(),
            Path::new("/out/not-a-directory")
        );
        assert!(Launch::ordinary("idle").dir(out).is_err());
        let refused = Launch::named("initialization").evidence_dir(Path::new("/out/file/evidence"));
        assert_eq!(
            strings(refused.arguments(out, None)),
            ["--evidence-dir", "/out/file/evidence"]
        );
        assert_eq!(refused.listed(out), json!("file/evidence"));
        assert_eq!(refused.dir(out).unwrap(), Path::new("/out/file/evidence"));
        // The same flags in another order: the script before the photograph, the catalog after
        // the evidence directory and the developer flag last.
        let order = [
            Flag::Evidence,
            Flag::Catalog,
            Flag::DataRoot,
            Flag::Script,
            Flag::Open,
            Flag::Developer,
            Flag::Disable,
            Flag::Endpoint,
            Flag::Window,
        ];
        let held = Launch::named("hold")
            .developer()
            .open_all(std::slice::from_ref(&fixture))
            .catalog(Path::new("/out/held.sqlite"))
            .order(order);
        assert_eq!(
            strings(held.arguments(out, Some(Path::new("/out/hold-script.json")))),
            [
                "--evidence-dir",
                "/out/hold",
                "--catalog",
                "/out/held.sqlite",
                "--evidence-script",
                "/out/hold-script.json",
                "--open",
                "/f/photo.jpg",
                "--developer"
            ]
        );
    }

    #[test]
    #[should_panic(expected = "names every flag once")]
    fn an_order_that_leaves_a_flag_out_is_refused() {
        let mut order = ORDER;
        order[0] = Flag::Open;
        let _ = Launch::app().order(order);
    }

    /// The one watch loop ends on what it waits for before the editor's exit, runs its sample
    /// between polls, and fails past its deadline.
    #[test]
    fn a_watch_ends_on_what_it_reaches_or_fails_late() {
        let tmp = tempfile::tempdir().unwrap();
        let sleeper = || {
            spawn(
                &root().unwrap(),
                &std::env::current_exe().unwrap(),
                &[
                    "--ignored".into(),
                    "--exact".into(),
                    "scenario::launch::tests::sleeping_child".into(),
                ],
                &tmp.path().join("child.log"),
            )
            .unwrap()
        };
        let mut child = sleeper();
        let samples = std::cell::Cell::new(0);
        let poll = Poll {
            every: Duration::from_millis(1),
            from: Instant::now(),
            deadline: Duration::from_secs(20),
            late: "late",
        };
        let reached = watch(
            &mut child,
            poll,
            || Ok((samples.get() >= 3).then_some(samples.get())),
            |_| {
                samples.set(samples.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert!(matches!(reached, Watched::Reached(3)));
        let poll = Poll {
            every: Duration::from_millis(1),
            from: Instant::now(),
            deadline: Duration::from_millis(20),
            late: "The watch ran late",
        };
        let error = until_exit(&mut child, poll, |_| Ok(()))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "The watch ran late");
    }

    /// A watcher done with an editor that still runs: the launch ends it, lists it as stopped
    /// with no exit code, returns and keeps what the watcher recorded, and an ordinary launch
    /// records itself by its data root.
    #[test]
    fn a_launch_its_watcher_stops_is_listed_as_stopped() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("run");
        let mut run = Run::tool(
            &root().unwrap(),
            &out,
            "unit",
            &std::env::current_exe().unwrap(),
            Duration::from_secs(20),
        )
        .unwrap();
        let data = tmp.path().join("data");
        let launched = run
            .launch(
                Launch::ordinary("idle")
                    .data_root(&data)
                    .deadline(Duration::from_millis(1))
                    .watch(Box::new(|_, _, deadline| {
                        Ok((None, json!({"deadline_ms":deadline.as_millis() as u64})))
                    }))
                    .keep("idle.json"),
            )
            .unwrap();
        assert_eq!(launched.dir, data);
        assert_eq!(launched.watched, json!({"deadline_ms":1}));
        assert_eq!(read_json(&out.join("idle.json")).unwrap(), launched.watched);
        let listed = &run.launches[0];
        assert_eq!(listed["evidence"], Value::Null);
        assert_eq!(listed["log"], "idle.log");
        assert_eq!(listed["stopped"], true);
        assert_eq!(listed["exit_code"], Value::Null);
        assert!(
            listed["command"]
                .as_array()
                .unwrap()
                .contains(&json!("--data-root"))
        );
        // A launch that must refuse with one code and exits with another is the run's failure.
        let error = run
            .launch(Launch::named("refused").exits(2))
            .unwrap_err()
            .to_string();
        assert!(error.contains(", not 2; see refused.log"), "{error}");
        assert!(run.finish(Err("stopped".into()), |_| Ok(())).is_err());
        let result = read_json(&out.join("result.json")).unwrap();
        assert_eq!(result["tool"], "unit");
        assert_eq!(result["launches"].as_array().unwrap().len(), 2);
        assert!(
            fs::read_to_string(out.join("reproduce.md"))
                .unwrap()
                .starts_with("# unit run")
        );
    }

    #[test]
    fn a_launch_that_cannot_start_leaves_its_script_and_a_failed_result() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("run");
        let mut run = Run::start(
            &root().unwrap(),
            &out,
            "unit",
            &tmp.path().join("absent-binary"),
            Duration::from_millis(100),
        )
        .unwrap();
        assert!(
            Run::start(
                &root().unwrap(),
                &out,
                "unit",
                Path::new("x"),
                Duration::ZERO
            )
            .is_err()
        );
        let launch = Launch::named("launch2")
            .open_all(&[PathBuf::from("/f.jpg")])
            .window(["1440", "900"])
            .catalog(Path::new("/c.sqlite"))
            .script(
                "script2.json",
                luxforge_evidence::write(&[luxforge_evidence::Step::wait(1)]),
            )
            .disable("luxforge.crop");
        // The binary is absent, so the spawn fails, and nothing is recorded as started.
        assert!(run.launch(launch).is_err());
        assert!(run.launches.is_empty());
        assert_eq!(
            read_json(&out.join("script2.json")).unwrap(),
            json!([{"wait":{"ms":1}}])
        );
        run.record("launch2", json!({"checked":true}));
        assert!(run.finish(Err("stopped".into()), |_| Ok(())).is_err());
        let result = read_json(&out.join("result.json")).unwrap();
        assert_eq!(result["status"], "failed");
        assert_eq!(result["error"], "stopped");
        assert_eq!(result["launches"], json!([]));
        assert_eq!(result["launch2"], json!({"checked":true}));
        assert!(out.join("reproduce.md").is_file());
    }

    /// A launch that starts is listed with its command and its exit code, and one that exits
    /// unsuccessfully is the run's failure. The test binary refuses the editor's flags, so it is a
    /// process that starts and fails.
    #[test]
    fn a_started_launch_is_listed_with_its_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("run");
        let bin = std::env::current_exe().unwrap();
        let mut run = Run::start(
            &root().unwrap(),
            &out,
            "unit",
            &bin,
            Duration::from_secs(20),
        )
        .unwrap();
        let error = run
            .launch(Launch::named("launch1"))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("Launch 1 exit"), "{error}");
        assert_eq!(run.launches.len(), 1);
        let listed = &run.launches[0];
        assert_eq!(listed["evidence"], "launch1");
        assert!(listed["exit_code"].as_i64().is_some_and(|code| code != 0));
        assert_eq!(listed["command"][1], "--hidden-window");
        assert!(out.join("launch1.log").is_file());
    }

    #[test]
    fn a_replay_copies_the_recorded_run_and_launches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let recorded = tmp.path().join("recorded");
        fs::create_dir_all(recorded.join("launch1")).unwrap();
        write_json(&recorded.join("result.json"), &json!({"status":"passed"})).unwrap();
        fs::write(recorded.join("launch1/events.jsonl"), "").unwrap();
        write_json(
            &recorded.join("launch1/unit-checks.json"),
            &json!({"stale":true}),
        )
        .unwrap();
        let out = tmp.path().join("replayed");
        let mut run = Run::replay(&root().unwrap(), &recorded, &out, "unit").unwrap();
        assert!(run.replaying());
        // The recorded checks stay with the recording, where a check the replay cannot take
        // again reads them; the copy holds only what the replay writes.
        assert!(out.join("launch1/events.jsonl").is_file());
        assert!(!out.join("launch1/unit-checks.json").exists());
        assert_eq!(
            run.recorded(&out.join("launch1/unit-checks.json")),
            Some(recorded.join("launch1/unit-checks.json"))
        );
        assert_eq!(
            run.launch(Launch::named("launch1")).unwrap().dir,
            out.join("launch1")
        );
        assert!(run.launch(Launch::named("launch2")).is_err());
        assert!(run.finish(Ok(()), |_| Err("not run".into())).is_ok());
        assert_eq!(
            read_json(&out.join("replay.json")).unwrap(),
            json!({"scenario":"unit","status":"passed"})
        );
        // The recorded result is the recorded one still, in the copy and at the source.
        assert_eq!(
            read_json(&out.join("result.json")).unwrap()["status"],
            "passed"
        );
        assert!(Run::replay(&root().unwrap(), &recorded, &out, "unit").is_err());
    }
}
