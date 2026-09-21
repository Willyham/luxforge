//! Non-interactive test launches must not activate the owner's desktop.
use crate::*;

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

    #[cfg(target_os = "macos")]
    #[test]
    fn the_frontmost_application_has_a_name() {
        let name = frontmost().expect("LaunchServices names the frontmost application");
        assert!(!name.is_empty() && !name.contains('='), "{name:?}");
    }
}
