//! A version request must finish without a catalog or JSON input/live session.
#[test]
fn slow_both_command_lines_report_the_same_compiled_version_without_a_catalog() {
    for (name, binary) in [
        ("luxforge-json", env!("CARGO_BIN_EXE_luxforge-json")),
        ("luxforge-ctl", env!("CARGO_BIN_EXE_luxforge-ctl")),
    ] {
        let output = std::process::Command::new(binary)
            .arg("--version")
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
}
