//! `cargo xtask check`: the repository and dependency-policy checks, formatting, Clippy and the
//! workspace's tests, with every test binary at once.
//!
//! `cargo test` runs its test binaries one after another, so a workspace run takes the sum of every
//! binary's wall clock while most of the host waits behind one binary's slowest few tests. This
//! builds the test binaries through Cargo once, then runs all of them as child processes at the
//! same time, each in its package's directory as Cargo runs it, and prints one line per binary as
//! it finishes. A failing binary's whole output is printed after the rest.
//!
//! A quick run leaves out every test whose own name begins with [`SLOW`], and the doctests, and
//! says how many tests it left out. The whole run leaves out nothing.
use crate::*;
use std::{
    io::{Seek, SeekFrom},
    process::Stdio,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// The prefix of a test's own name (the last segment of its path) that keeps it out of a quick run.
pub const SLOW: &str = "slow_";

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

/// Whether a listed test's own name marks it as slow.
fn slow(name: &str) -> bool {
    name.rsplit("::").next().unwrap_or(name).starts_with(SLOW)
}

fn command(binary: &Binary) -> Command {
    let mut command = Command::new(&binary.executable);
    command
        .current_dir(&binary.dir)
        .env("CARGO_MANIFEST_DIR", &binary.dir)
        .stdin(Stdio::null());
    command
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
fn run_one(binary: &Binary, quick: bool) -> Outcome {
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
        list.args(["--list", "--format", "terse"]);
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
        args.push("--exact".into());
        for name in listed
            .lines()
            .filter_map(|line| line.strip_suffix(": test"))
            .filter(|name| slow(name))
        {
            args.push("--skip".into());
            args.push(name.into());
            outcome.left_out += 1;
        }
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
    let binaries = build(root)?;
    let started = Instant::now();
    let (sender, receiver) = mpsc::channel();
    let outcomes = thread::scope(|scope| {
        for binary in &binaries {
            let sender = sender.clone();
            scope.spawn(move || sender.send(run_one(binary, quick)));
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

/// The repository and dependency-policy checks, formatting, Clippy and the tests. The first three
/// share nothing with the build, so they run beside Clippy and the tests rather than before them.
/// `quick` leaves the slow tests and the doctests out ([`tests`]).
pub fn check(root: &Path, quick: bool) -> Result {
    let text = |result: Result| result.map_err(|e| e.to_string());
    let (repository, formatted, built) = thread::scope(|scope| {
        let repository = scope
            .spawn(|| text(repository::check(root).and_then(|()| policy::checked(root).map(drop))));
        let formatted = scope.spawn(|| {
            let out = cargo_command()
                .args(["fmt", "--all", "--", "--check"])
                .current_dir(root)
                .output()
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "Formatting differs:\n{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
        });
        let built = text(cargo(root, "lint", false).and_then(|()| tests(root, quick)));
        let joined = |handle: thread::ScopedJoinHandle<'_, std::result::Result<(), String>>| {
            handle.join().unwrap_or_else(|_| Err("panicked".into()))
        };
        (joined(repository), joined(formatted), built)
    });
    let failures: Vec<String> = [repository, formatted, built]
        .into_iter()
        .filter_map(std::result::Result::err)
        .collect();
    ensure(failures.is_empty(), failures.join("\n"))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_test_whose_own_name_begins_with_the_prefix_is_slow() {
        assert!(slow("slow_whole_conformance"));
        assert!(slow("render::spatial::tests::slow_a_point_query"));
        assert!(!slow("render::slow_tests::a_point_query"));
        assert!(!slow("transport::tests::a_slow_server_times_out"));
    }
}
