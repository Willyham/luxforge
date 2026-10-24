//! Settings › Agents ([design](../../../../docs/design/mcp.md#settings--agents)): reading the
//! server beside this build and `session.clients` off the update loop, Copy, and Disconnect
//! through `session.disconnect` on the desktop's own client, which holds permission authority.
//! The tab follows connections without polling: a transport client's connection, identification
//! and disconnection are recorded in the event log, the owner wakes the desktop's event sync for
//! each, and an open tab reads the list again then ([`Editor::agents_followed`]).
use super::{
    Editor,
    message::{
        Message,
        settings::{AgentsMessage, SettingsMessage},
    },
    outcome::Outcome,
    tasks::{call, owner_task},
};
use crate::state::{
    agents::{AgentsRead, ClientList, Server},
    settings::SettingsTab,
};
use iced::Task;
use luxforge_core::{ClientId, OwnerHandle, agents::Client};
use serde_json::{Value, json};

/// One read of the tab, as its task runs it: the server beside this build, looked for again, and
/// `session.clients`.
pub(crate) fn read_agents(owner: &OwnerHandle, client: ClientId) -> AgentsRead {
    let clients = call(owner, client, "session.clients", json!({})).and_then(|(answer, _)| {
        serde_json::from_value::<ClientList>(answer)
            .map_err(|error| format!("unreadable client list: {error}"))
    });
    AgentsRead {
        server: Server::of_this_build(),
        clients,
    }
}

/// `session.disconnect` for `client_id`, as Disconnect's task sends it.
pub(crate) fn disconnect(
    owner: &OwnerHandle,
    client: ClientId,
    client_id: u64,
) -> Result<(), String> {
    call(
        owner,
        client,
        "session.disconnect",
        json!({"client_id": client_id}),
    )
    .map(|_| ())
}

impl Editor {
    /// One Agents tab message.
    pub(super) fn agents_update(&mut self, message: AgentsMessage) -> Task<Message> {
        match message {
            AgentsMessage::Read(read) => {
                let AgentsRead { server, clients } = *read;
                let agents = &mut self.settings.agents;
                agents.reading = false;
                agents.server = Some(server);
                match clients {
                    Ok(list) => {
                        agents.clients = Some(list);
                        agents.error = None;
                    }
                    Err(reason) => agents.error = Some(reason),
                }
                self.event("agents_read", || {
                    json!({"clients": self.settings.agents.clients,
                           "error": self.settings.agents.error})
                });
                if std::mem::take(&mut self.settings.agents.again) {
                    return self.read_agents();
                }
                self.agents_answered();
            }
            AgentsMessage::Choose(index) => {
                if let Some(client) = Client::ALL.get(index) {
                    self.settings.agents.chosen = *client;
                }
            }
            AgentsMessage::Copy => {
                let Some(Ok(snippet)) = self.settings.agents.snippet() else {
                    return Task::none();
                };
                self.status.text = format!("Copied the {} configuration", snippet.client.name());
                self.event(
                    "agents_configuration_copied",
                    || json!({"client": snippet.client.id(), "text": snippet.text}),
                );
                self.settings.agents.copied = Some((snippet.client, snippet.text.clone()));
                return iced::clipboard::write(snippet.text);
            }
            AgentsMessage::Disconnect(client_id) => {
                if !self.settings.agents.disconnecting.insert(client_id) {
                    return Task::none();
                }
                self.settings.agents.disconnect_error = None;
                self.event("agents_disconnect", || json!({"client_id": client_id}));
                let (owner, client) = (self.owner.clone(), self.client);
                return owner_task(
                    move || disconnect(&owner, client, client_id),
                    move |result| {
                        Message::Settings(SettingsMessage::Agents(AgentsMessage::Disconnected {
                            client_id,
                            result,
                        }))
                    },
                );
            }
            AgentsMessage::Disconnected { client_id, result } => {
                self.settings.agents.disconnecting.remove(&client_id);
                if let Err(reason) = &result {
                    let label = self
                        .settings
                        .agents
                        .label(client_id)
                        .unwrap_or("the client")
                        .to_owned();
                    let text = format!("Could not disconnect {label}: {reason}");
                    self.status.text = text.clone();
                    self.settings.agents.disconnect_error = Some(text);
                }
                self.event(
                    "agents_disconnected",
                    || json!({"client_id": client_id, "error": result.as_ref().err()}),
                );
                // The client is gone from the owner's table whether or not its connection has
                // closed yet: the list is read again, as a refusal may also have been for a
                // client that had already left.
                return self.read_agents();
            }
        }
        Task::none()
    }

    /// Read the tab: the server beside this build and the connected clients. One read at a time;
    /// a change while one is out reads once more when it answers.
    pub(crate) fn read_agents(&mut self) -> Task<Message> {
        let agents = &mut self.settings.agents;
        if agents.reading {
            agents.again = true;
            return Task::none();
        }
        agents.reading = true;
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || read_agents(&owner, client),
            |read| Message::Settings(SettingsMessage::Agents(AgentsMessage::Read(Box::new(read)))),
        )
    }

    /// The event sync read a client's connection, identification or disconnection, or a gap: an
    /// open Agents tab reads the list again, and a closed one reads nothing until it opens.
    pub(crate) fn agents_followed(&mut self, sessions: bool) -> Task<Message> {
        if sessions && self.settings.open == Some(SettingsTab::Agents) {
            self.read_agents()
        } else {
            Task::none()
        }
    }

    /// The tab has nothing in flight: an evidence step waiting on it can be captured.
    fn agents_answered(&mut self) {
        let agents = &self.settings.agents;
        if !agents.reading && agents.disconnecting.is_empty() {
            self.outcome(Outcome::AgentsRead);
        }
    }

    /// What a captured frame records of the Agents tab while it is open: the server as looked
    /// for, the client chosen, the configuration and every row as drawn, what Copy last copied,
    /// and, for correlation, `session.clients` as the desktop last read it. `null` while the tab
    /// is closed.
    pub(crate) fn agents_summary(&self) -> Value {
        let Some(tab) = &self.workspace.settings.agents else {
            return Value::Null;
        };
        let agents = &self.settings.agents;
        let server = match &agents.server {
            None => json!(null),
            Some(Server::Found(path)) => json!({"found": path}),
            Some(Server::Missing(path)) => json!({"missing": path}),
            Some(Server::Unknown(reason)) => json!({"unknown": reason}),
        };
        json!({
            "server": server,
            "server_line": format!("{:?}", tab.server),
            "chosen": agents.chosen.id(),
            "snippet": tab.snippet.as_ref().map(|snippet| json!({
                "text": snippet.text, "location": snippet.location,
                "instruction": snippet.instruction,
            })),
            "snippet_error": tab.snippet_error,
            "rows": tab.rows.iter().map(|row| json!({
                "client_id": row.client_id, "label": row.label, "detail": row.detail,
                "disconnecting": row.disconnecting,
            })).collect::<Vec<_>>(),
            "empty": tab.empty,
            "error": tab.error,
            "copied": agents.copied.as_ref().map(|(client, text)| json!({
                "client": client.id(), "text": text,
            })),
            "reading": agents.reading,
            "session_clients": agents.clients,
        })
    }
}
