//! A module crate built into its component: `cargo build --release` for [`WASM_TARGET`] with the
//! repository's pinned toolchain, then the core module made a component of the `adjustment` world
//! in `wit/adjustment.wit`. Nothing is imported: a core module that imports anything fails here.
use super::Error;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The target a module is built for: plain WebAssembly, with no WASI and no imports.
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// The repository's `rust-toolchain.toml`, whose channel builds every module.
pub const TOOLCHAIN: &str = include_str!("../../../../rust-toolchain.toml");

/// The interface every component exports, read from the repository's WIT.
const WIT: &str = include_str!("../../../../wit/adjustment.wit");
const WORLD: &str = "adjustment";

/// The pinned channel, `1.94.0`, from [`TOOLCHAIN`].
pub fn toolchain_channel() -> &'static str {
    TOOLCHAIN
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("channel")?
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .strip_prefix('"')?
                .strip_suffix('"')
        })
        .expect("rust-toolchain.toml names a channel")
}

/// The variables `cargo run` and `cargo test` set for the process that runs this, which must not
/// reach the module's build: they would name another package, target directory or flags.
fn inherited(key: &str) -> bool {
    key.starts_with("CARGO_PKG_")
        || key.starts_with("CARGO_MANIFEST_")
        || key.starts_with("CARGO_BUILD_")
        || matches!(
            key,
            "CARGO_CRATE_NAME"
                | "CARGO_BIN_NAME"
                | "CARGO_PRIMARY_PACKAGE"
                | "CARGO_TARGET_DIR"
                | "CARGO_TARGET_TMPDIR"
                | "CARGO_ENCODED_RUSTFLAGS"
                | "RUSTFLAGS"
                | "RUSTDOCFLAGS"
                | "OUT_DIR"
        )
}

/// The Cargo that runs this process, else the one that built it, else the one on the path. A test
/// binary run directly, as `cargo xtask check` runs them, is not given `CARGO`.
fn cargo_program() -> PathBuf {
    std::env::var_os("CARGO")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

/// The `rustc` beside `cargo`, when it is a file: a toolchain's own pair, so a shim earlier on the
/// path cannot swap the compiler.
fn rustc_beside(cargo: &Path) -> Option<PathBuf> {
    let rustc = cargo
        .parent()?
        .join(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    rustc.is_file().then_some(rustc)
}

fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(inherited) {
            command.env_remove(&key);
        }
    }
    command.env("RUSTUP_TOOLCHAIN", toolchain_channel());
    command
}

/// Check that `rustc` is the pinned release and has the WebAssembly target's standard library.
fn check_toolchain(rustc: &Path) -> Result<(), Error> {
    let channel = toolchain_channel();
    let run = |args: &[&str]| -> Result<String, Error> {
        let output = command(rustc)
            .args(args)
            .output()
            .map_err(|error| format!("cannot run {}: {error}", rustc.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} {} failed: {}",
                rustc.display(),
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let version = run(&["-vV"])?;
    let release = version
        .lines()
        .find_map(|line| line.strip_prefix("release: "))
        .unwrap_or("unknown");
    if release != channel {
        return Err(format!(
            "a module is built with the pinned toolchain {channel}, but {} is {release}",
            rustc.display()
        ));
    }
    let sysroot = run(&["--print", "sysroot"])?;
    let target = Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(WASM_TARGET);
    if !target.is_dir() {
        return Err(format!(
            "toolchain {channel} has no {WASM_TARGET} standard library; install it with \
             `rustup target add {WASM_TARGET} --toolchain {channel}`"
        ));
    }
    Ok(())
}

/// Build the module crate at `project` (a directory holding its `Cargo.toml`) for [`WASM_TARGET`]
/// in release, into `target_dir`, with the pinned toolchain, and answer its core module's path.
/// The crate's lockfile is kept (`--locked`) when it has one. Cargo's diagnostics go to standard
/// error.
pub fn build_core_module(project: &Path, target_dir: &Path) -> Result<PathBuf, Error> {
    let manifest = project.join("Cargo.toml");
    let manifest = manifest
        .canonicalize()
        .map_err(|error| format!("{} is not a crate: {error}", manifest.display()))?;
    let cargo = cargo_program();
    let rustc = rustc_beside(&cargo).unwrap_or_else(|| PathBuf::from("rustc"));
    check_toolchain(&rustc)?;
    let mut build = command(&cargo);
    build
        .current_dir(project)
        .env("RUSTC", &rustc)
        .args(["build", "--release", "--target", WASM_TARGET])
        .args(["--message-format", "json-render-diagnostics"])
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .stdout(Stdio::piped());
    if project.join("Cargo.lock").is_file() {
        build.arg("--locked");
    }
    let mut child = build
        .spawn()
        .map_err(|error| format!("cannot run {}: {error}", cargo.display()))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut artifact = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|error| format!("reading Cargo's messages: {error}"))?;
        if let Some(path) = wasm_artifact(&line, &manifest) {
            artifact = Some(path);
        }
    }
    let status = child
        .wait()
        .map_err(|error| format!("waiting for Cargo: {error}"))?;
    if !status.success() {
        return Err(format!(
            "cargo build of {} for {WASM_TARGET} failed ({status})",
            manifest.display()
        ));
    }
    artifact.ok_or_else(|| {
        format!(
            "{} built no `.wasm`: a module crate is a `cdylib` (`[lib] crate-type = [\"cdylib\"]`)",
            manifest.display()
        )
    })
}

/// The `.wasm` a Cargo JSON message reports for the `cdylib` of the package at `manifest`.
fn wasm_artifact(line: &str, manifest: &Path) -> Option<PathBuf> {
    let message: serde_json::Value = serde_json::from_str(line).ok()?;
    if message["reason"] != "compiler-artifact" {
        return None;
    }
    let ours = message["manifest_path"]
        .as_str()
        .and_then(|path| Path::new(path).canonicalize().ok())
        .is_some_and(|path| path == manifest);
    let cdylib = message["target"]["crate_types"]
        .as_array()?
        .iter()
        .any(|kind| kind == "cdylib");
    if !(ours && cdylib) {
        return None;
    }
    message["filenames"]
        .as_array()?
        .iter()
        .filter_map(|name| name.as_str())
        .find(|name| name.ends_with(".wasm"))
        .map(PathBuf::from)
}

/// The component of a core module that exports the `adjust` interface through the canonical ABI,
/// as `export_adjustment!` does: the `adjustment` world's metadata embedded, then encoded and
/// validated by `wit-component`. A core module that imports anything, or lacks an export the world
/// needs, is refused with `wit-component`'s reason.
pub fn componentize(core: &[u8]) -> Result<Vec<u8>, Error> {
    let mut resolve = wit_parser::Resolve::default();
    let package = resolve
        .push_str("wit/adjustment.wit", WIT)
        .map_err(|error| format!("the SDK's WIT does not parse: {error:#}"))?;
    let world = resolve
        .select_world(&[package], Some(WORLD))
        .map_err(|error| format!("the SDK's WIT has no {WORLD} world: {error:#}"))?;
    let mut module = core.to_vec();
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )
    .map_err(|error| format!("cannot embed the {WORLD} world: {error:#}"))?;
    wit_component::ComponentEncoder::default()
        .validate(true)
        .module(&module)
        .and_then(|mut encoder| encoder.encode())
        .map_err(|error| format!("the core module is not an {WORLD} module: {error:#}"))
}

/// [`build_core_module`] then [`componentize`]: the component of the module crate at `project`.
pub fn build_component(project: &Path, target_dir: &Path) -> Result<Vec<u8>, Error> {
    let core = build_core_module(project, target_dir)?;
    let bytes =
        std::fs::read(&core).map_err(|error| format!("cannot read {}: {error}", core.display()))?;
    componentize(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pinned_channel_is_read_from_the_toolchain_file() {
        let channel = toolchain_channel();
        assert!(
            channel.split('.').count() == 3
                && channel.chars().all(|c| c.is_ascii_digit() || c == '.'),
            "{channel}"
        );
    }

    #[test]
    fn a_build_keeps_none_of_the_running_package_s_variables() {
        for key in [
            "CARGO_PKG_NAME",
            "CARGO_MANIFEST_DIR",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
        ] {
            assert!(inherited(key), "{key}");
        }
        for key in ["RUSTC_WRAPPER", "PATH", "CARGO_HOME", "RUSTUP_HOME"] {
            assert!(!inherited(key), "{key}");
        }
    }

    #[test]
    fn the_wasm_artifact_is_the_cdylib_of_the_package_built() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("Cargo.toml")
            .canonicalize()
            .unwrap();
        let message = |path: &Path, kinds: &str, file: &str| {
            format!(
                r#"{{"reason":"compiler-artifact","manifest_path":{},"target":{{"crate_types":{kinds}}},"filenames":[{}]}}"#,
                serde_json::json!(path),
                serde_json::json!(file)
            )
        };
        assert_eq!(
            wasm_artifact(&message(&manifest, r#"["cdylib"]"#, "/t/m.wasm"), &manifest),
            Some(PathBuf::from("/t/m.wasm"))
        );
        assert_eq!(
            wasm_artifact(
                &message(&manifest, r#"["rlib"]"#, "/t/libm.rlib"),
                &manifest
            ),
            None
        );
        let other =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../luxforge-test-macros/Cargo.toml");
        assert_eq!(
            wasm_artifact(&message(&other, r#"["cdylib"]"#, "/t/o.wasm"), &manifest),
            None
        );
        assert_eq!(
            wasm_artifact(r#"{"reason":"build-finished"}"#, &manifest),
            None
        );
    }
}
