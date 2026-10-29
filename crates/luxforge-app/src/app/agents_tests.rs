//! Settings › Agents against a real owner: opening the tab reads the server beside this build and
//! `session.clients`, a client's identification reaches an open tab through the event sync, Copy
//! puts the core's configuration on the clipboard, and Disconnect sends `session.disconnect`
//! through the desktop's own client and ends the other client's session.
use super::{
    Editor,
    agents::{disconnect, read_agents},
    message::{
        Message,
        palette::PaletteMessage,
        settings::{AgentsMessage, SettingsMessage},
        sync::SyncMessage,
    },
    settings_tests::{finish, launch},
    tasks::{call, sync_now},
};
use crate::state::{
    agents::{NONE_CONNECTED, Server, UNNAMED},
    palette::PaletteAction,
    settings::SettingsTab,
};
use luxforge_core::{
    ClientId,
    agents::{Client, Launch, Platform},
};
use serde_json::json;
use std::path::PathBuf;

fn agents(message: AgentsMessage) -> Message {
    Message::Settings(SettingsMessage::Agents(message))
}

/// Answer the tab's read in flight as its task would.
fn answer_read(editor: &mut Editor) {
    assert!(editor.settings.agents.reading, "a read in flight");
    let read = read_agents(&editor.owner, editor.client);
    let _ = editor.update(agents(AgentsMessage::Read(Box::new(read))));
}

/// Another client of the owner, named `label` when one is given.
fn other_client(editor: &Editor, label: Option<&str>) -> ClientId {
    let other = editor.owner.register();
    if let Some(label) = label {
        call(
            &editor.owner,
            other,
            "session.identify",
            json!({"label": label}),
        )
        .unwrap();
    }
    other
}

/// The number `session.clients` names a client by.
fn number(client: ClientId) -> u64 {
    serde_json::to_value(client).unwrap().as_u64().unwrap()
}

fn open_agents(editor: &mut Editor) {
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Agents,
    )));
    answer_read(editor);
}

#[test]
fn agents_the_tab_sits_after_models_and_the_palette_opens_it() {
    let shown: Vec<SettingsTab> = SettingsTab::shown(false, true, true).collect();
    assert_eq!(
        shown,
        [
            SettingsTab::General,
            SettingsTab::Appearance,
            SettingsTab::Modules,
            SettingsTab::Models,
            SettingsTab::Agents,
            SettingsTab::Experiments,
        ]
    );
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query("agents".into())));
    let entry = editor
        .workspace
        .palette
        .entries
        .iter()
        .find(|entry| entry.label == "Settings \u{b7} Agents")
        .expect("the palette lists Settings · Agents")
        .clone();
    assert_eq!(entry.action, PaletteAction::Settings(SettingsTab::Agents));
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    assert_eq!(editor.settings.open, Some(SettingsTab::Agents));
    assert!(editor.settings.agents.reading, "opening the tab reads it");
    finish(editor, root);
}

#[test]
fn agents_opening_the_tab_looks_for_the_server_and_lists_no_other_client() {
    let (mut editor, root) = launch();
    open_agents(&mut editor);
    // The test binary has whatever is beside it; the tab says what it found.
    assert_eq!(editor.settings.agents.server, Some(Server::of_this_build()));
    let tab = editor.workspace.settings.agents.clone().expect("the tab");
    assert!(tab.rows.is_empty());
    assert_eq!(tab.empty, Some(NONE_CONNECTED));
    let summary = editor.settings_summary();
    assert_eq!(summary["open"], "agents");
    assert_eq!(summary["agents"]["session_clients"], json!({"clients": []}));
    assert_eq!(summary["agents"]["empty"], NONE_CONNECTED);
    finish(editor, root);
}

#[test]
fn agents_a_missing_server_draws_no_configuration_to_copy() {
    let (mut editor, root) = launch();
    open_agents(&mut editor);
    editor.settings.agents.server = Some(Server::Missing(PathBuf::from(
        "/nowhere/target/release/luxforge-mcp",
    )));
    let _ = editor.update(agents(AgentsMessage::Copy));
    let tab = editor.workspace.settings.agents.clone().unwrap();
    assert_eq!(tab.snippet, None);
    assert_eq!(editor.settings.agents.copied, None, "Copy copies nothing");
    finish(editor, root);
}

#[test]
fn agents_copy_puts_the_chosen_clients_core_configuration_on_the_clipboard() {
    let (mut editor, root) = launch();
    open_agents(&mut editor);
    let binary = PathBuf::from("/Applications/Luxforge.app/Contents/Helpers/luxforge-mcp");
    editor.settings.agents.server = Some(Server::Found(binary.clone()));
    let _ = editor.update(agents(AgentsMessage::Choose(2)));
    assert_eq!(editor.settings.agents.chosen, Client::Codex);
    let _ = editor.update(agents(AgentsMessage::Copy));
    let core = luxforge_core::agents::snippet(
        Client::Codex,
        Launch {
            binary: &binary,
            catalog: None,
            headless: false,
        },
        Platform::current(),
    )
    .unwrap();
    assert_eq!(
        editor.settings.agents.copied,
        Some((Client::Codex, core.text.clone()))
    );
    assert_eq!(editor.status.text, "Copied the Codex configuration");
    let summary = editor.settings_summary();
    assert_eq!(summary["agents"]["copied"]["text"], core.text);
    assert_eq!(summary["agents"]["snippet"]["text"], core.text);
    finish(editor, root);
}

#[test]
fn agents_an_identification_reaches_the_open_tab_through_the_event_sync() {
    let (mut editor, root) = launch();
    open_agents(&mut editor);
    other_client(&editor, Some("Claude Code via MCP"));
    let polled = sync_now(
        &editor.owner,
        editor.client,
        None,
        editor.sync.sequence,
        &[],
        None,
    )
    .unwrap();
    assert!(polled.sessions, "session.identify is a session event");
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled.clone()))));
    answer_read(&mut editor);
    let tab = editor.workspace.settings.agents.clone().unwrap();
    let [row] = tab.rows.as_slice() else {
        panic!("one connected client: {:?}", tab.rows)
    };
    assert_eq!(row.label, "Claude Code via MCP");
    assert!(
        row.detail.starts_with("Connected just now"),
        "{}",
        row.detail
    );
    assert_eq!(tab.empty, None);
    // With the sheet closed, the same event reads nothing.
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
    assert!(!editor.settings.agents.reading);
    finish(editor, root);
}

#[test]
fn agents_disconnect_sends_session_disconnect_and_the_client_leaves_the_list() {
    let (mut editor, root) = launch();
    let named = other_client(&editor, Some("luxforge-ctl"));
    let unnamed = other_client(&editor, None);
    open_agents(&mut editor);
    let labels: Vec<String> = editor
        .workspace
        .settings
        .agents
        .iter()
        .flat_map(|tab| &tab.rows)
        .map(|row| row.label.clone())
        .collect();
    assert_eq!(labels, ["luxforge-ctl", UNNAMED]);

    let _ = editor.update(agents(AgentsMessage::Disconnect(number(unnamed))));
    let row = editor.workspace.settings.agents.as_ref().unwrap().rows[1].clone();
    assert!(row.disconnecting, "Disconnect is disabled until it answers");
    // A second press while the first is out sends nothing more.
    let _ = editor.update(agents(AgentsMessage::Disconnect(number(unnamed))));
    let result = disconnect(&editor.owner, editor.client, number(unnamed));
    assert_eq!(result, Ok(()));
    let _ = editor.update(agents(AgentsMessage::Disconnected {
        client_id: number(unnamed),
        result,
    }));
    answer_read(&mut editor);
    let rows = editor.workspace.settings.agents.clone().unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].client_id, number(named));
    // The disconnected client's session has ended: its next request is refused.
    let refused = super::tasks::call_detailed(&editor.owner, unnamed, "session.state", json!({}))
        .expect_err("the client was disconnected");
    assert!(refused.to_string().contains("disconnected"), "{refused}");

    // A refusal is said in the tab and the status bar, and the list is read again.
    let _ = editor.update(agents(AgentsMessage::Disconnect(number(named))));
    let _ = editor.update(agents(AgentsMessage::Disconnected {
        client_id: number(named),
        result: Err("forbidden: no".into()),
    }));
    assert_eq!(
        editor.status.text,
        "Could not disconnect luxforge-ctl: forbidden: no"
    );
    assert!(editor.settings.agents.reading);
    answer_read(&mut editor);
    let tab = editor.workspace.settings.agents.clone().unwrap();
    assert_eq!(
        tab.error.as_deref(),
        Some("Could not disconnect luxforge-ctl: forbidden: no")
    );
    assert_eq!(tab.rows.len(), 1);
    finish(editor, root);
}

#[test]
fn agents_a_change_while_a_read_is_out_reads_once_more() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Agents,
    )));
    let _ = editor.agents_followed(true);
    assert!(editor.settings.agents.again);
    answer_read(&mut editor);
    assert!(
        editor.settings.agents.reading,
        "the read answered and the next one is out"
    );
    assert!(!editor.settings.agents.again);
    finish(editor, root);
}
