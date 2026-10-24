use super::*;
use std::path::PathBuf;

const BUNDLED: &str = "/Applications/Luxforge.app/Contents/Helpers/luxforge-mcp";

fn live(binary: &Path) -> Launch<'_> {
    Launch {
        binary,
        catalog: None,
        headless: false,
    }
}

fn text(client: Client, launch: Launch<'_>, platform: Platform) -> String {
    snippet(client, launch, platform).unwrap().text
}

/// The command and arguments a JSON snippet starts, read back with a JSON parser.
fn json_command(text: &str, typed: bool) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    let servers = value["mcpServers"].as_object().unwrap();
    assert_eq!(servers.len(), 1);
    let server = servers[SERVER_NAME].as_object().unwrap();
    assert_eq!(server.len(), if typed { 3 } else { 2 }, "{server:?}");
    if typed {
        assert_eq!(server["type"], "stdio");
    }
    std::iter::once(&server["command"])
        .chain(server["args"].as_array().unwrap())
        .map(|word| word.as_str().unwrap().to_owned())
        .collect()
}

/// The command and arguments Codex's table starts, read back with a TOML parser.
fn toml_command(text: &str) -> Vec<String> {
    let document = toml_edit::Document::parse(text).unwrap();
    let servers = document["mcp_servers"].as_table().unwrap();
    assert_eq!(servers.len(), 1);
    let server = servers[SERVER_NAME].as_table().unwrap();
    assert_eq!(server.len(), 2);
    std::iter::once(server["command"].as_str().unwrap().to_owned())
        .chain(
            server["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|word| word.as_str().unwrap().to_owned()),
        )
        .collect()
}

/// The words a POSIX shell reads from a Claude Code command after `--`.
#[cfg(unix)]
fn shell_command(text: &str) -> Vec<String> {
    let prefix = format!("claude mcp add --scope user {SERVER_NAME} -- ");
    let words = text.strip_prefix(&prefix).unwrap();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "for word in {words}; do printf '%s\\0' \"$word\"; done"
        ))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .split_terminator('\0')
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_client_has_an_id_a_name_and_a_snippet_in_the_offered_order() {
    let ids: Vec<_> = Client::ALL.iter().map(|client| client.id()).collect();
    assert_eq!(
        ids,
        ["claude-code", "claude-desktop", "codex", "cursor", "json"]
    );
    for client in Client::ALL {
        assert_eq!(Client::from_id(client.id()), Some(client));
        assert!(!client.name().is_empty());
    }
    assert_eq!(Client::from_id("claude"), None);
    let all = snippets(live(Path::new(BUNDLED)), Platform::MacOs).unwrap();
    assert_eq!(
        all.iter().map(|s| s.client).collect::<Vec<_>>(),
        Client::ALL
    );
    for snippet in &all {
        assert!(!snippet.text.ends_with('\n'));
        assert!(!snippet.instruction.contains('\n'));
    }
}

#[test]
fn the_bundled_server_has_each_clients_configuration() {
    let launch = live(Path::new(BUNDLED));
    let mac = Platform::MacOs;
    assert_eq!(
        text(Client::ClaudeCode, launch, mac),
        format!("claude mcp add --scope user luxforge -- '{BUNDLED}'")
    );
    let json = format!(
        "{{\n  \"mcpServers\": {{\n    \"luxforge\": {{\n      \"command\": \"{BUNDLED}\",\n      \"args\": []\n    }}\n  }}\n}}"
    );
    assert_eq!(text(Client::ClaudeDesktop, launch, mac), json);
    assert_eq!(text(Client::Json, launch, mac), json);
    assert_eq!(
        text(Client::Cursor, launch, mac),
        format!(
            "{{\n  \"mcpServers\": {{\n    \"luxforge\": {{\n      \"type\": \"stdio\",\n      \"command\": \"{BUNDLED}\",\n      \"args\": []\n    }}\n  }}\n}}"
        )
    );
    assert_eq!(
        text(Client::Codex, launch, mac),
        format!("[mcp_servers.luxforge]\ncommand = \"{BUNDLED}\"\nargs = []")
    );
    let headless = Launch {
        catalog: Some(Path::new("/Users/someone/Pictures/Luxforge")),
        headless: true,
        ..launch
    };
    assert_eq!(
        text(Client::ClaudeDesktop, headless, mac),
        format!(
            "{{\n  \"mcpServers\": {{\n    \"luxforge\": {{\n      \"command\": \"{BUNDLED}\",\n      \"args\": [\n        \"--headless\",\n        \"--catalog\",\n        \"/Users/someone/Pictures/Luxforge\"\n      ]\n    }}\n  }}\n}}"
        )
    );
    assert_eq!(
        text(Client::Codex, headless, mac),
        format!(
            "[mcp_servers.luxforge]\ncommand = \"{BUNDLED}\"\nargs = [\"--headless\", \"--catalog\", \"/Users/someone/Pictures/Luxforge\"]"
        )
    );
}

#[test]
fn each_configuration_file_is_named_for_its_platform() {
    let launch = live(Path::new(BUNDLED));
    let location = |client, platform| snippet(client, launch, platform).unwrap().location;
    let named = |path: &str| Some(path.to_owned());
    assert_eq!(location(Client::ClaudeCode, Platform::MacOs), None);
    assert_eq!(location(Client::Json, Platform::Linux), None);
    for (platform, desktop, codex, cursor) in [
        (
            Platform::MacOs,
            "~/Library/Application Support/Claude/claude_desktop_config.json",
            "~/.codex/config.toml",
            "~/.cursor/mcp.json",
        ),
        (
            Platform::Windows,
            r"%APPDATA%\Claude\claude_desktop_config.json",
            r"%USERPROFILE%\.codex\config.toml",
            r"%USERPROFILE%\.cursor\mcp.json",
        ),
        (
            Platform::Linux,
            "~/.config/Claude/claude_desktop_config.json",
            "~/.codex/config.toml",
            "~/.cursor/mcp.json",
        ),
    ] {
        assert_eq!(location(Client::ClaudeDesktop, platform), named(desktop));
        assert_eq!(location(Client::Codex, platform), named(codex));
        assert_eq!(location(Client::Cursor, platform), named(cursor));
    }
}

#[test]
fn headless_and_catalog_arguments_follow_the_path_in_every_format() {
    let binary = Path::new("/opt/Luxforge/luxforge-mcp");
    let catalog = Path::new("/home/someone/Photos/main.luxforge");
    for (headless, args) in [
        (
            false,
            vec!["--catalog", "/home/someone/Photos/main.luxforge"],
        ),
        (
            true,
            vec![
                "--headless",
                "--catalog",
                "/home/someone/Photos/main.luxforge",
            ],
        ),
    ] {
        let launch = Launch {
            binary,
            catalog: Some(catalog),
            headless,
        };
        let mut expected = vec!["/opt/Luxforge/luxforge-mcp"];
        expected.extend(&args);
        let quoted = args
            .iter()
            .map(|arg| {
                if arg.starts_with('/') {
                    format!("'{arg}'")
                } else {
                    (*arg).to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            text(Client::ClaudeCode, launch, Platform::Linux),
            format!(
                "claude mcp add --scope user luxforge -- '/opt/Luxforge/luxforge-mcp' {quoted}"
            )
        );
        assert_eq!(
            json_command(&text(Client::ClaudeDesktop, launch, Platform::Linux), false),
            expected
        );
        assert_eq!(
            json_command(&text(Client::Cursor, launch, Platform::Linux), true),
            expected
        );
        assert_eq!(
            json_command(&text(Client::Json, launch, Platform::Linux), false),
            expected
        );
        assert_eq!(
            toml_command(&text(Client::Codex, launch, Platform::Linux)),
            expected
        );
    }
}

#[test]
fn awkward_paths_survive_every_format() {
    let folder = "/Users/some one/it's \"here\"/back\\slash/ünïcode $HOME `x` \t";
    let binary = PathBuf::from(format!("{folder}/luxforge-mcp"));
    let catalog = PathBuf::from(format!("{folder}/cat'alog"));
    let launch = Launch {
        binary: &binary,
        catalog: Some(&catalog),
        headless: true,
    };
    let expected = vec![
        binary.to_str().unwrap().to_owned(),
        "--headless".to_owned(),
        "--catalog".to_owned(),
        catalog.to_str().unwrap().to_owned(),
    ];
    for platform in [Platform::MacOs, Platform::Linux, Platform::Windows] {
        for client in [Client::ClaudeDesktop, Client::Cursor, Client::Json] {
            let typed = client == Client::Cursor;
            assert_eq!(
                json_command(&text(client, launch, platform), typed),
                expected
            );
        }
        assert_eq!(
            toml_command(&text(Client::Codex, launch, platform)),
            expected
        );
    }
    #[cfg(unix)]
    for platform in [Platform::MacOs, Platform::Linux] {
        assert_eq!(
            shell_command(&text(Client::ClaudeCode, launch, platform)),
            expected
        );
    }
    // PowerShell doubles a quote, typographic ones included, inside single quotes.
    let windows = Path::new("/Program Files/it's ‘Luxforge’/luxforge-mcp");
    assert_eq!(
        text(Client::ClaudeCode, live(windows), Platform::Windows),
        "claude mcp add --scope user luxforge -- '/Program Files/it''s ‘‘Luxforge’’/luxforge-mcp'"
    );
}

#[test]
fn the_server_is_found_in_the_bundles_helpers_or_beside_the_editor() {
    assert_eq!(
        server_beside(
            Path::new("/Applications/Luxforge.app/Contents/MacOS/luxforge"),
            Platform::MacOs
        ),
        Path::new(BUNDLED)
    );
    // A development build, and the Linux payload, keep it beside the editor.
    assert_eq!(
        server_beside(
            Path::new("/Users/someone/luxforge/target/release/luxforge"),
            Platform::MacOs
        ),
        Path::new("/Users/someone/luxforge/target/release/luxforge-mcp")
    );
    assert_eq!(
        server_beside(
            Path::new("/home/someone/Luxforge/luxforge"),
            Platform::Linux
        ),
        Path::new("/home/someone/Luxforge/luxforge-mcp")
    );
    assert_eq!(
        server_beside(Path::new("/Luxforge/luxforge.exe"), Platform::Windows),
        Path::new("/Luxforge/luxforge-mcp.exe")
    );
}

#[test]
fn toml_control_characters_are_escaped() {
    assert_eq!(
        toml_string("a\u{1}b\u{7f}c\"\\"),
        r#""a\u0001b\u007Fc\"\\""#
    );
}

#[test]
fn relative_or_unreadable_paths_and_headless_without_a_catalog_are_refused() {
    let absolute = Path::new("/opt/luxforge-mcp");
    assert!(matches!(
        snippet(
            Client::Json,
            live(Path::new("luxforge-mcp")),
            Platform::Linux
        ),
        Err(SnippetError::NotAbsolute(_))
    ));
    let relative = Launch {
        binary: absolute,
        catalog: Some(Path::new("catalog")),
        headless: true,
    };
    assert!(matches!(
        snippets(relative, Platform::Linux),
        Err(SnippetError::NotAbsolute(_))
    ));
    let headless = Launch {
        binary: absolute,
        catalog: None,
        headless: true,
    };
    assert_eq!(
        snippet(Client::Codex, headless, Platform::Linux),
        Err(SnippetError::HeadlessWithoutCatalog)
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let unreadable = Path::new(std::ffi::OsStr::from_bytes(b"/opt/\xff/luxforge-mcp"));
        assert!(matches!(
            snippet(Client::Json, live(unreadable), Platform::Linux),
            Err(SnippetError::NotUnicode(_))
        ));
    }
}
