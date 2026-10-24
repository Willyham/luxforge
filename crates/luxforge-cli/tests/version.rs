//! A version request must finish without a catalog or JSON input/live session, and without
//! creating any of the application's files.
use luxforge_test_macros::slow;

#[test]
#[slow]
fn every_command_line_reports_the_same_compiled_version_without_a_catalog() {
    let home = luxforge_testbase::paths::temp_dir("version-home");
    for (name, binary) in [
        ("luxforge-json", env!("CARGO_BIN_EXE_luxforge-json")),
        ("luxforge-ctl", env!("CARGO_BIN_EXE_luxforge-ctl")),
        ("luxforge-mcp", env!("CARGO_BIN_EXE_luxforge-mcp")),
    ] {
        let output = std::process::Command::new(binary)
            .arg("--version")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("APPDATA", &home)
            .env("LOCALAPPDATA", &home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_STATE_HOME")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!("{name} {}", luxforge_cli::VERSION)
        );
        assert!(output.stderr.is_empty());
    }
    assert_eq!(luxforge_cli::VERSION, luxforge_core::APPLICATION_VERSION);
    let created: Vec<_> = std::fs::read_dir(&home).unwrap().collect();
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        created.is_empty(),
        "a version request created application files"
    );
}
