//! Behavioral checks for generated gestures, independent of a production module's identity.
use super::{
    Editor,
    message::{
        Message, action::ActionMessage, control::ControlMessage, draft::DraftMessage,
        history::HistoryMessage, preview::PreviewMessage, sync::SyncMessage, view::ViewMessage,
    },
    tasks,
    testing::{self, *},
};
use crate::state::palette::PaletteAction;
use crate::state::{fields, tools};
use luxforge_core::{AssetId, HistorySelection, POINTER_MODE, RawPayload, WhiteBalanceMode};
use luxforge_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

const ACTION: &str = "fixture-set";

fn editor() -> (Editor, PathBuf, AssetId) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(vec![
        controls_descriptor(),
    ]))));
    // The owner does not hold this photograph, so its draft requests are answered by the stand-in.
    stand_in(&mut editor);
    let asset = AssetId::new();
    let current = entry(&asset, 4, None);
    let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    (editor, catalog, asset)
}

fn field_request(editor: &mut Editor, parameter: &str) -> Value {
    let request = editor.request_for(ACTION, Some(parameter)).unwrap();
    assert_eq!(request["method"], "edit.fixture-set");
    assert_eq!(request["params"]["mutation"]["expected_revision"], 4);
    assert_eq!(
        request["params"].as_object().unwrap().len(),
        3,
        "only asset, mutation and one field"
    );
    request["params"][parameter].clone()
}

#[test]
fn discrete_controls_submit_one_typed_field_without_a_draft() {
    for (parameter, value) in [("enabled", json!(true)), ("mode", json!("two"))] {
        let (mut editor, catalog, _) = editor();
        let _ = editor.update(Message::Control(ControlMessage::Discrete {
            action: ACTION.into(),
            parameter: parameter.into(),
            value: value.clone(),
        }));
        assert!(editor.busy, "selection starts one ordinary mutation");
        assert!(editor.slider_gesture().is_none());
        assert_eq!(field_request(&mut editor, parameter), value);
        finish(editor, catalog);
    }
}

#[test]
fn typing_waits_for_enter_and_invalid_text_commits_nothing() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::Control(ControlMessage::Field {
        action: ACTION.into(),
        parameter: "count".into(),
        text: "7".into(),
    }));
    assert!(!editor.busy, "typing is not a commit");
    let _ = editor.update(Message::Control(ControlMessage::Field {
        action: ACTION.into(),
        parameter: "count".into(),
        text: "bad".into(),
    }));
    let _ = editor.update(Message::Control(ControlMessage::Submit {
        action: ACTION.into(),
        parameter: Some("count".into()),
    }));
    assert!(!editor.busy);
    assert_eq!(editor.controls.fields.get(ACTION, "count"), Some("bad"));
    assert!(editor.controls.editing.is_some());
    let _ = editor.update(Message::Control(ControlMessage::Field {
        action: ACTION.into(),
        parameter: "count".into(),
        text: "7".into(),
    }));
    let _ = editor.update(Message::Control(ControlMessage::Submit {
        action: ACTION.into(),
        parameter: Some("count".into()),
    }));
    assert!(editor.busy);
    assert!(editor.slider_gesture().is_none());
    assert_eq!(field_request(&mut editor, "count"), json!(7));
    finish(editor, catalog);
}

#[test]
fn slider_fractions_use_soft_bounds_and_preserve_fine_step() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::Control(ControlMessage::Fraction {
        action: ACTION.into(),
        parameter: "amount".into(),
        fraction: 0.501,
    }));
    let value = field_request(&mut editor, "amount").as_f64().unwrap();
    assert!(
        (value - 0.01).abs() < 1e-9,
        "fine step must not round back to zero: {value}"
    );
    assert!(editor.slider_gesture().is_some());
    finish(editor, catalog);
}

#[test]
fn picker_and_curve_share_bounded_draft_and_commit_once() {
    for (parameter, message, expected) in [
        (
            "rgb",
            Message::Control(ControlMessage::Picker {
                action: ACTION.into(),
                parameter: "rgb".into(),
                event: ColorPickerEvent::Hue(0.0),
            }),
            json!([128, 32, 32]),
        ),
        (
            "master",
            Message::Control(ControlMessage::Curve {
                action: ACTION.into(),
                parameter: "master".into(),
                event: CurveEditorEvent::Move {
                    index: 1,
                    position: [0.5, 0.75],
                },
            }),
            json!([[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]]),
        ),
    ] {
        let (mut editor, catalog, _) = editor();
        let log = attach_log(&mut editor);
        for _ in 0..3 {
            let _ = editor.update(message.clone());
        }
        assert_eq!(field_request(&mut editor, parameter), expected);
        for _ in 0..2 {
            let _ = editor.update(Message::Control(ControlMessage::Released {
                action: ACTION.into(),
                parameter: parameter.into(),
            }));
        }
        let records = logged(&mut editor, &log);
        let events = |name: &str| {
            records
                .iter()
                .filter(|record| record["event"] == name)
                .collect::<Vec<_>>()
        };
        assert_eq!(events("slider_draft_begin").len(), 1);
        let sets = events("slider_draft_set");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0]["detail"]["fields"], json!({parameter:expected}));
        assert_eq!(events("slider_draft_commit").len(), 1);
        finish(editor, catalog);
    }
}

#[test]
fn picker_remembers_unrepresentable_gray_hue_without_a_noop_commit() {
    let (mut editor, catalog, _) = editor();
    editor.set_control_field_value(ACTION, "rgb", &json!([128, 128, 128]));
    let hue = 0.67_f32;
    let _ = editor.update(Message::Control(ControlMessage::Picker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Hue(hue),
    }));
    assert_eq!(
        editor.control_field_value(ACTION, "rgb"),
        Some(json!([128, 128, 128]))
    );
    assert!(editor.slider_gesture().is_none());
    let _ = editor.update(Message::Control(ControlMessage::Picker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Release,
    }));
    assert!(!editor.busy, "a hue-only gray gesture changes no RGB field");
    let _ = editor.update(Message::Control(ControlMessage::Picker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Plane([1.0, 0.5]),
    }));
    let expected = luxforge_ui::hsv_to_rgb([f64::from(hue), 1.0, 0.5]);
    assert_eq!(field_request(&mut editor, "rgb"), json!(expected));
    assert!(editor.slider_gesture().is_some());
    finish(editor, catalog);
}

#[test]
fn picker_rejects_cached_hsv_when_the_rgb_field_changes_elsewhere() {
    let (mut editor, catalog, _) = editor();
    editor.set_control_field_value(ACTION, "rgb", &json!([0, 0, 0]));
    let _ = editor.update(Message::Control(ControlMessage::Picker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Hue(0.67),
    }));
    editor.set_control_field_value(ACTION, "rgb", &json!([0, 255, 0]));
    let _ = editor.update(Message::Control(ControlMessage::Picker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Plane([1.0, 0.5]),
    }));
    assert_eq!(field_request(&mut editor, "rgb"), json!([0, 128, 0]));
    finish(editor, catalog);
}

#[test]
fn picker_keeps_black_saturation_until_value_becomes_visible() {
    let (mut editor, catalog, _) = editor();
    editor.set_control_field_value(ACTION, "rgb", &json!([0, 0, 0]));
    for event in [
        ColorPickerEvent::Plane([1.0, 0.0]),
        ColorPickerEvent::Hue(0.5),
    ] {
        let _ = editor.update(Message::Control(ControlMessage::Picker {
            action: ACTION.into(),
            parameter: "rgb".into(),
            event,
        }));
    }
    assert!(editor.slider_gesture().is_none());
    let _ = editor.update(Message::Control(ControlMessage::Picker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Plane([1.0, 0.5]),
    }));
    assert_eq!(field_request(&mut editor, "rgb"), json!([0, 128, 128]));
    finish(editor, catalog);
}

#[test]
fn curve_channel_selection_changes_no_request_value_or_recipe() {
    let (mut editor, catalog, _) = editor();
    let fields = editor.controls.fields.clone();
    let _ = editor.update(Message::Control(ControlMessage::Curve {
        action: ACTION.into(),
        parameter: "master".into(),
        event: CurveEditorEvent::Channel(1),
    }));
    assert_eq!(editor.controls.fields, fields);
    assert!(editor.slider_gesture().is_none());
    assert!(!editor.busy);
    assert_eq!(editor.document.state.as_ref().unwrap().revision, 4);
    finish(editor, catalog);
}

#[test]
fn escape_cancels_picker_and_curve_and_commits_nothing() {
    for message in [
        Message::Control(ControlMessage::Picker {
            action: ACTION.into(),
            parameter: "rgb".into(),
            event: ColorPickerEvent::Hue(0.0),
        }),
        Message::Control(ControlMessage::Curve {
            action: ACTION.into(),
            parameter: "master".into(),
            event: CurveEditorEvent::Move {
                index: 1,
                position: [0.5, 0.75],
            },
        }),
    ] {
        let (mut editor, catalog, _) = editor();
        let log = attach_log(&mut editor);
        let _ = editor.update(message);
        assert!(editor.slider_gesture().is_some(), "{}", editor.status.text);
        let _ = editor.update(Message::Draft(DraftMessage::Cancel));
        assert!(
            editor.gesture.is_none(),
            "Discard ends the gesture and its draft in the same update"
        );
        let records = logged(&mut editor, &log);
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "slider_draft_commit")
        );
        assert!(
            records
                .iter()
                .any(|record| record["event"] == "slider_draft_cancelled")
        );
        assert_eq!(editor.document.state.as_ref().unwrap().revision, 4);
        finish(editor, catalog);
    }
}

#[test]
fn stepper_button_is_one_complete_draft_gesture() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::Control(ControlMessage::Step {
        action: ACTION.into(),
        parameter: "count".into(),
        direction: 1,
    }));
    assert_eq!(field_request(&mut editor, "count"), json!(3));
    assert_eq!(
        core_draft(&editor).and_then(|draft| draft.in_flight()),
        Some(super::draft::Round::Commit),
        "the step's release commits in the update that opened the draft"
    );
    finish(editor, catalog);
}

#[test]
fn field_arrow_nudge_stays_local_until_enter() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::Control(ControlMessage::FieldNudge {
        action: ACTION.into(),
        parameter: "coordinate".into(),
        direction: 1,
        shift: false,
        option: true,
    }));
    assert!(!editor.busy);
    assert!(editor.slider_gesture().is_none());
    assert_eq!(
        editor.controls.editing,
        Some((ACTION.into(), "coordinate".into()))
    );
    assert!((field_request(&mut editor, "coordinate").as_f64().unwrap() - 5.1).abs() < 1e-9);
    let _ = editor.update(Message::Control(ControlMessage::Submit {
        action: ACTION.into(),
        parameter: Some("coordinate".into()),
    }));
    assert!(editor.busy);
    assert!(editor.slider_gesture().is_none());
    finish(editor, catalog);
}

#[test]
fn action_copy_uses_the_clicked_controls_preset() {
    let (mut editor, catalog, _) = editor();
    for preset in [json!({"amount": 1.0}), json!({"enabled": true})] {
        let request = editor
            .request_for_preset(ACTION, None, preset.as_object())
            .unwrap();
        let mut params = request["params"].as_object().unwrap().clone();
        assert_eq!(params.remove("mutation").unwrap()["expected_revision"], 4);
        params.remove("asset_id").unwrap();
        assert_eq!(Value::Object(params), preset);
    }
    finish(editor, catalog);
}

/// A discrete control commits at once, so under an open gesture it is refused as a slider or a
/// preset started over it is: a toggle, an action button and a field's Enter each send nothing,
/// leave the gesture as it was, and put what the gesture needs in the status bar. The toggle does
/// not show a value that was never sent, and the typed text stays in its field.
#[test]
fn a_discrete_control_is_refused_while_a_gesture_is_open() {
    const REASON: &str = "Finish or discard the slider draft before running another edit";
    let (mut editor, catalog, _) = editor();
    hold_slider(&mut editor, ACTION, "amount");
    let held = editor.gesture.clone().map(|gesture| format!("{gesture:?}"));
    let unchanged = |editor: &Editor, case: &str| {
        assert!(!editor.busy, "{case}: nothing was sent");
        assert_eq!(editor.status.text, REASON, "{case}");
        assert_eq!(
            editor.gesture.clone().map(|gesture| format!("{gesture:?}")),
            held,
            "{case}: the gesture is untouched"
        );
    };

    let before = editor
        .controls
        .fields
        .get(ACTION, "enabled")
        .map(str::to_owned);
    let _ = editor.update(Message::Control(ControlMessage::Discrete {
        action: ACTION.into(),
        parameter: "enabled".into(),
        value: json!(true),
    }));
    unchanged(&editor, "a toggle");
    assert_eq!(
        editor
            .controls
            .fields
            .get(ACTION, "enabled")
            .map(str::to_owned),
        before,
        "the toggle shows the committed value"
    );

    editor.status.text.clear();
    let _ = editor.update(Message::Action(ActionMessage::Run {
        action: ACTION.into(),
        preset: serde_json::Map::from_iter([("amount".to_owned(), json!(0.0))]),
    }));
    unchanged(&editor, "an action button");

    editor.status.text.clear();
    let _ = editor.update(Message::Control(ControlMessage::Field {
        action: ACTION.into(),
        parameter: "count".into(),
        text: "7".into(),
    }));
    let _ = editor.update(Message::Control(ControlMessage::Submit {
        action: ACTION.into(),
        parameter: Some("count".into()),
    }));
    unchanged(&editor, "a field's Enter");
    assert_eq!(editor.controls.fields.get(ACTION, "count"), Some("7"));
    assert!(
        editor.controls.editing.is_some(),
        "the field keeps its typed text"
    );

    // With the gesture gone, the same button runs.
    editor.gesture = None;
    let _ = editor.update(Message::Action(ActionMessage::Run {
        action: ACTION.into(),
        preset: serde_json::Map::from_iter([("amount".to_owned(), json!(0.0))]),
    }));
    assert!(editor.busy, "{}", editor.status.text);
    finish(editor, catalog);
}

/// Every way a generated control starts — Enter, a discrete value, a rail, a step, the picker, the
/// curve, a field reset and an action button — asks the one refusal and writes its reason when
/// refused, sending nothing and leaving the field as it was. Each used to return without a word
/// when the photograph was not editable.
#[test]
fn every_refused_control_start_says_why() {
    let (mut editor, catalog, asset) = editor();
    let fraction = |parameter: &str| {
        Message::Control(ControlMessage::Fraction {
            action: ACTION.into(),
            parameter: parameter.into(),
            fraction: 0.9,
        })
    };
    let starts = [
        (
            "Enter",
            Message::Control(ControlMessage::Submit {
                action: ACTION.into(),
                parameter: Some("coordinate".into()),
            }),
        ),
        (
            "a toggle",
            Message::Control(ControlMessage::Discrete {
                action: ACTION.into(),
                parameter: "enabled".into(),
                value: json!(true),
            }),
        ),
        ("a rail", fraction("amount")),
        (
            "a step",
            Message::Control(ControlMessage::Step {
                action: ACTION.into(),
                parameter: "count".into(),
                direction: 1,
            }),
        ),
        (
            "the picker",
            Message::Control(ControlMessage::Picker {
                action: ACTION.into(),
                parameter: "rgb".into(),
                event: ColorPickerEvent::Hue(0.5),
            }),
        ),
        (
            "the curve",
            Message::Control(ControlMessage::Curve {
                action: ACTION.into(),
                parameter: "master".into(),
                event: CurveEditorEvent::Move {
                    index: 1,
                    position: [0.5, 0.75],
                },
            }),
        ),
        (
            "a field reset",
            Message::Control(ControlMessage::ResetField {
                action: ACTION.into(),
                parameter: "amount".into(),
            }),
        ),
        (
            "an action button",
            Message::Action(ActionMessage::Run {
                action: ACTION.into(),
                preset: Map::from_iter([("amount".to_owned(), json!(0.0))]),
            }),
        ),
    ];
    let refused = |editor: &mut Editor, name: &str, message: Message, reason: &str| {
        editor.set_control_field_value(ACTION, "amount", &json!(3.0));
        let fields = editor.controls.fields.clone();
        let (sequence, busy) = (editor.sync.sequence, editor.busy);
        editor.status.text.clear();
        let task = editor.update(message);
        assert_eq!(editor.status.text, reason, "{name}");
        assert_eq!(task.units(), 0, "{name}: nothing is sent");
        assert_eq!(editor.sync.sequence, sequence, "{name}: nothing is called");
        assert_eq!(editor.busy, busy, "{name}");
        assert!(editor.gesture.is_none(), "{name}: no draft opens");
        assert_eq!(
            editor.controls.fields, fields,
            "{name}: the field is as it was"
        );
        assert!(
            editor.controls.pending_reset.is_none(),
            "{name}: nothing waits"
        );
    };

    // A previewed history entry refuses every one of them.
    crate::state::testing::show(
        &mut editor.session,
        editor.document.state.as_ref(),
        HistorySelection::Entry(entry(&asset, 2, None).id),
    );
    for (name, message) in starts.clone() {
        refused(&mut editor, name, message, crate::state::NOT_CURRENT);
    }

    // A request in flight refuses what commits at once; a drafting control's gesture goes ahead
    // and its own round trips wait their turn, and a field reset waits for the request.
    crate::state::testing::show(
        &mut editor.session,
        editor.document.state.as_ref(),
        HistorySelection::Current,
    );
    editor.busy = true;
    for (name, message) in starts {
        if matches!(
            name,
            "a rail" | "a step" | "the picker" | "the curve" | "a field reset"
        ) {
            continue;
        }
        refused(&mut editor, name, message, crate::state::IN_FLIGHT);
    }
    let _ = editor.update(fraction("amount"));
    assert!(editor.slider_gesture().is_some(), "{}", editor.status.text);
    finish(editor, catalog);
}

/// `busy` belongs to the request that set it. A gesture's commit and the committed frame read back
/// after a gesture never set it, so their answers landing while an unrelated request is in flight
/// (an import, a restore) leave its controls disabled; a history selection's own answer is what
/// clears the flag the selection set.
#[test]
fn a_gesture_answer_leaves_busy_to_the_request_that_set_it() {
    let (mut editor, catalog, asset) = editor();
    hold_slider(&mut editor, ACTION, "amount");
    let _ = editor.update(Message::Draft(DraftMessage::Commit));
    assert_eq!(
        core_draft(&editor).and_then(|draft| draft.in_flight()),
        Some(super::draft::Round::Commit)
    );

    editor.busy = true;
    let current = editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .clone();
    let next = entry(&asset, 5, Some(&current.id));
    let committed = refresh_for(&asset, &next, Vec::new(), &[&next], false);
    let job = committed.job.clone();
    answer_commit(&mut editor, Ok(Some(committed)));
    assert!(editor.gesture.is_none(), "the gesture committed");
    assert_eq!(editor.document.state.as_ref().unwrap().revision, 5);
    assert!(editor.busy, "the commit's answer leaves busy set");

    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        super::tasks::PreviewPayload {
            job,
            session: editor.session.clone(),
        },
    )))));
    assert!(editor.busy, "and so does a frame read back after a gesture");

    let _ = editor.update(Message::History(HistoryMessage::Selected(Err(
        "cancelled".into()
    ))));
    assert!(!editor.busy, "a selection's own answer clears it");
    finish(editor, catalog);
}

/// A module's only group is drawn without a header, so its disclosure message records nothing,
/// while a group of a module with several still toggles.
#[test]
fn toggling_a_modules_only_group_records_nothing() {
    let (mut editor, catalog, _, _, _, _) = drafting();
    let _ = editor.update(Message::Control(ControlMessage::ToggleGroup {
        module_id: "luxforge.presence".into(),
        path: vec![0],
    }));
    assert!(
        editor.controls.ui.group_expanded.is_empty(),
        "the only group has no disclosure"
    );
    let _ = editor.update(Message::Control(ControlMessage::ToggleGroup {
        module_id: "luxforge.basic".into(),
        path: vec![1],
    }));
    assert_eq!(
        editor
            .controls
            .ui
            .group_expanded
            .get(&tools::group_key("luxforge.basic", &[1])),
        Some(&false)
    );
    finish(editor, catalog);
}

/// Generated fields follow the displayed entry's values for the module's one layer, except the
/// one being typed or dragged; a module whose layer is gone shows its defaults again, and a
/// module that reports no values keeps whatever was typed into it.
#[test]
fn fields_are_seeded_from_the_displayed_entrys_values() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (action, parameter) = patch_control(&editor);
    let (pick, x, _) = tools::point_pick(&editor.modules).expect("a canvas pick");
    let (pick, x) = (pick.to_owned(), x.to_owned());
    editor.controls.fields.set(&pick, &x, "42".into());
    let asset = editor
        .document
        .state
        .as_ref()
        .expect("open")
        .asset
        .id
        .clone();
    let module = editor
        .modules
        .iter()
        .find(|module| module.action(&action).is_some())
        .expect("the declaring module")
        .id
        .clone();

    let seeded = |editor: &mut Editor, values: Option<Value>| {
        let current = editor
            .document
            .state
            .as_ref()
            .expect("open")
            .current_entry
            .clone();
        let mut refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
        refresh.recipe.layers = values
            .into_iter()
            .map(|values| luxforge_core::LayerDescription {
                id: luxforge_core::LayerId::new(),
                effect: "test.effect".into(),
                module: Some(module.clone()),
                title: Some("Test".into()),
                summary: "Test".into(),
                values: values.as_object().cloned().unwrap_or_default(),
                available: true,
                mask: None,
                artifacts: Vec::new(),
                neutral: false,
                input_stage: None,
                input_orientation: None,
            })
            .collect();
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    };

    seeded(&mut editor, Some(json!({ parameter.clone(): -25.0 })));
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some("-25"),
        "the slider shows the authoritative value of the module's one layer"
    );
    assert_eq!(
        editor.controls.fields.get(&pick, &x),
        Some("42"),
        "a module that reports no values keeps what was typed into it"
    );

    // A field being dragged is not overwritten by the refresh that arrives under it.
    editor.controls.dragging = Some((action.clone(), parameter.clone()));
    editor.controls.fields.set(&action, &parameter, "3".into());
    seeded(&mut editor, Some(json!({ parameter.clone(): -25.0 })));
    assert_eq!(editor.controls.fields.get(&action, &parameter), Some("3"));
    editor.controls.dragging = None;

    // The same for a field being typed.
    editor.controls.editing = Some((action.clone(), parameter.clone()));
    editor
        .controls
        .fields
        .set(&action, &parameter, "2.5".into());
    seeded(&mut editor, Some(json!({ parameter.clone(): -25.0 })));
    assert_eq!(editor.controls.fields.get(&action, &parameter), Some("2.5"));
    editor.controls.editing = None;

    // The layer is gone: the fields it reported values for show their declared defaults.
    seeded(&mut editor, None);
    let default = tools::declared_action(&editor.modules, &action)
        .and_then(|declared| declared.parameter(&parameter))
        .map(fields::seed_text)
        .expect("a declared default");
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some(default.as_str())
    );
    assert_eq!(editor.controls.fields.get(&pick, &x), Some("42"));
    finish(editor, catalog);
}

/// Invalid text in a generated field shows the declared range and commits nothing, for every
/// field of the patch action.
#[test]
fn invalid_text_shows_the_declared_range_and_commits_nothing() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (action, _) = patch_control(&editor);
    let declared = tools::declared_action(&editor.modules, &action)
        .expect("the declared patch action")
        .clone();
    for parameter in &declared.parameters {
        let luxforge_core::ParameterKind::Number { min, max } = parameter.kind else {
            continue;
        };
        let outside = crate::state::number::number_text(max + 150.0);
        editor.busy = false;
        let _ = editor.update(Message::Control(ControlMessage::EditValue {
            action: action.clone(),
            parameter: parameter.name.clone(),
        }));
        let _ = editor.update(Message::Control(ControlMessage::Field {
            action: action.clone(),
            parameter: parameter.name.clone(),
            text: outside.clone(),
        }));
        let _ = editor.update(Message::Control(ControlMessage::Submit {
            action: action.clone(),
            parameter: Some(parameter.name.clone()),
        }));
        assert!(
            !editor.busy,
            "{} committed an out-of-range value",
            parameter.name
        );
        assert!(
            editor
                .status
                .text
                .contains(&crate::state::number::number_text(min))
                && editor
                    .status
                    .text
                    .contains(&crate::state::number::number_text(max)),
            "{} does not report its declared range: {}",
            parameter.name,
            editor.status.text
        );
        assert_eq!(
            editor.controls.fields.get(&action, &parameter.name),
            Some(outside.as_str()),
            "{} did not stay editable",
            parameter.name
        );
        // The panel shows the same range under the field rather than a silent correction.
        editor.rederive();
        let slider = editor
            .workspace
            .tools
            .all()
            .flat_map(|section| section.controls.iter())
            .flat_map(flatten)
            .find(|slider| slider.action == action && slider.parameter == parameter.name)
            .expect("the generated slider");
        assert!(
            slider.invalid.is_some(),
            "{} is not shown as invalid",
            parameter.name
        );
        let _ = editor.update(Message::Control(ControlMessage::ResetField {
            action: action.clone(),
            parameter: parameter.name.clone(),
        }));
    }
    finish(editor, catalog);
}

/// A group's reset button submits exactly that group's own fields at their declared defaults,
/// as one patch through the declared action; the module's header reset runs the module's own
/// declared reset action; and a double-click on one label submits that field alone.
#[test]
fn group_module_and_field_resets_each_run_one_declared_action() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let modules = editor.modules.clone();
    let mut groups = 0usize;
    // The product modules' groups hold number controls and, for the Tone curve, one curve control
    // whose channels are its fields; the controls proof's one group holds every control kind, and
    // `proof_controls_tests` proves its reset.
    for module in modules.iter().filter(|module| !module.developer) {
        for (index, control) in module.controls.iter().enumerate() {
            let luxforge_core::Control::Group(luxforge_core::GroupControl {
                label,
                controls,
                reset: Some(reset),
                ..
            }) = control
            else {
                continue;
            };
            let declared = tools::declared_action(&editor.modules, &reset.action)
                .expect("a declared reset action")
                .clone();
            if !declared.patch {
                continue;
            }
            groups += 1;
            let mut named: Vec<&str> = reset.preset.keys().map(String::as_str).collect();
            named.sort_unstable();
            let mut own: Vec<&str> = controls
                .iter()
                .flat_map(|control| match control {
                    luxforge_core::Control::Number(luxforge_core::NumberControl {
                        parameter,
                        ..
                    }) => vec![parameter.as_str()],
                    luxforge_core::Control::Curve(luxforge_core::CurveControl {
                        channels, ..
                    }) => channels
                        .iter()
                        .map(|channel| channel.parameter.as_str())
                        .collect(),
                    _ => Vec::new(),
                })
                .collect();
            own.sort_unstable();
            assert_eq!(named, own, "{label} resets exactly its own fields");
            for (name, value) in &reset.preset {
                assert_eq!(
                    Some(value),
                    declared
                        .parameter(name)
                        .and_then(|parameter| parameter.default.as_ref()),
                    "{label}: {name} is not reset to its declared default"
                );
            }
            // A patch action's reset control submits its preset and nothing else, so the
            // request carries this group's fields and leaves every other field alone.
            assert_eq!(
                fields::action_params(&declared, &reset.preset, &editor.controls.fields)
                    .expect("a request"),
                reset.preset,
                "{label} sends more than its own preset"
            );
            editor.busy = false;
            let _ = editor.update(Message::Control(ControlMessage::ResetGroup {
                module_id: module.id.clone(),
                path: vec![index],
            }));
            assert!(editor.busy, "{label}: {}", editor.status.text);
            assert!(
                editor
                    .status
                    .text
                    .starts_with(&format!("Running edit.{}", reset.action)),
                "{label}: {}",
                editor.status.text
            );
        }
        let Some(reset) = &module.reset else {
            continue;
        };
        editor.busy = false;
        let _ = editor.update(Message::Control(ControlMessage::ResetModule(
            module.id.clone(),
        )));
        assert!(editor.busy, "{}: {}", module.id, editor.status.text);
        assert!(
            editor
                .status
                .text
                .starts_with(&format!("Running edit.{}", reset.action)),
            "{}: {}",
            module.id,
            editor.status.text
        );
    }
    assert!(groups >= 3, "the built-ins declare grouped resets");

    // A double-click on one label is that one field, at its declared default, as one patch.
    let (action, parameter) = patch_control(&editor);
    editor.busy = false;
    editor
        .controls
        .fields
        .set(&action, &parameter, "1.5".into());
    let _ = editor.update(Message::Control(ControlMessage::ResetField {
        action: action.clone(),
        parameter: parameter.clone(),
    }));
    let default = tools::declared_action(&editor.modules, &action)
        .and_then(|declared| declared.parameter(&parameter))
        .map(fields::seed_text)
        .expect("a declared default");
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some(default.as_str())
    );
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

/// Selecting a history entry shows that entry's own saved values in the disabled fields, and
/// returning to current puts the current ones back. The values come from the displayed entry's
/// own `recipe.describe` rows: nothing is recomputed on the desktop.
/// The section dot reads the current entry's rows, which the editor keeps whichever entry is
/// displayed: rows read for a historical preview never replace them, a refresh during a preview
/// brings them separately, and rows read on returning to the current entry are theirs again.
#[test]
fn the_current_entrys_rows_are_kept_whichever_entry_is_displayed() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let current = editor
        .document
        .state
        .as_ref()
        .expect("open")
        .current_entry
        .clone();
    let asset = current.asset_id.clone();
    let rows = |entry: &luxforge_core::HistoryEntry| testing::described(entry);
    let read = |entry: &luxforge_core::HistoryEntry| {
        Message::Sync(SyncMessage::RecipeDescribed(Ok(Box::new(
            crate::app::tasks::RecipeRead {
                recipe: rows(entry),
                masks: luxforge_core::mask::commands::MaskListing {
                    entry_id: entry.id.clone(),
                    masks: Vec::new(),
                },
            },
        ))))
    };
    let current_rows = |editor: &Editor| {
        editor
            .document
            .current_recipe
            .as_ref()
            .map(|recipe| recipe.entry_id.clone())
    };
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refresh_for(&asset, &current, vec![current.clone()], &[&current], false),
    )))));
    assert_eq!(current_rows(&editor), Some(current.id.clone()));

    let older = entry(&asset, 2, None);
    let _ = editor.update(read(&older));
    assert_eq!(
        editor
            .document
            .recipe
            .as_ref()
            .map(|recipe| &recipe.entry_id),
        Some(&older.id)
    );
    assert_eq!(
        current_rows(&editor),
        Some(current.id.clone()),
        "a preview's rows are not the current entry's"
    );

    let mut previewing = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    previewing.recipe = rows(&older);
    previewing.current_recipe = Some(rows(&current));
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        previewing,
    )))));
    assert_eq!(current_rows(&editor), Some(current.id.clone()));

    let _ = editor.update(read(&current));
    assert_eq!(current_rows(&editor), Some(current.id.clone()));
    finish(editor, catalog);
}

#[test]
fn historical_values_fill_the_disabled_fields_and_return_to_current_restores_them() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    let (action, parameter) = patch_control(&editor);
    let asset = editor
        .document
        .state
        .as_ref()
        .expect("open")
        .asset
        .id
        .clone();
    let module = editor
        .modules
        .iter()
        .find(|module| module.action(&action).is_some())
        .expect("the declaring module")
        .id
        .clone();
    let described = |editor: &mut Editor, value: f64| {
        let current = editor
            .document
            .state
            .as_ref()
            .expect("open")
            .current_entry
            .clone();
        let mut refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
        refresh.recipe.layers = vec![luxforge_core::LayerDescription {
            id: luxforge_core::LayerId::new(),
            effect: "test.effect".into(),
            module: Some(module.clone()),
            title: Some("Test".into()),
            summary: "Test".into(),
            values: json!({ parameter.clone(): value })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            available: true,
            mask: None,
            artifacts: Vec::new(),
            neutral: false,
            input_stage: None,
            input_orientation: None,
        }];
        Box::new(refresh)
    };

    // The current state.
    let current = described(&mut editor, 2.0);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(current))));
    assert_eq!(editor.controls.fields.get(&action, &parameter), Some("2"));

    // A historical entry is selected: its own rows seed the same fields, and the section is
    // disabled with its values still visible.
    let older = entry(&asset, 2, None);
    crate::state::testing::show(
        &mut editor.session,
        editor.document.state.as_ref(),
        HistorySelection::Entry(older.id.clone()),
    );
    editor.document.display_entry = Some(older.id.clone());
    let historical = described(&mut editor, -1.0);
    let _ = editor.update(Message::Sync(SyncMessage::RecipeDescribed(Ok(Box::new(
        crate::app::tasks::RecipeRead {
            recipe: historical.recipe,
            masks: historical.masks,
        },
    )))));
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some("-1"),
        "the fields do not follow the previewed entry"
    );
    editor.rederive();
    let section = editor
        .workspace
        .tools
        .all()
        .find(|section| section.module_id == module)
        .expect("the module's section");
    assert!(!section.enabled, "the panel is editable during a preview");
    assert_eq!(
        section.disabled_reason.as_deref(),
        Some(crate::state::NOT_CURRENT)
    );
    assert!(
        section
            .controls
            .iter()
            .flat_map(flatten)
            .any(|slider| slider.display == "-1"),
        "the previewed values are not visible"
    );

    // Return to current: the current entry's values come back.
    crate::state::testing::show(
        &mut editor.session,
        editor.document.state.as_ref(),
        HistorySelection::Current,
    );
    let current = described(&mut editor, 2.0);
    editor.document.display_entry = Some(current.state.current_entry.id.clone());
    let _ = editor.update(Message::Sync(SyncMessage::RecipeDescribed(Ok(Box::new(
        crate::app::tasks::RecipeRead {
            recipe: current.recipe,
            masks: current.masks,
        },
    )))));
    assert_eq!(
        editor.controls.fields.get(&action, &parameter),
        Some("2"),
        "returning to current did not restore the current values"
    );
    finish(editor, catalog);
}

/// Every action control a built-in descriptor generates sends the exact `edit.<action>`
/// request an independent JSON client would send: the method name, every required parameter
/// the control's own preset does not already supply, and no field the action does not declare.
/// The same message the control's click would raise then runs cleanly through `Editor::update`.
#[test]
fn every_generated_action_control_matches_its_declared_schema() {
    let modules = descriptors();
    let (mut editor, catalog) = opened_with_modules(modules.clone(), 1);

    let mut checked = 0usize;
    for (_, _, action) in tools::palette_entries(&modules, editor.developer, None, None) {
        let PaletteAction::Run { action, preset } = action else {
            continue;
        };
        let declared = tools::declared_action(&modules, &action)
            .unwrap_or_else(|| panic!("{action} is not declared by any module"));
        let request = editor
            .request_for(&action, None)
            .unwrap_or_else(|| panic!("{action}: {}", editor.status.text));
        assert_eq!(request["method"], json!(format!("edit.{action}")));
        let params = request["params"].as_object().expect("an object");
        for parameter in &declared.parameters {
            if parameter.required
                && parameter.default.is_none()
                && !preset.contains_key(&parameter.name)
            {
                assert!(
                    params.contains_key(&parameter.name),
                    "{action} is missing its required parameter {}",
                    parameter.name
                );
            }
        }
        let known: Vec<&str> = declared
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .chain(["asset_id", "mutation"])
            .collect();
        for key in params.keys() {
            assert!(
                known.contains(&key.as_str()),
                "{action} sends the undeclared field {key}"
            );
        }
        // The exact message a click on the generated control raises runs the same request.
        editor.busy = false;
        let _ = editor.update(Message::Action(ActionMessage::Run {
            action: action.clone(),
            preset: preset.clone(),
        }));
        assert!(editor.busy, "{action} did not run through RunAction");
        assert!(
            editor
                .status
                .text
                .starts_with(&format!("Running edit.{action}")),
            "{action}: {}",
            editor.status.text
        );
        checked += 1;
    }
    assert!(checked > 0, "at least one built-in action was exercised");

    // Every module's own header reset (`ControlMessage::ResetModule`) is the same round trip.
    for module in &modules {
        let Some(reset) = &module.reset else {
            continue;
        };
        assert!(
            tools::declared_action(&modules, &reset.action).is_some(),
            "{} declares an undeclared reset action",
            module.id
        );
        editor.busy = false;
        let _ = editor.update(Message::Control(ControlMessage::ResetModule(
            module.id.clone(),
        )));
        assert!(editor.busy, "{} reset did not run", module.id);
        assert!(
            editor
                .status
                .text
                .starts_with(&format!("Running edit.{}", reset.action)),
            "{}: {}",
            module.id,
            editor.status.text
        );
        checked += 1;
    }
    assert!(
        checked > 1,
        "the crop module's own header reset was exercised too"
    );
    finish(editor, catalog);
}

/// `ControlMessage::ResetGroup` finds a group's reset by its position in the module's controls and
/// runs it exactly as `ControlMessage::ResetModule` runs a header reset. No built-in module declares
/// a group reset yet, so this drives the mechanism on a descriptor built for the purpose.
#[test]
fn reset_group_dispatches_the_action_at_its_declared_position() {
    let mut module = crop_descriptor();
    let luxforge_core::Control::Group(luxforge_core::GroupControl { reset, .. }) =
        &mut module.controls[0]
    else {
        unreachable!("the fixture's first control is a group")
    };
    *reset = Some(luxforge_core::ResetAction {
        action: "crop-reset".into(),
        preset: Map::new(),
    });
    let (mut editor, catalog) = opened_with_modules(vec![module.clone()], 2);

    let _ = editor.update(Message::Control(ControlMessage::ResetGroup {
        module_id: module.id.clone(),
        path: vec![0],
    }));
    assert!(editor.busy, "{}", editor.status.text);
    assert!(
        editor.status.text.starts_with("Running edit.crop-reset"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// The picker control is the way into and out of its module's pick mode: one `workspace.set`
/// for the module, and one for the pointer when it is already active. It commits nothing, and
/// its context menu copies exactly the request the click sends.
#[test]
fn a_picker_control_enters_and_leaves_its_modules_mode_through_workspace_set() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 5);
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let (module_id, _, _) = sample_mode(&editor);
    let revision = editor
        .document
        .state
        .as_ref()
        .expect("an open asset")
        .revision;
    editor.rederive();
    let picker = |editor: &Editor| -> crate::state::tools::PickerControl {
        editor
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == module_id)
            .expect("the declaring module's section")
            .pickers()
            .first()
            .map(|picker| (*picker).clone())
            .expect("its declared picker")
    };

    // Not in the mode: the button is unselected and a click enters that module's mode.
    let before = picker(&editor);
    assert!(!before.selected);
    assert_eq!(before.target, module_id);
    assert_eq!(
        editor.mode_request(&module_id),
        json!({"method":"workspace.set","params":{"mode": module_id}}),
        "the copied request is the one the click sends"
    );
    let _ = editor.update(Message::View(ViewMessage::SetMode(before.target.clone())));

    // The session adopts the mode, as the `workspace.set` round trip does.
    editor.session.workspace.mode = module_id.clone();
    editor.rederive();
    let after = picker(&editor);
    assert!(after.selected, "the button reads selected in its own mode");
    assert_eq!(
        after.target, POINTER_MODE,
        "clicking it again leaves the mode"
    );
    assert_eq!(
        editor.mode_request(&module_id),
        json!({"method":"workspace.set","params":{"mode": POINTER_MODE}})
    );
    let _ = editor.update(Message::View(ViewMessage::SetMode(after.target.clone())));
    let _ = editor.update(Message::Action(ActionMessage::CopyModeRequest(
        module_id.clone(),
    )));
    assert_eq!(editor.status.text, "Copied the workspace.set request");

    // Nothing about it is an edit: no history entry, no revision, no draft.
    assert_eq!(
        editor
            .document
            .state
            .as_ref()
            .expect("an open asset")
            .revision,
        revision
    );
    assert_eq!(editor.displayed_entry(), Some(entry_id));
    assert!(editor.slider_gesture().is_none() && editor.crop().is_none());

    // The mode strip no longer offers it: the panel is the only place it lives.
    assert!(
        !editor
            .workspace
            .canvas
            .modes
            .iter()
            .any(|mode| mode.id == module_id),
        "a pick mode is not a mode-strip entry"
    );
    finish(editor, catalog);
}

/// On a RAW photo, Basic's Temperature and Tint are the development's `set-raw` fields, and they
/// follow the displayed entry's own `recipe.describe` row, exactly as every other module's do: the
/// desktop reads no RAW payload. Under As shot they show the camera's as-shot equivalent the core
/// reports, not the 6504 K and 0 no one set; a custom value shows itself; a field being edited is
/// left alone until the selection changes.
#[test]
fn raw_fields_show_the_displayed_entrys_described_values() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    // The real descriptors, because the RAW parameters' declared precision is what decides how
    // a seeded field reads.
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let original = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
    let historical = raw_entry(&asset, 0, None, &original);
    let mut adjusted = original.clone();
    adjusted.wb_mode = WhiteBalanceMode::Custom;
    adjusted.temperature_kelvin = Some(3500.0);
    adjusted.tint = Some(12.0);
    adjusted.gains = luxforge_core::gains_from_temperature_tint(3500.0, 12.0, Z6_CAM_XYZ).unwrap();
    let current = raw_entry(&asset, 4, Some(&historical.id), &adjusted);

    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        raw_refresh(&asset, &current),
    )))));
    let shown = |editor: &Editor| {
        ["set-raw.temperature", "set-raw.tint"].map(|key| {
            editor.controls.fields.summary()[key]
                .as_str()
                .map(str::to_owned)
        })
    };
    assert_eq!(
        shown(&editor),
        [Some("3500"), Some("12")].map(|text| text.map(str::to_owned))
    );
    // And Basic's section draws them, in kelvin, in Temperature's place.
    let temperature = editor
        .workspace
        .tools
        .all()
        .flat_map(|section| crate::state::control_tree::walk(&section.controls))
        .find_map(|control| match control {
            crate::state::tools::ControlModel::Slider(slider)
                if slider.action == "set-raw" && slider.parameter == "temperature" =>
            {
                Some(slider.clone())
            }
            _ => None,
        })
        .expect("Basic's Temperature, resolved to the RAW development's");
    assert_eq!(
        (temperature.label.as_str(), temperature.unit.as_deref()),
        ("Temperature", Some("K"))
    );
    assert_eq!(temperature.display, "3500");
    // The explicit gains have no control, so no field shows them.
    assert_eq!(editor.controls.fields.get("set-raw-red-gain", "gain"), None);

    // A historical As shot entry: the fields change when its rows arrive, not before, and the
    // field being edited is released by the selection.
    editor.controls.editing = Some(("set-raw".into(), "temperature".into()));
    let mut session = editor.session.clone();
    session
        .preview
        .select(&asset, HistorySelection::Entry(historical.id.clone()))
        .unwrap();
    session.revision += 1;
    let job = raw_refresh(&asset, &historical).job;
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload { job, session },
    )))));
    assert_eq!(editor.document.display_entry, Some(historical.id.clone()));
    assert!(editor.controls.editing.is_none());
    assert!(
        !editor.recipe_rows_shown(),
        "an evidence frame waits for the displayed entry's own rows"
    );
    let read = raw_refresh(&asset, &historical);
    let _ = editor.update(Message::Sync(SyncMessage::RecipeDescribed(Ok(Box::new(
        tasks::RecipeRead {
            recipe: read.recipe,
            masks: read.masks,
        },
    )))));
    assert!(editor.recipe_rows_shown());
    let [kelvin, tint] =
        luxforge_core::temperature_tint_from_gains(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
    assert_eq!((kelvin.round(), tint.round()), (4861.0, -50.0));
    assert_eq!(
        shown(&editor),
        [Some("4861"), Some("-50")].map(|text| text.map(str::to_owned)),
        "As shot shows the camera's own white balance as a temperature and tint"
    );

    // Return to current: the custom values come back.
    let mut session = editor.session.clone();
    session.preview.return_current();
    session.revision += 1;
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload {
            job: raw_refresh(&asset, &current).job,
            session,
        },
    )))));
    let read = raw_refresh(&asset, &current);
    let _ = editor.update(Message::Sync(SyncMessage::RecipeDescribed(Ok(Box::new(
        tasks::RecipeRead {
            recipe: read.recipe,
            masks: read.masks,
        },
    )))));
    assert_eq!(editor.document.display_entry, Some(current.id));
    assert_eq!(
        shown(&editor),
        [Some("3500"), Some("12")].map(|text| text.map(str::to_owned))
    );

    // A describe that fails for the displayed entry does not hold a frame forever: it is
    // captured with the failure in the status bar.
    let mut session = editor.session.clone();
    session
        .preview
        .select(&asset, HistorySelection::Entry(historical.id.clone()))
        .unwrap();
    session.revision += 1;
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload {
            job: raw_refresh(&asset, &historical).job,
            session,
        },
    )))));
    assert!(!editor.recipe_rows_shown());
    let _ = editor.update(Message::Sync(SyncMessage::RecipeDescribed(Err(
        "unavailable".into(),
    ))));
    assert!(editor.recipe_rows_shown());
    assert_eq!(editor.status.text, "Recipe unavailable: unavailable");
    finish(editor, catalog);
}

/// The fixture's curve, `(action, first channel)`: the key its samples are requested under.
fn fixture_curve() -> (String, String) {
    (ACTION.into(), "master".into())
}

/// An editor with `modules` registered and a JPEG open, ready to show which curves it samples.
fn sampling(modules: Vec<luxforge_core::ModuleDescriptor>) -> (Editor, PathBuf) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(modules))));
    let asset = AssetId::new();
    let current = entry(&asset, 4, None);
    let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    (editor, catalog)
}

/// A curve is sampled once it is on screen, and the tools panel is what says so: a section its
/// module declares `collapsed` draws no curve until it is opened, and a developer section outside
/// developer mode is not drawn at all.
#[test]
fn a_curve_in_a_hidden_section_queries_no_samples() {
    let (editor, catalog) = sampling(vec![controls_descriptor()]);
    assert!(
        editor
            .curve_sampling
            .requested
            .contains_key(&fixture_curve()),
        "an expanded section's curve is sampled"
    );
    finish(editor, catalog);

    let collapsed = luxforge_core::ModuleDescriptor {
        collapsed: true,
        ..controls_descriptor()
    };
    let module_id = collapsed.id.clone();
    let (mut editor, catalog) = sampling(vec![collapsed]);
    assert!(!editor.workspace.tools.all().any(|section| section.expanded));
    assert!(
        editor.curve_sampling.requested.is_empty(),
        "a section declared collapsed shows no curve"
    );
    let _ = editor.update(Message::Control(ControlMessage::ToggleSection(module_id)));
    assert!(
        editor
            .curve_sampling
            .requested
            .contains_key(&fixture_curve()),
        "opening the section shows its curve"
    );
    finish(editor, catalog);

    let developer = luxforge_core::ModuleDescriptor {
        developer: true,
        ..controls_descriptor()
    };
    let (editor, catalog) = sampling(vec![developer]);
    assert!(!editor.developer);
    assert!(
        editor.curve_sampling.requested.is_empty(),
        "a developer section is not drawn outside developer mode"
    );
    finish(editor, catalog);
}

/// A tabbed module shows one tab at a time, so a curve in another tab is not sampled until its
/// tab is selected.
#[test]
fn a_curve_in_a_hidden_tab_queries_no_samples() {
    let mut tabs = controls_descriptor();
    tabs.layout = luxforge_core::ModuleLayout::Tabs;
    let luxforge_core::Control::Group(luxforge_core::GroupControl { controls, .. }) =
        &mut tabs.controls[0]
    else {
        unreachable!("the fixture's controls are one group")
    };
    let curve = controls
        .iter()
        .position(|control| matches!(control, luxforge_core::Control::Curve(_)))
        .expect("the fixture's curve");
    let curve = controls.remove(curve);
    tabs.controls
        .push(luxforge_core::Control::group("Curve", vec![curve]).into());
    let module_id = tabs.id.clone();
    let (mut editor, catalog) = sampling(vec![tabs]);
    assert!(
        editor.curve_sampling.requested.is_empty(),
        "the curve's tab is not the one shown"
    );
    let _ = editor.update(Message::Control(ControlMessage::SelectTab {
        module_id,
        index: 1,
    }));
    assert!(
        editor
            .curve_sampling
            .requested
            .contains_key(&fixture_curve()),
        "selecting the curve's tab shows it"
    );
    finish(editor, catalog);
}

/// A module whose effects exist only on RAW photos draws no section on a JPEG, so its curve is
/// not sampled there; on a RAW photo it is.
#[test]
fn a_curve_module_that_does_not_apply_to_the_photo_queries_no_samples() {
    let mut raw_only = controls_descriptor();
    raw_only.effects = vec![luxforge_core::EffectDescriptor {
        fit_settle: Default::default(),
        id: "fixture.raw-only".into(),
        format: 1,
        stage: luxforge_core::EffectStage::Color,
        order: 0,
        maskable: false,
        artifacts: false,
        single: false,
        sources: vec![luxforge_core::SourceTag::Raw],
    }];
    let (mut editor, catalog) = sampling(vec![raw_only]);
    assert_eq!(editor.workspace.tools.all().count(), 0);
    assert!(
        editor.curve_sampling.requested.is_empty(),
        "a module that does not apply to a JPEG shows no curve"
    );
    let asset = editor.document.state.as_ref().unwrap().asset.id.clone();
    let payload = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
    let raw = raw_entry(&asset, 5, None, &payload);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        raw_refresh(&asset, &raw),
    )))));
    assert!(
        editor
            .curve_sampling
            .requested
            .contains_key(&fixture_curve()),
        "on a RAW photo the module applies and its curve is sampled"
    );
    finish(editor, catalog);
}

/// A group's reset is the one its header shows, which the tools panel resolved for the photo and
/// the target: on a RAW photo's global target Basic's White balance runs the RAW development's
/// As shot, not Basic's own reset.
#[test]
fn reset_group_runs_the_reset_the_panel_resolved() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let payload = RawPayload::for_as_shot(Z6_AS_SHOT, Z6_CAM_XYZ).unwrap();
    let current = raw_entry(&asset, 4, None, &payload);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        raw_refresh(&asset, &current),
    )))));
    let basic = tools::module_of(&editor.modules, "luxforge.basic")
        .expect("Basic")
        .clone();
    let path = basic
        .controls
        .iter()
        .position(|control| {
            luxforge_core::resolve_group_reset(
                &basic.id,
                control,
                Some(luxforge_core::SourceTag::Raw),
                None,
            )
            .is_some_and(|resolved| resolved.variant)
        })
        .expect("a group whose reset a RAW variant provides");
    let declared = match &basic.controls[path] {
        luxforge_core::Control::Group(luxforge_core::GroupControl { reset, .. }) => {
            reset.clone().expect("a declared reset")
        }
        _ => unreachable!("a group"),
    };
    let shown = editor
        .workspace
        .tools
        .all()
        .find(|section| section.module_id == basic.id)
        .and_then(|section| section.group_reset(&[path]))
        .cloned()
        .expect("the reset the header shows");
    assert_ne!(shown.action, declared.action, "the variant's reset");
    let _ = editor.update(Message::Control(ControlMessage::ResetGroup {
        module_id: basic.id.clone(),
        path: vec![path],
    }));
    assert!(
        editor
            .status
            .text
            .starts_with(&format!("Running edit.{}", shown.action)),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

#[test]
fn detail_controls_generate_two_groups_with_hint_and_editable_zero_strength_fields() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 4);
    editor.rederive();
    let section = editor
        .workspace
        .tools
        .all()
        .find(|section| section.module_id == "luxforge.detail")
        .unwrap();
    assert_eq!(section.hint.as_deref(), Some("Judge fine detail at 100%"));
    assert!(!section.expanded);
    assert!(section.enabled);
    assert_eq!(section.controls.len(), 2);
    for (control, label) in section
        .controls
        .iter()
        .zip(["Sharpening", "Noise reduction"])
    {
        let tools::ControlModel::Group(group) = control else {
            panic!("expected generated group");
        };
        assert_eq!(group.label, label);
        assert_eq!(group.controls.len(), 4);
        assert!(
            group
                .controls
                .iter()
                .all(|control| matches!(control, tools::ControlModel::Slider(_)))
        );
    }
    for name in ["sharpening", "luminance", "colour"] {
        assert_eq!(editor.controls.fields.get("set-detail", name), Some("0"));
    }
    for (name, value) in [
        ("radius", "1.0"),
        ("sharpen-detail", "25"),
        ("sharpen-masking", "0"),
        ("luminance-detail", "50"),
        ("colour-detail", "50"),
    ] {
        assert_eq!(editor.controls.fields.get("set-detail", name), Some(value));
        let request = editor.request_for("set-detail", Some(name)).unwrap();
        assert_eq!(request["method"], "edit.set-detail");
        assert!(request["params"][name].is_number());
    }
    let action = editor
        .modules
        .iter()
        .find(|m| m.id == "luxforge.detail")
        .unwrap()
        .action("set-detail")
        .unwrap();
    assert_eq!(
        action
            .parameters
            .iter()
            .find(|p| p.name == "radius")
            .unwrap()
            .notes,
        "Takes effect when Amount is above 0"
    );
    assert_eq!(
        action
            .parameters
            .iter()
            .find(|p| p.name == "luminance-detail")
            .unwrap()
            .notes,
        "Takes effect when Luminance is above 0"
    );
    assert_eq!(
        action
            .parameters
            .iter()
            .find(|p| p.name == "colour-detail")
            .unwrap()
            .notes,
        "Takes effect when Colour is above 0"
    );
    finish(editor, catalog);
}

/// Geometry controls read their global layer while maskable adjustments follow the selected mask.
/// Local command completion, another client's refresh and leaving Mask mode all read stored values.
#[test]
fn perspective_fields_follow_local_commits_and_external_refresh_while_a_mask_is_selected() {
    use luxforge_core::{MASK_MODE, PERSPECTIVE_EFFECT};
    const ACTION: &str = "set-perspective";
    let catalog = luxforge_testbase::paths::temp_catalog("perspective-field-readback");
    let (mut editor, asset, agent) = testing::real_photo(&catalog);
    let owner = editor.owner.clone();
    let refresh = |editor: &mut Editor| {
        let read = tasks::refresh(
            &owner,
            editor.client,
            asset.clone(),
            tasks::Scope::Open,
            None,
        )
        .unwrap();
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(read)))));
    };
    let mode = |editor: &mut Editor, name: &str| {
        let _ = editor.update(Message::View(ViewMessage::SetMode(name.into())));
        tasks::call(&owner, editor.client, "workspace.set", json!({"mode":name})).unwrap();
        let (session, _) = tasks::call(&owner, editor.client, "session.state", json!({})).unwrap();
        let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(
            serde_json::from_value(session).unwrap(),
        ))));
    };
    let assert_fields = |editor: &Editor, h: i64, v: i64| {
        assert_eq!(
            editor.controls.fields.get(ACTION, "horizontal"),
            Some(h.to_string().as_str())
        );
        assert_eq!(
            editor.controls.fields.get(ACTION, "vertical"),
            Some(v.to_string().as_str())
        );
        let row = editor
            .document
            .recipe
            .as_ref()
            .unwrap()
            .layers
            .iter()
            .find(|row| row.effect == PERSPECTIVE_EFFECT)
            .unwrap();
        assert_eq!(row.values["horizontal"], h);
        assert_eq!(row.values["vertical"], v);
        assert!(!row.neutral);
        assert!(row.mask.is_none());
    };
    let preset = Map::from_iter([
        ("horizontal".into(), json!(-35)),
        ("vertical".into(), json!(20)),
    ]);
    let request = editor
        .request_for_preset(ACTION, None, Some(&preset))
        .unwrap();
    let _ = editor.update(Message::Action(ActionMessage::Run {
        action: ACTION.into(),
        preset,
    }));
    let read = tasks::command_now(
        &owner,
        editor.client,
        asset.clone(),
        request["method"].as_str().unwrap(),
        request["params"].clone(),
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(read)))));
    assert_fields(&editor, -35, 20);

    let revision = editor.document.state.as_ref().unwrap().revision;
    tasks::call(
        &owner,
        agent,
        "edit.set-basic",
        json!({"asset_id":asset,"exposure":1.0,"mutation":tasks::mutation(revision)}),
    )
    .unwrap();
    refresh(&mut editor);
    let revision = editor.document.state.as_ref().unwrap().revision;
    tasks::call(&owner, agent, "mask.create-linear", json!({"asset_id":asset,"x0":0.1,"y0":0.1,"x1":0.9,"y1":0.9,"mutation":tasks::mutation(revision)})).unwrap();
    refresh(&mut editor);
    mode(&mut editor, MASK_MODE);
    let mask = editor
        .section_target()
        .expect("Mask mode selects the existing mask")
        .clone();
    assert_fields(&editor, -35, 20);
    let revision = editor.document.state.as_ref().unwrap().revision;
    tasks::call(
        &owner,
        agent,
        "edit.set-basic",
        json!({"asset_id":asset,"mask":mask,"exposure":2.0,"mutation":tasks::mutation(revision)}),
    )
    .unwrap();
    refresh(&mut editor);
    assert_eq!(
        editor.controls.fields.get("set-basic", "exposure"),
        Some("2.00")
    );
    assert_fields(&editor, -35, 20);

    let revision = editor.document.state.as_ref().unwrap().revision;
    tasks::call(&owner, agent, "edit.set-perspective", json!({"asset_id":asset,"horizontal":-30,"vertical":15,"mutation":tasks::mutation(revision)})).unwrap();
    refresh(&mut editor);
    assert_fields(&editor, -30, 15);
    mode(&mut editor, POINTER_MODE);
    assert!(editor.section_target().is_none());
    assert_fields(&editor, -30, 15);
    assert_eq!(
        editor.controls.fields.get("set-basic", "exposure"),
        Some("1.00"),
        "leaving Mask mode restores the global adjustment values without needing a typed field"
    );
    mode(&mut editor, MASK_MODE);
    assert_fields(&editor, -30, 15);
    assert_eq!(
        editor.controls.fields.get("set-basic", "exposure"),
        Some("2.00")
    );
    mode(&mut editor, POINTER_MODE);
    let request = editor
        .request_for_preset("reset-perspective", None, Some(&Map::new()))
        .unwrap();
    let _ = editor.update(Message::Control(ControlMessage::ResetModule(
        "luxforge.perspective".into(),
    )));
    let read = tasks::command_now(
        &owner,
        editor.client,
        asset.clone(),
        request["method"].as_str().unwrap(),
        request["params"].clone(),
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(read)))));
    assert_eq!(editor.controls.fields.get(ACTION, "horizontal"), Some("0"));
    assert_eq!(editor.controls.fields.get(ACTION, "vertical"), Some("0"));
    assert!(
        editor
            .document
            .recipe
            .as_ref()
            .unwrap()
            .layers
            .iter()
            .find(|row| row.effect == PERSPECTIVE_EFFECT)
            .unwrap()
            .neutral
    );
    finish(editor, catalog);
}
