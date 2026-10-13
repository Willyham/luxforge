//! The editor answers a version request before it parses a launch, opens a window, a catalog or
//! the network, and creates none of the application's files.
#[test]
fn the_editor_reports_the_shared_version_without_launching() {
    let home = luxforge_testbase::paths::temp_dir("editor-version-home");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_luxforge"))
        .arg("--version")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("APPDATA", &home)
        .env("LOCALAPPDATA", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let created: Vec<_> = std::fs::read_dir(&home).unwrap().collect();
    let _ = std::fs::remove_dir_all(&home);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("luxforge {}", luxforge_core::APPLICATION_VERSION)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(luxforge_cli::VERSION, luxforge_core::APPLICATION_VERSION);
    assert!(
        created.is_empty(),
        "a version request created application files"
    );
}
