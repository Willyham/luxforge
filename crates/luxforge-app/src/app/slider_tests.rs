//! The slider gesture of a generated control, driven through the one core draft: what a drag, a
//! release, a double-click reset and Escape send, and every race a draft closes.
use super::{
    message::{
        control::ControlMessage, crop::CropMessage, draft::DraftMessage, history::HistoryMessage,
        sync::SyncMessage,
    },
    tasks::mutation,
    testing::{
        Z6_AS_SHOT, Z6_CAM_XYZ, attach_log, descriptors, drafting, entry, events, finish, logged,
        opened_with_modules, patch_control, raw_entry, raw_refresh, refresh_for,
    },
    *,
};
use crate::state::{fields, tools};
use luxforge_core::{AssetId, RawPayload, WhiteBalanceMode};
use serde_json::Map;
use std::collections::BTreeMap;

#[test]
fn perspective_integer_drag_is_one_draft_and_cancel_writes_nothing() {
    for parameter in ["horizontal", "vertical"] {
        let (mut editor, catalog, log, _, _, _) = drafting();
        let history = editor.document.history.entries.len();
        for value in [10.0, 25.0, 40.0, 40.0] {
            let _ = testing::slide(&mut editor, "set-perspective", parameter, value);
        }
        let records = logged(&mut editor, &log);
        assert_eq!(events(&records, "slider_draft_begin").len(), 1);
        let sent: Vec<_> = events(&records, "slider_draft_set")
            .iter()
            .map(|record| record["fields"].clone())
            .collect();
        assert_eq!(
            sent,
            [
                json!({parameter:10}),
                json!({parameter:25}),
                json!({parameter:40})
            ]
        );
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::Draft(DraftMessage::Cancel));
        assert!(events(&logged(&mut editor, &log), "slider_draft_commit").is_empty());
        assert!(editor.gesture.is_none());
        assert_eq!(editor.document.history.entries.len(), history);
        assert_eq!(editor.document.state.as_ref().unwrap().revision, 4);

        // A second release while the first commit is outstanding cannot submit another entry.
        let log = attach_log(&mut editor);
        let _ = testing::slide(&mut editor, "set-perspective", parameter, 40.0);
        let _ = testing::let_go(&mut editor, "set-perspective", parameter);
        let _ = testing::let_go(&mut editor, "set-perspective", parameter);
        assert_eq!(
            events(&logged(&mut editor, &log), "slider_draft_commit").len(),
            1
        );
        finish(editor, catalog);
    }
}

#[test]
fn releasing_or_cancelling_clears_the_displayed_draft_stamp_immediately() {
    for cancel in [false, true] {
        let (mut editor, catalog, _, _, action, parameter) = drafting();
        let _ = testing::slide(&mut editor, &action, &parameter, 1.0);
        let id = editor.session.draft.as_ref().unwrap().draft_id.clone();
        editor.presentation.displayed_draft_id = Some(id);
        editor.presentation.displayed_draft_revision = Some(1);
        if cancel {
            let _ = editor.discard();
        } else {
            let _ = editor.release();
        }
        assert_eq!(editor.presentation.displayed_draft_id, None);
        assert_eq!(editor.presentation.displayed_draft_revision, None);
        finish(editor, catalog);
    }
}

/// A drag of a patch action's slider opens exactly one draft and sends one `draft.set` per new
/// value, in the update that produced it: the first in the update of the press, since
/// `draft.begin` answers there too.
#[test]
fn a_drag_sends_one_draft_set_per_new_value_from_the_press_on() {
    let (mut editor, catalog, log, _, action, parameter) = drafting();

    // The values are ones the widget would send: it quantizes each drag to the parameter's
    // declared step and precision before the message is published.
    let _ = testing::slide(&mut editor, &action, &parameter, 25.0);
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "slider_draft_begin").len(),
        1,
        "a gesture opens one draft"
    );
    let sets = events(&records, "slider_draft_set");
    assert_eq!(
        sets.len(),
        1,
        "the press's own value goes out in the update that opened the draft: {sets:?}"
    );
    assert_eq!(
        sets[0]["fields"],
        json!({ parameter.clone(): 25.0 }),
        "as one field patch"
    );

    let log = attach_log(&mut editor);
    for value in [50.0, 75.0] {
        let _ = testing::slide(&mut editor, &action, &parameter, value);
    }
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some("75"),
        "the field follows the pointer"
    );
    assert_eq!(
        editor.controls.dragging,
        Some((action.clone(), parameter.clone()))
    );
    assert!(
        editor.status.text.starts_with("Drafting "),
        "the status bar names the gesture: {}",
        editor.status.text
    );
    // A move to the value already accepted sends nothing again.
    let _ = testing::slide(&mut editor, &action, &parameter, 75.0);

    let records = logged(&mut editor, &log);
    assert!(
        events(&records, "slider_draft_begin").is_empty(),
        "the open gesture's moves open no second draft"
    );
    let sets = events(&records, "slider_draft_set");
    let sent: Vec<&Value> = sets.iter().map(|set| &set["fields"]).collect();
    assert_eq!(
        sent,
        [
            &json!({ parameter.clone(): 50.0 }),
            &json!({ parameter.clone(): 75.0 })
        ],
        "one draft.set per new value, none for a repeat"
    );
    assert_eq!(
        editor
            .session
            .draft
            .as_ref()
            .map(|draft| draft.draft_revision),
        Some(3),
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
        .find(|action| {
            !action.patch
                && action.parameters.len() == 1
                && matches!(
                    action.parameters[0].kind,
                    luxforge_core::ParameterKind::Number { .. }
                )
        })
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
        matches!(declared.kind, luxforge_core::ParameterKind::Number { .. }),
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
    let (mut editor, catalog, log, _, _, _) = drafting();
    let (action, parameter) = single_parameter_control(&editor);

    for value in [0.25, 0.5] {
        let _ = testing::slide(&mut editor, &action, &parameter, value);
    }
    assert!(
        editor.slider_gesture().is_some(),
        "the gesture opened a draft: {}",
        editor.status.text
    );
    let _ = testing::slide(&mut editor, &action, &parameter, 0.75);
    let _ = testing::let_go(&mut editor, &action, &parameter);

    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "slider_draft_begin").len(),
        1,
        "one gesture opens one draft"
    );
    let sets = events(&records, "slider_draft_set");
    let sent: Vec<&Value> = sets.iter().map(|set| &set["fields"]).collect();
    assert_eq!(
        sent,
        [
            &json!({ parameter.clone(): 0.25 }),
            &json!({ parameter.clone(): 0.5 }),
            &json!({ parameter.clone(): 0.75 })
        ],
        "one draft.set per value, each carrying it as the whole request"
    );
    assert_eq!(
        events(&records, "slider_draft_preview").len(),
        3,
        "each accepted set queues the preview its value produces"
    );
    assert_eq!(
        events(&records, "slider_draft_commit").len(),
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
        // Crop's fields belong to its canvas draft, not the generic multi-field gesture.
        .filter(|module| {
            !matches!(
                module.canvas,
                Some(luxforge_core::CanvasInteraction::CropFrame { .. })
            )
        })
        .flat_map(|module| module.actions.iter())
        .find(|action| {
            !action.patch
                && action.parameters.len() > 1
                && matches!(
                    action.parameters[0].kind,
                    luxforge_core::ParameterKind::Integer { .. }
                        | luxforge_core::ParameterKind::Number { .. }
                )
        })
        .map(|action| (action.id.clone(), action.parameters[0].name.clone()))
        .expect("a built-in declares a multi-parameter action led by a slider field");

    for value in [3.0, 7.0] {
        let _ = testing::slide(&mut editor, &action, &parameter, value);
    }
    assert!(editor.slider_gesture().is_none(), "no draft was opened");
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some("7"),
        "the field still follows the pointer"
    );
    assert!(
        events(&logged(&mut editor, &log), "slider_draft_begin").is_empty(),
        "nothing was sent while dragging"
    );

    let _ = testing::let_go(&mut editor, &action, &parameter);
    assert_eq!(
        editor.status.text,
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
) -> luxforge_core::HistoryEntry {
    let current = editor
        .document
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    let _ = testing::slide(editor, action, parameter, value);
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
/// commit's answer and is sent once, against the revision it produced. Exposure and the RAW
/// variants of Temperature and Tint take the same path; what is sent is the field's own reset: As
/// shot for a RAW photo's Temperature and Tint, the declared default for Exposure.
#[test]
fn a_reset_during_a_gesture_commit_waits_and_names_the_revision_the_commit_produced() {
    let as_shot = json!({"white-balance": "as-shot"});
    let cases = [
        ("set-raw", "temperature", 5000.0, "set-raw", as_shot.clone()),
        ("set-raw", "tint", 12.0, "set-raw", as_shot),
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
        // Basic's Temperature and Tint are the RAW development's on a RAW photo's global target.
        if action != "set-basic" {
            editor
                .document
                .state
                .as_mut()
                .expect("an open asset")
                .asset
                .source = crate::state::testing::raw_source();
        }
        let revision = editor
            .document
            .state
            .as_ref()
            .expect("an open asset")
            .revision;
        let committed =
            double_click_before_the_commit_answers(&mut editor, &asset, action, parameter, value);
        let records = logged(&mut editor, &log);
        let queued = events(&records, "field_reset_queued");
        assert_eq!(queued.len(), 1, "{action}: the reset waits: {records:?}");
        assert_eq!(queued[0]["revision"], json!(revision));
        assert!(
            events(&records, "field_reset_sent").is_empty(),
            "{action}: nothing is sent against the revision the commit is replacing"
        );
        assert!(!editor.busy, "{action}: no request was started");
        assert!(editor.controls.pending_reset.is_some());

        // The commit answers with the next revision; the reset goes out in the same update.
        let log = attach_log(&mut editor);
        let refresh = refresh_for(&asset, &committed, Vec::new(), &[&committed], false);
        testing::answer_commit(&mut editor, Ok(Some(refresh)));
        let records = logged(&mut editor, &log);
        let sent = events(&records, "field_reset_sent");
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
        assert_eq!(editor.status.text, format!("Running edit.{reset}…"));
        assert!(editor.busy && editor.controls.pending_reset.is_none());
        finish(editor, catalog);
    }
}

/// A double-click on Temperature or Tint of a RAW photo's global target, with nothing in flight,
/// runs As shot at once — the very request an independent JSON client builds from the reset Basic's
/// RAW variant declares in `module.list` — and the field shows the authoritative value until the
/// answer brings the as-shot equivalent, never the 6504 K and 0 nothing set. Exposure and a JPEG's
/// Temperature, which declare no reset, still reset to their declared defaults.
#[test]
fn a_double_click_on_a_raw_white_balance_field_returns_to_as_shot() {
    let listed = serde_json::to_value(descriptors()).unwrap();
    let as_shot = json!({"action": "set-raw", "preset": {"white-balance": "as-shot"}});
    for (action, parameter) in [("set-raw", "temperature"), ("set-raw", "tint")] {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let log = attach_log(&mut editor);
        let asset = editor
            .document
            .state
            .as_ref()
            .expect("open")
            .asset
            .id
            .clone();
        let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
        let mut custom = original.clone();
        custom.wb_mode = WhiteBalanceMode::Custom;
        custom.temperature_kelvin = Some(5000.0);
        custom.tint = Some(12.0);
        custom.gains =
            luxforge_core::gains_from_temperature_tint(5000.0, 12.0, Z6_CAM_XYZ).unwrap();
        let current = raw_entry(&asset, 5, None, &custom);
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            raw_refresh(&asset, &current),
        )))));
        let committed = editor
            .controls
            .fields
            .get(action, parameter)
            .map(str::to_owned);
        assert_eq!(
            committed.as_deref(),
            Some(if parameter == "temperature" {
                "5000"
            } else {
                "12"
            })
        );

        // The JSON request a client builds from the reset the variant declares.
        let control = listed
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|module| module["controls"].as_array().cloned())
            .flatten()
            .flat_map(|group| group["controls"].as_array().cloned())
            .flatten()
            .flat_map(|control| control["variants"].as_array().cloned())
            .flatten()
            .map(|variant| variant["control"].clone())
            .find(|control| control["action"] == action && control["parameter"] == parameter)
            .expect("the listed variant");
        let reset = &control["reset"];
        assert_eq!(reset, &as_shot);
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
            "{parameter}: the desktop's reset request is the JSON client's"
        );

        // Typed but not committed, then double-clicked.
        editor.controls.fields.set(action, parameter, "7777".into());
        editor.controls.editing = Some((action.into(), parameter.into()));
        let _ = editor.update(Message::Control(ControlMessage::ResetField {
            action: action.into(),
            parameter: parameter.into(),
        }));
        let records = logged(&mut editor, &log);
        let sent = events(&records, "field_reset_sent");
        assert_eq!(sent.len(), 1, "{parameter}: {records:?}");
        assert_eq!(sent[0]["action"], as_shot["action"]);
        assert_eq!(sent[0]["preset"], as_shot["preset"]);
        assert_eq!(editor.status.text, "Running edit.set-raw…");
        assert!(editor.controls.editing.is_none());
        assert_eq!(
            editor
                .controls
                .fields
                .get(action, parameter)
                .map(str::to_owned),
            committed,
            "{parameter}: the field shows the committed value until the answer, not a default"
        );

        // The answer: As shot, whose rows report the camera's equivalent.
        let answered = raw_entry(&asset, 6, Some(&current.id), &original);
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            raw_refresh(&asset, &answered),
        )))));
        assert_eq!(
            editor.controls.fields.get(action, parameter),
            Some(if parameter == "temperature" {
                "4861"
            } else {
                "-50"
            }),
            "{parameter}: the as-shot equivalent"
        );
        finish(editor, catalog);
    }

    // Exposure and a JPEG's Temperature keep their declared defaults.
    for (action, parameter, preset) in [
        ("set-basic", "exposure", json!({"exposure": 0.0})),
        ("set-basic", "temperature", json!({"temperature": 0.0})),
    ] {
        let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
        let log = attach_log(&mut editor);
        let _ = editor.update(Message::Control(ControlMessage::ResetField {
            action: action.into(),
            parameter: parameter.into(),
        }));
        let records = logged(&mut editor, &log);
        let sent = events(&records, "field_reset_sent");
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
        .document
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
    assert!(
        editor.controls.pending_reset.is_some(),
        "it waits for the request"
    );
    let next = entry(&asset, current.sequence + 1, Some(&current.id));
    let refresh = refresh_for(&asset, &next, Vec::new(), &[&next], false);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    let sent = events(&logged(&mut editor, &log), "field_reset_sent")
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
    assert!(editor.controls.pending_reset.is_some());
    crate::state::testing::show(
        &mut editor.session,
        editor.document.state.as_ref(),
        luxforge_core::HistorySelection::Entry(current.id.clone()),
    );
    editor.busy = false;
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    let records = logged(&mut editor, &log);
    let dropped = events(&records, "field_reset_dropped");
    assert_eq!(dropped.len(), 1, "{records:?}");
    assert_eq!(dropped[0]["reason"], json!("a historical entry is shown"));
    assert!(editor.controls.pending_reset.is_none());
    assert!(events(&records, "field_reset_sent").is_empty());
    assert!(
        editor
            .status
            .text
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
    let (mut editor, catalog, log, _, _, _) = drafting();
    // A RAW photo's global target, where Basic's Temperature is the development's `set-raw`.
    editor
        .document
        .state
        .as_mut()
        .expect("an open asset")
        .asset
        .source = crate::state::testing::raw_source();
    // The owner accepts the value, but its preview job answers preparation-required.
    testing::stand_in(&mut editor)
        .sets
        .push_back("preparation-required: source-job-7".to_owned());
    let _ = testing::slide(&mut editor, "set-raw", "temperature", 5000.0);
    assert_eq!(
        editor.status.text,
        "Temperature cannot be previewed until the RAW development is ready; it shows on release"
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
    let unpreviewed = events(&records, "slider_draft_unpreviewed");
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
    for (action, parameter) in [("set-raw", "temperature"), ("set-basic", "exposure")] {
        let (mut editor, catalog, log, _, _, _) = drafting();
        let _ = testing::let_go(&mut editor, action, parameter);
        assert!(!editor.busy, "{action}: no request is in flight");
        assert_eq!(
            editor.gesture_refusal(crate::app::gesture::Starting::Action),
            None
        );
        assert!(
            !editor.status.text.starts_with("Running"),
            "{action}: {}",
            editor.status.text
        );
        assert!(events(&logged(&mut editor, &log), "slider_draft_begin").is_empty());
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

    editor
        .controls
        .fields
        .set(&action, &parameter, "1.25".to_owned());
    let _ = editor.update(Message::Control(ControlMessage::ResetField {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some(fields::seed_text(
            tools::declared_action(&editor.modules, &action)
                .and_then(|declared| declared.parameter(&parameter))
                .expect("the declared parameter")
        ))
        .as_deref(),
        "the field returns to its default"
    );
    assert_eq!(
        editor.status.text,
        format!("Running edit.{action}…"),
        "and the reset runs once as that action"
    );
    finish(editor, catalog);
}

/// A slider released while an unrelated request is in flight still commits: the pointer is up, so
/// the draft is never left open behind a refusal. Only the crop's deliberate Apply waits out
/// another request.
#[test]
fn a_slider_released_while_another_request_is_in_flight_commits() {
    let (mut editor, catalog, log, _, action, parameter) = drafting();
    let _ = testing::slide(&mut editor, &action, &parameter, 1.0);
    editor.busy = true;
    assert_eq!(
        editor.release_refusal(),
        None,
        "nothing refuses the release"
    );
    let _ = testing::let_go(&mut editor, &action, &parameter);
    assert_eq!(
        testing::core_draft(&editor).and_then(|draft| draft.in_flight()),
        Some(crate::app::draft::Round::Commit),
        "the release committed"
    );
    assert!(editor.controls.dragging.is_none(), "release ends the drag");
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "slider_draft_commit").len(), 1);
    testing::answer_commit(&mut editor, Ok(None));
    assert!(
        editor.slider_gesture().is_none(),
        "the draft does not stay open"
    );
    finish(editor, catalog);
}

/// Release commits exactly once, through `draft.commit` with the draft's own base revision.
/// A no-op outcome ends the gesture with no entry and no history refresh.
#[test]
fn release_commits_once_and_a_return_to_start_commits_nothing() {
    let (mut editor, catalog, log, _, action, parameter) = drafting();
    let history = editor.document.history.entries.len();
    let _ = testing::slide(&mut editor, &action, &parameter, 1.0);

    let _ = testing::let_go(&mut editor, &action, &parameter);
    let _ = testing::let_go(&mut editor, &action, &parameter);
    let records = logged(&mut editor, &log);
    let commits = events(&records, "slider_draft_commit");
    assert_eq!(commits.len(), 1, "one gesture is one commit: {commits:?}");
    assert_eq!(
        commits[0]["expected_revision"],
        json!(4),
        "the commit names the revision the draft was based on"
    );
    assert!(editor.controls.dragging.is_none(), "release ends the drag");

    // The gesture returned to its start: a no-op outcome, no entry, no history refresh.
    testing::answer_commit(&mut editor, Ok(None));
    assert!(editor.slider_gesture().is_none(), "the gesture is over");
    assert!(editor.session.draft.is_none(), "and so is the core draft");
    assert_eq!(
        editor.document.history.entries.len(),
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
    let (mut editor, catalog, log, _, action, parameter) = drafting();
    let default = editor
        .controls
        .fields
        .get(&action, &parameter)
        .expect("a seeded field")
        .to_owned();
    let _ = testing::slide(&mut editor, &action, &parameter, 2.0);

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
        matches!(
            escape,
            Some(Message::Draft(message::draft::DraftMessage::Cancel))
        ),
        "Escape discards an open slider gesture: {escape:?}"
    );
    let _ = editor.update(escape.expect("the mapped message"));

    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "slider_draft_cancelled").len(), 1);
    assert!(
        events(&records, "slider_draft_commit").is_empty(),
        "a cancelled gesture commits nothing"
    );
    assert!(editor.slider_gesture().is_none());
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
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
    let _ = testing::slide(&mut editor, &action, &parameter, 15.0);

    // Somebody else committed, which is also what this desktop's own undo looks like.
    let newer = entry(&asset, 9, None);
    let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(
        tasks::SyncResult::changed(refresh),
    ))));
    let draft = &editor.core_gesture().expect("the draft is kept").draft;
    assert!(draft.conflicted);
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
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
        events(&conflicts, "slider_draft_conflicted").len(),
        1,
        "the conflict is recorded once, with the revision that caused it"
    );
    let log2 = attach_log(&mut editor);
    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Commit));
    assert!(
        events(&logged(&mut editor, &log2), "slider_draft_commit").is_empty(),
        "a conflicted gesture refuses to commit"
    );
    assert!(editor.core_gesture().expect("kept").draft.conflicted);

    // Reapply rebases it on the new revision and re-sends the value this client set, in the
    // update of the press.
    let log3 = attach_log(&mut editor);
    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Reapply));
    let draft = &editor.core_gesture().expect("the rebased draft").draft;
    assert!(!draft.conflicted);
    assert_eq!(draft.base_revision, 9);
    let records = logged(&mut editor, &log3);
    let sets = events(&records, "slider_draft_set");
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
    let (mut editor, catalog, log, _, action, parameter) = drafting();
    let _ = testing::slide(&mut editor, &action, &parameter, 1.0);
    let sets = {
        let records = logged(&mut editor, &log);
        events(&records, "slider_draft_set")
            .first()
            .cloned()
            .cloned()
            .expect("one draft.set")
    };
    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Cancel));

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
        editor
            .status
            .text
            .starts_with(&format!("Running edit.{action}")),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// Every declared field of a patch action goes through one gesture path. The driver underneath is
/// field-agnostic, so each field is checked for what differs between fields, over the
/// descriptor's own parameters (no field is named here, and a new one is covered the day it is
/// declared): its first move drafts, sends exactly one `draft.set` naming that field alone and
/// shows the drafted value with the field's declared decimals. One field then goes through the
/// whole cycle: one commit on release, Escape cancelling without committing, and an external
/// commit surviving until Reapply clears it.
#[test]
fn every_patch_field_drafts_commits_cancels_and_reapplies_through_one_path() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (action, _) = patch_control(&editor);
    let declared = tools::declared_action(&editor.modules, &action)
        .expect("the declared patch action")
        .clone();
    let fields: Vec<(String, f64)> = declared
        .parameters
        .iter()
        .filter_map(|parameter| match parameter.kind {
            luxforge_core::ParameterKind::Number { min, max } => {
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
        let _ = testing::slide(&mut editor, &action, parameter, *value);
        assert!(
            editor.slider_gesture().is_some(),
            "{parameter} did not open a draft: {}",
            editor.status.text
        );
        let records = logged(&mut editor, &log);
        let sets = events(&records, "slider_draft_set");
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
            editor.controls.fields.get(&action, parameter),
            Some(shown.as_str()),
            "{parameter} shows the drafted value, with its declared decimals"
        );
        let _ = editor.update(Message::Draft(message::draft::DraftMessage::Cancel));
        assert!(
            editor.gesture.is_none(),
            "{parameter} left its draft closing"
        );
    }

    // The cycle, once, for the first field: the driver it goes through is the same for every one.
    let (parameter, value) = &fields[0];

    // The release: one commit, then the no-op outcome that ends the gesture.
    editor.busy = false;
    let _ = testing::slide(&mut editor, &action, parameter, *value);
    let log = attach_log(&mut editor);
    let _ = testing::let_go(&mut editor, &action, parameter);
    let records = logged(&mut editor, &log);
    assert_eq!(
        events(&records, "slider_draft_commit").len(),
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
    let _ = testing::slide(&mut editor, &action, parameter, *value);
    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Cancel));
    let records = logged(&mut editor, &log);
    assert!(
        events(&records, "slider_draft_commit").is_empty(),
        "{parameter} committed on Escape"
    );
    assert!(
        !events(&records, "slider_draft_cancelled").is_empty(),
        "{parameter} did not cancel"
    );
    assert!(
        editor.slider_gesture().is_none(),
        "{parameter} kept its draft"
    );
    // The slot is free once the owner has ended the draft.
    assert!(
        editor.gesture.is_none(),
        "{parameter} left its draft closing"
    );

    // An external commit under the gesture, then Reapply.
    editor.busy = false;
    let _ = testing::slide(&mut editor, &action, parameter, *value);
    let state = editor.document.state.as_mut().expect("open");
    state.revision += 1;
    let newer = state.revision;
    editor.gesture_revision(newer);
    assert!(
        editor
            .core_gesture()
            .is_some_and(|gesture| gesture.draft.conflicted),
        "{parameter} was not marked conflicted"
    );
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Reapply));
    let rebased = &editor.core_gesture().expect("the rebased draft").draft;
    assert!(!rebased.conflicted, "{parameter} stayed conflicted");
    assert_eq!(
        rebased.base_revision, newer,
        "{parameter} was not rebased on the new revision"
    );
    assert_eq!(
        rebased.sent(),
        Some(&json!({ parameter.clone(): value })),
        "{parameter} did not re-send the value this client set"
    );
    assert_eq!(
        events(&logged(&mut editor, &log), "slider_draft_set").len(),
        1,
        "{parameter}: the reapply re-sent it once"
    );
    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Cancel));
    assert!(editor.gesture.is_none());
    finish(editor, catalog);
}

/// A drag changes the section whose action it drafts and leaves every other section exactly as it
/// was.
#[test]
fn a_drag_changes_only_the_drafting_modules_section() {
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
    let sections = |editor: &Editor| -> BTreeMap<String, crate::state::tools::SectionModel> {
        editor
            .workspace
            .tools
            .all()
            .map(|section| (section.module_id.clone(), section.clone()))
            .collect()
    };
    // The preset library disables its rows while any draft is open, so opening the gesture
    // changes that section once while it is expanded (a collapsed one builds no controls, so
    // nothing in it can follow); nothing else outside the drafting module moves.
    let library = crate::state::presets::presets_control(&editor.modules)
        .map(|(module, _)| module.id.clone())
        .expect("the presets control");
    editor.controls.expanded.insert(library.clone(), true);
    editor.rederive();
    let before = sections(&editor);
    assert!(before.len() > 1, "more than one section is on screen");

    let _ = testing::slide(&mut editor, &action, &parameter, 0.5);
    let opened = sections(&editor);
    for (module, section) in &before {
        if module == &owner || module == &library {
            assert_ne!(&opened[module], section, "{module} follows the gesture");
        } else {
            assert_eq!(
                &opened[module], section,
                "{module} was changed by a drag in another module"
            );
        }
    }

    let _ = testing::slide(&mut editor, &action, &parameter, 0.7);
    let after = sections(&editor);
    for (module, section) in &opened {
        if module == &owner {
            assert_ne!(&after[module], section, "{module} follows its own field");
        } else {
            assert_eq!(
                &after[module], section,
                "{module} was changed by a move in another module"
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
    crate::app::testing::open_crop(&mut editor);
    let _ = testing::slide(&mut editor, &action, &parameter, 1.0);
    assert!(editor.slider_gesture().is_none(), "{}", editor.status.text);
    assert!(
        editor.status.text.contains("crop draft"),
        "{}",
        editor.status.text
    );
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));

    // The crop mode and Compare while a gesture is open.
    editor.busy = false;
    let _ = testing::slide(&mut editor, &action, &parameter, 1.0);
    assert!(editor.slider_gesture().is_some());
    let _ = editor.update(Message::View(ViewMessage::SetMode(crop)));
    assert!(editor.crop_gesture().is_none());
    assert!(
        editor.status.text.contains("slider draft"),
        "{}",
        editor.status.text
    );
    editor.document.original_entry = Some(entry(&asset, 0, None).id);
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert!(editor.document.compare_return.is_none());
    assert!(
        editor.status.text.contains("slider draft"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

#[test]
fn a_slider_drag_changes_the_field_and_sends_no_request() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (action, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
    let (action, x) = (action.to_owned(), x.to_owned());
    let sequence = editor.sync.sequence;
    let _ = testing::slide(&mut editor, &action, &x, 12.0);
    assert_eq!(editor.controls.fields.get(&action, &x), Some("12"));
    assert_eq!(editor.controls.dragging, Some((action.clone(), x.clone())));
    assert_eq!(editor.sync.sequence, sequence, "a drag calls nothing");
    let _ = testing::let_go(&mut editor, &action, &x);
    assert!(editor.controls.dragging.is_none(), "release ends the drag");
    finish(editor, catalog);
}

#[test]
fn a_slider_drag_of_many_moves_and_one_release_sends_exactly_one_request() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (action, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
    let (action, x) = (action.to_owned(), x.to_owned());

    for step in 0..25 {
        let _ = testing::slide(&mut editor, &action, &x, f64::from(step));
        assert!(!editor.busy, "a drag never starts a request");
        assert!(
            !editor.status.text.starts_with("Running edit."),
            "a drag never runs the action: {}",
            editor.status.text
        );
    }
    let _ = testing::let_go(&mut editor, &action, &x);
    assert!(editor.busy, "release submits exactly one request");
    assert!(
        editor
            .status
            .text
            .starts_with(&format!("Running edit.{action}")),
        "{}",
        editor.status.text
    );

    // A second release while the first request is still in flight sends nothing further, and the
    // status bar says why.
    let sequence = editor.sync.sequence;
    let _ = testing::let_go(&mut editor, &action, &x);
    assert_eq!(
        editor.sync.sequence, sequence,
        "already busy: no second request"
    );
    assert_eq!(editor.status.text, crate::state::IN_FLIGHT);
    finish(editor, catalog);
}
