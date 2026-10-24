//! `config`'s arguments and what it prints.
use super::*;
use luxforge_core::agents::snippet;
use std::path::Path;

const BINARY: &str = "/Applications/Luxforge.app/Contents/Helpers/luxforge-mcp";

fn parsed(args: &[&str]) -> Result<Request, String> {
    parse(args.iter().map(Into::into))
}

/// `config` with `args`: its status, standard output and standard error.
fn printed(args: &[&str], binary: std::io::Result<PathBuf>) -> (i32, String, String) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let status = run(
        args.iter().map(Into::into),
        binary,
        Platform::MacOs,
        &mut stdout,
        &mut stderr,
    );
    (
        status,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn config_reads_a_client_and_the_servers_arguments() {
    assert_eq!(
        parsed(&["codex"]),
        Ok(Request {
            client: Client::Codex,
            catalog: None,
            headless: false,
        })
    );
    assert_eq!(
        parsed(&["cursor", "--catalog", "/c", "--headless"]),
        Ok(Request {
            client: Client::Cursor,
            catalog: Some("/c".into()),
            headless: true,
        })
    );
    for args in [
        &[][..],
        &["claude"],
        &["--headless", "codex"],
        &["json", "--headless"],
        &["json", "--catalog"],
        &["json", "--catalog", "/a", "--catalog", "/b"],
        &["json", "--headless", "--headless", "--catalog", "/a"],
        &["json", "--tools", "compact"],
    ] {
        assert!(parsed(args).is_err(), "{args:?}");
    }
    assert!(
        parsed(&["claude"])
            .unwrap_err()
            .contains("claude-code, claude-desktop, codex, cursor, json")
    );
}

#[test]
fn config_prints_the_snippet_text_and_says_where_it_goes() {
    let (status, stdout, stderr) = printed(
        &["claude-desktop", "--headless", "--catalog", "/c"],
        Ok(BINARY.into()),
    );
    assert_eq!(status, 0);
    let expected = snippet(
        Client::ClaudeDesktop,
        Launch {
            binary: Path::new(BINARY),
            catalog: Some(Path::new("/c")),
            headless: true,
        },
        Platform::MacOs,
    )
    .unwrap();
    assert_eq!(stdout, format!("{}\n", expected.text));
    assert_eq!(
        stderr,
        format!(
            "In ~/Library/Application Support/Claude/claude_desktop_config.json:\n{}\n",
            expected.instruction
        )
    );
}

#[test]
fn a_relative_catalog_is_named_from_the_current_folder() {
    let (status, stdout, _) = printed(&["json", "--catalog", "photos"], Ok(BINARY.into()));
    assert_eq!(status, 0);
    let absolute = std::env::current_dir().unwrap().join("photos");
    let text: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        text["mcpServers"]["luxforge"]["args"][1],
        absolute.to_str().unwrap()
    );
}

#[test]
fn usage_and_path_failures_print_nothing_to_standard_output() {
    let (status, stdout, stderr) = printed(&["json", "--headless"], Ok(BINARY.into()));
    assert_eq!((status, stdout.as_str()), (EXIT_USAGE, ""));
    let error: serde_json::Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(error["error"]["code"], USAGE);
    let (status, stdout, stderr) = printed(&["json"], Err(std::io::Error::other("no executable")));
    assert_eq!((status, stdout.as_str()), (EXIT_FAILURE, ""));
    let error: serde_json::Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(error["error"]["code"], INTERNAL);
}
