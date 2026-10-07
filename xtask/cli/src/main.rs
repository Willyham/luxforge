//! Cheap Cargo entry point; photo/evidence commands build the full tooling only when requested.
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, ExitCode, ExitStatus},
};
use xtask_cli::{Args, Result, cargo, cargo_command, check, policy, repository, root};

const FULL_COMMANDS: &[&str] = &[
    "develop",
    "doctor",
    "fixtures",
    "generate-fixtures",
    "gazetteer",
    "generate-catalog",
    "raw-camera-metadata",
    "inspect-dng",
    "raw-authentic",
    "lens-qualification",
    "lensfun-import",
    "inventory",
    "package",
    "editor-acceptance",
    "editor-performance",
    "detail-performance",
    "detail-grid-performance",
    "editor-latency",
    "smoke",
    "verify",
    "check-capture",
    "catalog-measure",
    "hardening",
    "measure",
    "preview-error",
    "preview-corpus",
    "gpu-qualification",
    "__hang",
];

fn full_command(root: &Path, args: Vec<OsString>, release: bool) -> Command {
    let mut command = cargo_command();
    command
        .current_dir(root)
        .args(["run", "--locked", "--package", "xtask"]);
    if release {
        command.arg("--release");
    }
    command.arg("--").args(args);
    command
}

fn exit_code(status: ExitStatus) -> ExitCode {
    // A signal or a Windows status outside the portable byte range must still fail.
    ExitCode::from(
        status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1),
    )
}

fn main_result() -> Result<ExitCode> {
    let root = root()?;
    let original: Vec<_> = std::env::args_os().skip(1).collect();
    let op = original.first().cloned().unwrap_or_else(|| "help".into());
    let mut a = Args(original.iter().skip(1).cloned().collect());
    match op.to_str().ok_or("Invalid command")? {
        "check" => {
            let quick = a.flag("--quick");
            a.done()?;
            check::check(&root, quick)?;
            println!("Headless checks passed. GUI and platform acceptance remain separate.");
        }
        "check-repository" => {
            a.done()?;
            repository::check(&root)?;
        }
        "test" => {
            let quick = a.flag("--quick");
            a.done()?;
            check::tests(&root, quick)?;
        }
        "build" | "fmt" | "lint" => {
            let release = a.flag("--release");
            a.done()?;
            cargo(&root, op.to_str().unwrap(), release)?;
        }
        "audit" => {
            a.done()?;
            policy::audit(&root)?;
        }
        "help" => {
            a.done()?;
            println!("{}", xtask_cli::HELP);
        }
        op if FULL_COMMANDS.contains(&op) => {
            return Ok(exit_code(
                full_command(&root, original, env!("LUXFORGE_XTASK_PROFILE") == "release")
                    .status()?,
            ));
        }
        _ => return Err("Unknown command; use cargo xtask help".into()),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match main_result() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("FAIL: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_preserves_arguments_and_release_profile() {
        let arguments: Vec<OsString> = ["smoke", "--output", "a directory/$literal;name"]
            .into_iter()
            .map(Into::into)
            .collect();
        for release in [false, true] {
            let command = full_command(Path::new("."), arguments.clone(), release);
            let actual: Vec<_> = command.get_args().collect();
            let mut expected: Vec<OsString> = ["run", "--locked", "--package", "xtask"]
                .into_iter()
                .map(Into::into)
                .collect();
            if release {
                expected.push("--release".into());
            }
            expected.push("--".into());
            expected.extend(arguments.clone());
            assert_eq!(actual, expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn delegation_preserves_incomplete_and_failure_exit_codes() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code(ExitStatus::from_raw(2 << 8)), ExitCode::from(2));
        assert_eq!(exit_code(ExitStatus::from_raw(1 << 8)), ExitCode::FAILURE);
        assert_eq!(exit_code(ExitStatus::from_raw(15)), ExitCode::FAILURE);
    }

    #[test]
    fn every_full_tool_command_has_a_cli_route() {
        let source =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/main.rs"))
                .unwrap();
        let arms = regex::Regex::new(r#"(?m)^        ("[^"]+"(?: \| "[^"]+")*) =>"#).unwrap();
        let quotes = regex::Regex::new(r#""([^"]+)""#).unwrap();
        let mut commands = std::collections::BTreeSet::new();
        for arm in arms.captures_iter(&source) {
            for command in quotes.captures_iter(&arm[1]) {
                commands.insert(command[1].to_owned());
            }
        }
        let mut routes: std::collections::BTreeSet<_> = FULL_COMMANDS
            .iter()
            .map(|command| command.to_string())
            .collect();
        routes.extend(
            [
                "check",
                "check-repository",
                "test",
                "build",
                "fmt",
                "lint",
                "audit",
                "help",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        assert_eq!(commands, routes);
    }
}
