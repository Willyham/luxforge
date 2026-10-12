//! `cargo xtask check`: the repository and dependency-policy checks, formatting, Clippy and the
//! workspace's tests, with every test binary at once.
//!
//! `cargo test` runs its test binaries one after another, so a workspace run takes the sum of every
//! binary's wall clock while most of the host waits behind one binary's slowest few tests. This
//! builds the test binaries through Cargo once, then runs all of them as child processes at the
//! same time, each in its package's directory as Cargo runs it, and prints one line per binary as
//! it finishes. A failing binary's whole output is printed after the rest.
//!
//! A quick run leaves out every test marked `#[slow]` ([`slow_tests`]), and the doctests, and says
//! how many tests it left out. The whole run leaves out nothing.
use crate::*;
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    process::Stdio,
    sync::{OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

struct Binary {
    /// `package kind target`, as the line for this binary reads.
    label: String,
    executable: PathBuf,
    /// The package directory, which Cargo runs a test binary in.
    dir: PathBuf,
}

struct Outcome {
    label: String,
    elapsed: Duration,
    passed: bool,
    /// The `test result:` line's counts, or the reason there is none.
    counts: String,
    left_out: usize,
    output: String,
}

/// Every test binary of the workspace, built once. Compiler diagnostics go to the terminal.
fn build(root: &Path) -> Result<Vec<Binary>> {
    let out = cargo_command()
        .current_dir(root)
        .args([
            "test",
            "--locked",
            "--workspace",
            "--no-run",
            "--message-format=json-render-diagnostics",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()?;
    ensure(out.status.success(), "Building the tests failed")?;
    let mut binaries = Vec::new();
    for line in String::from_utf8(out.stdout)?.lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some(executable), Some(manifest)) = (
            message["executable"].as_str(),
            message["manifest_path"].as_str(),
        ) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" || message["profile"]["test"] != true {
            continue;
        }
        let dir = Path::new(manifest)
            .parent()
            .ok_or("A manifest path without a directory")?
            .to_path_buf();
        let package = dir.file_name().unwrap_or_default().to_string_lossy();
        let kind = message["target"]["kind"][0].as_str().unwrap_or("test");
        let target = message["target"]["name"].as_str().unwrap_or_default();
        binaries.push(Binary {
            label: format!("{package} {kind} {target}"),
            executable: executable.into(),
            dir,
        });
    }
    binaries.sort_by(|a, b| a.label.cmp(&b.label));
    binaries.dedup_by(|a, b| a.executable == b.executable);
    ensure(!binaries.is_empty(), "Cargo built no test binary")?;
    Ok(binaries)
}

/// The tests of a binary's JSON listing (`--list --format json`) marked `#[slow]`, by name. The
/// listing gives each test's source file, relative to `root`, and the line of its name; the mark
/// is one of the attribute lines above that line. libtest's JSON is unstable, so the listing is
/// asked for with `RUSTC_BOOTSTRAP`; the toolchain is pinned, and a listing this cannot read
/// fails the run rather than letting it leave nothing out.
fn slow_tests(root: &Path, listing: &str) -> Result<Vec<String>> {
    let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut slow = Vec::new();
    let mut read = false;
    for line in listing.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        read = true;
        if event["type"] != "test" || event["event"] != "discovered" {
            continue;
        }
        let (Some(name), Some(path), Some(at)) = (
            event["name"].as_str(),
            event["source_path"].as_str(),
            event["start_line"].as_u64(),
        ) else {
            return Err(format!("A listed test without its name, source or line: {line}").into());
        };
        if !sources.contains_key(path) {
            let text = fs::read_to_string(root.join(path))
                .map_err(|e| format!("Reading {path} for its tests' marks: {e}"))?;
            sources.insert(path.into(), text.lines().map(str::to_owned).collect());
        }
        let lines = &sources[path];
        let at = usize::try_from(at).unwrap_or(usize::MAX);
        if marked_slow(&lines[..at.saturating_sub(1).min(lines.len())]) {
            slow.push(name.to_owned());
        }
    }
    ensure(read, "A test listing that is not libtest's JSON")?;
    Ok(slow)
}

/// Whether the attributes that end `above`, the lines before a test's name, hold `#[slow]`. They
/// are read upwards to the end of the item before, past comments.
fn marked_slow(above: &[String]) -> bool {
    for line in above.iter().rev().map(|line| line.trim()) {
        if line.starts_with("//") {
            continue;
        }
        if line.is_empty() || line.ends_with(['}', ';', '{']) {
            return false;
        }
        if line == "#[slow]" || (line.starts_with("#[") && line.ends_with("::slow]")) {
            return true;
        }
    }
    false
}

fn command(binary: &Binary) -> Command {
    let mut command = Command::new(&binary.executable);
    command
        .current_dir(&binary.dir)
        .env("CARGO_MANIFEST_DIR", &binary.dir)
        .stdin(Stdio::null());
    command
}

/// How many tests each binary runs at once, unless `RUST_TEST_THREADS` says: half the host's
/// cores. Every binary runs at the same time, and libtest's default of one thread per core gives
/// the largest two alone more threads than the host has cores; contended, they spent as long in
/// the kernel as in their own code. With half, the whole run took about a quarter less time.
fn test_threads() -> Option<usize> {
    std::env::var_os("RUST_TEST_THREADS")
        .is_none()
        .then(|| thread::available_parallelism().map_or(1, |cores| (cores.get() / 2).max(1)))
}

/// Runs `command` to its end with its stdout and stderr in one anonymous file, and reads it back.
/// A file rather than a pipe: macOS has no `pipe2`, so a pipe made for one child is inheritable
/// until it is marked close-on-exec, and a child another thread spawns in that moment holds its
/// write end open, so this child's output would not end until that one's did.
fn captured(mut command: Command) -> std::io::Result<(std::process::ExitStatus, String)> {
    let mut file = tempfile::tempfile()?;
    let status = command
        .stdout(file.try_clone()?)
        .stderr(file.try_clone()?)
        .status()?;
    let mut text = String::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_string(&mut text)?;
    Ok((status, text))
}

/// One binary's run: in a quick run, its slow tests listed first and skipped by exact name.
fn run_one(root: &Path, binary: &Binary, quick: bool) -> Outcome {
    let started = Instant::now();
    let mut outcome = Outcome {
        label: binary.label.clone(),
        elapsed: Duration::ZERO,
        passed: false,
        counts: String::new(),
        left_out: 0,
        output: String::new(),
    };
    let mut args: Vec<String> = Vec::new();
    if quick {
        let mut list = command(binary);
        list.args(["--list", "--format", "json", "-Z", "unstable-options"])
            .env("RUSTC_BOOTSTRAP", "1");
        let listed = match captured(list) {
            Ok((status, listed)) if status.success() => listed,
            Ok((_, listed)) => {
                outcome.counts = "could not list its tests".into();
                outcome.output = listed;
                return outcome;
            }
            Err(e) => {
                outcome.counts = format!("could not start: {e}");
                return outcome;
            }
        };
        let slow = match slow_tests(root, &listed) {
            Ok(slow) => slow,
            Err(e) => {
                outcome.counts = e.to_string();
                return outcome;
            }
        };
        args.push("--exact".into());
        for name in slow {
            args.push("--skip".into());
            args.push(name);
            outcome.left_out += 1;
        }
    }
    if let Some(threads) = test_threads() {
        args.push(format!("--test-threads={threads}"));
    }
    let mut run = command(binary);
    run.args(&args);
    match captured(run) {
        Ok((status, output)) => {
            outcome.passed = status.success();
            outcome.counts = output
                .lines()
                .find_map(|line| line.strip_prefix("test result: "))
                .map(|result| {
                    result
                        .split(';')
                        .map(str::trim)
                        .filter(|part| {
                            !part.starts_with("0 ")
                                && !part.starts_with("finished")
                                && !part.ends_with("filtered out")
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| format!("no test result, {status}"));
            outcome.output = output;
        }
        Err(e) => outcome.counts = format!("could not start: {e}"),
    }
    outcome.elapsed = started.elapsed();
    outcome
}

/// Builds and runs every test binary at once, then, outside a quick run, the doctests.
pub fn tests(root: &Path, quick: bool) -> Result {
    run(root, &build(root)?, quick)
}

/// Runs every one of `binaries` at once, then, outside a quick run, the doctests.
fn run(root: &Path, binaries: &[Binary], quick: bool) -> Result {
    let started = Instant::now();
    let (sender, receiver) = mpsc::channel();
    let outcomes = thread::scope(|scope| {
        for binary in binaries {
            let sender = sender.clone();
            scope.spawn(move || sender.send(run_one(root, binary, quick)));
        }
        drop(sender);
        receiver
            .iter()
            .inspect(|o| {
                let left_out = if o.left_out > 0 {
                    format!(", {} slow left out", o.left_out)
                } else {
                    String::new()
                };
                println!(
                    "{} {:>5.1} s  {} ({}{left_out})",
                    if o.passed { "PASS" } else { "FAIL" },
                    o.elapsed.as_secs_f64(),
                    o.label,
                    o.counts,
                );
            })
            .collect::<Vec<_>>()
    });
    let failed: Vec<&Outcome> = outcomes.iter().filter(|o| !o.passed).collect();
    for o in &failed {
        println!("\n==== {} ====\n{}", o.label, o.output);
    }
    let left_out: usize = outcomes.iter().map(|o| o.left_out).sum();
    println!(
        "{} test binaries in {:.1} s{}",
        outcomes.len(),
        started.elapsed().as_secs_f64(),
        if quick {
            format!("; {left_out} slow tests and the doctests left out")
        } else {
            String::new()
        }
    );
    ensure(
        failed.is_empty(),
        format!(
            "Tests failed in {}",
            failed
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )?;
    if !quick {
        let status = cargo_command()
            .current_dir(root)
            .args(["test", "--locked", "--workspace", "--doc"])
            .status()?;
        ensure(status.success(), "Doctests failed")?;
    }
    Ok(())
}

/// Whether every workspace package is formatted, each package's `cargo fmt --check` at once:
/// one `cargo fmt --all` formats every target in one single-threaded `rustfmt`, several times
/// slower than its largest package alone. The packages and files are the same.
fn formatted(root: &Path) -> Result {
    let metadata: Value = serde_json::from_slice(
        &cargo_command()
            .current_dir(root)
            .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
            .stderr(Stdio::inherit())
            .output()?
            .stdout,
    )?;
    let packages: Vec<String> = metadata["packages"]
        .as_array()
        .ok_or("Cargo metadata lists no packages")?
        .iter()
        .filter_map(|package| package["name"].as_str().map(str::to_owned))
        .collect();
    let outputs = thread::scope(|scope| {
        let handles: Vec<_> = packages
            .iter()
            .map(|package| {
                scope.spawn(move || {
                    cargo_command()
                        .current_dir(root)
                        .args(["fmt", "--package", package, "--", "--check"])
                        .stdin(Stdio::null())
                        .output()
                        .map_err(|error| format!("cargo fmt for {package}: {error}"))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| Err("panicked".into())))
            .collect::<Vec<_>>()
    });
    let mut differs = String::new();
    for output in outputs {
        let output = output?;
        if !output.status.success() {
            differs.push_str(&String::from_utf8_lossy(&output.stdout));
            differs.push_str(&String::from_utf8_lossy(&output.stderr));
        }
    }
    ensure(
        differs.is_empty(),
        format!("Formatting differs:\n{differs}"),
    )
}

/// The repository and dependency-policy checks, formatting, Clippy and the tests. The first two
/// build nothing and take a second or two, so they run beside Clippy and the test build. A
/// failure among them that has already come when the tests are built ends the check before they
/// run; otherwise they are waited for after the tests. `quick` leaves the slow tests and the
/// doctests out ([`tests`]).
pub fn check(root: &Path, quick: bool) -> Result {
    let text = |result: Result| result.map_err(|e| e.to_string());
    let repository = OnceLock::new();
    let formatting = OnceLock::new();
    let failures = thread::scope(|scope| {
        scope.spawn(|| {
            repository.set(text(
                repository::check(root).and_then(|()| policy::checked(root).map(drop)),
            ))
        });
        scope.spawn(|| formatting.set(text(formatted(root))));
        let mut failures = Vec::new();
        match cargo(root, "lint", false)
            .and_then(|()| build(root))
            .map_err(|e| e.to_string())
        {
            Err(error) => failures.push(error),
            Ok(_)
                if [&repository, &formatting]
                    .iter()
                    .any(|gate| gate.get().is_some_and(std::result::Result::is_err)) =>
            {
                failures.push("The tests did not run.".into());
            }
            Ok(binaries) => failures.extend(text(run(root, &binaries, quick)).err()),
        }
        failures
    });
    let gates = [repository, formatting]
        .into_iter()
        .filter_map(|gate| gate.into_inner().unwrap_or(Err("panicked".into())).err());
    let failures: Vec<String> = gates.chain(failures).collect();
    ensure(failures.is_empty(), failures.join("\n"))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn above(source: &str) -> Vec<String> {
        source.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_test_is_slow_when_its_own_attributes_hold_the_mark() {
        assert!(marked_slow(&above("#[test]\n#[slow]")));
        assert!(marked_slow(&above(
            "#[test]\n#[luxforge_test_macros::slow]\n#[cfg(unix)]"
        )));
        assert!(marked_slow(&above(
            "}\n\n/// Every code { round } trips.\n#[test]\n// A note.\n#[slow]"
        )));
        assert!(!marked_slow(&above(
            "#[test]\n#[slow]\nfn other() {}\n#[test]"
        )));
        assert!(!marked_slow(&above("#[slow]\n\n#[test]")));
        assert!(!marked_slow(&above(
            "#[test]\n#[slow_tests]\n#[ignore = \"slow\"]"
        )));
    }

    #[test]
    fn the_listing_names_each_marked_test_and_refuses_one_without_its_source() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("src/lib.rs"),
            "#[test]\n#[slow]\nfn sweep() {}\n\n#[test]\nfn quick() {}\n",
        )
        .unwrap();
        let event = |name: &str, line: u32| {
            format!(
                "{{ \"type\": \"test\", \"event\": \"discovered\", \"name\": \"{name}\", \
                 \"source_path\": \"src/lib.rs\", \"start_line\": {line} }}"
            )
        };
        let listing = format!(
            "{{ \"type\": \"suite\", \"event\": \"discovery\" }}\n{}\n{}",
            event("tests::sweep", 3),
            event("tests::quick", 6)
        );
        assert_eq!(slow_tests(root.path(), &listing).unwrap(), ["tests::sweep"]);
        let bare = r#"{ "type": "test", "event": "discovered", "name": "tests::quick" }"#;
        assert!(slow_tests(root.path(), bare).is_err());
        assert!(slow_tests(root.path(), "tests::quick: test").is_err());
    }
}
