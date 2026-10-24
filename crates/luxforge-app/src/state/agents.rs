//! Settings › Agents ([design](../../../../docs/design/mcp.md#settings--agents)): the
//! `luxforge-mcp` beside this build, every MCP client's configuration for it with Copy, and the
//! other live clients connected now with Disconnect. The configuration text is the core's
//! ([`luxforge_core::agents`]), the same `luxforge-mcp config` prints, and the list is
//! `session.clients` as the owner last answered it; this module holds what the desktop last read
//! and derives what the tab draws. Nothing here reads or writes another application's
//! configuration.
use luxforge_core::agents::{self, Client, Launch, Platform, Snippet, SnippetError};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The tab's one opening sentence.
pub(crate) const INTRO: &str = "Agents use the same operations you do: every edit an agent makes \
     is in history and can be undone, and an agent can never grant consent or permissions.";

/// What a listed client without a label is called.
pub(crate) const UNNAMED: &str = "Unnamed client";

/// The line under Connected when no other client is.
pub(crate) const NONE_CONNECTED: &str = "No agent is connected.";

/// The command that builds `luxforge-mcp` beside an editor of this build's profile.
pub(crate) const BUILD_COMMAND: &str = if cfg!(debug_assertions) {
    "cargo build -p luxforge-cli --bin luxforge-mcp"
} else {
    "cargo build --release -p luxforge-cli --bin luxforge-mcp"
};

/// Where `luxforge-mcp` is for this build, as the tab last looked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Server {
    /// It is beside this build, at this path.
    Found(PathBuf),
    /// It should be at this path, and is not.
    Missing(PathBuf),
    /// This build could not find its own executable, so not the server beside it either.
    Unknown(String),
}

impl Server {
    /// The server beside the editor at `editor`, its own canonical executable path: found when a
    /// file is there.
    pub(crate) fn beside(editor: &Path) -> Self {
        let path = agents::server_beside(editor, Platform::current());
        if path.is_file() {
            Self::Found(path)
        } else {
            Self::Missing(path)
        }
    }

    /// The server beside the running editor. This stats a file, so it runs off the update loop.
    pub(crate) fn of_this_build() -> Self {
        match std::env::current_exe().and_then(|exe| exe.canonicalize()) {
            Ok(editor) => Self::beside(&editor),
            Err(error) => Self::Unknown(error.to_string()),
        }
    }
}

/// One row of `session.clients`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ListedClient {
    pub(crate) client_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    pub(crate) authority: String,
    pub(crate) transport: String,
    /// Unix milliseconds.
    pub(crate) connected_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_request_at: Option<i64>,
    pub(crate) requests: u64,
}

/// `session.clients`' answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClientList {
    pub(crate) clients: Vec<ListedClient>,
}

/// What one read of the tab answered: the server beside this build and the other live clients.
#[derive(Clone, Debug)]
pub(crate) struct AgentsRead {
    pub(crate) server: Server,
    pub(crate) clients: Result<ClientList, String>,
}

/// The tab's own state: what it last read, the client chosen, the reads and disconnections in
/// flight and what Copy last put on the clipboard.
#[derive(Clone, Debug)]
pub(crate) struct Agents {
    pub(crate) server: Option<Server>,
    pub(crate) clients: Option<ClientList>,
    /// Why the last read of the clients failed, until one succeeds.
    pub(crate) error: Option<String>,
    /// Why the last Disconnect failed, until the next one is pressed.
    pub(crate) disconnect_error: Option<String>,
    /// The client whose configuration is shown.
    pub(crate) chosen: Client,
    /// A read is in flight.
    pub(crate) reading: bool,
    /// Something changed while the read in flight was out: read once more when it answers.
    pub(crate) again: bool,
    /// Clients whose Disconnect has not answered yet.
    pub(crate) disconnecting: BTreeSet<u64>,
    /// The client and the text Copy last put on the clipboard.
    pub(crate) copied: Option<(Client, String)>,
}

impl Default for Agents {
    fn default() -> Self {
        Self {
            server: None,
            clients: None,
            error: None,
            disconnect_error: None,
            chosen: Client::ClaudeCode,
            reading: false,
            again: false,
            disconnecting: BTreeSet::new(),
            copied: None,
        }
    }
}

impl Agents {
    /// The chosen client's configuration for the server beside this build: `None` until it is
    /// found, an error when no configuration can hold its path.
    pub(crate) fn snippet(&self) -> Option<Result<Snippet, SnippetError>> {
        let Some(Server::Found(binary)) = &self.server else {
            return None;
        };
        Some(agents::snippet(
            self.chosen,
            Launch {
                binary,
                catalog: None,
                headless: false,
            },
            Platform::current(),
        ))
    }

    /// The label a listed client is shown with.
    pub(crate) fn label(&self, client_id: u64) -> Option<&str> {
        self.clients
            .iter()
            .flat_map(|list| &list.clients)
            .find(|client| client.client_id == client_id)
            .map(|client| client.label.as_deref().unwrap_or(UNNAMED))
    }

    /// Every connected client's label as its row draws it, in list order.
    pub(crate) fn labels(&self) -> impl Iterator<Item = &str> {
        self.clients
            .iter()
            .flat_map(|list| &list.clients)
            .map(|client| client.label.as_deref().unwrap_or(UNNAMED))
    }
}

/// What the server part of the tab says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServerLine {
    /// Not looked for yet.
    Reading,
    Found(String),
    /// Where it should be, and how to build it there.
    Missing {
        missing: String,
        build: String,
    },
    Unknown(String),
}

/// The chosen client's configuration as the tab draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnippetView {
    pub(crate) text: String,
    /// "Goes in" the file it belongs in, when it has one.
    pub(crate) location: Option<String>,
    pub(crate) instruction: String,
}

/// One connected client as the tab draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientRow {
    pub(crate) client_id: u64,
    pub(crate) label: String,
    /// When it connected, its last request and how many it has sent.
    pub(crate) detail: String,
    /// Its Disconnect has not answered yet, so the button is disabled.
    pub(crate) disconnecting: bool,
}

/// The tab as it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentsModel {
    pub(crate) server: ServerLine,
    /// The segmented choice's labels, in [`Client::ALL`]'s order, and the chosen one; shown only
    /// with the server found.
    pub(crate) choices: Vec<&'static str>,
    pub(crate) chosen: usize,
    pub(crate) snippet: Option<SnippetView>,
    /// Why no configuration can be made for the server found.
    pub(crate) snippet_error: Option<String>,
    pub(crate) rows: Vec<ClientRow>,
    /// The line shown in place of the rows: reading, or none connected.
    pub(crate) empty: Option<&'static str>,
    pub(crate) error: Option<String>,
}

/// The tab for `agents` at `now_ms`, Unix milliseconds, which the connected rows' times are said
/// against.
pub(crate) fn derive(agents: &Agents, now_ms: i64) -> AgentsModel {
    let server = match &agents.server {
        None => ServerLine::Reading,
        Some(Server::Found(path)) => ServerLine::Found(path.display().to_string()),
        Some(Server::Missing(path)) => ServerLine::Missing {
            missing: format!(
                "This build has no luxforge-mcp beside it: {} is missing.",
                path.display()
            ),
            build: format!("Build it with {BUILD_COMMAND}, then open this tab again."),
        },
        Some(Server::Unknown(reason)) => {
            ServerLine::Unknown(format!("Could not find this build's executable: {reason}"))
        }
    };
    let (snippet, snippet_error) = match agents.snippet() {
        Some(Ok(snippet)) => (
            Some(SnippetView {
                text: snippet.text,
                location: snippet.location.map(|file| format!("Goes in {file}")),
                instruction: snippet.instruction,
            }),
            None,
        ),
        Some(Err(error)) => (
            None,
            Some(format!("No client can be given this server: {error}")),
        ),
        None => (None, None),
    };
    let rows: Vec<ClientRow> = agents
        .clients
        .iter()
        .flat_map(|list| &list.clients)
        .map(|client| ClientRow {
            client_id: client.client_id,
            label: client.label.clone().unwrap_or_else(|| UNNAMED.to_owned()),
            detail: detail(client, now_ms),
            disconnecting: agents.disconnecting.contains(&client.client_id),
        })
        .collect();
    let empty = match &agents.clients {
        None if agents.error.is_none() => Some("Reading the connected clients\u{2026}"),
        Some(list) if list.clients.is_empty() => Some(NONE_CONNECTED),
        _ => None,
    };
    let error = agents
        .error
        .as_ref()
        .map(|reason| format!("Could not read the connected clients: {reason}"))
        .or_else(|| agents.disconnect_error.clone());
    AgentsModel {
        server,
        choices: Client::ALL.into_iter().map(Client::name).collect(),
        chosen: Client::ALL
            .iter()
            .position(|client| *client == agents.chosen)
            .unwrap_or_default(),
        snippet,
        snippet_error,
        rows,
        empty,
        error,
    }
}

/// "Connected 12 minutes ago · last request just now · 38 requests".
fn detail(client: &ListedClient, now_ms: i64) -> String {
    let last = match client.last_request_at {
        Some(at) => format!("last request {}", super::update::ago(now_ms - at)),
        None => "no requests yet".to_owned(),
    };
    let requests = match client.requests {
        1 => "1 request".to_owned(),
        count => format!("{count} requests"),
    };
    format!(
        "Connected {} \u{b7} {last} \u{b7} {requests}",
        super::update::ago(now_ms - client.connected_at)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn found() -> Agents {
        Agents {
            server: Some(Server::Found(PathBuf::from(
                "/Applications/Luxforge.app/Contents/Helpers/luxforge-mcp",
            ))),
            clients: Some(ClientList::default()),
            ..Agents::default()
        }
    }

    fn listed(client_id: u64, label: Option<&str>) -> ListedClient {
        ListedClient {
            client_id,
            label: label.map(str::to_owned),
            authority: "edit".into(),
            transport: "loopback".into(),
            connected_at: NOW - 12 * 60_000,
            last_request_at: Some(NOW - 2_000),
            requests: 38,
        }
    }

    #[test]
    fn agents_the_found_server_shows_the_chosen_clients_core_snippet() {
        let mut agents = found();
        agents.chosen = Client::Codex;
        let model = derive(&agents, NOW);
        let path = "/Applications/Luxforge.app/Contents/Helpers/luxforge-mcp";
        assert_eq!(model.server, ServerLine::Found(path.into()));
        assert_eq!(
            model.choices,
            [
                "Claude Code",
                "Claude Desktop",
                "Codex",
                "Cursor",
                "Other (MCP JSON)"
            ]
        );
        assert_eq!(model.chosen, 2);
        let core = agents::snippet(
            Client::Codex,
            Launch {
                binary: Path::new(path),
                catalog: None,
                headless: false,
            },
            Platform::current(),
        )
        .unwrap();
        let snippet = model.snippet.unwrap();
        assert_eq!(snippet.text, core.text);
        assert_eq!(
            snippet.location,
            core.location.map(|file| format!("Goes in {file}"))
        );
        assert_eq!(snippet.instruction, core.instruction);
        assert_eq!(model.empty, Some(NONE_CONNECTED));
        assert!(model.rows.is_empty());
    }

    #[test]
    fn agents_a_missing_server_names_where_and_how_and_offers_no_configuration() {
        let agents = Agents {
            server: Some(Server::Missing(PathBuf::from("/build/target/luxforge-mcp"))),
            ..found()
        };
        let model = derive(&agents, NOW);
        assert_eq!(
            model.server,
            ServerLine::Missing {
                missing: "This build has no luxforge-mcp beside it: /build/target/luxforge-mcp \
                          is missing."
                    .into(),
                build: format!("Build it with {BUILD_COMMAND}, then open this tab again."),
            }
        );
        assert!(
            BUILD_COMMAND.starts_with("cargo build ") && BUILD_COMMAND.contains("luxforge-mcp")
        );
        assert_eq!(model.snippet, None);
        assert_eq!(model.snippet_error, None);
    }

    #[test]
    fn agents_a_relative_server_path_says_no_client_can_be_given_it() {
        let agents = Agents {
            server: Some(Server::Found(PathBuf::from("luxforge-mcp"))),
            ..found()
        };
        let model = derive(&agents, NOW);
        assert_eq!(model.snippet, None);
        assert_eq!(
            model.snippet_error.as_deref(),
            Some("No client can be given this server: luxforge-mcp is not an absolute path")
        );
    }

    #[test]
    fn agents_each_connected_client_is_a_row_and_an_unlabelled_one_is_unnamed() {
        let mut agents = found();
        let mut quiet = listed(9, None);
        quiet.last_request_at = None;
        quiet.requests = 1;
        quiet.connected_at = NOW - 10_000;
        agents.clients = Some(ClientList {
            clients: vec![listed(7, Some("Claude Code via MCP")), quiet],
        });
        agents.disconnecting.insert(9);
        let model = derive(&agents, NOW);
        assert_eq!(
            model.rows,
            [
                ClientRow {
                    client_id: 7,
                    label: "Claude Code via MCP".into(),
                    detail: "Connected 12 minutes ago \u{b7} last request just now \u{b7} 38 \
                             requests"
                        .into(),
                    disconnecting: false,
                },
                ClientRow {
                    client_id: 9,
                    label: UNNAMED.into(),
                    detail: "Connected just now \u{b7} no requests yet \u{b7} 1 request".into(),
                    disconnecting: true,
                },
            ]
        );
        assert_eq!(model.empty, None);
        assert_eq!(agents.label(9), Some(UNNAMED));
        // The labels an evidence step waits on are the rows' own, read before the derive.
        assert!(
            agents
                .labels()
                .eq(model.rows.iter().map(|row| row.label.as_str()))
        );
    }

    #[test]
    fn agents_before_the_first_read_the_tab_says_it_is_reading() {
        let model = derive(&Agents::default(), NOW);
        assert_eq!(model.server, ServerLine::Reading);
        assert_eq!(model.empty, Some("Reading the connected clients\u{2026}"));
        let failed = Agents {
            error: Some("connection closed".into()),
            ..Agents::default()
        };
        let model = derive(&failed, NOW);
        assert_eq!(model.empty, None);
        assert_eq!(
            model.error.as_deref(),
            Some("Could not read the connected clients: connection closed")
        );
    }

    #[test]
    fn agents_the_server_is_looked_for_beside_the_editor() {
        let root = luxforge_testbase::paths::temp_path("agents-server");
        std::fs::create_dir_all(&root).unwrap();
        let editor = root.join("luxforge-app");
        let expected = agents::server_beside(&editor, Platform::current());
        assert_eq!(Server::beside(&editor), Server::Missing(expected.clone()));
        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        std::fs::write(&expected, b"").unwrap();
        assert_eq!(Server::beside(&editor), Server::Found(expected));
        std::fs::remove_dir_all(root).unwrap();
    }
}
