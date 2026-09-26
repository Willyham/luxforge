//! The slider gesture of a generated control, driven through the one core draft: what a drag, a
//! release, a double-click reset and Escape send, and every race a draft closes.
use super::{
    message::{ControlMessage, CropMessage, HistoryMessage, SyncMessage},
    tasks::mutation,
    testing::{
        Z6_AS_SHOT, Z6_CAM_XYZ, attach_log, begun, boot, descriptors, draft_events, drafting,
        entry, finish, logged, opened_with_modules, patch_control, raw_entry, raw_refresh,
        refresh_for,
    },
    *,
};
use crate::state::fields;
use lightwell_core::{AssetId, CropStage, RawPayload, WhiteBalanceMode};
use serde_json::Map;

/// A drag of a patch action's slider opens exactly one draft, sends exactly one `draft.set`
/// for the newest value, and sends nothing at all while a round trip is in flight.
#[test]
fn a_drag_sends_one_draft_set_for_the_newest_value() {
    let (mut editor, catalog, log, asset, action, parameter) = drafting();

    // Every move while `draft.begin` is in flight replaces one pending value: the answer sends
    // the last. The values are ones the widget would send: it quantizes each drag to the
    // parameter's declared step and precision before the message is published.
    for value in [25.0, 50.0, 75.0] {
        let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value,
        }));
    }
    assert_eq!(
        editor.fields.get(&action, &parameter),
        Some("75"),
        "the field follows the pointer"
    );
    assert_eq!(editor.dragging, Some((action.clone(), parameter.clone())));
    assert!(
        editor.status.starts_with("Drafting "),
        "the status bar names the gesture: {}",
        editor.status
    );

    begun(&mut editor, &asset, &action, 4);
    // A move to the value already accepted sends nothing again.
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 75.0,
    }));

    let records = logged(&mut editor, &log);
    assert_eq!(
        draft_events(&records, "slider_draft_begin").len(),
        1,
        "a gesture opens one draft"
    );
    let sets = draft_events(&records, "slider_draft_set");
    assert_eq!(
        sets.len(),
        1,
        "three moves with one round trip in flight send one draft.set: {sets:?}"
    );
    assert_eq!(
        sets[0]["fields"],
        json!({ parameter.clone(): 75.0 }),
        "and it carries the newest value, as one field patch"
    );
    assert_eq!(
        editor
            .session
            .draft
            .as_ref()
            .map(|draft| draft.draft_revision),
        Some(1),
        "the adopted draft carries the revision the frame is correlated with"
    );
    finish(editor, catalog);
}

/// The first action a registered module declares with exactly one parameter and no field
/// patch, and that parameter: the shape whose slider drafts for the second reason.
fn single_parameter_control(editor: &Editor) -> (String, String) {
    let action = editor
        .modules
        .iter()
        .flat_map(|module| module.actions.iter())
        .find(|action| !action.patch && action.parameters.len() == 1)
        .expect("a built-in declares a single-parameter action");
    (action.id.clone(), action.parameters[0].name.clone())
}

/// The single-parameter action these tests drive is the shape the rule is about: one number
/// parameter with a declared default, which is what a RAW slider sends.
#[test]
fn the_single_parameter_control_under_test_is_one_number_with_a_default() {
    let (editor, catalog, _, _, _, _) = drafting();
    let (action, parameter) = single_parameter_control(&editor);
    let declared = tools::declared_action(&editor.modules, &action)
        .and_then(|declared| declared.parameter(&parameter))
        .expect("the declared parameter");
    assert!(
        matches!(declared.kind, lightwell_core::ParameterKind::Number { .. }),
        "{action}.{parameter} is {:?}",
        declared.kind
    );
    assert!(
        declared.default.is_some(),
        "{action}.{parameter} declares the default a reset sends"
    );
    finish(editor, catalog);
}

/// A slider of an ordinary action whose one parameter is the whole request drafts exactly like
/// a patch action's: one `draft.begin`, one `draft.set` per tick for the newest value, one
/// `draft.commit` on release. The one field it sends is the complete request, so the core needs
/// nothing special and the person sees the preview move while dragging.
#[test]
fn a_single_parameter_actions_slider_drafts_previews_and_commits_once() {
    let (mut editor, catalog, log, asset, _, _) = drafting();
    let (action, parameter) = single_parameter_control(&editor);

    for value in [0.25, 0.5] {
        let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value,
        }));
    }
    assert!(
        editor.slider_gesture().is_some(),
        "the gesture opened a draft: {}",
        editor.status
    );
    begun(&mut editor, &asset, &action, 4);
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 0.75,
    }));
    let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
        action: action.clone(),
        parameter: parameter.clone(),
    }));

    let records = logged(&mut editor, &log);
    assert_eq!(
        draft_events(&records, "slider_draft_begin").len(),
        1,
        "one gesture opens one draft"
    );
    let sets = draft_events(&records, "slider_draft_set");
    assert_eq!(sets.len(), 2, "one draft.set per accepted value: {sets:?}");
    assert_eq!(
        sets[0]["fields"],
        json!({ parameter.clone(): 0.5 }),
        "and it carries the newest value, as the whole request"
    );
    assert_eq!(sets[1]["fields"], json!({ parameter.clone(): 0.75 }));
    assert_eq!(
        draft_events(&records, "slider_draft_preview").len(),
        2,
        "each accepted set queues the preview its value produces"
    );
    assert_eq!(
        draft_events(&records, "slider_draft_commit").len(),
        1,
        "release commits exactly once"
    );
    finish(editor, catalog);
}

/// An action with a second parameter keeps the older gesture: one field of it is not a request,
/// so dragging only changes the text and release submits the whole action once.
#[test]
fn a_multi_parameter_actions_slider_sends_nothing_until_release() {
    let (mut editor, catalog, log, _, _, _) = drafting();
    let (action, parameter) = editor
        .modules
        .iter()
        .flat_map(|module| module.actions.iter())
        .find(|action| {
            !action.patch
                && action.parameters.len() > 1
                && matches!(
                    action.parameters[0].kind,
                    lightwell_core::ParameterKind::Integer { .. }
                        | lightwell_core::ParameterKind::Number { .. }
                )
        })
        .map(|action| (action.id.clone(), action.parameters[0].name.clone()))
        .expect("a built-in declares a multi-parameter action led by a slider field");

    for value in [3.0, 7.0] {
        let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value,
        }));
    }
    assert!(editor.slider_gesture().is_none(), "no draft was opened");
    assert_eq!(
        editor.fields.get(&action, &parameter),
        Some("7"),
        "the field still follows the pointer"
    );
    assert!(
        draft_events(&logged(&mut editor, &log), "slider_draft_begin").is_empty(),
        "nothing was sent while dragging"
    );

    let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    assert_eq!(
        editor.status,
        format!("Running edit.{action}…"),
        "release submits the whole action once"
    );
    assert!(editor.busy, "and exactly one request is in flight");
    finish(editor, catalog);
}

/// One double-click on a drafting slider, as the rail's wrapper and iced's slider deliver it:
/// the first press moves the value (the gesture opens, `draft.begin` and `draft.set` answer),
/// its release sends `draft.commit`, and the second press — the reset — arrives before that
/// commit has answered. Returns the entry the commit would produce.
fn double_click_before_the_commit_answers(
    editor: &mut Editor,
    asset: &AssetId,
    action: &str,
    parameter: &str,
    value: f64,
) -> lightwell_core::HistoryEntry {
    let current = editor
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.into(),
        parameter: parameter.into(),
        value,
    }));
    begun(editor, asset, action, current.sequence);
    let _ = editor.update(Message::Control(ControlMessage::Released {
        action: action.into(),
        parameter: parameter.into(),
    }));
    assert_eq!(
        editor
            .core_gesture()
            .and_then(|gesture| gesture.draft.in_flight()),
        Some(draft::Round::Commit),
        "the release's commit is in flight"
    );
    let _ = editor.update(Message::Control(ControlMessage::ResetField {
        action: action.into(),
        parameter: parameter.into(),
    }));
    entry(asset, current.sequence + 1, Some(&current.id))
}

/// The double-click race that lost every RAW white balance reset. The first press's commit is
/// still answering when the second press arrives — for a RAW temperature or tint for as long as
/// the mosaic takes to redevelop, a second or more — and a reset sent then names the revision
/// that commit is replacing, which the core refuses as stale. The reset now waits for the
/// commit's answer and is sent once, against the revision it produced. Basic's patch field and
/// every RAW slider take the same path; what is sent is the field's own reset: As shot for the
/// RAW temperature and tint, the declared default for RAW and Basic exposure.
#[test]
fn a_reset_during_a_gesture_commit_waits_and_names_the_revision_the_commit_produced() {
    let cases = [
        (
            "set-raw-exposure",
            "ev",
            0.35,
            "set-raw-exposure",
            json!({"ev": 0.0}),
        ),
        (
            "set-raw-temperature",
            "kelvin",
            5000.0,
            "use-as-shot-wb",
            json!({}),
        ),
        ("set-raw-tint", "tint", 12.0, "use-as-shot-wb", json!({})),
        (
            "set-basic",
            "exposure",
            0.4,
            "set-basic",
            json!({"exposure": 0.0}),
        ),
    ];
    for (action, parameter, value, reset, preset) in cases {
        let (mut editor, catalog, log, asset, _, _) = drafting();
        let revision = editor.state.as_ref().expect("an open asset").revision;
        let committed =
            double_click_before_the_commit_answers(&mut editor, &asset, action, parameter, value);
        let records = logged(&mut editor, &log);
        let queued = draft_events(&records, "field_reset_queued");
        assert_eq!(queued.len(), 1, "{action}: the reset waits: {records:?}");
        assert_eq!(queued[0]["revision"], json!(revision));
        assert!(
            draft_events(&records, "field_reset_sent").is_empty(),
            "{action}: nothing is sent against the revision the commit is replacing"
        );
        assert!(!editor.busy, "{action}: no request was started");
        assert!(editor.pending_reset.is_some());

        // The commit answers with the next revision; the reset goes out in the same update.
        let log = attach_log(&mut editor);
        let refresh = refresh_for(&asset, &committed, Vec::new(), &[&committed], false);
        testing::answer_commit(&mut editor, Ok(Some(refresh)));
        let records = logged(&mut editor, &log);
        let sent = draft_events(&records, "field_reset_sent");
        assert_eq!(sent.len(), 1, "{action}: sent once");
        assert_eq!(
            sent[0]["revision"],
            json!(revision + 1),
            "{action}: against the commit's revision"
        );
        assert_eq!(sent[0]["action"], json!(reset), "{action}");
        assert_eq!(sent[0]["preset"], preset, "{action}");
        assert_eq!(
            sent[0]["field"],
            json!({"action": action, "parameter": parameter})
        );
        assert_eq!(editor.status, format!("Running edit.{reset}…"));
        assert!(editor.busy && editor.pending_reset.is_none());
        finish(editor, catalog);
    }
}

/// A double-click on the RAW custom temperature or tint label, with nothing in flight, runs As
/// shot at once — the very request an independent JSON client builds from the control's reset
/// in `module.list` — and the field shows the authoritative value until the answer brings the
/// as-shot equivalent, never the 6504 K and 0 nothing set. RAW exposure and Basic's own
/// temperature, which declare no reset, still reset to their declared defaults.
#[test]
fn a_double_click_on_a_raw_white_balance_field_returns_to_as_shot() {
    let listed = serde_json::to_value(descriptors()).unwrap();
    for (action, parameter) in [("set-raw-temperature", "kelvin"), ("set-raw-tint", "tint")] {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let log = attach_log(&mut editor);
        let asset = editor.state.as_ref().expect("open").asset.id.clone();
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let mut custom = original.clone();
        custom.wb_mode = WhiteBalanceMode::Custom;
        custom.temperature_kelvin = Some(5000.0);
        custom.tint = Some(12.0);
        custom.gains =
            lightwell_core::gains_from_temperature_tint(5000.0, 12.0, Z6_CAM_XYZ).unwrap();
        let current = raw_entry(&asset, 5, None, &custom);
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            raw_refresh(&asset, &current),
        )))));
        let committed = editor.fields.get(action, parameter).map(str::to_owned);
        assert_eq!(
            committed.as_deref(),
            Some(if parameter == "kelvin" { "5000" } else { "12" })
        );

        // The JSON request a client builds from the control's declared reset.
        let control = listed
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|module| module["controls"][0]["controls"].as_array().cloned())
            .flatten()
            .find(|control| control["action"] == action && control["parameter"] == parameter)
            .expect("the listed control");
        let reset = &control["reset"];
        assert_eq!(reset, &json!({"action": "use-as-shot-wb", "preset": {}}));
        let mut params = json!({"asset_id": asset, "mutation": mutation(5)});
        params
            .as_object_mut()
            .unwrap()
            .extend(reset["preset"].as_object().unwrap().clone());
        let independent = json!({"method": format!("edit.{}", reset["action"].as_str().unwrap()), "params": params});
        let (sent, preset) = fields::field_reset(&editor.modules, action, parameter).unwrap();
        // Each envelope mints its own request identity; everything else is the same request.
        let without_request_id = |mut request: Value| {
            request["params"]["mutation"]
                .as_object_mut()
                .expect("a mutation")
                .remove("request_id");
            request
        };
        assert_eq!(
            editor
                .request_for_preset(&sent, None, Some(&preset))
                .map(without_request_id),
            Some(without_request_id(independent)),
            "{action}: the desktop's reset request is the JSON client's"
        );

        // Typed but not committed, then double-clicked.
        editor.fields.set(action, parameter, "7777".into());
        editor.editing = Some((action.into(), parameter.into()));
        let _ = editor.update(Message::Control(ControlMessage::ResetField {
            action: action.into(),
            parameter: parameter.into(),
        }));
        let records = logged(&mut editor, &log);
        let sent = draft_events(&records, "field_reset_sent");
        assert_eq!(sent.len(), 1, "{action}: {records:?}");
        assert_eq!(sent[0]["action"], json!("use-as-shot-wb"));
        assert_eq!(sent[0]["preset"], json!({}));
        assert_eq!(editor.status, "Running edit.use-as-shot-wb…");
        assert!(editor.editing.is_none());
        assert_eq!(
            editor.fields.get(action, parameter).map(str::to_owned),
            committed,
            "{action}: the field shows the committed value until the answer, not a default"
        );

        // The answer: As shot, whose rows report the camera's equivalent.
        let answered = raw_entry(&asset, 6, Some(&current.id), &original);
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            raw_refresh(&asset, &answered),
        )))));
        assert_eq!(
            editor.fields.get(action, parameter),
            Some(if parameter == "kelvin" { "4861" } else { "-50" }),
            "{action}: the as-shot equivalent"
        );
        finish(editor, catalog);
    }

    // Exposure and Basic's temperature keep their declared defaults.
    for (action, parameter, preset) in [
        ("set-raw-exposure", "ev", json!({"ev": 0.0})),
        ("set-basic", "temperature", json!({"temperature": 0.0})),
    ] {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::Control(ControlMessage::ResetField {
            action: action.into(),
            parameter: parameter.into(),
        }));
        let records = logged(&mut editor, &log);
        let sent = draft_events(&records, "field_reset_sent");
        assert_eq!(sent.len(), 1, "{action}");
        assert_eq!(sent[0]["action"], json!(action));
        assert_eq!(sent[0]["preset"], preset);
        finish(editor, catalog);
    }
}

/// A reset that arrives while another request is in flight waits for its answer too, and is
/// dropped, with its reason, if what it was asked for is no longer on screen by then.
#[test]
fn a_waiting_reset_runs_after_a_request_and_is_dropped_on_a_historical_entry() {
    let (mut editor, catalog, log, asset, _, _) = drafting();
    let (action, parameter) = single_parameter_control(&editor);
    let current = editor
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    editor.busy = true;
    let _ = editor.update(Message::Control(ControlMessage::ResetField {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    assert!(editor.pending_reset.is_some(), "it waits for the request");
    let next = entry(&asset, current.sequence + 1, Some(&current.id));
    let refresh = refresh_for(&asset, &next, Vec::new(), &[&next], false);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    let sent = draft_events(&logged(&mut editor, &log), "field_reset_sent")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["revision"], json!(current.sequence + 1));

    // Asked for while a request is in flight, then the session shows a historical entry.
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Control(ControlMessage::ResetField {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    assert!(editor.pending_reset.is_some());
    editor.session.preview.selection = lightwell_core::HistorySelection::Entry(current.id.clone());
    editor.busy = false;
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    let records = logged(&mut editor, &log);
    let dropped = draft_events(&records, "field_reset_dropped");
    assert_eq!(dropped.len(), 1, "{records:?}");
    assert_eq!(dropped[0]["reason"], json!("a historical entry is shown"));
    assert!(editor.pending_reset.is_none());
    assert!(draft_events(&records, "field_reset_sent").is_empty());
    assert!(
        editor
            .status
            .ends_with("was not reset: a historical entry is shown")
    );
    finish(editor, catalog);
}

/// A RAW draft is accepted, but its preview job answers preparation-required: the development
/// is not in memory, because a redevelopment or a source preparation is in flight, and the core
/// renders no stale frame. (A drafted temperature over a development that is in memory previews
/// approximately instead; this is what is left.) The status bar says what the person will see
/// rather than the error code, and the gesture stays open and drained, so its release still
/// commits.
#[test]
fn a_draft_the_core_cannot_preview_says_so_and_stays_open() {
    let (mut editor, catalog, log, asset, _, _) = drafting();
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: "set-raw-temperature".into(),
        parameter: "kelvin".into(),
        value: 5000.0,
    }));
    // The owner accepts the value, but its preview job answers preparation-required.
    editor.fake_sets = Some(["preparation-required: source-job-7".to_owned()].into());
    begun(&mut editor, &asset, "set-raw-temperature", 4);
    assert_eq!(
        editor.status,
        "Custom temperature cannot be previewed until the RAW development is ready; it shows on release"
    );
    assert!(
        editor
            .core_gesture()
            .is_some_and(|gesture| gesture.draft.drained()),
        "the gesture is still open and has nothing in flight"
    );
    assert!(
        editor
            .slider_gesture()
            .is_some_and(|slider| slider.unpreviewed),
        "and no frame of its own is coming for the value it holds"
    );
    let records = logged(&mut editor, &log);
    let unpreviewed = draft_events(&records, "slider_draft_unpreviewed");
    assert_eq!(
        unpreviewed.last().map(|detail| &detail["error"]),
        Some(&json!("preparation-required: source-job-7"))
    );
    assert_eq!(unpreviewed.last().unwrap()["value"], json!(5000.0));
    finish(editor, catalog);
}

/// A drafting slider opens its draft on its first change, so a release with no draft open
/// changed nothing and sends nothing — not the unchanged field, which would hold the section
/// busy through the moment a double-click's second press arrives, and which for a RAW custom
/// white balance under As shot would switch it to Custom.
#[test]
fn releasing_a_drafting_slider_that_never_moved_sends_nothing() {
    for (action, parameter) in [("set-raw-temperature", "kelvin"), ("set-basic", "exposure")] {
        let (mut editor, catalog, log, _, _, _) = drafting();
        let _ = editor.update(Message::Control(ControlMessage::Released {
            action: action.into(),
            parameter: parameter.into(),
        }));
        let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
            action: action.into(),
            parameter: parameter.into(),
        }));
        assert!(!editor.busy, "{action}: no request is in flight");
        assert!(editor.editable());
        assert!(
            !editor.status.starts_with("Running"),
            "{action}: {}",
            editor.status
        );
        assert!(draft_events(&logged(&mut editor, &log), "slider_draft_begin").is_empty());
        finish(editor, catalog);
    }
}

/// The double-click reset follows the same rule: one field is one action where that field is
/// the whole request, and it sends the parameter's declared default.
#[test]
fn the_double_click_reset_of_a_single_parameter_action_sends_its_declared_default() {
    let (mut editor, catalog, _, _, _, _) = drafting();
    let (action, parameter) = single_parameter_control(&editor);
    let default = tools::declared_action(&editor.modules, &action)
        .and_then(|declared| declared.parameter(&parameter))
        .and_then(|declared| declared.default.clone())
        .expect("the parameter declares a default");
    let (sent, preset) = fields::field_reset(&editor.modules, &action, &parameter)
        .expect("a single-parameter action resets that field as one action");
    assert_eq!(sent, action, "exposure declares no reset of its own");
    assert_eq!(
        preset,
        [(parameter.clone(), default.clone())]
            .into_iter()
            .collect::<serde_json::Map<_, _>>(),
        "the request is the declared default, not an invented one"
    );

    editor.fields.set(&action, &parameter, "1.25".to_owned());
    let _ = editor.update(Message::Control(ControlMessage::ResetField {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    assert_eq!(
        editor.fields.get(&action, &parameter),
        Some(fields::seed_text(
            tools::declared_action(&editor.modules, &action)
                .and_then(|declared| declared.parameter(&parameter))
                .expect("the declared parameter")
        ))
        .as_deref(),
        "the field returns to its default"
    );
    assert_eq!(
        editor.status,
        format!("Running edit.{action}…"),
        "and the reset runs once as that action"
    );
    finish(editor, catalog);
}

/// Release commits exactly once, through `draft.commit` with the draft's own base revision.
/// A no-op outcome ends the gesture with no entry and no history refresh.
#[test]
fn release_commits_once_and_a_return_to_start_commits_nothing() {
    let (mut editor, catalog, log, asset, action, parameter) = drafting();
    let history = editor.history.entries.len();
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 1.0,
    }));
    begun(&mut editor, &asset, &action, 4);

    let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    let records = logged(&mut editor, &log);
    let commits = draft_events(&records, "slider_draft_commit");
    assert_eq!(commits.len(), 1, "one gesture is one commit: {commits:?}");
    assert_eq!(
        commits[0]["expected_revision"],
        json!(4),
        "the commit names the revision the draft was based on"
    );
    assert!(editor.dragging.is_none(), "release ends the drag");

    // The gesture returned to its start: a no-op outcome, no entry, no history refresh.
    testing::answer_commit(&mut editor, Ok(None));
    assert!(editor.slider_gesture().is_none(), "the gesture is over");
    assert!(editor.session.draft.is_none(), "and so is the core draft");
    assert_eq!(
        editor.history.entries.len(),
        history,
        "a no-op adds no history entry"
    );
    assert_eq!(editor.snapshot()["draft"], json!(null));
    finish(editor, catalog);
}

/// Escape discards the gesture: `draft.cancel`, no commit, and the field returns to the
/// authoritative value the displayed entry reports.
#[test]
fn escape_cancels_the_gesture_and_commits_nothing() {
    let (mut editor, catalog, log, asset, action, parameter) = drafting();
    let default = editor
        .fields
        .get(&action, &parameter)
        .expect("a seeded field")
        .to_owned();
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 2.0,
    }));
    begun(&mut editor, &asset, &action, 4);

    // Exactly the message the keyboard table raises for Escape while a gesture is open.
    let context = keymap::KeyContext {
        slider_drafting: true,
        ..editor.key_context()
    };
    let escape = keymap::keymap(
        &iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }),
        iced::event::Status::Ignored,
        &context,
    );
    assert!(
        matches!(escape, Some(Message::Draft(message::DraftMessage::Cancel))),
        "Escape discards an open slider gesture: {escape:?}"
    );
    let _ = editor.update(escape.expect("the mapped message"));

    let records = logged(&mut editor, &log);
    assert_eq!(draft_events(&records, "slider_draft_cancelled").len(), 1);
    assert!(
        draft_events(&records, "slider_draft_commit").is_empty(),
        "a cancelled gesture commits nothing"
    );
    assert!(editor.slider_gesture().is_none());
    assert_eq!(
        editor.fields.get(&action, &parameter),
        Some(default.as_str()),
        "the field returns to the authoritative value"
    );
    finish(editor, catalog);
}

/// An external revision during a gesture keeps the draft, marks it conflicted, raises the
/// Changed elsewhere notice and refuses the commit until Discard or Reapply answers it.
#[test]
fn an_external_commit_during_a_gesture_conflicts_it_and_reapply_clears_it() {
    let (mut editor, catalog, log, asset, action, parameter) = drafting();
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 15.0,
    }));
    begun(&mut editor, &asset, &action, 4);

    // Somebody else committed, which is also what this desktop's own undo looks like.
    let newer = entry(&asset, 9, None);
    let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(
        tasks::SyncResult::changed(refresh),
    ))));
    let draft = &editor.core_gesture().expect("the draft is kept").draft;
    assert!(draft.conflicted);
    assert_eq!(
        editor.fields.get(&action, &parameter),
        Some("15"),
        "the drafted value stays on the slider"
    );
    assert_eq!(
        editor.snapshot()["notices"],
        json!(["Changed elsewhere"]),
        "the captured frame records the chrome it drew"
    );
    assert_eq!(editor.snapshot()["draft"]["conflicted"], json!(true));

    // Commit is refused while it is conflicted; nothing is sent.
    let conflicts = logged(&mut editor, &log);
    assert_eq!(
        draft_events(&conflicts, "slider_draft_conflicted").len(),
        1,
        "the conflict is recorded once, with the revision that caused it"
    );
    let log2 = attach_log(&mut editor);
    let _ = editor.update(Message::Draft(message::DraftMessage::Commit));
    assert!(
        draft_events(&logged(&mut editor, &log2), "slider_draft_commit").is_empty(),
        "a conflicted gesture refuses to commit"
    );
    assert!(editor.core_gesture().expect("kept").draft.conflicted);

    // Reapply rebases it on the new revision and re-sends the value this client set.
    let log3 = attach_log(&mut editor);
    let mut rebased = lightwell_core::Draft::new(&action, asset.clone(), 9);
    rebased.draft_revision = 1;
    rebased.fields = json!({ parameter.clone(): 1.5 })
        .as_object()
        .cloned()
        .expect("an object");
    rebased.conflicted = false;
    let _ = editor.update(Message::Draft(message::DraftMessage::Reapply));
    testing::answer_reapply(&mut editor, Ok(rebased));
    let draft = &editor.core_gesture().expect("the rebased draft").draft;
    assert!(!draft.conflicted);
    assert_eq!(draft.base_revision, 9);
    let records = logged(&mut editor, &log3);
    let sets = draft_events(&records, "slider_draft_set");
    assert_eq!(
        sets.len(),
        1,
        "a reapply re-sends the drafted value and re-requests its preview"
    );
    assert_eq!(sets[0]["fields"], json!({ parameter.clone(): 15.0 }));
    assert!(editor.workspace.canvas.notices.is_empty());
    finish(editor, catalog);
}

/// The gesture's request is the request an independent JSON client sends: `draft.set` carries
/// exactly the one field the slider moved, and it is the same field `edit.<action>` carries
/// when the same value is typed and submitted with Enter.
#[test]
fn a_gesture_and_a_json_client_send_the_same_one_field_patch() {
    let (mut editor, catalog, log, asset, action, parameter) = drafting();
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 1.0,
    }));
    begun(&mut editor, &asset, &action, 4);
    let sets = {
        let records = logged(&mut editor, &log);
        draft_events(&records, "slider_draft_set")
            .first()
            .cloned()
            .cloned()
            .expect("one draft.set")
    };
    let _ = editor.update(Message::Draft(message::DraftMessage::Cancel));

    // The same value typed into the field and submitted with Enter.
    editor.busy = false;
    let request = editor
        .request_for(&action, Some(&parameter))
        .expect("the control's own request");
    assert_eq!(request["method"], json!(format!("edit.{action}")));
    let params = request["params"]
        .as_object()
        .expect("an object")
        .clone()
        .into_iter()
        .filter(|(key, _)| key != "asset_id" && key != "mutation")
        .collect::<Map<String, Value>>();
    // The gesture's fields and the client's parameters are the same patch, field for field.
    let declared = tools::declared_action(&editor.modules, &action).expect("declared");
    assert!(declared.patch);
    assert_eq!(params.len(), 1, "a patch sends one field: {params:?}");
    assert_eq!(
        sets["fields"]
            .as_object()
            .expect("an object")
            .keys()
            .collect::<Vec<_>>(),
        params.keys().collect::<Vec<_>>(),
        "the drag and the JSON client name the same field"
    );

    // Enter in the field runs exactly that request.
    let _ = editor.update(Message::Control(ControlMessage::Submit {
        action: action.clone(),
        parameter: Some(parameter.clone()),
    }));
    assert!(
        editor.status.starts_with(&format!("Running edit.{action}")),
        "{}",
        editor.status
    );
    finish(editor, catalog);
}

/// Every declared field of a patch action goes through one gesture path: it drafts on the
/// first move, sends exactly one `draft.set` naming that field, commits once on release,
/// cancels without committing, and survives an external commit until Reapply clears it. The
/// loop is over the descriptor's own parameters, so no field is named here and a new one is
/// covered the day it is declared.
#[test]
fn every_patch_field_drafts_commits_cancels_and_reapplies_through_one_path() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let asset = editor.state.as_ref().expect("open").asset.id.clone();
    let (action, _) = patch_control(&editor);
    let declared = tools::declared_action(&editor.modules, &action)
        .expect("the declared patch action")
        .clone();
    let fields: Vec<(String, f64)> = declared
        .parameters
        .iter()
        .filter_map(|parameter| match parameter.kind {
            lightwell_core::ParameterKind::Number { min, max } => {
                // Any value inside the declared range that is not the neutral default: half
                // the positive end, or half the negative one for a range without a positive.
                let value = if max / 2.0 != 0.0 {
                    max / 2.0
                } else {
                    min / 2.0
                };
                Some((parameter.name.clone(), value))
            }
            _ => None,
        })
        .collect();
    assert!(
        fields.len() >= 2,
        "the patch action declares its fields: {fields:?}"
    );

    for (parameter, value) in &fields {
        let log = attach_log(&mut editor);
        // The drag: one draft, one set for this field alone.
        editor.busy = false;
        let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: *value,
        }));
        assert!(
            editor.slider_gesture().is_some(),
            "{parameter} did not open a draft: {}",
            editor.status
        );
        begun(&mut editor, &asset, &action, 4);
        let records = logged(&mut editor, &log);
        let sets = draft_events(&records, "slider_draft_set");
        assert_eq!(sets.len(), 1, "{parameter}: {sets:?}");
        assert_eq!(
            sets[0]["fields"],
            json!({ parameter.clone(): value }),
            "{parameter} drafts its own field alone"
        );
        let shown = fields::declared(&editor.modules, &action, parameter)
            .and_then(crate::state::number::NumberSpec::of)
            .map(|spec| spec.format(*value))
            .expect("the declared parameter");
        assert_eq!(
            editor.fields.get(&action, parameter),
            Some(shown.as_str()),
            "{parameter} shows the drafted value, with its declared decimals"
        );

        // The release: one commit, then the no-op outcome that ends the gesture.
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
            action: action.clone(),
            parameter: parameter.clone(),
        }));
        let records = logged(&mut editor, &log);
        assert_eq!(
            draft_events(&records, "slider_draft_commit").len(),
            1,
            "{parameter} committed once"
        );
        testing::answer_commit(&mut editor, Ok(None));
        assert!(
            editor.slider_gesture().is_none(),
            "{parameter} left a gesture open"
        );

        // Escape: the gesture ends and commits nothing.
        editor.busy = false;
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: *value,
        }));
        begun(&mut editor, &asset, &action, 4);
        let _ = editor.update(Message::Draft(message::DraftMessage::Cancel));
        let records = logged(&mut editor, &log);
        assert!(
            draft_events(&records, "slider_draft_commit").is_empty(),
            "{parameter} committed on Escape"
        );
        assert!(
            !draft_events(&records, "slider_draft_cancelled").is_empty(),
            "{parameter} did not cancel"
        );
        assert!(
            editor.slider_gesture().is_none(),
            "{parameter} kept its draft"
        );
        // The slot is free once the owner has ended the draft.
        testing::answer_cancel(&mut editor);
        assert!(
            editor.gesture.is_none(),
            "{parameter} left its draft closing"
        );

        // An external commit under the gesture, then Reapply.
        editor.busy = false;
        let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: parameter.clone(),
            value: *value,
        }));
        begun(&mut editor, &asset, &action, 4);
        editor.gesture_revision(5);
        assert!(
            editor
                .core_gesture()
                .is_some_and(|gesture| gesture.draft.conflicted),
            "{parameter} was not marked conflicted"
        );
        let _ = editor.update(Message::Draft(message::DraftMessage::Reapply));
        let log = attach_log(&mut editor);
        testing::answer_reapply(
            &mut editor,
            Ok(lightwell_core::Draft::new(&action, asset.clone(), 5)),
        );
        let rebased = &editor.core_gesture().expect("the rebased draft").draft;
        assert!(!rebased.conflicted, "{parameter} stayed conflicted");
        assert_eq!(
            rebased.base_revision, 5,
            "{parameter} was not rebased on the new revision"
        );
        assert_eq!(
            rebased.sent(),
            Some(&json!({ parameter.clone(): value })),
            "{parameter} did not re-send the value this client set"
        );
        assert_eq!(
            draft_events(&logged(&mut editor, &log), "slider_draft_set").len(),
            1,
            "{parameter}: the reapply re-sent it once"
        );
        let _ = editor.update(Message::Draft(message::DraftMessage::Cancel));
        testing::answer_cancel(&mut editor);
        assert!(editor.gesture.is_none());
    }
    finish(editor, catalog);
}

/// A drag re-derives the section whose action it drafts and leaves every other section exactly
/// as it was, which is the per-section version rule the panel is built on.
#[test]
fn a_drag_re_derives_only_the_drafting_modules_section() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    editor.developer = true;
    editor.rederive();
    let (action, parameter) = patch_control(&editor);
    let owner = editor
        .modules
        .iter()
        .find(|module| module.action(&action).is_some())
        .expect("the declaring module")
        .id
        .clone();
    let versions = |editor: &Editor| -> BTreeMap<String, u64> {
        editor
            .workspace
            .tools
            .all()
            .map(|section| (section.module_id.clone(), section.version))
            .collect()
    };
    let before = versions(&editor);
    assert!(before.len() > 1, "more than one section is on screen");
    // The preset library disables its rows while any draft is open, so opening the gesture
    // re-derives that section once; nothing else outside the drafting module moves.
    let library = crate::state::presets::presets_control(&editor.modules)
        .map(|(module, _)| module.id.clone())
        .expect("the presets control");

    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 0.5,
    }));
    let opened = versions(&editor);
    for (module, version) in &before {
        if module == &owner || module == &library {
            assert!(
                opened[module] > *version,
                "{module} follows the gesture: {version} to {}",
                opened[module]
            );
        } else {
            assert_eq!(
                opened[module], *version,
                "{module} was re-derived by a drag in another module"
            );
        }
    }

    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action,
        parameter,
        value: 0.75,
    }));
    let after = versions(&editor);
    for (module, version) in &opened {
        if module == &owner {
            assert!(
                after[module] > *version,
                "{module} follows its own field: {version} to {}",
                after[module]
            );
        } else {
            assert_eq!(
                after[module], *version,
                "{module} was re-derived by a move in another module"
            );
        }
    }
    finish(editor, catalog);
}

/// At most one draft per client: a gesture is refused while the crop draft is open, and the
/// crop mode and Compare are refused while a gesture is open.
#[test]
fn one_draft_at_a_time_is_refused_from_either_side() {
    let (mut editor, catalog, _, asset, action, parameter) = drafting();
    let crop = tools::crop_frame(&editor.modules)
        .expect("a declared crop frame")
        .module
        .id
        .to_owned();

    // A gesture while the crop draft is open.
    let _ = editor.update(Message::Crop(CropMessage::Start));
    crate::app::testing::open_crop(
        &mut editor,
        CropStage {
            width: 480,
            height: 320,
            angle: 0.0,
        },
    );
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: parameter.clone(),
        value: 1.0,
    }));
    assert!(editor.slider_gesture().is_none(), "{}", editor.status);
    assert!(editor.status.contains("crop draft"), "{}", editor.status);
    let _ = editor.update(Message::Crop(CropMessage::Cancel));
    crate::app::testing::answer_cancel(&mut editor);

    // The crop mode and Compare while a gesture is open.
    editor.busy = false;
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action,
        parameter,
        value: 1.0,
    }));
    begun(&mut editor, &asset, "unused", 4);
    assert!(editor.slider_gesture().is_some());
    let _ = editor.update(Message::View(ViewMessage::SetMode(crop)));
    assert!(editor.crop().is_none() && editor.crop_pending().is_none());
    assert!(editor.status.contains("slider draft"), "{}", editor.status);
    editor.original_entry = Some(entry(&asset, 0, None).id);
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert!(editor.compare_return.is_none());
    assert!(editor.status.contains("slider draft"), "{}", editor.status);
    finish(editor, catalog);
}

#[test]
fn a_slider_drag_changes_the_field_and_sends_no_request() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let (action, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
    let (action, x) = (action.to_owned(), x.to_owned());
    let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
        action: action.clone(),
        parameter: x.clone(),
        value: 12.0,
    }));
    assert_eq!(editor.fields.get(&action, &x), Some("12"));
    assert_eq!(editor.dragging, Some((action.clone(), x.clone())));
    assert_eq!(editor.api_sequence, 0, "a drag calls nothing");
    let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
        action: action.clone(),
        parameter: x.clone(),
    }));
    assert!(editor.dragging.is_none(), "release ends the drag");
    // Editing a value and cancelling leaves the text exactly as it was.
    let _ = editor.update(Message::Control(ControlMessage::EditValue {
        action: action.clone(),
        parameter: x.clone(),
    }));
    assert_eq!(editor.editing, Some((action.clone(), x.clone())));
    let _ = editor.update(Message::Control(ControlMessage::CancelEdit));
    assert!(editor.editing.is_none());
    assert_eq!(editor.fields.get(&action, &x), Some("12"));
    finish(editor, catalog);
}

#[test]
fn a_slider_drag_of_many_moves_and_one_release_sends_exactly_one_request() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (action, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
    let (action, x) = (action.to_owned(), x.to_owned());

    for step in 0..25 {
        let _ = editor.update(Message::Control(ControlMessage::SliderMoved {
            action: action.clone(),
            parameter: x.clone(),
            value: f64::from(step),
        }));
        assert!(!editor.busy, "a drag never starts a request");
        assert!(
            !editor.status.starts_with("Running edit."),
            "a drag never runs the action: {}",
            editor.status
        );
    }
    let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
        action: action.clone(),
        parameter: x.clone(),
    }));
    assert!(editor.busy, "release submits exactly one request");
    assert!(
        editor.status.starts_with(&format!("Running edit.{action}")),
        "{}",
        editor.status
    );

    // A second release while the first request is still in flight sends nothing further.
    let busy_status = editor.status.clone();
    let _ = editor.update(Message::Control(ControlMessage::SliderReleased {
        action,
        parameter: x,
    }));
    assert_eq!(
        editor.status, busy_status,
        "already busy: no second request"
    );
    finish(editor, catalog);
}
