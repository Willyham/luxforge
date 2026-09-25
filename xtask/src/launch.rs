//! Non-interactive test launches must not activate the owner's desktop.
use crate::*;

/// The host-wide lock file two timing runs cannot both hold. It lives in the OS temporary
/// directory rather than an output directory so it is shared by every checkout, worktree and
/// terminal on this machine: a figure measured while another run was launching editors is not a
/// figure of this machine.
const LOCK: &str = "luxforge-timing.lock";

/// How a timing child is told that its parent already holds the lock. Without it, every timing
/// component `verify` spawns would refuse the run that started it.
const GATE_ENV: &str = "LUXFORGE_TIMING_GATE";

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
/// as well as for its own on Unix; Windows has the equivalent `tasklist` query. When the process
/// inspection command cannot be run at all the answer is "alive", because refusing a timing run
/// costs a rerun while stealing a live one's lock costs the measurement.
fn alive(pid: u32) -> bool {
    // No supported process table can contain this sentinel. Keeping it deterministic also lets
    // stale-lock tests run in restricted environments where process inspection is unavailable.
    if pid == u32::MAX {
        return false;
    }

    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        return match Command::new("tasklist")
            .args(["/FI", &filter, "/FO", "CSV", "/NH"])
            .output()
        {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).lines().any(|line| {
                    line.split(',')
                        .nth(1)
                        .is_some_and(|value| value.trim().trim_matches('"') == pid.to_string())
                })
            }
            Ok(_) | Err(_) => true,
        };
    }

    #[cfg(not(windows))]
    {
        match Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .output()
        {
            Ok(out) => out.status.success() && !out.stdout.trim_ascii().is_empty(),
            Err(_) => true,
        }
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

/// The pid of the frontmost application, through LaunchServices, which needs no Automation
/// permission. One check proves that an automated launch is never that application; the hidden
/// window and the background bundle are what keep it so, and every run relies on them without
/// re-measuring the desktop.
#[cfg(target_os = "macos")]
pub fn frontmost_pid() -> Result<u32> {
    let asn = lsappinfo(&["front"])?;
    ensure(
        asn.starts_with("ASN:"),
        format!("lsappinfo named no frontmost application: {asn:?}"),
    )?;
    // `lsappinfo info -only pid ASN:...` answers `"pid"=1234`.
    let answer = lsappinfo(&["info", "-only", "pid", &asn])?;
    parse_frontmost_pid(&answer).ok_or_else(|| {
        format!("lsappinfo gave the frontmost application no pid: {answer:?}").into()
    })
}

#[cfg(target_os = "macos")]
fn parse_frontmost_pid(answer: &str) -> Option<u32> {
    answer
        .split_once('=')
        .and_then(|(_, value)| value.trim().trim_matches('"').parse().ok())
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

impl Background {
    pub fn new(binary: &Path) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            // Iced's winit runner activates ordinary unbundled executables. A background-only
            // bundle prevents activation before the event loop starts, while renderer readbacks
            // still use the native GPU. Do not use a symlink: Cocoa resolves it to the original
            // executable and loses the bundle identity. Never modify the built/packaged app.
            let bundle = tempfile::Builder::new()
                .prefix("luxforge-test-")
                .tempdir()?;
            let contents = bundle.path().join("Luxforge Test.app/Contents");
            fs::create_dir_all(contents.join("MacOS"))?;
            let executable = contents.join("MacOS/luxforge-test");
            fs::copy(binary, &executable)?;
            fs::write(
                contents.join("Info.plist"),
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>org.luxforge.background-test</string>
<key>CFBundleName</key><string>Luxforge Test</string>
<key>CFBundleExecutable</key><string>luxforge-test</string>
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

        // A lock left behind by an impossible process id is stale, not a refusal. Using a
        // sentinel rather than a reaped child avoids PID reuse races in parallel test runners.
        let gone = u32::MAX;
        assert!(!alive(gone), "pid {gone} must not exist");
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
        assert!(!alive(u32::MAX));
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
    fn frontmost_pid_parser_accepts_launchservices_output() {
        assert_eq!(parse_frontmost_pid("\"pid\"=1234"), Some(1234));
        assert_eq!(parse_frontmost_pid("\"pid\"=[ NULL ]"), None);
        assert_eq!(parse_frontmost_pid(""), None);
    }
}
