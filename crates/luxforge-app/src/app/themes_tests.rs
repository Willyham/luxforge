//! The interface's theme against a real owner: the core's tokens become the widget crate's palette
//! name for name; the first frame is drawn in the stored theme, and one that cannot be shown leaves
//! Luxforge Dark with the status bar naming it and why; the Appearance tab's rows, its choice, its
//! import and its delete build the requests an independent JSON client sends; the palette's entries
//! choose a theme; another client's change reaches a window with no photograph open; and a theme
//! change sends no `asset.state`, `history.list` or preview job. Tasks are run here as the plain
//! functions they wrap, so their answers reach the editor as the messages the runtime delivers.
use super::{
    Boot, Editor,
    message::{
        Message, palette::PaletteMessage, settings::SettingsMessage, sync::SyncMessage,
        theme::ThemeMessage,
    },
    settings_tests::answer_preference,
    tasks::{self, call, owner_calls},
    themes::{
        ReadTheme, choose_change, delete_now, delete_params, import_now, import_params, list_now,
        read_now, theme_params, widget_palette, widget_theme,
    },
};
use crate::state::{MenuTarget, palette::PaletteAction, settings::SettingsTab};
use iced::Color;
use luxforge_core::{
    AssetId, ClientId, OwnerHandle,
    preferences::LaunchPreferences,
    theme::{LUXFORGE_DARK_ID, Mode, Palette as Tokens, ThemeDocument, Token},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// The light Luxforge theme document the scenarios import.
fn paper() -> PathBuf {
    fixture("fixtures/themes/paper.lftheme")
}

fn paper_text() -> String {
    std::fs::read_to_string(paper()).unwrap()
}

/// A host over a preferences directory of its own, and a second client standing in for an agent.
struct Host {
    owner: OwnerHandle,
    join: Option<std::thread::JoinHandle<()>>,
    agent: ClientId,
    root: PathBuf,
}

impl Host {
    fn start(name: &str) -> Self {
        let root = luxforge_testbase::paths::temp_path(name);
        let (owner, join) = OwnerHandle::start_with_host(
            &root.join("catalog.sqlite"),
            Arc::new(luxforge_core::ModuleRegistry::builtin()),
            luxforge_core::HostConfig {
                preferences_dir: Some(root.join("config")),
                ..luxforge_core::HostConfig::unconfigured()
            },
        )
        .unwrap();
        let agent = owner.register();
        Self {
            owner,
            join: Some(join),
            agent,
            root,
        }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config")
    }

    /// Import the paper theme as the agent does, and answer its id.
    fn import_paper(&self) -> String {
        let (imported, _) = call(
            &self.owner,
            self.agent,
            "theme.import",
            json!({"format": "luxforge", "content": paper_text(),
                   "mutation": {"request_id": "agent-import", "actor": "agent"}}),
        )
        .unwrap();
        imported["theme"]["id"].as_str().unwrap().to_owned()
    }

    /// The editor as a launch builds it: the launch preferences read from the directory first, as
    /// `Config::resolve_launch` reads them, then the editor over them.
    fn launch(&mut self) -> Editor {
        let config = crate::Config {
            launch_theme: Some(LaunchPreferences::read(Some(self.config())).theme),
            ..crate::Config::default()
        };
        let (editor, _) = Editor::new(Boot {
            owner: self.owner.clone(),
            join: self.join.take().expect("one launch per host"),
            live_server: None,
            config,
            client: None,
            initial_import: None,
            window: (1440.0, 900.0),
        });
        editor
    }
}

fn finish(mut editor: Editor, host: Host) {
    editor.owner.stop();
    if let Some(join) = editor.owner_join.take() {
        join.join().unwrap();
    }
    drop(editor);
    std::fs::remove_dir_all(host.root).unwrap();
}

/// Answer the library's listing in flight as its task would.
fn answer_list(editor: &mut Editor) {
    assert!(editor.themes.listing.in_flight(), "a listing in flight");
    let listed = list_now(&editor.owner, editor.client);
    let _ = editor.update(Message::Theme(ThemeMessage::Listed(listed)));
}

/// Answer the theme read in flight as its task would.
fn answer_read(editor: &mut Editor) {
    let id = editor
        .themes
        .reading
        .clone()
        .expect("a theme read in flight");
    let result = read_now(&editor.owner, editor.client, &id).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::Read { id, result }));
}

/// The core's resolved tokens for the paper document.
fn paper_tokens() -> (Mode, Tokens) {
    let (_, resolved) = ThemeDocument::read(&paper_text()).unwrap();
    (resolved.mode, resolved.tokens)
}

fn colour(value: luxforge_core::theme::Rgba) -> Color {
    Color::from_rgba8(value.r, value.g, value.b, f32::from(value.a) / 255.0)
}

/// The request without its fresh request id, which is new on every call.
fn without_request_id(mut params: Value) -> Value {
    params["mutation"]
        .as_object_mut()
        .expect("a mutation envelope")
        .remove("request_id");
    params
}

#[test]
fn the_core_and_widget_token_lists_are_equal_in_order() {
    let core: Vec<&str> = Token::ALL.iter().map(|token| token.name()).collect();
    assert_eq!(core, luxforge_ui::Palette::TOKEN_NAMES);
}

/// Luxforge Dark resolved by the core is the widget crate's Luxforge Dark within one code per
/// channel, alpha included; the desktop draws the widget crate's exactly.
#[test]
fn the_cores_luxforge_dark_is_within_one_code_of_the_widget_crates() {
    let core = widget_palette(&Tokens::luxforge_dark());
    let widget = luxforge_ui::Palette::luxforge_dark();
    for name in luxforge_ui::Palette::TOKEN_NAMES {
        let [a, b] =
            [core.token(name).unwrap(), widget.token(name).unwrap()].map(Color::into_rgba8);
        for (x, y) in a.iter().zip(b) {
            assert!(x.abs_diff(y) <= 1, "{name}: {a:?} against {b:?}");
        }
    }
    let drawn = widget_theme(LUXFORGE_DARK_ID, Mode::Dark, &Tokens::luxforge_dark());
    assert_eq!(
        *drawn.palette(),
        widget,
        "Luxforge Dark is the widget crate's own"
    );
}

#[test]
fn a_mapped_theme_draws_every_resolved_token_in_its_mode() {
    let (mode, tokens) = paper_tokens();
    assert_eq!(mode, Mode::Light);
    let theme = widget_theme("theme-paper", mode, &tokens);
    assert_eq!(theme.mode(), luxforge_ui::Mode::Light);
    for (token, value) in tokens.iter() {
        assert_eq!(
            theme.palette().token(token.name()),
            Some(colour(value)),
            "{}",
            token.name()
        );
    }
    let again = widget_theme("theme-paper", mode, &tokens);
    assert_ne!(
        again.generation(),
        theme.generation(),
        "every theme built is a new generation, so canvas caches rebuild once"
    );
}

/// The launch reads the stored theme before the first frame: the editor is built holding it, and
/// its first message reads no theme.
#[test]
fn the_first_frame_is_drawn_in_the_stored_theme() {
    let mut host = Host::start("themes-launch");
    let id = host.import_paper();
    call(
        &host.owner,
        host.agent,
        "preferences.set",
        json!({"theme": id}),
    )
    .unwrap();
    let mut editor = host.launch();
    let (_, tokens) = paper_tokens();
    assert_eq!(
        editor.theme.palette().background,
        colour(tokens[Token::Background])
    );
    assert_eq!(editor.theme.mode(), luxforge_ui::Mode::Light);
    assert_eq!(
        (
            editor.themes.drawn.id.as_str(),
            editor.themes.drawn.chosen.as_str()
        ),
        (id.as_str(), id.as_str())
    );
    assert_eq!(editor.themes.drawn.problem, None);
    // The canvas background is Theme by default, so the canvas is the theme's surround.
    assert_eq!(
        crate::view::canvas::background_colour(
            editor.workspace.canvas.background,
            editor.theme.palette()
        ),
        colour(tokens[Token::Surround])
    );
    owner_calls::take();
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    assert!(editor.themes.reading.is_none(), "nothing left to read");
    assert_eq!(editor.theme_summary()["tokens"]["background"], "#f2f1ee");
    finish(editor, host);
}

/// A stored choice the library cannot show leaves Luxforge Dark on screen, the status bar names the
/// theme and the reason, the stored choice is kept, and it is not read again.
#[test]
fn a_missing_active_theme_falls_back_to_luxforge_dark_naming_it_and_its_reason() {
    let mut host = Host::start("themes-fallback");
    std::fs::create_dir_all(host.config()).unwrap();
    let stored = r#"{"format":1,"theme":"theme-0123456789abcdef"}"#;
    std::fs::write(host.config().join("preferences.json"), stored).unwrap();
    let mut editor = host.launch();
    assert_eq!(
        *editor.theme.palette(),
        luxforge_ui::Palette::luxforge_dark()
    );
    assert_eq!(
        editor.status.text,
        "The theme theme-0123456789abcdef cannot be shown, so Luxforge Dark is: unknown theme \
         theme-0123456789abcdef"
    );
    assert_eq!(editor.themes.drawn.chosen, "theme-0123456789abcdef");
    assert_eq!(editor.themes.drawn.id, LUXFORGE_DARK_ID);
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    assert!(
        editor.themes.reading.is_none(),
        "a fallen-back choice is not read again"
    );
    assert_eq!(
        std::fs::read_to_string(host.config().join("preferences.json")).unwrap(),
        stored,
        "the stored choice is not changed"
    );
    assert_eq!(
        editor.theme_summary()["problem"],
        json!(editor.status.text.clone())
    );

    // A later choice that cannot be shown says so in the same words.
    let id = {
        let agent = editor.owner.register();
        let (imported, _) = call(
            &editor.owner,
            agent,
            "theme.import",
            json!({"format": "luxforge", "content": paper_text(),
                   "mutation": {"request_id": "later", "actor": "agent"}}),
        )
        .unwrap();
        imported["theme"]["id"].as_str().unwrap().to_owned()
    };
    let _ = editor.update(Message::Theme(ThemeMessage::Choose(id.clone())));
    assert_eq!(editor.themes.reading.as_deref(), Some(id.as_str()));
    let _ = editor.update(Message::Theme(ThemeMessage::Read {
        id: id.clone(),
        result: Err(format!("unknown theme {id}")),
    }));
    assert_eq!(
        editor.status.text,
        format!("The theme {id} cannot be shown, so Luxforge Dark is: unknown theme {id}")
    );
    assert_eq!(
        (
            editor.themes.drawn.chosen.as_str(),
            editor.themes.drawn.id.as_str()
        ),
        (id.as_str(), LUXFORGE_DARK_ID)
    );
    finish(editor, host);
}

/// The Appearance tab lists the library, its row's click chooses a theme through the preference
/// writer with the request an independent client sends, the theme is drawn when its read
/// answers, and Luxforge Dark again at once.
#[test]
fn a_rows_click_chooses_the_theme_through_the_writer_and_draws_it() {
    let mut host = Host::start("themes-choose");
    let id = host.import_paper();
    let mut editor = host.launch();
    answer_list(&mut editor);
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Appearance,
    )));
    assert_eq!(
        editor.workspace.settings.open,
        Some(SettingsTab::Appearance)
    );
    answer_list(&mut editor);
    let rows = &editor.workspace.settings.appearance.rows;
    let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names.first(), Some(&"Luxforge Dark"));
    assert_eq!(names.last(), Some(&"Paper"));
    let row = rows.last().unwrap();
    assert_eq!(
        (row.mode, row.origin.as_str(), row.deletable, row.active),
        ("Light", "Imported file", true, false)
    );
    assert!(rows[0].active && !rows[0].deletable);

    let _ = editor.update(Message::Theme(ThemeMessage::Choose(id.clone())));
    // The request an independent client sends, and the check moves before its answer lands.
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"theme": id})
    );
    assert_eq!(choose_change(id.clone()).params(), json!({"theme": id}));
    assert!(
        editor
            .workspace
            .settings
            .appearance
            .rows
            .last()
            .unwrap()
            .active
    );
    assert_eq!(editor.themes.reading.as_deref(), Some(id.as_str()));
    answer_read(&mut editor);
    answer_preference(&mut editor);
    let (_, tokens) = paper_tokens();
    assert_eq!(
        editor.theme.palette().surface,
        colour(tokens[Token::Surface])
    );
    assert_eq!(editor.themes.drawn.id, id);
    assert!(editor.theme_settled());
    let agent = editor.owner.register();
    assert_eq!(
        call(&editor.owner, agent, "theme.list", json!({}))
            .unwrap()
            .0["active"],
        id.as_str()
    );

    // Choosing it again sends nothing; Luxforge Dark is drawn at once, with no read.
    let _ = editor.update(Message::Theme(ThemeMessage::Choose(id.clone())));
    assert!(editor.preferences.idle());
    let _ = editor.update(Message::Theme(ThemeMessage::Choose(
        LUXFORGE_DARK_ID.into(),
    )));
    assert!(editor.themes.reading.is_none());
    assert_eq!(
        *editor.theme.palette(),
        luxforge_ui::Palette::luxforge_dark()
    );
    answer_preference(&mut editor);
    assert!(editor.theme_settled());
    finish(editor, host);
}

/// Each library action builds the request an independent JSON client sends, and the owner answers
/// the desktop's exactly as it answers that client's.
#[test]
fn every_appearance_action_sends_what_an_independent_client_sends() {
    let mut host = Host::start("themes-parity");
    let text = paper_text();
    assert_eq!(
        without_request_id(import_params(&text)),
        json!({"format": "luxforge", "content": text, "mutation": {"actor": "desktop"}})
    );
    assert_eq!(
        without_request_id(delete_params("theme-x")),
        json!({"theme_id": "theme-x", "mutation": {"actor": "desktop"}})
    );
    assert_eq!(theme_params("theme-x"), json!({"theme_id": "theme-x"}));

    let mut editor = host.launch();
    answer_list(&mut editor);
    let _ = editor.update(Message::Theme(ThemeMessage::ImportPicked(Some(paper()))));
    assert!(editor.themes.pending, "the import task was started");
    let imported = import_now(&editor.owner, editor.client, &paper()).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::Imported(imported.clone())));
    let change = imported.unwrap();
    let id = change.result["theme"]["id"].as_str().unwrap().to_owned();
    assert_eq!(editor.status.text, "Imported theme \u{201c}Paper\u{201d}");
    assert!(editor.status.copy.is_some(), "Copy copies the whole report");
    assert!(!editor.themes.pending);
    assert!(
        editor.sync.own_requests.contains(&change.request),
        "the sync skips the desktop's own import"
    );
    assert!(editor.themes.list.as_ref().unwrap().find(&id).is_some());

    // An independent client's import of the same text resolves to the same theme; a name the
    // library holds is a conflict for both.
    let independent = call(
        &editor.owner,
        host.agent,
        "theme.inspect",
        json!({"format": "luxforge", "content": text}),
    )
    .unwrap()
    .0;
    let read = |method: &str| {
        call(&editor.owner, host.agent, method, json!({"theme_id": id}))
            .unwrap()
            .0
    };
    assert_eq!(
        read("theme.read")["theme"]["resolved"],
        independent["theme"]["resolved"]
    );
    // Export and Copy import report read what an independent client reads.
    assert_eq!(
        super::themes::report_now(&editor.owner, editor.client, &id).unwrap(),
        serde_json::to_string_pretty(&read("theme.read")["theme"]["report"]).unwrap()
    );
    assert_eq!(
        call(
            &editor.owner,
            editor.client,
            "theme.export",
            theme_params(&id)
        )
        .unwrap()
        .0,
        read("theme.export")
    );

    // A row's menu deletes a stored theme; the library after it no longer lists it.
    let _ = editor.update(Message::View(super::message::view::ViewMessage::OpenMenu(
        MenuTarget::Theme(id.clone()),
    )));
    let _ = editor.update(Message::Theme(ThemeMessage::Delete(id.clone())));
    assert!(editor.themes.pending && editor.view_state.menu.is_none());
    let deleted = delete_now(&editor.owner, editor.client, &id).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::Deleted(deleted)));
    assert_eq!(editor.status.text, "Deleted the theme");
    assert!(editor.themes.list.as_ref().unwrap().find(&id).is_none());
    finish(editor, host);
}

/// Delete on the active row is sent and refused by the host, and the tab and the status bar show
/// the refusal's reason.
#[test]
fn deleting_the_active_theme_shows_the_hosts_refusal() {
    let mut host = Host::start("themes-delete-active");
    let id = host.import_paper();
    call(
        &host.owner,
        host.agent,
        "preferences.set",
        json!({"theme": id}),
    )
    .unwrap();
    let mut editor = host.launch();
    answer_list(&mut editor);
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Appearance,
    )));
    let _ = editor.update(Message::Theme(ThemeMessage::Delete(id.clone())));
    let refused = delete_now(&editor.owner, editor.client, &id).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::Deleted(refused)));
    let reason =
        format!("conflict: theme {id} is the active theme; choose another before deleting it");
    assert_eq!(editor.status.text, reason);
    assert_eq!(
        editor.workspace.settings.appearance.refusal.as_deref(),
        Some(reason.as_str())
    );
    assert!(editor.themes.list.as_ref().unwrap().find(&id).is_some());
    finish(editor, host);
}

/// The palette lists Settings · Appearance and one Theme entry per theme; running one chooses it.
#[test]
fn the_palette_opens_appearance_and_chooses_a_theme() {
    let mut host = Host::start("themes-palette");
    let id = host.import_paper();
    let mut editor = host.launch();
    answer_list(&mut editor);
    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query("appearance".into())));
    assert_eq!(
        editor
            .workspace
            .palette
            .entries
            .first()
            .map(|entry| &entry.action),
        Some(&PaletteAction::Settings(SettingsTab::Appearance))
    );
    let _ = editor.update(Message::Palette(PaletteMessage::Query(
        "theme: paper".into(),
    )));
    let entries = &editor.workspace.palette.entries;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].label, "Theme: Paper");
    assert_eq!(entries[0].action, PaletteAction::Theme(id.clone()));
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"theme": id})
    );
    assert_eq!(editor.themes.reading.as_deref(), Some(id.as_str()));
    finish(editor, host);
}

/// Another client imports and chooses a theme while no photograph is open: the event sync runs
/// all the same, lists the library, reads the preferences and draws the theme, reading no asset.
#[test]
fn another_clients_theme_change_reaches_a_window_with_no_photograph() {
    let mut host = Host::start("themes-elsewhere");
    let mut editor = host.launch();
    answer_list(&mut editor);
    assert!(editor.document.state.is_none());
    let agent = host.agent;
    let id = host.import_paper();
    call(
        &editor.owner,
        agent,
        "preferences.set",
        json!({"theme": id}),
    )
    .unwrap();

    // The owner's wake becomes a poll with no photograph open.
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    assert!(
        editor.sync.poll.in_flight(),
        "the sync polls with no photograph"
    );
    owner_calls::take();
    let polled = tasks::sync_now(
        &editor.owner,
        editor.client,
        None,
        editor.sync.sequence,
        &[],
        None,
    )
    .unwrap();
    assert!(polled.themes && polled.preferences && polled.refresh.is_none());
    assert_eq!(owner_calls::take(), ["events.since"], "no asset is read");
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
    assert!(
        editor.themes.listing.in_flight(),
        "the library is listed again"
    );
    assert!(
        editor.preferences.reading.in_flight(),
        "the preferences are read again"
    );
    answer_list(&mut editor);
    assert!(editor.themes.list.as_ref().unwrap().find(&id).is_some());
    let read = call(&editor.owner, editor.client, "preferences.read", json!({}))
        .and_then(|(answer, _)| crate::state::preferences::parse(answer));
    let _ = editor.update(Message::Preferences(
        super::message::preferences::PreferenceMessage::Read(read),
    ));
    assert_eq!(editor.themes.reading.as_deref(), Some(id.as_str()));
    answer_read(&mut editor);
    assert_eq!(editor.themes.drawn.id, id);
    assert_eq!(editor.theme.mode(), luxforge_ui::Mode::Light);
    finish(editor, host);
}

/// The evidence steps drive the Appearance tab's own messages: the sheet opened at the tab, a
/// theme document imported through its import task, a theme chosen by id waiting for its read and
/// its write, a theme already drawn captured on the next frame and an id the library does not
/// list failing the step; each frame records the theme drawn and its tokens.
#[test]
fn the_theme_steps_drive_the_appearance_tabs_own_messages() {
    let mut host = Host::start("themes-steps");
    let id = host.import_paper();
    let mut editor = host.launch();
    answer_list(&mut editor);
    super::testing::attach_script(
        &mut editor,
        &format!(
            r#"[{{"settings":{{"open":true,"tab":"appearance"}}}},
                {{"theme_import":{{"path":{path}}}}},
                {{"theme":{{"id":"{id}"}}}},
                {{"theme":{{"id":"{id}"}}}},
                {{"theme":{{"id":"theme-nowhere"}}}}]"#,
            path = json!(paper()),
        ),
    );
    let evidence = |editor: &Editor| {
        let evidence = editor.evidence.as_ref().expect("evidence");
        (evidence.awaiting, evidence.capture_pending)
    };
    let next = |editor: &mut Editor| {
        editor.evidence.as_mut().unwrap().capture_pending = false;
        let _ = editor.next_step();
    };

    next(&mut editor);
    assert_eq!(editor.settings.open, Some(SettingsTab::Appearance));
    assert_eq!(
        evidence(&editor),
        (Some(super::evidence::Settle::Flags), false)
    );
    super::settings_tests::answer_read(&mut editor);
    assert!(evidence(&editor).1, "captured once the sheet has read");

    // The paper theme is already in the library, so its import is refused by name and the step
    // records the refusal with its frame.
    next(&mut editor);
    assert_eq!(
        evidence(&editor),
        (Some(super::evidence::Settle::Themes), false)
    );
    let imported = import_now(&editor.owner, editor.client, &paper()).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::Imported(imported)));
    assert!(evidence(&editor).1);
    assert!(
        editor
            .status
            .text
            .starts_with("conflict: a theme named \"Paper\" already exists"),
        "{}",
        editor.status.text
    );

    next(&mut editor);
    assert_eq!(
        evidence(&editor),
        (Some(super::evidence::Settle::Theme), false)
    );
    answer_read(&mut editor);
    assert!(!evidence(&editor).1, "waits for the write too");
    answer_preference(&mut editor);
    assert!(evidence(&editor).1, "captured once drawn and stored");
    let frame = editor.snapshot();
    assert_eq!(frame["theme"]["id"], id.as_str());
    assert_eq!(frame["theme"]["mode"], "light");
    let (_, tokens) = paper_tokens();
    for (name, token) in [
        ("surround", Token::Surround),
        ("background", Token::Background),
        ("surface", Token::Surface),
        ("control", Token::Control),
        ("text", Token::Text),
    ] {
        assert_eq!(
            frame["theme"]["tokens"][name],
            json!(tokens[token].to_string()),
            "{name}"
        );
    }
    let rows = frame["settings"]["appearance"]["rows"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|row| row["id"] == id.as_str() && row["active"] == true)
    );

    next(&mut editor);
    assert_eq!(
        evidence(&editor),
        (None, true),
        "already drawn: the next frame"
    );
    assert!(editor.preferences.idle());

    next(&mut editor);
    assert_eq!(
        editor.status.text,
        "the theme library lists no theme theme-nowhere"
    );
    assert!(evidence(&editor).1, "a failed step is still captured");
    finish(editor, host);
}

/// With a photograph open, a theme change is a preference write and a theme read: no
/// `asset.state`, `history.list`, preview job or upload, and no request begins.
#[test]
fn a_theme_change_sends_no_asset_state_history_list_or_preview_job() {
    let mut host = Host::start("themes-counted");
    let id = host.import_paper();
    let asset: AssetId = super::testing::import_and_adopt(
        &host.owner,
        host.agent,
        &fixture("fixtures/s0/orientation-1.jpg"),
    );
    let mut editor = host.launch();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(
        super::testing::descriptors(),
    ))));
    let opened = tasks::refresh(
        &editor.owner,
        editor.client,
        asset,
        tasks::Scope::Open,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(opened)))));
    answer_list(&mut editor);
    let requested = editor.activity.requested;
    let generation = editor.presentation.preview_generation;

    owner_calls::take();
    let _ = editor.update(Message::Theme(ThemeMessage::Choose(id.clone())));
    answer_read(&mut editor);
    answer_preference(&mut editor);
    let calls = owner_calls::take();
    assert_eq!(calls, ["theme.read", "preferences.set"]);
    for forbidden in ["asset.state", "history.list", "preview_job"] {
        assert!(!calls.iter().any(|call| call == forbidden), "{forbidden}");
    }
    assert_eq!(editor.activity.requested, requested, "no request began");
    assert_eq!(editor.presentation.preview_generation, generation);
    assert!(!editor.busy);
    assert_eq!(editor.themes.drawn.id, id);
    let read: ReadTheme = read_now(&editor.owner, editor.client, &id).unwrap();
    assert_eq!(
        editor.theme.palette().text,
        colour(read.tokens[Token::Text])
    );
    finish(editor, host);
}
