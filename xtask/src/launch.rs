//! Non-interactive test launches must not activate the owner's desktop.
use crate::*;

/// The host-wide lock file two timing runs cannot both hold. It lives in the OS temporary
/// directory rather than an output directory so it is shared by every checkout, worktree and
/// terminal on this machine: a figure measured while another run was launching editors is not a
/// figure of this machine.
const LOCK: &str = "lightwell-timing.lock";

/// How a timing child is told that its parent already holds the lock. Without it, every timing
/// component `verify` spawns would refuse the run that started it.
const GATE_ENV: &str = "LIGHTWELL_TIMING_GATE";

/// The one-minute load average above which a timing figure is recorded as `unreliable` rather than
/// compared against a target. The baselines in `docs/specs/performance.md` were taken between 2.3
/// and 5.7 on this fourteen-core host.
pub const LOAD_THRESHOLD: f64 = 8.0;

/// Whether a figure taken at this load may be compared against a target at all.
pub fn unreliable(load: Option<f64>) -> bool {
    load.is_some_and(|value| value > LOAD_THRESHOLD)
}

/// Exclusive, host-wide permission to run a timing component.
///
/// Held for the whole of a timing run and released when it is dropped, including on failure. A
/// process killed outright cannot drop it, so a lock file whose pid is no longer alive is stale and
/// is taken over rather than believed forever.
#[derive(Debug)]
pub struct TimingGate {
    path: PathBuf,
    /// False for a gate inherited from the parent process, which releases it.
    owned: bool,
}

/// Whether a process with this id still exists. `ps` answers for a process this user cannot signal
/// as well as for its own. When `ps` cannot be run at all the answer is "alive", because refusing a
/// timing run costs a rerun while stealing a live one's lock costs the measurement.
fn alive(pid: u32) -> bool {
    match Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
    {
        Ok(out) => out.status.success() && !out.stdout.is_empty(),
        Err(_) => true,
    }
}

fn pid_of(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

impl TimingGate {
    fn file() -> PathBuf {
        std::env::temp_dir().join(LOCK)
    }

    /// The refusal a held lock produces, in the one wording the message, the summary entry and the
    /// documentation all use.
    pub fn refusal(pid: u32) -> String {
        format!("another timing run (pid {pid}) is alive")
    }

    /// Take the lock, or name the live pid that holds it.
    pub fn take() -> Result<std::result::Result<Self, u32>> {
        Self::take_at(&Self::file(), std::env::var(GATE_ENV).ok().as_deref())
    }

    /// Take the lock or fail with the refusal.
    pub fn acquire() -> Result<Self> {
        match Self::take()? {
            Ok(gate) => Ok(gate),
            Err(pid) => Err(Self::refusal(pid).into()),
        }
    }

    /// The live pid holding the lock, without taking it: how a run refuses early instead of doing
    /// minutes of other work first. A stale file is cleared here as it would be by a take.
    pub fn holder() -> Result<Option<u32>> {
        match Self::take()? {
            // Taking and immediately releasing is the only honest probe: a lock that was free a
            // moment ago may be taken before the caller reaches its timing components, which is why
            // the caller takes it for real later as well.
            Ok(_) => Ok(None),
            Err(pid) => Ok(Some(pid)),
        }
    }

    /// The take, against an explicit path and inherited-gate value, so the stale-pid decision can be
    /// tested without a second process.
    fn take_at(path: &Path, inherited: Option<&str>) -> Result<std::result::Result<Self, u32>> {
        // Three attempts: each one either takes the lock, refuses to a live pid, or clears exactly
        // the stale file it read. Another process taking the cleared file in between is a refusal,
        // not a shared lock, so the loop terminates with one of the two answers.
        for _ in 0..3 {
            // The lock file appears with its pid already in it, never empty: written beside the lock
            // and linked into place, because linking fails when the name is taken. Creating the file
            // and then writing the pid would leave a window in which another run read it as a lock
            // with no owner and cleared it.
            static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let staging = path.with_file_name(format!(
                "{LOCK}.{}.{}",
                std::process::id(),
                ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            fs::write(&staging, format!("{}\n", std::process::id()))?;
            let linked = fs::hard_link(&staging, path);
            let _ = fs::remove_file(&staging);
            match linked {
                Ok(()) => {
                    return Ok(Ok(Self {
                        path: path.into(),
                        owned: true,
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let held = fs::read_to_string(path).unwrap_or_default();
            if inherited.is_some_and(|value| value.trim() == held.trim() && !held.trim().is_empty())
            {
                return Ok(Ok(Self {
                    path: path.into(),
                    owned: false,
                }));
            }
            match pid_of(&held) {
                Some(pid) if alive(pid) => return Ok(Err(pid)),
                // A lock file whose pid is dead, or that holds nothing a pid can be read from, is
                // stale. Clear it only while it still holds what was just read, so a lock taken in
                // the meantime is never removed.
                _ => {
                    if fs::read_to_string(path).unwrap_or_default() == held {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
        Err(format!(
            "The timing lock {} could not be taken or cleared",
            path.display()
        )
        .into())
    }

    /// The name and value a timing child needs so it shares this run's lock instead of refusing it.
    pub fn child_env(&self) -> (&'static str, String) {
        (
            GATE_ENV,
            self.pid().map(|pid| pid.to_string()).unwrap_or_default(),
        )
    }

    /// The pid recorded in the lock file this gate holds.
    pub fn pid(&self) -> Option<u32> {
        pid_of(&fs::read_to_string(&self.path).ok()?)
    }
}

impl Drop for TimingGate {
    fn drop(&mut self) {
        if self.owned {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Keep the temporary bundle alive until its child has exited and been reaped.
pub struct Background {
    pub executable: PathBuf,
    #[cfg(target_os = "macos")]
    _bundle: tempfile::TempDir,
}

pub const MODE: &str = if cfg!(target_os = "macos") {
    "macos-background-bundle"
} else {
    "direct"
};

/// The window the editor creates for an automated launch is never placed on screen. The background
/// bundle keeps the process from activating; this keeps its window off the desktop as well, so a
/// run cannot flash over whatever the owner is doing. Renderer readbacks are unaffected.
const HIDDEN_WINDOW: &str = "--hidden-window";

/// The arguments an automated editor launch runs with: the runner's own, behind the hidden-window
/// flag. Every harness launch of the editor goes through this, so the flag has one home and the
/// recorded command array shows exactly what ran.
pub fn editor_args(args: &[OsString]) -> Vec<OsString> {
    std::iter::once(HIDDEN_WINDOW.into())
        .chain(args.iter().cloned())
        .collect()
}

/// How the frontmost application is read. LaunchServices answers it directly, so no Automation
/// permission is involved; `osascript` driving System Events would need one.
const FOCUS_METHOD: &str = "lsappinfo";

/// The frontmost application's display name, through LaunchServices.
#[cfg(target_os = "macos")]
pub fn frontmost() -> Result<String> {
    let asn = lsappinfo(&["front"])?;
    ensure(
        asn.starts_with("ASN:"),
        format!("lsappinfo named no frontmost application: {asn:?}"),
    )?;
    // `lsappinfo info -only name ASN:...` answers `"LSDisplayName"="Claude"`.
    let answer = lsappinfo(&["info", "-only", "name", &asn])?;
    let name = answer
        .split_once('=')
        .map(|(_, value)| value.trim().trim_matches('"'))
        .unwrap_or_default();
    ensure(
        !name.is_empty() && name != "[ NULL ]",
        format!("lsappinfo gave the frontmost application no name: {answer:?}"),
    )?;
    Ok(name.to_owned())
}

#[cfg(target_os = "macos")]
fn lsappinfo(args: &[&str]) -> Result<String> {
    let out = Command::new("lsappinfo").args(args).output()?;
    ensure(
        out.status.success(),
        format!("lsappinfo {args:?} exited {}", out.status),
    )?;
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// One launch's focus record, from the frontmost application before and after it.
fn record(before: &str, after: &str) -> Value {
    json!({"status":"checked","method":FOCUS_METHOD,"before":before,"after":after,"changed":before != after})
}

/// The frontmost application around one child launch.
///
/// An automated launch must never take the owner's desktop, so every runner records the pair and
/// fails when it changed. The check is only as strong as its reading: when the frontmost
/// application cannot be read the record says so and the run continues, because a missing check
/// that is visible is worth more than one that quietly passes.
pub struct Focus {
    before: std::result::Result<String, String>,
}

impl Focus {
    /// Read the frontmost application before a child starts.
    pub fn capture() -> Self {
        Self {
            before: read_frontmost(),
        }
    }

    /// Read it again once the launch is over and describe the pair.
    pub fn complete(&self) -> Value {
        if !cfg!(target_os = "macos") {
            return json!({"status":"not-checked","platform":std::env::consts::OS,"reason":"The frontmost application is read through macOS LaunchServices only"});
        }
        let unavailable =
            |error: &str| json!({"status":"unavailable","method":FOCUS_METHOD,"error":error});
        match (&self.before, read_frontmost()) {
            (Ok(before), Ok(after)) => record(before, &after),
            (Err(error), _) => unavailable(error),
            (_, Err(error)) => unavailable(&error),
        }
    }
}

fn read_frontmost() -> std::result::Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        frontmost().map_err(|error| error.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("the frontmost application is read on macOS only".into())
    }
}

/// Fail a run whose launch changed the frontmost application, naming both applications.
pub fn focus_unchanged(record: &Value) -> Result {
    ensure(
        record["changed"] != json!(true),
        format!(
            "The frontmost application changed across a launch: {} before, {} after. Automated launches must not take focus, and switching applications during a run fails it.",
            record["before"], record["after"]
        ),
    )
}

/// Fail a run when any of its launches changed the frontmost application.
pub fn focus_all_unchanged(records: &[Value]) -> Result {
    records.iter().try_for_each(focus_unchanged)
}

/// The focus verdict on a runner's own result: every launch it recorded, under `focus_check` or
/// `focus_checks`. Runners call it after their own checks, so a stolen desktop fails the run
/// without hiding what else the run found.
pub fn focus_verdict(result: &Value) -> Result {
    match result.get("focus_checks") {
        Some(Value::Array(records)) => focus_all_unchanged(records),
        _ => focus_unchanged(&result["focus_check"]),
    }
}

impl Background {
    pub fn new(binary: &Path) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            // Iced's winit runner activates ordinary unbundled executables. A background-only
            // bundle prevents activation before the event loop starts, while renderer readbacks
            // still use the native GPU. Do not use a symlink: Cocoa resolves it to the original
            // executable and loses the bundle identity. Never modify the built/packaged app.
            let bundle = tempfile::Builder::new()
                .prefix("lightwell-test-")
                .tempdir()?;
            let contents = bundle.path().join("Lightwell Test.app/Contents");
            fs::create_dir_all(contents.join("MacOS"))?;
            let executable = contents.join("MacOS/lightwell-test");
            fs::copy(binary, &executable)?;
            fs::write(
                contents.join("Info.plist"),
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>org.lightwell.background-test</string>
<key>CFBundleName</key><string>Lightwell Test</string>
<key>CFBundleExecutable</key><string>lightwell-test</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSBackgroundOnly</key><true/>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#,
            )?;
            Ok(Self {
                executable,
                _bundle: bundle,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(Self {
                executable: binary.into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_arguments_carry_the_hidden_window_flag() {
        let args: Vec<OsString> = vec!["--evidence-dir".into(), "out".into()];
        let built = editor_args(&args);
        assert_eq!(built[0], OsString::from(HIDDEN_WINDOW));
        assert_eq!(built[1..], args[..]);
    }

    #[test]
    fn a_changed_frontmost_application_fails_and_names_both() {
        let same = record("Claude", "Claude");
        assert_eq!(same["changed"], json!(false));
        assert_eq!(same["status"], json!("checked"));
        focus_unchanged(&same).unwrap();

        let changed = record("Claude", "Finder");
        assert_eq!(changed["changed"], json!(true));
        let message = focus_unchanged(&changed).unwrap_err().to_string();
        assert!(
            message.contains("Claude") && message.contains("Finder"),
            "the failure must name both applications: {message}"
        );
        assert!(
            focus_all_unchanged(&[same, changed])
                .unwrap_err()
                .to_string()
                .contains("Finder")
        );
    }

    #[test]
    fn an_unreadable_frontmost_application_is_recorded_without_failing() {
        let focus = Focus {
            before: Err("lsappinfo is unavailable".into()),
        };
        let recorded = focus.complete();
        if cfg!(target_os = "macos") {
            assert_eq!(recorded["status"], json!("unavailable"));
            assert_eq!(recorded["error"], json!("lsappinfo is unavailable"));
        } else {
            assert_eq!(recorded["status"], json!("not-checked"));
        }
        focus_unchanged(&recorded).unwrap();
    }

    /// A pid that certainly no longer exists: a child of this process, run to completion and reaped.
    fn dead_pid() -> u32 {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap true");
        pid
    }

    #[test]
    fn one_timing_run_at_a_time_and_a_dead_holder_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOCK);

        // A free lock is taken, and the file names this process.
        let held = TimingGate::take_at(&path, None).unwrap().unwrap();
        assert_eq!(held.pid(), Some(std::process::id()));

        // A second take while it is held refuses, naming the live pid.
        let refused = TimingGate::take_at(&path, None).unwrap().unwrap_err();
        assert_eq!(refused, std::process::id());
        assert!(
            TimingGate::refusal(refused).contains(&format!("pid {refused}")),
            "the refusal names the pid"
        );

        // A child told the pid in the file shares the lock instead of refusing it, and releasing
        // that inherited gate leaves the lock in place.
        let (key, value) = held.child_env();
        assert_eq!(key, GATE_ENV);
        let inherited = TimingGate::take_at(&path, Some(&value)).unwrap().unwrap();
        assert!(!inherited.owned);
        drop(inherited);
        assert!(path.is_file(), "an inherited gate never releases the lock");

        // The owner releases it.
        drop(held);
        assert!(!path.exists());

        // A lock left behind by a process that no longer exists is stale, not a refusal.
        let gone = dead_pid();
        assert!(!alive(gone), "pid {gone} was reaped");
        fs::write(&path, format!("{gone}\n")).unwrap();
        let taken = TimingGate::take_at(&path, None).unwrap().unwrap();
        assert_eq!(taken.pid(), Some(std::process::id()));
        drop(taken);

        // So is a file whose writer died before it could record a pid.
        fs::write(&path, "").unwrap();
        let after_empty = TimingGate::take_at(&path, None).unwrap().unwrap();
        assert_eq!(after_empty.pid(), Some(std::process::id()));

        // An inherited value that does not match the lock file is not this run's lock.
        assert_eq!(
            TimingGate::take_at(&path, Some("999999"))
                .unwrap()
                .unwrap_err(),
            std::process::id()
        );
    }

    #[test]
    fn a_live_process_is_alive_and_the_threshold_marks_only_what_exceeds_it() {
        assert!(alive(std::process::id()));
        assert!(alive(1), "launchd or init always exists");
        assert_eq!(LOAD_THRESHOLD, 8.0);
        assert!(!unreliable(None));
        assert!(!unreliable(Some(5.7)));
        assert!(
            !unreliable(Some(8.0)),
            "the threshold itself is not over it"
        );
        assert!(unreliable(Some(8.01)));
        assert!(unreliable(Some(37.0)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_frontmost_application_has_a_name() {
        let name = frontmost().expect("LaunchServices names the frontmost application");
        assert!(!name.is_empty() && !name.contains('='), "{name:?}");
    }
}
