#[test]
fn malformed_commands_fail_before_delegation() {
    for args in [
        vec!["unknown-command"],
        vec!["check-repository", "--unknown"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_xtask-cli"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown"));
    }
}

#[cfg(unix)]
#[test]
fn delegated_command_keeps_arguments_status_and_a_clean_cargo_environment() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let temp = tempfile::tempdir().unwrap();
    let cargo = temp.path().join("cargo");
    let trace = temp.path().join("arguments");
    fs::write(
        &cargo,
        r#"#!/bin/sh
if [ "${CARGO_PKG_NAME+x}" = x ] || [ "${CARGO_MANIFEST_DIR+x}" = x ]; then exit 99; fi
printf '%s\n' "$@" > "$XTASK_TEST_TRACE"
exit 2
"#,
    )
    .unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = Command::new(env!("CARGO_BIN_EXE_xtask-cli"))
        .args(["smoke", "--output", "a directory/$literal;name"])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("XTASK_TEST_TRACE", &trace)
        .env("CARGO_PKG_NAME", "caller")
        .env("CARGO_MANIFEST_DIR", "caller-directory")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected = "run\n--locked\n--package\nxtask\n".to_owned();
    if env!("LUXFORGE_XTASK_PROFILE") == "release" {
        expected.push_str("--release\n");
    }
    expected.push_str("--\nsmoke\n--output\na directory/$literal;name\n");
    assert_eq!(fs::read_to_string(trace).unwrap(), expected);
}
