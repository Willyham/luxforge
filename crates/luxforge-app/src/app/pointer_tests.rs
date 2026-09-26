//! The pointer over the photograph: the hover readout and canvas picks, located through the core
//! and answered by the mode on screen.
use super::{
    message::{ControlMessage, PointerMessage},
    testing::{
        attach_log, begun, descriptors, finish, logged, opened, opened_with_modules, patch_control,
        pick_events, pick_fields, pick_mode, picking, sample_mode,
    },
    *,
};
use luxforge_core::{ContentPoint, EntryId};
use serde_json::Map;

/// A `sample-apply` mode's pick is the chain the declaration describes: locate the content
/// pixel, ask that module's own query about it, and submit the fields it answers with — the
/// ones the action declares, and only those — as one command.
#[test]
fn a_sample_apply_pick_queries_the_located_pixel_and_submits_the_answer_once() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (mode, query, action) = sample_mode(&editor);
    editor.session.workspace.mode = mode.clone();
    let entry_id = editor.displayed_entry().expect("a displayed entry");
    let log = attach_log(&mut editor);

    let _ = editor.update(Message::Pointer(PointerMessage::Located {
        entry: entry_id.clone(),
        mode: mode.clone(),
        view: (7, 9),
        result: Ok(ContentPoint {
            content_x: 100,
            content_y: 42,
            width: 480,
            height: 320,
        }),
    }));
    assert_eq!(editor.status, "Sampling (100, 42)…");
    assert!(!editor.busy, "the query committed before it answered");
    assert_eq!(
        pick_events(&logged(&mut editor, &log)),
        vec![&json!({"query":query,"action":action,"view_x":7,"view_y":9,"x":100,"y":42})]
    );

    // The query's answer: two fields the action declares and one it does not.
    let log = attach_log(&mut editor);
    let declared = tools::declared_action(&editor.modules, &action)
        .expect("the declared action")
        .clone();
    let named: Vec<&str> = declared
        .parameters
        .iter()
        .take(2)
        .map(|parameter| parameter.name.as_str())
        .collect();
    let mut answer = Map::new();
    answer.insert(named[0].into(), json!(-12.0));
    answer.insert(named[1].into(), json!(5.0));
    answer.insert("patch".into(), json!({"x": 98, "y": 40}));
    let _ = editor.update(Message::Pointer(PointerMessage::SampleQueried {
        entry: entry_id,
        action: action.clone(),
        point: (100, 42),
        result: Ok(Value::Object(answer)),
    }));
    assert!(
        editor.busy,
        "the answer was not submitted: {}",
        editor.status
    );
    assert!(
        editor.status.starts_with(&format!("Running edit.{action}")),
        "{}",
        editor.status
    );
    let records = logged(&mut editor, &log);
    let sampled: Vec<&Value> = records
        .iter()
        .filter(|record| record["event"] == json!("canvas_sample"))
        .map(|record| &record["detail"])
        .collect();
    assert_eq!(sampled.len(), 1, "{sampled:?}");
    assert_eq!(
        sampled[0]["fields"],
        json!({ named[0]: -12.0, named[1]: 5.0 }),
        "the metadata the action does not declare was submitted"
    );
    finish(editor, catalog);
}

/// A refused query commits nothing and shows the core's own reason, whose prefix names why.
#[test]
fn a_refused_sample_shows_its_reason_and_commits_nothing() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (mode, _, action) = sample_mode(&editor);
    editor.session.workspace.mode = mode;
    let entry_id = editor.displayed_entry().expect("a displayed entry");
    let revision = editor.state.as_ref().expect("open").revision;
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Pointer(PointerMessage::SampleQueried {
        entry: entry_id,
        action,
        point: (100, 42),
        result: Err(
            "validation: clipped: a sampled pixel is at code 0 or 255, so this patch carries no usable colour"
                .into(),
        ),
    }));
    assert!(
        editor.status.starts_with("clipped:"),
        "the reason's own prefix is not what the status leads with: {}",
        editor.status
    );
    assert!(!editor.busy, "a refused sample committed something");
    assert_eq!(editor.state.as_ref().expect("open").revision, revision);
    let records = logged(&mut editor, &log);
    assert!(
        records
            .iter()
            .any(|record| record["event"] == json!("canvas_sample")
                && record["detail"]["error"].is_string()),
        "the refusal is not in the evidence"
    );
    finish(editor, catalog);
}

/// A pick answers to the canvas mode that is on screen and to nothing else, and it is refused
/// while a draft is open rather than displacing it.
#[test]
fn a_pick_answers_only_to_the_mode_on_screen_and_is_refused_during_a_draft() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (sample_module, _, _) = sample_mode(&editor);
    let (pick_action, x, y) = pick_fields(&editor);
    let entry_id = editor.displayed_entry().expect("a displayed entry");

    // The pointer mode: a click reaches no module's pick at all.
    let before = editor.status.clone();
    let _ = editor.update(Message::Pointer(PointerMessage::Picked { x: 7, y: 9 }));
    assert_eq!(editor.status, before, "a pick ran in the pointer mode");

    // The sample-apply mode: the located point is not written into the point-pick module's
    // coordinate fields, because that module's canvas is not the one on screen.
    editor.session.workspace.mode = sample_module.clone();
    let _ = editor.update(Message::Pointer(PointerMessage::Located {
        entry: entry_id,
        mode: sample_module.clone(),
        view: (7, 9),
        result: Ok(ContentPoint {
            content_x: 100,
            content_y: 42,
            width: 480,
            height: 320,
        }),
    }));
    assert_eq!(editor.fields.get(&pick_action, &x), Some("0"));
    assert_eq!(editor.fields.get(&pick_action, &y), Some("0"));

    // A pick while a slider gesture is open is refused, and the gesture is untouched.
    let (action, parameter) = patch_control(&editor);
    let asset = editor.state.as_ref().expect("open").asset.id.clone();
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter,
        value: 1.0,
    }));
    begun(&mut editor, &asset, &action, 4);
    assert!(editor.slider_gesture().is_some());
    let _ = editor.update(Message::Pointer(PointerMessage::Picked { x: 7, y: 9 }));
    assert!(editor.status.contains("slider draft"), "{}", editor.status);
    assert!(
        editor.slider_gesture().is_some(),
        "the pick discarded a draft"
    );
    finish(editor, catalog);
}

/// Hover keeps one sample in flight with only the newest position waiting, and an answer for a
/// stack the canvas has left is dropped instead of shown.
#[test]
fn hover_keeps_one_sample_in_flight_and_drops_a_mismatched_identity() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 4);
    let _ = editor.update(Message::Pointer(PointerMessage::Moved(Some((3, 4)))));
    assert!(editor.sample_in_flight, "the first move asks");
    assert_eq!(editor.pending_sample, None);
    // Two further moves while the first is outstanding: only the newest is kept.
    let _ = editor.update(Message::Pointer(PointerMessage::Moved(Some((5, 6)))));
    let _ = editor.update(Message::Pointer(PointerMessage::Moved(Some((7, 8)))));
    assert_eq!(editor.pending_sample, Some((7, 8)));
    assert!(editor.sample_in_flight, "still exactly one in flight");
    // The same position again is not a second request.
    let _ = editor.update(Message::Pointer(PointerMessage::Moved(Some((7, 8)))));
    assert_eq!(editor.pending_sample, Some((7, 8)));

    // An answer for another stack describes an image the canvas has left.
    let _ = editor.update(Message::Pointer(PointerMessage::Sampled {
        entry: EntryId::new(),
        result: Ok(Readout {
            x: 3,
            y: 4,
            rgba: [1, 2, 3, 255],
        }),
    }));
    assert!(editor.readout.is_none(), "a mismatched identity is dropped");
    // The newest position was released as the next request when the first answered.
    assert!(editor.sample_in_flight);
    assert_eq!(editor.pending_sample, None);

    let _ = editor.update(Message::Pointer(PointerMessage::Sampled {
        entry: entry_id,
        result: Ok(Readout {
            x: 7,
            y: 8,
            rgba: [128, 64, 255, 255],
        }),
    }));
    let readout = editor.readout.as_ref().expect("an adopted readout");
    assert_eq!(readout.rgba, [128, 64, 255, 255]);
    assert!(!editor.sample_in_flight);
    editor.rederive();
    // The readout is the status bar's. No frame has been analysed in this test, so the plot
    // draws its pending notice inside its own area, and neither reaches the other.
    assert_eq!(
        editor.workspace.status.readout.as_deref(),
        Some("R 128 \u{b7} G 64 \u{b7} B 255 \u{b7} 7, 8")
    );
    assert_eq!(
        editor.workspace.histogram.notice().as_deref(),
        Some("No analysis yet")
    );
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["readout"]["rgba"], json!([128, 64, 255, 255]));
    assert_eq!(
        snapshot["status_bar"]["readout"],
        json!("R 128 \u{b7} G 64 \u{b7} B 255 \u{b7} 7, 8")
    );
    assert_eq!(snapshot["histogram"]["notice"], json!("No analysis yet"));

    // The pointer leaving clears the readout and any waiting position.
    let _ = editor.update(Message::Pointer(PointerMessage::Moved(None)));
    assert!(editor.readout.is_none() && editor.pending_sample.is_none());
    editor.rederive();
    assert_eq!(editor.workspace.status.readout, None);
    assert_eq!(editor.snapshot()["status_bar"]["readout"], Value::Null);
    finish(editor, catalog);
}

#[test]
fn a_canvas_pick_fills_the_located_content_coordinate_without_committing() {
    let (mut editor, catalog, entry_id) = picking();
    let (action, x, y) = pick_fields(&editor);
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Pointer(PointerMessage::Moved(Some((7, 9)))));
    assert_eq!(editor.pointer, Some((7, 9)));
    // The click itself fills nothing: which content pixel it is is the core's answer.
    let _ = editor.update(Message::Pointer(PointerMessage::Picked { x: 7, y: 9 }));
    assert_eq!(editor.fields.get(&action, &x), Some("0"));
    assert_eq!(editor.fields.get(&action, &y), Some("0"));
    let _ = editor.update(Message::Pointer(PointerMessage::Located {
        entry: entry_id,
        mode: pick_mode(&editor),
        view: (7, 9),
        result: Ok(ContentPoint {
            content_x: 100,
            content_y: 42,
            width: 400,
            height: 300,
        }),
    }));
    assert_eq!(editor.fields.get(&action, &x), Some("100"));
    assert_eq!(editor.fields.get(&action, &y), Some("42"));
    assert_eq!(
        editor.status,
        format!("Picked (100, 42) from view (7, 9) into {action}")
    );
    // The evidence carries both pixels, so a capture can be read against the view and the stack.
    assert_eq!(
        pick_events(&logged(&mut editor, &log)),
        vec![&json!({"action":action,"view_x":7,"view_y":9,"x":100,"y":42})]
    );
    // A pick commits nothing: the open stack and its revision are untouched.
    let state = editor.state.as_ref().expect("the open asset");
    assert_eq!(state.revision, 4);
    assert!(state.current_entry.snapshot.recipe.layers.is_empty());
    let _ = editor.update(Message::Control(ControlMessage::Field {
        action: action.clone(),
        parameter: x.clone(),
        text: "11".into(),
    }));
    assert_eq!(editor.fields.get(&action, &x), Some("11"));
    assert_eq!(editor.editing, Some((action.clone(), x.clone())));
    // The correlated state carries the module identities and what the controls hold.
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["controls"][format!("{action}.{x}")], json!("11"));
    assert_eq!(
        snapshot["modules"].as_array().map(Vec::len),
        Some(editor.modules.len())
    );
    assert_eq!(snapshot["developer"], json!(false));
    // The pick answered to the mode it was made in, which the frame records.
    assert_eq!(snapshot["workspace"]["mode"], json!(pick_mode(&editor)));
    finish(editor, catalog);
}

#[test]
fn a_located_point_for_another_entry_is_dropped() {
    let (mut editor, catalog, _) = picking();
    let (action, x, y) = pick_fields(&editor);
    let log = attach_log(&mut editor);
    let before = editor.status.clone();
    // The canvas moved to another stack while the mapping was in flight.
    let _ = editor.update(Message::Pointer(PointerMessage::Located {
        entry: EntryId::new(),
        mode: pick_mode(&editor),
        view: (7, 9),
        result: Ok(ContentPoint {
            content_x: 100,
            content_y: 42,
            width: 400,
            height: 300,
        }),
    }));
    assert_eq!(editor.fields.get(&action, &x), Some("0"));
    assert_eq!(editor.fields.get(&action, &y), Some("0"));
    assert_eq!(editor.status, before);
    assert!(pick_events(&logged(&mut editor, &log)).is_empty());
    finish(editor, catalog);
}

#[test]
fn a_point_outside_the_content_stage_reports_and_fills_nothing() {
    let (mut editor, catalog, entry_id) = picking();
    let (action, x, y) = pick_fields(&editor);
    let _ = editor.update(Message::Control(ControlMessage::Field {
        action: action.clone(),
        parameter: x.clone(),
        text: "5".into(),
    }));
    let log = attach_log(&mut editor);
    let refusal = "validation: point (7, 9) is outside the 4x3 rendered image";
    let _ = editor.update(Message::Pointer(PointerMessage::Located {
        entry: entry_id,
        mode: pick_mode(&editor),
        view: (7, 9),
        result: Err(refusal.into()),
    }));
    assert_eq!(
        editor.fields.get(&action, &x),
        Some("5"),
        "nothing is filled"
    );
    assert_eq!(editor.fields.get(&action, &y), Some("0"));
    assert_eq!(editor.status, refusal);
    assert_eq!(
        pick_events(&logged(&mut editor, &log)),
        vec![&json!({"mode":pick_mode(&editor),"view_x":7,"view_y":9,"error":refusal})]
    );
    finish(editor, catalog);
}

/// A point pick commits exactly when its module declares `commit`: the two located coordinates are
/// the whole request, sent as one command through the one request builder. The desktop names no
/// action to decide it, so the same module with the declaration turned off fills its fields
/// instead.
#[test]
fn a_point_pick_commits_only_when_its_module_declares_it() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (mode, action) = editor
        .modules
        .iter()
        .find_map(|module| match module.canvas.as_ref()? {
            luxforge_core::CanvasInteraction::PointPick {
                action,
                commit: true,
                ..
            } => Some((module.id.clone(), action.clone())),
            _ => None,
        })
        .expect("a module whose point pick commits");
    editor.session.workspace.mode = mode.clone();
    let entry_id = editor.displayed_entry().expect("a displayed entry");
    let located = |entry: EntryId| {
        Message::Pointer(PointerMessage::Located {
            entry,
            mode: mode.clone(),
            view: (7, 9),
            result: Ok(ContentPoint {
                content_x: 100,
                content_y: 42,
                width: 480,
                height: 320,
            }),
        })
    };

    // The copied request of that pick is the one the pick sends: the envelope and the two fields.
    let fields = Map::from_iter([("x".to_owned(), json!(100)), ("y".to_owned(), json!(42))]);
    let (method, request) = editor.request(&action, &fields).expect("a request");
    assert_eq!(method, format!("edit.{action}"));
    assert_eq!(request["x"], json!(100));
    assert_eq!(request["y"], json!(42));
    assert_eq!(request["mutation"]["expected_revision"], json!(4));

    let _ = editor.update(located(entry_id.clone()));
    assert!(editor.busy, "the pick was not committed: {}", editor.status);
    assert_eq!(editor.status, format!("Running edit.{action}…"));
    editor.busy = false;

    // Without the declaration the same pick fills the coordinates and commits nothing.
    for module in editor.modules.iter_mut() {
        if let Some(luxforge_core::CanvasInteraction::PointPick { commit, .. }) =
            module.canvas.as_mut()
            && module.id == mode
        {
            *commit = false;
        }
    }
    let _ = editor.update(located(entry_id));
    assert!(!editor.busy, "a filling pick committed");
    assert_eq!(editor.fields.get(&action, "x"), Some("100"));
    assert_eq!(editor.fields.get(&action, "y"), Some("42"));
    finish(editor, catalog);
}
