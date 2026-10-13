//! Remembered state against a real owner: a launch over stored preferences starts with those
//! panels, overlays, colour and brush through one `workspace.set` and writes nothing back; a
//! toggle of a remembered switch and a change of the brush are stored through the writer, and a
//! mode, overlay or zoom change is not; and an export from the Export sheet sends what an API
//! client sends, writes the same file and is stored as the last export, which Export again
//! repeats, while an evidence step's export stores nothing. The sheet's preset changes are the
//! API's.
use super::{
    Boot, Editor,
    export::{ExportRequest, export_params, read_now, screen_pixels_per_inch, write_now},
    job_reads::Reader,
    message::{
        Message,
        export::{ExportMessage, SheetMessage},
        mask::BrushEdit,
        mask::MaskMessage,
        sync::SyncMessage,
        view::ViewMessage,
    },
    settings_tests::{answer_preference, finish},
    tasks::call,
    testing::{Followed, descriptors, import_and_adopt},
};
use crate::mask_draft::NEUTRAL_BRUSH;
use luxforge_core::{AssetId, ClientSession, MaskOverlayColour};
use luxforge_test_macros::slow;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// An editor launched over a preferences directory of its own, holding `stored` as another client
/// set it before the launch. With `photo`, a real photograph is imported by that client and open
/// in the editor, as [`super::testing::real_photo`] opens one.
fn launch(stored: Value, photo: bool) -> (Editor, PathBuf, Option<AssetId>) {
    let root = luxforge_testbase::paths::temp_path("remembered");
    let host = luxforge_core::HostConfig {
        preferences_dir: Some(root.join("config")),
        ..luxforge_core::HostConfig::unconfigured()
    };
    let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
        &root.join("catalog.sqlite"),
        Arc::new(luxforge_core::ModuleRegistry::builtin()),
        host,
    )
    .unwrap();
    let agent = owner.register();
    if stored != json!({}) {
        call(&owner, agent, "preferences.set", stored).unwrap();
    }
    let asset = photo.then(|| {
        import_and_adopt(
            &owner,
            agent,
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/s0/orientation-1.jpg"),
        )
    });
    let (mut editor, _) = Editor::new(Boot {
        owner: owner.clone(),
        join,
        live_server: None,
        config: crate::Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    if let Some(asset) = &asset {
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
        let refreshed = super::tasks::refresh(
            &owner,
            editor.client,
            asset.clone(),
            super::tasks::Scope::Open,
            None,
        )
        .unwrap();
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            refreshed,
        )))));
    }
    (editor, root, asset)
}

/// Send `params` as the `workspace.set` a control's task sends and hand its answer to the desktop
/// as the runtime would.
fn workspace_set(editor: &mut Editor, params: Value) {
    let answer = call(&editor.owner, editor.client, "workspace.set", params)
        .map(|(session, _)| serde_json::from_value::<ClientSession>(session).unwrap());
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(answer)));
}

/// What the host stores, read as another client would.
fn stored(editor: &Editor) -> Value {
    let agent = editor.owner.register();
    call(&editor.owner, agent, "preferences.read", json!({}))
        .unwrap()
        .0
}

fn brush(editor: &mut Editor, edit: BrushEdit) {
    let _ = editor.update(Message::Mask(MaskMessage::Brush(edit)));
}

fn nudge(name: &str, steps: f64) -> BrushEdit {
    BrushEdit::Nudge {
        name: name.into(),
        steps,
    }
}

/// A launch over stored values starts with those panels, overlays, colour and brush, through one
/// `workspace.set` made before the first frame, and writes nothing back.
#[test]
fn remembered_workspace_and_brush_seed_the_first_session_through_one_workspace_set() {
    let (editor, root, _) = launch(
        json!({
            "workspace": {"state_panel": false, "tools_panel": true, "thirds": true,
                          "clip_shadows": true, "clip_highlights": false},
            "mask_overlay_colour": "white",
            "brush": {"size": 0.25, "feather": 20.0, "flow": 60.0},
        }),
        false,
    );
    let workspace = &editor.session.workspace;
    assert!(!workspace.state_panel && workspace.tools_panel && workspace.thirds);
    assert!(workspace.clip_shadows && !workspace.clip_highlights);
    assert_eq!(workspace.mask_overlay_colour, MaskOverlayColour::White);
    assert_eq!(
        workspace.mode,
        luxforge_core::POINTER_MODE,
        "not remembered"
    );
    assert_eq!(editor.session.revision, 1, "one workspace.set");
    // The first derived frame already shows them.
    assert_eq!(editor.snapshot()["workspace"]["thirds"], json!(true));
    let brush = editor.mask_panel.brush;
    assert_eq!((brush.size, brush.feather, brush.flow), (0.25, 20.0, 60.0));
    assert!(!brush.erase && !brush.limit_to_colour);
    assert_eq!(brush.colour_refine, NEUTRAL_BRUSH.colour_refine);
    assert!(editor.preferences.idle(), "seeding writes nothing back");
    finish(editor, root);
}

/// Nothing stored, as in every evidence run: the session keeps its defaults with no call at all,
/// and the brush is the neutral one.
#[test]
fn remembered_defaults_send_nothing_at_launch() {
    let (editor, root, _) = launch(json!({}), false);
    assert_eq!(editor.session.revision, 0, "no workspace.set");
    assert_eq!(
        editor.session.workspace,
        luxforge_core::WorkspaceState::default()
    );
    assert_eq!(editor.mask_panel.brush, NEUTRAL_BRUSH);
    assert!(editor.preferences.idle());
    finish(editor, root);
}

/// Toggling a remembered switch stores the five through the writer; a mode, mask overlay, GPU
/// preview or zoom change stores nothing.
#[test]
fn remembered_workspace_is_stored_on_a_toggle_and_not_on_a_mode_or_zoom_change() {
    let (mut editor, root, _) = launch(json!({}), true);
    assert!(editor.preferences.idle());

    // The state panel's toggle (Cmd+Option+[).
    workspace_set(&mut editor, json!({"state_panel": false}));
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"workspace": {"state_panel": false, "tools_panel": true, "thirds": false,
                             "clip_shadows": false, "clip_highlights": false}})
    );
    // Thirds (O) and a clipping triangle (J) while that write is out merge into one waiting.
    workspace_set(&mut editor, json!({"thirds": true}));
    workspace_set(&mut editor, json!({"clip_highlights": true}));
    assert_eq!(editor.preferences.outstanding(), 2);
    assert_eq!(
        editor.preferences.waiting().unwrap().params(),
        json!({"workspace": {"state_panel": false, "tools_panel": true, "thirds": true,
                             "clip_shadows": false, "clip_highlights": true}})
    );
    answer_preference(&mut editor);
    answer_preference(&mut editor);
    assert!(editor.preferences.idle());
    assert_eq!(
        stored(&editor)["workspace"],
        json!({"state_panel": false, "tools_panel": true, "thirds": true,
               "clip_shadows": false, "clip_highlights": true})
    );

    // Not remembered: the canvas mode, the mask overlay mode and zoom.
    workspace_set(&mut editor, json!({"mode": luxforge_core::MASK_MODE}));
    workspace_set(&mut editor, json!({"mask_overlay": "tint"}));
    let zoomed = call(
        &editor.owner,
        editor.client,
        "view.set",
        json!({"zoom": {"mode": "percent", "value": 100.0}}),
    )
    .map(|(session, _)| serde_json::from_value::<ClientSession>(session).unwrap());
    let _ = editor.update(Message::View(ViewMessage::SessionUpdated(zoomed)));
    assert!(editor.preferences.idle(), "nothing stored");

    // Off and on again while the first write is out: the newer value goes next and is kept.
    workspace_set(&mut editor, json!({"thirds": false}));
    assert!(editor.preferences.writing().is_some());
    workspace_set(&mut editor, json!({"thirds": true}));
    answer_preference(&mut editor);
    assert_eq!(
        editor.preferences.writing().unwrap().params()["workspace"]["thirds"],
        json!(true),
        "the newer value goes next"
    );
    answer_preference(&mut editor);
    assert!(editor.preferences.idle());
    assert_eq!(stored(&editor)["workspace"]["thirds"], json!(true));
    finish(editor, root);
}

/// A change of size, feather or flow by key, nudge, typed value or reset is stored; a held key
/// writes one call in flight and one waiting; erase, Limit to colour and the colour refine are not
/// remembered.
#[test]
fn remembered_brush_is_stored_on_a_change_and_coalesced_under_repeated_keys() {
    let (mut editor, root, _) = launch(
        json!({"brush": {"size": 0.2, "feather": 50.0, "flow": 100.0}}),
        false,
    );
    assert_eq!(editor.mask_panel.brush.size, 0.2);

    // A held ] key: thirty nudges, one call in flight and one merged change waiting.
    brush(&mut editor, nudge("size", 1.0));
    let first = editor.mask_panel.brush.size;
    for _ in 1..30 {
        brush(&mut editor, nudge("size", 1.0));
        assert!(editor.preferences.outstanding() <= 2);
    }
    let size = editor.mask_panel.brush.size;
    assert!(
        (first - 0.21).abs() < 1e-9 && (size - 0.5).abs() < 1e-9,
        "{size}"
    );
    assert_eq!(
        editor.preferences.writing().unwrap().params()["brush"]["size"],
        json!(first)
    );
    assert_eq!(
        editor.preferences.waiting().unwrap().params()["brush"]["size"],
        json!(size)
    );
    // Shift+[ and a typed flow merge into the same waiting change.
    brush(&mut editor, nudge("feather", -1.0));
    brush(
        &mut editor,
        BrushEdit::Set {
            name: "flow".into(),
            value: 60.0,
        },
    );
    assert_eq!(editor.preferences.outstanding(), 2);
    answer_preference(&mut editor);
    answer_preference(&mut editor);
    assert!(editor.preferences.idle());
    assert_eq!(
        stored(&editor)["brush"],
        json!({"size": size, "feather": 45.0, "flow": 60.0})
    );

    // Not remembered: they belong to the stroke.
    brush(&mut editor, BrushEdit::Erase(true));
    brush(&mut editor, BrushEdit::LimitToColour(true));
    brush(
        &mut editor,
        BrushEdit::Set {
            name: "colour_refine".into(),
            value: 30.0,
        },
    );
    brush(&mut editor, BrushEdit::EraseHeld(true));
    assert!(editor.preferences.idle(), "nothing stored");

    // A reset stores the neutral value.
    brush(&mut editor, BrushEdit::Reset("size".into()));
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"brush": {"size": NEUTRAL_BRUSH.size, "feather": 45.0, "flow": 60.0}})
    );
    answer_preference(&mut editor);
    assert_eq!(stored(&editor)["brush"]["size"], json!(NEUTRAL_BRUSH.size));
    // A nudge past the range's end changes nothing and stores nothing.
    brush(&mut editor, nudge("flow", 100.0));
    answer_preference(&mut editor);
    brush(&mut editor, nudge("flow", 1.0));
    assert!(editor.preferences.idle());
    finish(editor, root);
}

/// One gesture on the open Export sheet.
fn sheet(editor: &mut Editor, message: SheetMessage) {
    let _ = editor.update(Message::Export(ExportMessage::Sheet(message)));
}

/// Run an export request's chain as the runtime would, from its plan to its end, and say what
/// `export.write` was sent, without its mutation envelope.
fn run(editor: &mut Editor, request: ExportRequest) -> Value {
    let _ = editor.start_export(request.clone());
    assert!(editor.export.active(), "{}", editor.status.text);
    let (choice, result) = write_now(&editor.owner, editor.client, request);
    let choice = choice.expect("the plan named a destination");
    let mut sent = export_params(&choice);
    sent.as_object_mut().unwrap().remove("mutation");
    let _ = editor.update(Message::Export(ExportMessage::Sent {
        choice: Some(Box::new(choice)),
        result,
    }));
    if let Some(job) = editor
        .export
        .run
        .as_ref()
        .and_then(|run| run.job_id.clone())
    {
        // The reader the subscription starts while the job is live; its last message is the end.
        let mut reader = Followed::new(super::export::export_reads(&Reader {
            identity: job,
            owner: editor.owner.clone(),
            client: editor.client,
            presentation_visible: true,
        }));
        while let Some(message) = reader.next() {
            let _ = editor.update(message);
        }
    }
    assert!(editor.export.run.is_none());
    sent
}

/// One request through an independent client, a change under a mutation envelope of its own.
fn agent_call(
    editor: &Editor,
    agent: luxforge_core::ClientId,
    method: &str,
    mut params: Value,
) -> Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    if !matches!(method, "export.plan" | "export.presets.list") {
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        params["mutation"] = json!({"request_id": format!("agent-{next}"), "actor": "agent"});
    }
    call(&editor.owner, agent, method, params)
        .unwrap_or_else(|error| panic!("{method}: {error}"))
        .0
}

/// The sheet's Export sends exactly what an independent JSON client sends for the same settings —
/// Web's, into a chosen folder, at this window's density — and the two write the same bytes. It is
/// stored as the last export, and Export again repeats it, counting the name up.
#[test]
#[slow]
fn the_sheets_export_is_what_an_api_client_sends_and_export_again_repeats_it() {
    let (mut editor, root, asset) = launch(json!({}), true);
    let asset = asset.unwrap();
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let _ = editor.update(Message::Export(ExportMessage::OpenSheet {
        reference: false,
    }));
    sheet(&mut editor, SheetMessage::FolderChosen(Some(out.clone())));
    editor.answer_sheet_plan();
    let shown = editor.workspace.export_sheet.clone().expect("the sheet");
    assert_eq!(
        shown.name.as_deref(),
        Some("\u{2192} orientation-1-web.jpg")
    );
    assert!(shown.can_export, "{:?}", shown.refusals);

    // What an independent client sends: Web's settings as the API lists them, into the folder, at
    // the density this window names.
    let agent = editor.owner.register();
    let mut settings =
        agent_call(&editor, agent, "export.presets.list", json!({}))["presets"][0]["settings"]
            .clone();
    settings["folder"] = json!({"path": out});
    let remembered = settings.clone();
    settings["pixels_per_inch"] = json!(screen_pixels_per_inch(
        editor.view_state.system_scale_factor
    ));
    let entry = editor.displayed_entry().unwrap();
    let request = editor.sheet_request().unwrap();
    let sent = run(&mut editor, request);
    let written = out.join("orientation-1-web.jpg");
    assert_eq!(
        sent,
        json!({"asset_id": asset, "entry_id": entry, "settings": settings, "destination": written})
    );
    assert!(
        editor
            .status
            .text
            .starts_with("Exported orientation-1-web.jpg \u{b7} ")
            && editor.status.text.ends_with("\u{b7} quality 80"),
        "{}",
        editor.status.text
    );
    let independent = root.join("independent");
    std::fs::create_dir_all(&independent).unwrap();
    let mut theirs = settings.clone();
    theirs["folder"] = json!({"path": independent});
    let plan = agent_call(
        &editor,
        agent,
        "export.plan",
        json!({"asset_id": asset, "entry_id": entry, "settings": theirs}),
    );
    let queued = agent_call(
        &editor,
        agent,
        "export.write",
        json!({"asset_id": asset, "entry_id": entry, "settings": theirs,
               "destination": plan["suggested"]}),
    );
    let job = queued["job_id"].as_str().unwrap().to_owned();
    luxforge_testbase::wait_until("the independent export", || {
        read_now(&editor.owner, agent, &job).unwrap()["status"] == "ready"
    });
    assert_eq!(
        std::fs::read(&written).unwrap(),
        std::fs::read(independent.join("orientation-1-web.jpg")).unwrap(),
        "the same settings write the same file"
    );

    // Stored as the last export, with no preset: its folder is not Web's.
    let last = json!({"settings": remembered, "preset_id": null});
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"export_last": last})
    );
    answer_preference(&mut editor);
    assert_eq!(stored(&editor)["export_last"], last);
    let _ = editor.update(Message::Export(ExportMessage::Menu));
    assert_eq!(
        editor
            .workspace
            .title
            .export_menu
            .as_ref()
            .unwrap()
            .again
            .as_deref(),
        Some("Export again with the last settings")
    );

    // Export again: the same request, the name counted up; the same last export stores nothing.
    let request = editor.again_request().unwrap();
    let sent = run(&mut editor, request);
    assert_eq!(sent["settings"], settings);
    assert_eq!(
        sent["destination"],
        json!(out.join("orientation-1-web-2.jpg"))
    );
    assert!(out.join("orientation-1-web-2.jpg").exists());
    assert!(editor.preferences.idle());
    finish(editor, root);
}

/// The sheet's preset changes are the API's: a preset an agent created is a card, Save as preset…
/// creates one of the current settings, Update "Name"… replaces its settings and Delete removes
/// it, each listed by the API as the sheet shows it.
#[test]
#[slow]
fn the_sheets_preset_changes_are_the_apis() {
    let (mut editor, root, _) = launch(json!({}), true);
    let agent = editor.owner.register();
    let listed = |editor: &Editor| {
        agent_call(editor, agent, "export.presets.list", json!({}))["presets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|preset| {
                (
                    preset["name"].as_str().unwrap().to_owned(),
                    preset["settings"]["quality"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let web =
        agent_call(&editor, agent, "export.presets.list", json!({}))["presets"][0]["settings"]
            .clone();
    let _ = agent_call(
        &editor,
        agent,
        "export.presets.create",
        json!({"name": "Agent's", "settings": web}),
    );
    let _ = editor.update(Message::Export(ExportMessage::OpenSheet {
        reference: false,
    }));
    editor.answer_sheet_plan();
    let names = |editor: &Editor| {
        editor
            .workspace
            .export_sheet
            .as_ref()
            .unwrap()
            .cards
            .iter()
            .map(|card| card.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&editor), ["Web", "Print", "Master", "Agent's"]);

    sheet(&mut editor, SheetMessage::Quality(60));
    let shown = editor.workspace.export_sheet.clone().unwrap();
    assert!(
        shown.cards[0].changed && shown.update.is_none(),
        "Web is built in"
    );
    sheet(
        &mut editor,
        SheetMessage::Name(crate::state::export_sheet::NamingTarget::New),
    );
    sheet(&mut editor, SheetMessage::NameText("Mine".into()));
    sheet(&mut editor, SheetMessage::NameSubmit);
    assert_eq!(
        names(&editor),
        ["Web", "Print", "Master", "Agent's", "Mine"]
    );
    let shown = editor.workspace.export_sheet.clone().unwrap();
    assert!(shown.cards[4].selected && !shown.cards[4].changed);
    assert!(listed(&editor).contains(&("Mine".to_owned(), json!(60))));

    sheet(&mut editor, SheetMessage::Quality(70));
    assert_eq!(
        editor
            .workspace
            .export_sheet
            .as_ref()
            .unwrap()
            .update
            .as_deref(),
        Some("Update \u{201c}Mine\u{201d}\u{2026}")
    );
    sheet(&mut editor, SheetMessage::Update);
    assert!(listed(&editor).contains(&("Mine".to_owned(), json!(70))));
    assert!(!editor.workspace.export_sheet.as_ref().unwrap().cards[4].changed);

    let mine = editor
        .export
        .sheet
        .as_ref()
        .unwrap()
        .preset_id
        .clone()
        .unwrap();
    sheet(
        &mut editor,
        SheetMessage::Card(mine, crate::state::export_sheet::CardChoice::Delete),
    );
    assert_eq!(names(&editor), ["Web", "Print", "Master", "Agent's"]);
    assert!(!listed(&editor).iter().any(|(name, _)| name == "Mine"));
    assert_eq!(editor.export.sheet.as_ref().unwrap().preset_id, None);
    finish(editor, root);
}

/// An evidence step's export stores nothing, and neither does an export that is refused.
#[test]
#[slow]
fn an_evidence_export_and_a_refused_one_store_nothing() {
    let (mut editor, root, _) = launch(json!({}), true);
    let evidence = root.join("evidence");
    std::fs::create_dir_all(&evidence).unwrap();
    let request = |editor: &Editor, name: &str| ExportRequest {
        asset_id: editor.document.state.as_ref().unwrap().asset.id.clone(),
        entry_id: editor.displayed_entry().unwrap(),
        settings: luxforge_core::ExportSettings {
            folder: luxforge_core::ExportFolder::Path(evidence.clone()),
            ..luxforge_core::ExportSettings::baseline(luxforge_core::MetadataLevel::None)
        },
        destination: Some(evidence.join(name)),
        reference: false,
        remember: None,
    };
    let first = request(&editor, "step.jpg");
    let _ = run(&mut editor, first);
    assert!(evidence.join("step.jpg").exists(), "{}", editor.status.text);
    assert!(editor.preferences.idle(), "nothing stored");

    // The same file again is refused, never replaced, and stores nothing even when it would.
    let refused = ExportRequest {
        remember: Some(luxforge_core::preferences::ExportLast {
            settings: luxforge_core::ExportSettings::baseline(luxforge_core::MetadataLevel::None),
            preset_id: None,
        }),
        ..request(&editor, "step.jpg")
    };
    let _ = run(&mut editor, refused);
    assert_eq!(editor.status.text, super::export::refused_text("step.jpg"));
    assert!(editor.preferences.idle());
    assert_eq!(stored(&editor)["export_last"], Value::Null);
    finish(editor, root);
}
