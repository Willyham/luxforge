//! `luxforge-mcp config CLIENT` prints exactly the core's snippet for its own executable.
use luxforge_core::agents::{Client, Launch, Platform, snippet};
use luxforge_test_macros::slow;
use std::path::Path;

fn config(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_luxforge-mcp"))
        .arg("config")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

#[test]
#[slow]
fn config_prints_the_core_snippet_for_its_own_path() {
    let binary = Path::new(env!("CARGO_BIN_EXE_luxforge-mcp"))
        .canonicalize()
        .unwrap();
    let catalog = luxforge_testbase::paths::temp_dir("mcp config catalog's 'path'");
    for client in Client::ALL {
        for headless in [None, Some(false), Some(true)] {
            let mut args = vec![client.id()];
            if headless == Some(true) {
                args.push("--headless");
            }
            if headless.is_some() {
                args.extend(["--catalog", catalog.to_str().unwrap()]);
            }
            let launch = Launch {
                binary: &binary,
                catalog: headless.map(|_| catalog.as_path()),
                headless: headless == Some(true),
            };
            let expected = snippet(client, launch, Platform::current()).unwrap();
            let output = config(&args);
            assert!(output.status.success(), "{args:?}: {output:?}");
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("{}\n", expected.text),
                "{args:?}"
            );
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains(&expected.instruction), "{stderr}");
            if let Some(location) = &expected.location {
                assert!(stderr.contains(location.as_str()), "{stderr}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&catalog);
    let output = config(&["claude"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "usage");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("claude-code, claude-desktop, codex, cursor, json")
    );
}
