//! The Presets section against a real owner and catalog: the requests its messages build are the
//! ones an independent JSON client sends, and the library it shows is the one the owner holds after
//! each of this desktop's calls and after another client's. Tasks are run here as the plain
//! functions they wrap, so the answers reach the editor as the same messages the runtime delivers.
use super::{
    Boot, Editor,
    message::{MenuTarget, Message, PaletteAction, PresetMessage},
    tasks::{self, call},
};
use crate::{
    Config,
    app::testing::descriptors,
    state::presets::{PresetRow, PresetsModel},
};
use lightwell_core::{AssetId, ClientId, MAX_PRESET_BYTES, OwnerHandle};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightwell-presets-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// An editor with every built-in discovered, one real photograph open and the library listed, plus
/// a second client of the same owner standing in for an agent.
struct Library {
    editor: Editor,
    catalog: PathBuf,
    asset: AssetId,
    agent: ClientId,
}

impl Library {
    fn opened() -> Self {
        let catalog = scratch("catalog.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let agent = owner.register();
        let (queued, _) = call(
            &owner,
            agent,
            "catalog.import",
            json!({"path": fixture("fixtures/s0/orientation-1.jpg"), "mutation": crate::app::tasks::request()}),
        )
        .unwrap();
        let job_id = queued["job_id"].as_str().expect("a source job").to_owned();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (status, _) = call(&owner, agent, "job.status", json!({"job_id": job_id})).unwrap();
            match status["state"].as_str() {
                Some("ready") => break,
                Some("queued" | "preparing") => {
                    assert!(Instant::now() < deadline, "source preparation: {status}");
                    std::thread::sleep(Duration::from_millis(1));
                }
                other => panic!("source preparation failed {other:?}: {status}"),
            }
        }
        let (adopted, _) = call(&owner, agent, "job.adopt", json!({"job_id": job_id})).unwrap();
        let asset =
            AssetId::parse(adopted["asset"]["asset"]["id"].as_str().expect("an asset")).unwrap();
        let (mut editor, _) = Editor::new(Boot {
            owner: owner.clone(),
            join,
            live_server: None,
            config: Config::default(),
            window: (1440.0, 900.0),
        });
        let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
        let mut library = Self {
            editor,
            catalog,
            asset,
            agent,
        };
        library.refresh();
        let listed = tasks::list_presets(&owner, library.editor.client);
        let _ = library
            .editor
            .update(Message::Preset(PresetMessage::Listed(listed)));
        assert!(library.editor.editable(), "{}", library.editor.status);
        library
    }

    fn owner(&self) -> OwnerHandle {
        self.editor.owner.clone()
    }

    /// Read the asset back into the editor, as a command's completion does.
    fn refresh(&mut self) {
        let refreshed = tasks::refresh(
            &self.owner(),
            self.editor.client,
            self.asset.clone(),
            true,
            self.editor.api_sequence,
            None,
        )
        .unwrap();
        let _ = self
            .editor
            .update(Message::Refreshed(Ok(Box::new(refreshed))));
    }

    /// Import one fixture through the section's own import path, answered by the task's function.
    fn import(&mut self, path: &str) -> Result<tasks::PresetChange, String> {
        let path = fixture(path);
        let _ = self
            .editor
            .update(Message::Preset(PresetMessage::ImportPicked(Some(
                path.clone(),
            ))));
        assert!(self.editor.presets.pending, "the import task was started");
        let result = tasks::preset_import_now(&self.owner(), self.editor.client, &path);
        let _ = self.editor.update(Message::Preset(PresetMessage::Imported(
            result.clone().map(Box::new),
        )));
        result
    }

    fn presets(&self) -> &PresetsModel {
        self.editor
            .workspace
            .tools
            .all()
            .find_map(|section| section.presets())
            .expect("the presets section")
    }

    fn row(&self, name: &str) -> PresetRow {
        self.presets()
            .rows()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("no row {name}"))
            .clone()
    }

    fn names(&self) -> Vec<(String, String)> {
        self.presets()
            .rows()
            .map(|row| (row.name.clone(), row.group.clone()))
            .collect()
    }

    fn finish(mut self) {
        self.editor.owner.stop();
        self.editor.owner_join.take().unwrap().join().unwrap();
        let catalog = self.catalog.clone();
        drop(self);
        std::fs::remove_file(catalog).unwrap();
    }
}

/// The request without its fresh request id, which is new on every build.
fn without_request_id(mut request: Value) -> Value {
    request["params"]["mutation"]
        .as_object_mut()
        .expect("a mutation envelope")
        .remove("request_id");
    request
}

#[test]
fn a_rows_click_sends_exactly_the_apply_request_and_commits_one_entry() {
    let mut library = Library::opened();
    library
        .import("fixtures/presets/soft-film.lwpreset")
        .unwrap();
    let row = library.row("Soft film");
    assert!(row.enabled);
    let action = library.presets().action.clone();
    let fields = row.apply.clone().expect("the row applies");
    let settings = library
        .editor
        .presets
        .find(&row.id)
        .expect("the listed preset")
        .settings
        .clone();
    let revision = library.editor.state.as_ref().unwrap().revision;
    let request = library
        .editor
        .request_for_preset(&action, None, Some(&fields))
        .expect("a request");
    assert_eq!(
        without_request_id(request.clone()),
        json!({
            "method": "edit.apply-preset",
            "params": {
                "asset_id": library.asset,
                "mutation": {"expected_revision": revision, "actor": "desktop"},
                "settings": settings,
                "name": "Soft film",
                "preset-id": row.id,
            }
        }),
        "the settings, the name and the library identity, and nothing else"
    );
    // The palette entry runs the very message the row's click sends.
    let entry = library
        .editor
        .workspace
        .palette
        .entries
        .iter()
        .find(|entry| entry.label == "Apply preset: Soft film")
        .expect("a palette entry");
    assert_eq!(
        entry.action,
        PaletteAction::Run {
            action: action.clone(),
            preset: fields.clone(),
        }
    );
    // The click takes the ordinary action path: one command, with its refresh and preview.
    let _ = library.editor.update(Message::RunAction {
        action,
        preset: fields,
    });
    assert!(library.editor.busy, "the command was sent");
    assert_eq!(library.editor.status, "Running edit.apply-preset…");
    // The same request, sent as the task sends it, commits one entry labelled by the preset.
    let (outcome, _) = call(
        &library.owner(),
        library.editor.client,
        "edit.apply-preset",
        request["params"].clone(),
    )
    .unwrap();
    assert_eq!(outcome["outcome"], json!("applied"));
    let (state, _) = call(
        &library.owner(),
        library.agent,
        "asset.state",
        json!({"asset_id": library.asset}),
    )
    .unwrap();
    assert_eq!(state["revision"], json!(revision + 1));
    assert_eq!(state["current_entry"]["label"], json!("Preset: Soft film"));
    library.finish();
}

#[test]
fn create_captures_exactly_the_checked_groups_of_the_displayed_entry() {
    let mut library = Library::opened();
    // An entry with a Basic layer to capture from.
    let revision = library.editor.state.as_ref().unwrap().revision;
    call(
        &library.owner(),
        library.agent,
        "edit.set-basic",
        json!({"asset_id": library.asset, "exposure": 0.5, "contrast": 20, "temperature": 30,
            "mutation": {"expected_revision": revision, "request_id": "agent-1", "actor": "agent"}}),
    )
    .unwrap();
    library.refresh();
    let entry = library.editor.displayed_entry().expect("a displayed entry");
    let labels: Vec<String> = library
        .presets()
        .form
        .checks
        .iter()
        .map(|check| check.label.clone())
        .collect();
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::ToggleForm));
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::Name("Tone only".into())));
    for label in labels {
        let checked = label == "Basic \u{00b7} Tone";
        let _ = library
            .editor
            .update(Message::Preset(PresetMessage::Check { label, checked }));
    }
    assert!(library.presets().form.open && library.presets().form.can_create);
    let (capture, create) = library.editor.preset_create_requests().unwrap();
    assert_eq!(
        capture,
        json!({
            "asset_id": library.asset,
            "entry_id": entry,
            "fields": {"set-basic": ["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]},
        }),
        "the Tone group's fields, of the entry on screen"
    );
    assert_eq!(
        create["mutation"]["actor"],
        json!("desktop"),
        "the create carries a fresh request envelope"
    );
    assert!(create["mutation"]["request_id"].is_string());
    assert!(create["mutation"].get("expected_revision").is_none());
    let mut named = create.clone();
    named.as_object_mut().unwrap().remove("mutation");
    assert_eq!(named, json!({"name": "Tone only", "group": "User presets"}));
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::Create));
    assert!(library.editor.presets.pending);
    let created = tasks::preset_create_now(
        &library.owner(),
        library.editor.client,
        capture.clone(),
        create.clone(),
    );
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::Created(
            created.map(Box::new),
        )));
    assert!(!library.editor.presets.pending);
    assert_eq!(
        library.editor.status,
        "Saved preset \u{201c}Tone only\u{201d} in User presets"
    );
    assert!(
        !library.presets().form.open,
        "a created preset closes the form"
    );
    assert_eq!(
        library.names(),
        [("Tone only".to_owned(), "User presets".to_owned())]
    );
    let stored = library
        .editor
        .presets
        .find(&library.row("Tone only").id)
        .unwrap()
        .settings
        .clone();
    let basic = stored["set-basic"].as_object().expect("the Basic fields");
    assert_eq!(basic.len(), 6, "the six Tone fields and not temperature");
    assert_eq!(basic["exposure"].as_f64(), Some(0.5));
    assert_eq!(basic["contrast"].as_f64(), Some(20.0));
    assert_eq!(basic["shadows"].as_f64(), Some(0.0));
    // A second preset of the same name in the same group is the core's conflict, shown as it is.
    // It is a new press, so a new request; resending the first one would be answered as its retry.
    let mut again = create;
    again["mutation"] = json!(tasks::request());
    let duplicate =
        tasks::preset_create_now(&library.owner(), library.editor.client, capture, again);
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::Created(
            duplicate.map(Box::new),
        )));
    let error = library.presets().form.error.clone().expect("the refusal");
    assert!(error.starts_with("conflict: "), "{error}");
    assert_eq!(library.editor.status, error);
    library.finish();
}

#[test]
fn another_clients_preset_event_refreshes_the_library_in_the_same_poll() {
    let mut library = Library::opened();
    // Catch up first: this desktop's own calls leave events behind too.
    let caught_up = tasks::sync_now(
        &library.owner(),
        library.editor.client,
        library.asset.clone(),
        library.editor.api_sequence,
        None,
    )
    .unwrap();
    let _ = library.editor.update(Message::Synced(Ok(caught_up)));
    let quiet = tasks::sync_now(
        &library.owner(),
        library.editor.client,
        library.asset.clone(),
        library.editor.api_sequence,
        None,
    )
    .unwrap();
    assert!(
        quiet.refresh.is_none() && quiet.presets.is_none(),
        "a poll that saw nothing reads nothing"
    );
    // An agent imports a preset: the next poll lists the library and reads no asset state.
    let content = std::fs::read_to_string(fixture("fixtures/presets/soft-film.lwpreset")).unwrap();
    call(
        &library.owner(),
        library.agent,
        "preset.import",
        json!({"content": content, "mutation": {"request_id": "agent-import", "actor": "agent"}}),
    )
    .unwrap();
    let polled = tasks::sync_now(
        &library.owner(),
        library.editor.client,
        library.asset.clone(),
        library.editor.api_sequence,
        None,
    )
    .unwrap();
    assert!(polled.presets.is_some(), "a preset event lists the library");
    assert!(polled.refresh.is_none(), "and costs no asset refresh");
    let _ = library.editor.update(Message::Synced(Ok(polled)));
    assert_eq!(
        library.names(),
        [("Soft film".to_owned(), "Synthetic".to_owned())],
        "the agent's preset appears without a restart"
    );
    // An edit by the agent is still an asset refresh, and not a listing.
    let revision = library.editor.state.as_ref().unwrap().revision;
    call(
        &library.owner(),
        library.agent,
        "edit.set-basic",
        json!({"asset_id": library.asset, "exposure": 1.0,
            "mutation": {"expected_revision": revision, "request_id": "agent-2", "actor": "agent"}}),
    )
    .unwrap();
    let polled = tasks::sync_now(
        &library.owner(),
        library.editor.client,
        library.asset.clone(),
        library.editor.api_sequence,
        None,
    )
    .unwrap();
    assert!(polled.refresh.is_some() && polled.presets.is_none());
    library.finish();
}

#[test]
fn a_preset_file_over_one_mebibyte_is_refused_before_it_is_read() {
    let mut library = Library::opened();
    let large = scratch("large.xmp");
    std::fs::write(&large, vec![b' '; MAX_PRESET_BYTES + 1]).unwrap();
    let error = tasks::read_preset_file(&large).unwrap_err();
    assert!(
        error.starts_with("resource-limit: ") && error.contains("at most 1 MiB"),
        "{error}"
    );
    // Exactly the limit is read; text that is not UTF-8 is refused rather than guessed at.
    let binary = scratch("binary.xmp");
    std::fs::write(&binary, vec![0xff; MAX_PRESET_BYTES]).unwrap();
    let error = tasks::read_preset_file(&binary).unwrap_err();
    assert!(error.starts_with("unsupported-input: "), "{error}");
    // Through the section, the refusal reaches the status bar and the owner never hears of it.
    let before = library.editor.api_sequence;
    let (events, _) = call(
        &library.owner(),
        library.agent,
        "events.since",
        json!({"after": 0}),
    )
    .unwrap();
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::ImportPicked(Some(
            large.clone(),
        ))));
    let refused = tasks::preset_import_now(&library.owner(), library.editor.client, &large);
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::Imported(
            refused.map(Box::new),
        )));
    assert!(library.editor.status.starts_with("resource-limit: "));
    assert!(!library.editor.presets.pending);
    let (after, _) = call(
        &library.owner(),
        library.agent,
        "events.since",
        json!({"after": 0}),
    )
    .unwrap();
    assert_eq!(events["events"], after["events"], "nothing was sent");
    assert_eq!(library.editor.api_sequence, before);
    std::fs::remove_file(large).unwrap();
    std::fs::remove_file(binary).unwrap();
    library.finish();
}

#[test]
fn an_import_reports_its_counts_and_copy_takes_the_whole_report() {
    let mut library = Library::opened();
    let change = library.import("fixtures/presets/develop.xmp").unwrap();
    let report = &change.result["report"];
    let count = |list: &str| report[list].as_array().map(Vec::len).unwrap();
    assert_eq!(
        library.editor.status,
        format!(
            "Imported \u{201c}Soft Film\u{201d}: {} mapped, {} unsupported, {} refused",
            count("mapped"),
            count("unsupported"),
            count("refused")
        )
    );
    let (line, copied) = library
        .editor
        .status_copy
        .clone()
        .expect("a report to copy");
    assert_eq!(
        line, library.editor.status,
        "Copy follows the line on screen"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&copied).unwrap(),
        *report,
        "Copy takes the whole report, not the line"
    );
    let row = library.row("Soft Film");
    assert!(row.partial && row.imported);
    assert_eq!(row.group, "Synthetic Looks");
    // A second import of the same name and group is refused by the library, and shown as it is.
    let error = library.import("fixtures/presets/develop.xmp").unwrap_err();
    assert!(error.starts_with("conflict: "), "{error}");
    assert_eq!(library.editor.status, error);
    library.finish();
}

#[test]
fn delete_from_a_rows_menu_lists_the_library_again() {
    let mut library = Library::opened();
    library
        .import("fixtures/presets/soft-film.lwpreset")
        .unwrap();
    library.import("fixtures/presets/develop.xmp").unwrap();
    let id = library.row("Soft film").id;
    let _ = library
        .editor
        .update(Message::OpenMenu(MenuTarget::Preset(id.clone())));
    assert_eq!(library.editor.menu, Some(MenuTarget::Preset(id.clone())));
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::Delete(id.clone())));
    assert!(library.editor.menu.is_none() && library.editor.presets.pending);
    let deleted = tasks::preset_delete_now(&library.owner(), library.editor.client, &id);
    let _ = library
        .editor
        .update(Message::Preset(PresetMessage::Deleted(
            deleted.map(Box::new),
        )));
    assert_eq!(library.editor.status, "Deleted the preset");
    assert_eq!(
        library.names(),
        [("Soft Film".to_owned(), "Synthetic Looks".to_owned())]
    );
    library.finish();
}
