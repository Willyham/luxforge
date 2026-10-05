//! Remembered state against a real owner: a launch over stored preferences starts with those
//! panels, overlays, colour and brush through one `workspace.set` and writes nothing back; a
//! toggle of a remembered switch and a change of the brush are stored through the writer, and a
//! mode, overlay or zoom change is not; and a dialog-chosen export stores its folder, which the
//! next dialog opens on, while an evidence step's export stores nothing.
use super::{
    Boot, Editor,
    export::{ExportChoice, dialog_start, plan_now},
    job_reads::Reader,
    message::{
        Message, export::ExportMessage, mask::BrushEdit, mask::MaskMessage, sync::SyncMessage,
        view::ViewMessage,
    },
    settings_tests::{answer_preference, finish},
    tasks::call,
    testing::{Followed, descriptors, import_and_adopt},
};
use crate::mask_draft::NEUTRAL_BRUSH;
use luxforge_core::{AssetId, ClientSession, MaskOverlayColour};
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

    // Not remembered: the canvas mode, the mask overlay mode, the GPU preview and zoom.
    workspace_set(&mut editor, json!({"mode": luxforge_core::MASK_MODE}));
    workspace_set(&mut editor, json!({"mask_overlay": "tint"}));
    workspace_set(&mut editor, json!({"gpu_preview": false}));
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

/// Answer an export's chain as the runtime would, from the chosen destination to its end.
fn export_to(editor: &mut Editor, asset: &AssetId, destination: PathBuf) {
    let entry = editor.displayed_entry().unwrap();
    let plan = plan_now(&editor.owner, editor.client, asset, &entry).unwrap();
    let choice = ExportChoice {
        asset_id: asset.clone(),
        entry_id: entry,
        destination,
        keep_metadata: false,
        pixels_per_inch: None,
        plan,
    };
    let queued = super::export::send_now(&editor.owner, editor.client, &choice);
    let _ = editor.update(Message::Export(ExportMessage::Chosen(Ok(Some(Box::new(
        choice,
    ))))));
    let _ = editor.update(Message::Export(ExportMessage::Queued(queued)));
    let job = editor
        .export
        .run
        .as_ref()
        .and_then(|run| run.job_id.clone())
        .expect("a queued job");
    // The reader the subscription starts while the job is live; its last message is the end.
    let mut reader = Followed::new(super::export::export_reads(&Reader {
        identity: job,
        owner: editor.owner.clone(),
        client: editor.client,
    }));
    while let Some(message) = reader.next() {
        let _ = editor.update(message);
    }
    assert!(editor.export.run.is_none());
}

/// A dialog-chosen export stores its folder once it succeeds, and the next dialog opens there; an
/// export whose destination an evidence step named stores nothing.
#[test]
fn remembered_export_folder_is_stored_after_a_dialog_export_and_not_after_an_evidence_one() {
    let (mut editor, root, asset) = launch(json!({}), true);
    let asset = asset.unwrap();
    let original = editor
        .document
        .state
        .as_ref()
        .unwrap()
        .asset
        .locator
        .clone();

    // An evidence step's destination: nothing is stored.
    let evidence = root.join("evidence");
    std::fs::create_dir_all(&evidence).unwrap();
    let _ = editor.export_start(false, Some(evidence.join("step.jpg")));
    export_to(&mut editor, &asset, evidence.join("step.jpg"));
    assert!(evidence.join("step.jpg").exists(), "{}", editor.status.text);
    assert!(editor.preferences.idle(), "nothing stored");
    assert_eq!(stored(&editor)["export_folder"], Value::Null);

    // The save dialog's destination: its folder is stored once the export succeeds.
    let chosen = root.join("chosen");
    std::fs::create_dir_all(&chosen).unwrap();
    let _ = editor.update(Message::Export(ExportMessage::Start {
        keep_metadata: false,
    }));
    assert!(editor.view_state.picker_open, "the dialog is open");
    export_to(&mut editor, &asset, chosen.join("mine.jpg"));
    assert!(chosen.join("mine.jpg").exists(), "{}", editor.status.text);
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"export_folder": chosen})
    );
    answer_preference(&mut editor);
    assert_eq!(stored(&editor)["export_folder"], json!(chosen));

    // The next dialog opens on the remembered folder, at the plan's suggestion.
    let entry = editor.displayed_entry().unwrap();
    let plan = plan_now(&editor.owner, editor.client, &asset, &entry).unwrap();
    let (folder, name) = dialog_start(&plan, &original);
    assert_eq!(folder, chosen);
    assert!(name.ends_with("-edited.jpg"), "{name}");

    // The same folder again stores nothing new.
    let _ = editor.update(Message::Export(ExportMessage::Start {
        keep_metadata: false,
    }));
    export_to(&mut editor, &asset, chosen.join("again.jpg"));
    assert!(editor.preferences.idle());
    finish(editor, root);
}

/// A dialog export that fails or is cancelled stores nothing.
#[test]
fn remembered_export_folder_is_not_stored_for_a_cancelled_or_failed_export() {
    let (mut editor, root, asset) = launch(json!({}), true);
    let asset = asset.unwrap();
    let _ = editor.update(Message::Export(ExportMessage::Start {
        keep_metadata: false,
    }));
    let _ = editor.update(Message::Export(ExportMessage::Chosen(Ok(None))));
    assert!(editor.preferences.idle());

    // An existing file is refused, never replaced.
    let folder = root.join("taken");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("taken.jpg"), b"kept").unwrap();
    let _ = editor.update(Message::Export(ExportMessage::Start {
        keep_metadata: false,
    }));
    let entry = editor.displayed_entry().unwrap();
    let choice = ExportChoice {
        asset_id: asset.clone(),
        entry_id: entry.clone(),
        destination: folder.join("taken.jpg"),
        keep_metadata: false,
        pixels_per_inch: None,
        plan: plan_now(&editor.owner, editor.client, &asset, &entry).unwrap(),
    };
    let queued = super::export::send_now(&editor.owner, editor.client, &choice);
    let _ = editor.update(Message::Export(ExportMessage::Chosen(Ok(Some(Box::new(
        choice,
    ))))));
    let _ = editor.update(Message::Export(ExportMessage::Queued(queued)));
    assert!(editor.export.run.is_none(), "{}", editor.status.text);
    assert!(editor.preferences.idle());
    finish(editor, root);
}
