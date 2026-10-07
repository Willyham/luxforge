//! Shared command parsing and Cargo helpers. No photo or editor dependencies.
use serde_json::Value;
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
};
pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn ensure(ok: bool, message: impl Into<String>) -> Result {
    if ok {
        Ok(())
    } else {
        Err(message.into().into())
    }
}

pub fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub fn output(root: &Path, program: impl AsRef<OsStr>, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()?;
    ensure(out.status.success(), String::from_utf8_lossy(&out.stderr))?;
    Ok(String::from_utf8(out.stdout)?)
}

pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
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

pub fn root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .find(|p| {
            p.join("tools/task-plan.schema.json").is_file() && p.join("xtask/Cargo.toml").is_file()
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| "Run from a Luxforge checkout".into())
}

/// A `cargo` command without the package variables `cargo run` set for this process. `ring`'s build
/// script declares `rerun-if-env-changed` on `CARGO_MANIFEST_DIR`, `CARGO_PKG_NAME` and the version
/// parts, so a Cargo that inherited them from `cargo xtask` would rebuild `ring`, and every crate
/// above it, after a build started from a shell, and the next shell build would rebuild it back.
pub fn cargo_command() -> Command {
    let mut command = Command::new("cargo");
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| {
            key.starts_with("CARGO_PKG_")
                || key.starts_with("CARGO_MANIFEST_")
                || matches!(
                    key,
                    "CARGO_CRATE_NAME" | "CARGO_BIN_NAME" | "CARGO_PRIMARY_PACKAGE"
                )
        }) {
            command.env_remove(key);
        }
    }
    command
}

pub fn cargo(root: &Path, op: &str, release: bool) -> Result {
    let mut args = match op {
        "build" => vec![
            "build",
            "--locked",
            "--package",
            "luxforge-app",
            "--package",
            "luxforge-cli",
        ],
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
    let status = cargo_command().args(&args).current_dir(root).status()?;
    ensure(
        status.success(),
        format!("Command {args:?} failed: {status}"),
    )
}

pub struct Args(pub Vec<OsString>);
impl Args {
    pub fn flag(&mut self, key: &str) -> bool {
        if let Some(i) = self.0.iter().position(|x| x == key) {
            self.0.remove(i);
            true
        } else {
            false
        }
    }
    pub fn value(&mut self, key: &str) -> Result<Option<OsString>> {
        if let Some(i) = self.0.iter().position(|x| x == key) {
            self.0.remove(i);
            ensure(i < self.0.len(), format!("Missing {key} value"))?;
            Ok(Some(self.0.remove(i)))
        } else {
            Ok(None)
        }
    }
    pub fn path(&mut self, key: &str) -> Result<PathBuf> {
        self.value(key)?
            .map(PathBuf::from)
            .ok_or_else(|| format!("Required: {key}").into())
    }
    pub fn done(&self) -> Result {
        ensure(
            self.0.is_empty(),
            format!("Unknown arguments: {:?}", self.0),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spawned_cargo_inherits_none_of_the_package_variables_cargo_sets() {
        let removed: Vec<OsString> = cargo_command()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_owned())
            .collect();
        // `cargo test` sets the same package variables for this process as `cargo run` sets for
        // `xtask`, so every one of them present here must be removed.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("CARGO_PKG_") || name.starts_with("CARGO_MANIFEST_") {
                assert!(
                    removed.contains(&key),
                    "{name} would reach the spawned Cargo"
                );
            }
        }
        assert!(
            !removed.iter().any(|key| key == "CARGO"),
            "the path to Cargo is kept"
        );
    }
}
