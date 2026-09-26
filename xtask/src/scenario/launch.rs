//! The launch envelope every smoke runner shares: a [`Run`] owns one output directory, hashes what
//! the run opens and the binary that opens it, makes each editor [`Launch`] the scenario asks for,
//! records every launch in `result.json`'s `launches` list, checks the sources are unchanged and
//! writes `result.json` and `reproduce.md`.
//!
//! A run can also be a **replay**: the same scenario code over a copy of a recorded run, where each
//! launch is the one recorded there rather than a new editor process. Everything after a launch —
//! the preamble, the verifier, the checks files — runs exactly as it does after a real one, which
//! is what makes `smoke --verify-only` a replay of the checks and not a second implementation of
//! them.
use crate::launch::{Background, MODE, editor_args};
use crate::*;
use std::{
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

/// Launch the editor itself: the caller's arguments with the hidden-window flag the harness always
/// passes. Children that are not the editor (the probe binary, xtask's own test children) use
/// [`spawn`] directly.
pub fn spawn_editor(root: &Path, bin: &Path, args: &[OsString], log: &Path) -> Result<Guard> {
    spawn(root, bin, &editor_args(args), log)
}

pub fn wait(child: &mut Guard, timeout: Duration) -> Result<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.child.try_wait()? {
            return Ok(status);
        }
        ensure(
            start.elapsed() < timeout,
            "Child timed out; killed and reaped",
        )?;
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits for a launched editor in place of [`wait`], and returns what it recorded while it did.
pub type Watcher = Box<dyn FnOnce(&mut Guard, Duration) -> Result<(ExitStatus, Value)>>;

enum ScriptFile {
    /// Written into the output directory under this name.
    Kept(String),
    /// Written to a temporary file that goes with the launch, and this copy kept as `script.json`.
    Secret(Value),
}

/// One editor launch: what it opens and with which flags, where its evidence and log go, and the
/// evidence script it runs. Its arguments are always assembled in one order: `--catalog`, each
/// `--disable-module`, `--evidence-dir`, `--developer`, `--proof-endpoint`, each `--open`,
/// `--evidence-script` and `--window-size`.
pub struct Launch {
    evidence: String,
    log: String,
    catalog: Option<PathBuf>,
    disabled: Vec<String>,
    developer: bool,
    endpoint: Option<String>,
    sources: Vec<PathBuf>,
    script: Option<(Value, ScriptFile)>,
    window: Option<[String; 2]>,
    watch: Option<(String, Watcher)>,
}

impl Launch {
    fn new(evidence: &str, log: &str) -> Self {
        Self {
            evidence: evidence.into(),
            log: log.into(),
            catalog: None,
            disabled: Vec::new(),
            developer: false,
            endpoint: None,
            sources: Vec::new(),
            script: None,
            window: None,
            watch: None,
        }
    }

    /// A scenario's one ordinary launch: its evidence in `app/`, its console in `subprocess.log`.
    pub fn app() -> Self {
        Self::new("app", "subprocess.log")
    }

    /// One of several launches: its evidence in `<name>/`, its console in `<name>.log`.
    pub fn named(name: &str) -> Self {
        Self::new(name, &format!("{name}.log"))
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

    /// Wait for the editor with `watcher` instead of [`wait`], and write what it returns into the
    /// output directory as `file`.
    pub fn watch(mut self, file: &str, watcher: Watcher) -> Self {
        self.watch = Some((file.into(), watcher));
        self
    }

    /// The editor's arguments for this launch, before the hidden-window flag every harness launch
    /// adds, with its evidence in `evidence` and its script, if it has one, at `script`.
    fn arguments(&self, evidence: &Path, script: Option<&Path>) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(catalog) = &self.catalog {
            args.extend(["--catalog".into(), catalog.into()]);
        }
        for module in &self.disabled {
            args.extend(["--disable-module".into(), module.into()]);
        }
        args.extend(["--evidence-dir".into(), evidence.into()]);
        if self.developer {
            args.push("--developer".into());
        }
        if let Some(url) = &self.endpoint {
            args.extend(["--proof-endpoint".into(), url.into()]);
        }
        for source in &self.sources {
            args.extend(["--open".into(), source.into()]);
        }
        if let Some(script) = script {
            args.extend(["--evidence-script".into(), script.into()]);
        }
        if let Some([width, height]) = &self.window {
            args.extend(["--window-size".into(), width.into(), height.into()]);
        }
        args
    }
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

    /// Make one launch and return its evidence directory. The launch is recorded before the
    /// editor is waited for, with its exit code once it has one, and a launch that does not exit
    /// successfully is an error. A replay makes no process: the evidence directory is the recorded
    /// one.
    pub fn launch(&mut self, launch: Launch) -> Result<PathBuf> {
        let evidence = self.out.join(&launch.evidence);
        let Mode::Launch { bin, timeout } = &self.mode else {
            ensure(
                evidence.is_dir(),
                format!("The recorded run has no {} directory", launch.evidence),
            )?;
            return Ok(evidence);
        };
        let (bin, timeout) = (bin.clone(), *timeout);
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
        let args = launch.arguments(&evidence, script.as_deref());
        // The recorded command is what actually runs, hidden-window flag included.
        let args = editor_args(&args);
        let command = std::iter::once(bin.as_os_str())
            .chain(args.iter().map(OsString::as_os_str))
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut child = spawn(&self.root, &bin, &args, &self.out.join(&launch.log))?;
        let index = self.launches.len();
        self.launches.push(
            json!({"evidence":launch.evidence,"log":launch.log,"command":command,"exit_code":null}),
        );
        let status = match launch.watch {
            Some((file, watcher)) => {
                let (status, recorded) = watcher(&mut child, timeout)?;
                write_json(&self.out.join(file), &recorded)?;
                status
            }
            None => wait(&mut child, timeout)?,
        };
        drop(scratch);
        self.launches[index]["exit_code"] = json!(status.code());
        let who = if launch.evidence == "app" {
            "Application".to_owned()
        } else {
            format!("Launch {}", index + 1)
        };
        ensure(status.success(), format!("{who} exit {status}"))?;
        Ok(evidence)
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
        let mut text = format!(
            "# Smoke run\n\nScenario: {scenario}. Status: {}.\n\nLaunch mode: {MODE}. Reproduce with `cargo xtask smoke --scenario {scenario} --output NEW_DIR --binary PATH`; on macOS each launch copies the binary into a temporary background-only bundle and the editor runs with `--hidden-window`, so no window is ever placed on the desktop. Running an argument array below directly bypasses that focus protection. `cargo xtask smoke --verify-only DIR --output NEW_DIR` reruns this run's checks over a copy of it without launching anything.\n\n",
            self.result["status"].as_str().unwrap_or_default()
        );
        if let Some(note) = &self.note {
            text.push_str(note);
            text.push_str("\n\n");
        }
        for launch in &self.launches {
            text.push_str(&format!(
                "Launch `{}`:\n\n```json\n{}\n```\n\n",
                launch["evidence"].as_str().unwrap_or_default(),
                serde_json::to_string_pretty(&launch["command"])?
            ));
        }
        text.push_str("Actual renderer readback.\n");
        Ok(text)
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
        let (evidence, fixture, script) = (
            Path::new("/out/app"),
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
            strings(gallery.arguments(evidence, Some(script))),
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
            strings(repeated.arguments(evidence, None)),
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
            strings(capabilities.arguments(evidence, Some(Path::new("/tmp/s.json")))),
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
            strings(reopened.arguments(Path::new("/out/launch2"), None)),
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
            (reopened.evidence.as_str(), reopened.log.as_str()),
            ("launch2", "launch2.log")
        );
        let app = Launch::app();
        assert_eq!(
            (app.evidence.as_str(), app.log.as_str()),
            ("app", "subprocess.log")
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
            run.launch(Launch::named("launch1")).unwrap(),
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
