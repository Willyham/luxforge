//! What a person pastes into an agent to give it Luxforge's MCP server, `luxforge-mcp`
//! (`docs/design/mcp.md#settings--agents`): one snippet per MCP client from the server's absolute
//! path and its optional arguments. `luxforge-mcp config CLIENT` prints a snippet's text and the
//! desktop's Settings › Agents tab shows the same snippets, so the two always agree.
//!
//! Nothing here reads or writes another application's configuration: a snippet is text, with the
//! file it belongs in named for the person.
use std::path::Path;

/// The name every snippet gives the server in the client's configuration.
pub const SERVER_NAME: &str = "luxforge";

/// An MCP client Luxforge has a snippet for, in the order the desktop offers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Client {
    /// `claude mcp add`, run in a terminal.
    ClaudeCode,
    /// The `mcpServers` block of `claude_desktop_config.json`.
    ClaudeDesktop,
    /// The `[mcp_servers.luxforge]` table of Codex's `config.toml`.
    Codex,
    /// The `mcpServers` block of Cursor's `mcp.json`.
    Cursor,
    /// The `mcpServers` block most other clients read.
    Json,
}

impl Client {
    /// Every client, in the order the desktop offers them.
    pub const ALL: [Client; 5] = [
        Client::ClaudeCode,
        Client::ClaudeDesktop,
        Client::Codex,
        Client::Cursor,
        Client::Json,
    ];

    /// The client's identifier, as `luxforge-mcp config` takes it.
    pub fn id(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::ClaudeDesktop => "claude-desktop",
            Client::Codex => "codex",
            Client::Cursor => "cursor",
            Client::Json => "json",
        }
    }

    /// The client's name, as a person reads it.
    pub fn name(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::ClaudeDesktop => "Claude Desktop",
            Client::Codex => "Codex",
            Client::Cursor => "Cursor",
            Client::Json => "Other (MCP JSON)",
        }
    }

    /// The client `id` names, if any.
    pub fn from_id(id: &str) -> Option<Client> {
        Client::ALL.into_iter().find(|client| client.id() == id)
    }
}

/// The platform a snippet is for: it decides the shell quoting of a command and where a
/// client's configuration file lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    /// Linux and every other Unix.
    Linux,
}

impl Platform {
    /// The platform this build runs on.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// How the server is started: its absolute path, and the catalog it serves when one is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Launch<'a> {
    /// The absolute path of `luxforge-mcp`.
    pub binary: &'a Path,
    /// `--catalog PATH`: the catalog whose live session the server attaches to, or with
    /// [`Launch::headless`] the catalog its own headless owner opens. Absolute.
    pub catalog: Option<&'a Path>,
    /// `--headless`: the server starts its own `luxforge-json` over [`Launch::catalog`] instead of
    /// attaching to a running Luxforge.
    pub headless: bool,
}

/// What kind of text a snippet is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetKind {
    /// A command to run in a terminal.
    Command,
    /// A JSON document to merge into a configuration file.
    Json,
    /// A TOML table to add to a configuration file.
    Toml,
}

/// One client's configuration for a [`Launch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub client: Client,
    pub kind: SnippetKind,
    /// The text to copy, without a trailing newline.
    pub text: String,
    /// The file the text belongs in, as a person finds it on this platform (`~` for the home
    /// folder on macOS and Linux, environment variables on Windows); `None` for a command, and for
    /// the generic block, whose file depends on the client.
    pub location: Option<String>,
    /// One line saying what to do with the text.
    pub instruction: String,
}

/// Why no snippet could be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnippetError {
    /// A path is relative: a client starts the server from a folder of its own choosing.
    NotAbsolute(String),
    /// A path is not Unicode, so no configuration format can hold it.
    NotUnicode(String),
    /// `headless` without a catalog.
    HeadlessWithoutCatalog,
}

impl std::fmt::Display for SnippetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnippetError::NotAbsolute(path) => write!(f, "{path} is not an absolute path"),
            SnippetError::NotUnicode(path) => {
                write!(
                    f,
                    "{path} is not valid Unicode, so no client can be given it"
                )
            }
            SnippetError::HeadlessWithoutCatalog => f.write_str("--headless requires --catalog"),
        }
    }
}

impl std::error::Error for SnippetError {}

/// Where `luxforge-mcp` is shipped relative to `editor`, the editor's own canonical executable
/// path: `Contents/Helpers` of a macOS bundle (the editor is in `Contents/MacOS`), otherwise the
/// editor's own folder, as in the Linux payload and a development build's target folder. Whether
/// it is there is the caller's to check.
pub fn server_beside(editor: &Path, platform: Platform) -> std::path::PathBuf {
    let name = match platform {
        Platform::Windows => "luxforge-mcp.exe",
        Platform::MacOs | Platform::Linux => "luxforge-mcp",
    };
    let folder = editor.parent();
    let contents = folder
        .filter(|folder| folder.file_name().is_some_and(|name| name == "MacOS"))
        .and_then(Path::parent)
        .filter(|contents| contents.file_name().is_some_and(|name| name == "Contents"));
    match (platform, contents, folder) {
        (Platform::MacOs, Some(contents), _) => contents.join("Helpers").join(name),
        (_, _, Some(folder)) => folder.join(name),
        (_, _, None) => std::path::PathBuf::from(name),
    }
}

/// Every client's snippet for `launch` on `platform`, in [`Client::ALL`]'s order.
pub fn snippets(launch: Launch<'_>, platform: Platform) -> Result<Vec<Snippet>, SnippetError> {
    Client::ALL
        .into_iter()
        .map(|client| snippet(client, launch, platform))
        .collect()
}

/// `client`'s snippet for `launch` on `platform`.
pub fn snippet(
    client: Client,
    launch: Launch<'_>,
    platform: Platform,
) -> Result<Snippet, SnippetError> {
    let (command, args) = command_line(launch)?;
    let home = |unix: &str, windows: &str| match platform {
        Platform::Windows => windows.to_owned(),
        Platform::MacOs | Platform::Linux => unix.to_owned(),
    };
    let snippet = match client {
        Client::ClaudeCode => {
            let words: Vec<String> = std::iter::once(command.as_str())
                .chain(args.iter().map(String::as_str))
                .map(|word| shell_word(word, platform))
                .collect();
            Snippet {
                client,
                kind: SnippetKind::Command,
                text: format!(
                    "claude mcp add --scope user {SERVER_NAME} -- {}",
                    words.join(" ")
                ),
                location: None,
                instruction: "Run this in a terminal; Luxforge is then available to Claude Code in \
                              every project."
                    .to_owned(),
            }
        }
        Client::ClaudeDesktop => Snippet {
            client,
            kind: SnippetKind::Json,
            text: servers_json(&command, &args, false),
            location: Some(match platform {
                Platform::MacOs => {
                    "~/Library/Application Support/Claude/claude_desktop_config.json".to_owned()
                }
                Platform::Windows => r"%APPDATA%\Claude\claude_desktop_config.json".to_owned(),
                Platform::Linux => "~/.config/Claude/claude_desktop_config.json".to_owned(),
            }),
            instruction: "Add the luxforge entry to mcpServers in Claude Desktop's configuration \
                          (Settings › Developer › Edit Config), then quit and reopen Claude Desktop."
                .to_owned(),
        },
        Client::Codex => Snippet {
            client,
            kind: SnippetKind::Toml,
            text: format!(
                "[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = [{}]",
                toml_string(&command),
                args.iter()
                    .map(|arg| toml_string(arg))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            location: Some(home("~/.codex/config.toml", r"%USERPROFILE%\.codex\config.toml")),
            instruction: "Add this table to Codex's config.toml, then start a new Codex session."
                .to_owned(),
        },
        Client::Cursor => Snippet {
            client,
            kind: SnippetKind::Json,
            text: servers_json(&command, &args, true),
            location: Some(home("~/.cursor/mcp.json", r"%USERPROFILE%\.cursor\mcp.json")),
            instruction: "Add the luxforge entry to mcpServers in Cursor's mcp.json, or a \
                          project's .cursor/mcp.json for that project only."
                .to_owned(),
        },
        Client::Json => Snippet {
            client,
            kind: SnippetKind::Json,
            text: servers_json(&command, &args, false),
            location: None,
            instruction: "Add the luxforge entry to your MCP client's mcpServers configuration; \
                          it starts luxforge-mcp over standard input and output."
                .to_owned(),
        },
    };
    Ok(snippet)
}

/// The server's command and arguments, as Unicode text, checked.
fn command_line(launch: Launch<'_>) -> Result<(String, Vec<String>), SnippetError> {
    let text = |path: &Path| {
        let shown = path.to_string_lossy().into_owned();
        if !path.is_absolute() {
            return Err(SnippetError::NotAbsolute(shown));
        }
        path.to_str()
            .map(str::to_owned)
            .ok_or(SnippetError::NotUnicode(shown))
    };
    let command = text(launch.binary)?;
    let mut args = Vec::new();
    match (launch.headless, launch.catalog) {
        (true, None) => return Err(SnippetError::HeadlessWithoutCatalog),
        (headless, Some(catalog)) => {
            if headless {
                args.push("--headless".to_owned());
            }
            args.push("--catalog".to_owned());
            args.push(text(catalog)?);
        }
        (false, None) => {}
    }
    Ok((command, args))
}

/// `{"mcpServers": {"luxforge": {...}}}`, two-space indented with `command` before `args` as the
/// clients' own documentation writes it. Cursor's entry names its transport first.
fn servers_json(command: &str, args: &[String], typed: bool) -> String {
    let string = |text: &str| serde_json::to_string(text).expect("a string serializes");
    let mut fields = Vec::new();
    if typed {
        fields.push(format!("\"type\": {}", string("stdio")));
    }
    fields.push(format!("\"command\": {}", string(command)));
    fields.push(if args.is_empty() {
        "\"args\": []".to_owned()
    } else {
        let args: Vec<String> = args
            .iter()
            .map(|arg| format!("        {}", string(arg)))
            .collect();
        format!("\"args\": [\n{}\n      ]", args.join(",\n"))
    });
    let fields: Vec<String> = fields
        .into_iter()
        .map(|field| format!("      {field}"))
        .collect();
    format!(
        "{{\n  \"mcpServers\": {{\n    {}: {{\n{}\n    }}\n  }}\n}}",
        string(SERVER_NAME),
        fields.join(",\n")
    )
}

/// `text` as one shell word: a flag as it is, anything else in single quotes, with a quote inside
/// escaped as POSIX shells (on macOS and Linux) or PowerShell (on Windows) read it. A path is
/// always quoted, so the snippet reads the same whatever folder the server is in.
fn shell_word(text: &str, platform: Platform) -> String {
    // Only the flags are plain; a path always has a separator.
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if plain {
        return text.to_owned();
    }
    match platform {
        Platform::MacOs | Platform::Linux => format!("'{}'", text.replace('\'', r"'\''")),
        // PowerShell: a quote inside single quotes, which may be a typographic one, is doubled, and
        // nothing else is special.
        Platform::Windows => {
            let mut quoted = String::from("'");
            for c in text.chars() {
                if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                    quoted.push(c);
                }
                quoted.push(c);
            }
            quoted.push('\'');
            quoted
        }
    }
}

/// `text` as a TOML basic string.
fn toml_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if c.is_control() && (c as u32) < 0x80 => {
                quoted.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests;
