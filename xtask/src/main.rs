mod basic_acceptance;
mod basic_smoke;
mod crop_smoke;
mod diagnostics;
mod editor_acceptance;
mod editor_latency;
mod editor_performance;
mod fixtures;
mod histogram_smoke;
mod launch;
mod package;
mod policy;
mod raw;
mod raw_editor;
/// The independent f64 colour reference the core's own numerical tests use, compiled in rather
/// than copied, so the acceptance journey checks production against one written-from-the-formulas
/// oracle that production code can never import.
#[path = "../../crates/lightwell-core/tests/reference/mod.rs"]
mod reference;
mod repository;
mod smoke;
mod verify;
mod workspace_smoke;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn ensure(ok: bool, message: impl Into<String>) -> Result {
    if ok {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn write_json(path: &Path, value: &Value) -> Result {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
fn hash(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn run(root: &Path, program: impl AsRef<OsStr>, args: &[&str]) -> Result {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .status()?;
    ensure(
        status.success(),
        format!("Command {args:?} failed: {status}"),
    )
}
fn output(root: &Path, program: impl AsRef<OsStr>, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()?;
    ensure(out.status.success(), String::from_utf8_lossy(&out.stderr))?;
    Ok(String::from_utf8(out.stdout)?)
}
fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for item in fs::read_dir(root)? {
        let p = item?.path();
        if p.is_dir() {
            paths.extend(files(&p)?)
        } else {
            paths.push(p)
        }
    }
    paths.sort();
    Ok(paths)
}
fn root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .find(|p| {
            p.join("tools/task-plan.schema.json").is_file() && p.join("xtask/Cargo.toml").is_file()
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| "Run from a Lightwell checkout".into())
}
fn absolute(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        root.join(path)
    }
}
fn binary(root: &Path) -> Result<PathBuf> {
    let data: Value = serde_json::from_str(&output(
        root,
        "cargo",
        &["metadata", "--locked", "--no-deps", "--format-version", "1"],
    )?)?;
    Ok(Path::new(
        data["target_directory"]
            .as_str()
            .ok_or("Missing target directory")?,
    )
    .join("release")
    .join(format!("lightwell{}", std::env::consts::EXE_SUFFIX)))
}
fn host(root: &Path) -> Result<String> {
    output(root, "rustc", &["-vV"])?
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .ok_or_else(|| "Missing rustc host".into())
}
fn cargo(root: &Path, op: &str, release: bool) -> Result {
    let mut args = match op {
        "build" => vec!["build", "--locked", "--package", "lightwell-app"],
        "test" => vec!["test", "--locked", "--workspace"],
        "fmt" => vec!["fmt", "--all", "--", "--check"],
        "lint" => vec![
            "clippy",
            "--locked",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        _ => return Err("Unknown Cargo operation".into()),
    };
    if release {
        args.push("--release")
    }
    run(root, "cargo", &args)
}
struct Args(Vec<OsString>);
impl Args {
    fn flag(&mut self, key: &str) -> bool {
        if let Some(i) = self.0.iter().position(|x| x == key) {
            self.0.remove(i);
            true
        } else {
            false
        }
    }
    fn value(&mut self, key: &str) -> Result<Option<OsString>> {
        if let Some(i) = self.0.iter().position(|x| x == key) {
            self.0.remove(i);
            ensure(i < self.0.len(), format!("Missing {key} value"))?;
            Ok(Some(self.0.remove(i)))
        } else {
            Ok(None)
        }
    }
    fn path(&mut self, key: &str) -> Result<PathBuf> {
        self.value(key)?
            .map(PathBuf::from)
            .ok_or_else(|| format!("Required: {key}").into())
    }
    fn done(&self) -> Result {
        ensure(
            self.0.is_empty(),
            format!("Unknown arguments: {:?}", self.0),
        )
    }
}
fn samples(a: &mut Args, default: usize) -> Result<usize> {
    Ok(a.value("--samples")?
        .map(|s| s.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(default))
}
fn main() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e}");
            ExitCode::FAILURE
        }
    }
}
fn main_result() -> Result {
    let root = root()?;
    let mut args = std::env::args_os().skip(1);
    let op = args.next().unwrap_or_else(|| "help".into());
    let mut a = Args(args.collect());
    match op.to_str().ok_or("Invalid command")? {
        "develop" => {
            let debug = a.flag("--debug");
            let background = a.flag("--background");
            if background {
                ensure(
                    cfg!(target_os = "macos"),
                    "develop --background is currently supported only on macOS",
                )?;
                cargo(&root, "build", !debug)?;
                let mut bin = binary(&root)?;
                if debug {
                    bin = bin
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("debug")
                        .join(bin.file_name().unwrap());
                }
                let launch = launch::Background::new(&bin)?;
                ensure(
                    Command::new(&launch.executable)
                        .current_dir(&root)
                        .args(a.0)
                        .status()?
                        .success(),
                    "Application failed",
                )?;
                return Ok(());
            }
            let mut cmd = Command::new("cargo");
            cmd.current_dir(&root).args(["run", "--locked"]);
            if !debug {
                cmd.arg("--release");
            }
            ensure(
                cmd.args(["--package", "lightwell-app", "--bin", "lightwell", "--"])
                    .args(a.0)
                    .status()?
                    .success(),
                "Application failed",
            )?;
        }
        "check" => {
            a.done()?;
            repository::check(&root)?;
            policy::checked(&root)?;
            for op in ["fmt", "lint", "test"] {
                cargo(&root, op, false)?
            }
            println!("Headless checks passed. GUI and platform acceptance remain separate.");
        }
        "check-repository" => {
            a.done()?;
            repository::check(&root)?;
        }
        "build" | "fmt" | "lint" | "test" => {
            let release = a.flag("--release");
            a.done()?;
            cargo(&root, op.to_str().unwrap(), release)?;
        }
        "doctor" => {
            a.done()?;
            println!("Host: {}", host(&root)?);
            for (p, as_) in [
                ("rustc", vec!["--version"]),
                ("cargo", vec!["fmt", "--version"]),
                ("cargo", vec!["clippy", "--version"]),
            ] {
                run(&root, p, &as_)?
            }
            if cfg!(target_os = "macos") {
                run(&root, "clang", &["--version"])?;
            }
            if cfg!(target_os = "linux") {
                run(&root, "cc", &["--version"])?;
                run(&root, "pkg-config", &["--version"])?;
                ensure(
                    std::env::var_os("DISPLAY").is_some()
                        || std::env::var_os("WAYLAND_DISPLAY").is_some(),
                    "No graphical session for smoke (headless check/build still available)",
                )?;
            }
            println!(
                "Native graphics driver, SDK/runtime libraries and an unlocked graphical session are required for UI checks; not proven by Doctor."
            );
        }
        "fixtures" => {
            a.done()?;
            fixtures::check(&root)?;
        }
        "generate-fixtures" => {
            let out = a
                .value("--output")?
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("fixtures/generated"));
            a.done()?;
            fixtures::generate(&absolute(&root, &out))?;
        }
        "raw-corpus" => {
            let manifest = absolute(&root, &a.path("--manifest")?);
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            raw::corpus(&manifest, &out)?;
        }
        "raw-reference" => {
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            raw::reference(&out)?;
        }
        "raw-editor" => {
            let manifest = absolute(&root, &a.path("--manifest")?);
            let out = absolute(&root, &a.path("--output")?);
            let samples = samples(&mut a, 3)?;
            let selected_binary = a.value("--binary")?.map(PathBuf::from);
            a.done()?;
            let bin = selected_binary
                .as_ref()
                .map(|path| absolute(&root, path))
                .map(Ok)
                .unwrap_or_else(|| binary(&root))?;
            let _gate = launch::TimingGate::acquire()?;
            raw_editor::run(&root, &manifest, &out, &bin, samples)?;
        }
        "inventory" | "package" => {
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            if op == "package" {
                package::package(&root, &out)?
            } else {
                package::inventory(&root, &out)?;
            }
        }
        "audit" => {
            a.done()?;
            policy::audit(&root)?;
        }
        "editor-acceptance" => {
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            editor_acceptance::run(&root, &out)?;
        }
        "editor-performance" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            let samples = samples(&mut a, 10)?;
            a.done()?;
            let _gate = launch::TimingGate::acquire()?;
            editor_performance::run(&root, &source, &out, samples)?;
        }
        "editor-latency" => {
            let source = absolute(&root, &a.path("--source")?);
            let out = absolute(&root, &a.path("--output")?);
            let bin = a
                .value("--binary")?
                .map(|path| absolute(&root, Path::new(&path)))
                .map_or_else(|| binary(&root), Ok)?;
            let samples = samples(&mut a, 30)?;
            let crop = a
                .value("--crop")?
                .map(|s| s.to_string_lossy().parse::<f64>())
                .transpose()?;
            let idle = a.flag("--idle");
            let basic = a.flag("--basic");
            let mode = match a.value("--mode")?.as_deref().and_then(OsStr::to_str) {
                None | Some("drag") => editor_latency::Mode::Drag,
                Some("commit") => editor_latency::Mode::Commit,
                Some("burst") => editor_latency::Mode::Burst,
                Some(other) => {
                    return Err(format!("--mode is drag, commit or burst, not {other}").into());
                }
            };
            a.done()?;
            let _gate = launch::TimingGate::acquire()?;
            editor_latency::run(
                &root,
                &out,
                &bin,
                editor_latency::Options {
                    source: &source,
                    samples,
                    mode,
                    crop,
                    idle,
                    basic,
                },
            )?;
        }
        "smoke" => {
            let out = absolute(&root, &a.path("--output")?);
            let scenario = a
                .value("--scenario")?
                .unwrap_or_else(|| "load".into())
                .into_string()
                .map_err(|_| "Invalid scenario")?;
            let bin = a
                .value("--binary")?
                .map(PathBuf::from)
                .map(|p| absolute(&root, &p))
                .unwrap_or(binary(&root)?);
            a.done()?;
            smoke::dispatch(
                &root,
                &out,
                &scenario,
                &bin,
                std::time::Duration::from_secs(35),
            )?;
        }
        "verify" => {
            let out = absolute(&root, &a.path("--output")?);
            let tier = a
                .value("--tier")?
                .map(|t| {
                    t.into_string()
                        .map_err(|_| "Invalid tier".into())
                        .and_then(|t| verify::Tier::parse(&t))
                })
                .transpose()?
                .unwrap_or(verify::Tier::Quick);
            let bin = a.value("--binary")?.map(PathBuf::from);
            let manifest = a.value("--manifest")?.map(PathBuf::from);
            let jobs = a
                .value("--jobs")?
                .map(|s| s.to_string_lossy().parse::<usize>())
                .transpose()?
                .unwrap_or(verify::JOBS);
            a.done()?;
            verify::run(&root, &out, tier, bin, manifest, jobs)?;
        }
        "check-capture" => {
            let path = absolute(&root, &a.path("--image")?);
            let orientation = a
                .value("--orientation")?
                .map(|s| s.to_string_lossy().parse::<u8>())
                .transpose()?
                .unwrap_or(6);
            let aspect = a
                .value("--aspect")?
                .map(|s| s.to_string_lossy().parse::<f64>())
                .transpose()?;
            let columns = a
                .value("--columns")?
                .map(|s| -> Result<[u32; 2]> {
                    let text = s.to_string_lossy();
                    let (left, right) = text
                        .split_once(',')
                        .ok_or("--columns expects LEFT,RIGHT physical pixels")?;
                    Ok([left.trim().parse()?, right.trim().parse()?])
                })
                .transpose()?;
            a.done()?;
            println!(
                "{}",
                smoke::pixels(
                    &path,
                    &smoke::Expect {
                        aspect,
                        columns,
                        ..smoke::Expect::fit(orientation)
                    }
                )?
            );
        }
        "hardening" | "measure" => {
            let out = absolute(&root, &a.path("--output")?);
            let bin = absolute(&root, &a.path("--binary")?);
            let samples = samples(&mut a, 5)?;
            a.done()?;
            if op == "hardening" {
                diagnostics::hardening(&root, &out, &bin)?
            } else {
                // Timing runs never overlap, whether they were started by `verify` or by hand.
                let _gate = launch::TimingGate::acquire()?;
                diagnostics::measure(&root, &out, &bin, samples)?;
            }
        }
        "probe" => {
            let candidate = a
                .value("--candidate")?
                .ok_or("Required --candidate iced|egui")?
                .into_string()
                .map_err(|_| "Invalid candidate")?;
            let out = absolute(&root, &a.path("--output")?);
            a.done()?;
            diagnostics::probe(&root, &out, &candidate)?;
        }
        "__hang" => std::thread::sleep(std::time::Duration::from_secs(60)),
        "help" => println!(
            "cargo xtask doctor|check|check-repository|fmt|lint|test|build [--release]|develop [--debug] [--background] [app args]|fixtures|generate-fixtures [--output NEW]|audit|raw-corpus --manifest FILE --output NEW|raw-reference --output NEW|raw-editor --manifest FILE --output NEW [--samples N] [--binary PATH]|editor-acceptance --output NEW|editor-performance --source JPEG --output NEW [--samples N]|editor-latency --source JPEG --output NEW [--binary PATH] [--samples N] [--mode drag|commit|burst] [--crop DEGREES] [--basic] [--idle]|inventory --output NEW|package --output NEW|smoke --output NEW [--scenario NAME] [--binary PATH]|verify --output NEW [--tier quick|rendered|timing|full] [--jobs N] [--binary PATH] [--manifest FILE]|check-capture --image PNG [--orientation N] [--aspect R] [--columns LEFT,RIGHT]|hardening --binary PATH --output NEW|measure --binary PATH --output NEW [--samples N]|probe --candidate iced|egui --output NEW"
        ),
        _ => return Err("Unknown command; use cargo xtask help".into()),
    }
    Ok(())
}
