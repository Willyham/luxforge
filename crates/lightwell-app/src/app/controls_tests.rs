//! Behavioral checks for generated gestures, independent of a production module's identity.
use super::{Editor, message::Message, testing::*};
use lightwell_core::{AssetId, Draft};
use lightwell_ui::{ColorPickerEvent, CurveEditorEvent};
use serde_json::{Value, json};
use std::path::PathBuf;

const ACTION: &str = "fixture-set";

fn editor() -> (Editor, PathBuf, AssetId) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::ModulesLoaded(Ok(vec![controls_descriptor()])));
    let asset = AssetId::new();
    let current = entry(&asset, 4, None);
    let refresh = refresh_for(&asset, &current, vec![current.clone()], &[&current], false);
    let _ = editor.update(Message::Refreshed(Ok(Box::new(refresh))));
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
        let _ = editor.update(Message::ControlDiscrete {
            action: ACTION.into(),
            parameter: parameter.into(),
            value: value.clone(),
        });
        assert!(editor.busy, "selection starts one ordinary mutation");
        assert!(editor.slider_draft.is_none());
        assert_eq!(field_request(&mut editor, parameter), value);
        finish(editor, catalog);
    }
}

#[test]
fn typing_waits_for_enter_and_invalid_text_commits_nothing() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::Field {
        action: ACTION.into(),
        parameter: "count".into(),
        text: "7".into(),
    });
    assert!(!editor.busy);
    let _ = editor.update(Message::CancelEdit);
    assert!(!editor.busy, "leaving a field is not a commit");
    let _ = editor.update(Message::Field {
        action: ACTION.into(),
        parameter: "count".into(),
        text: "bad".into(),
    });
    let _ = editor.update(Message::Submit {
        action: ACTION.into(),
        parameter: Some("count".into()),
    });
    assert!(!editor.busy);
    assert_eq!(editor.fields.get(ACTION, "count"), Some("bad"));
    assert!(editor.editing.is_some());
    let _ = editor.update(Message::Field {
        action: ACTION.into(),
        parameter: "count".into(),
        text: "7".into(),
    });
    let _ = editor.update(Message::Submit {
        action: ACTION.into(),
        parameter: Some("count".into()),
    });
    assert!(editor.busy);
    assert!(editor.slider_draft.is_none());
    assert_eq!(field_request(&mut editor, "count"), json!(7));
    finish(editor, catalog);
}

#[test]
fn slider_fractions_use_soft_bounds_and_preserve_fine_step() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::ControlFraction {
        action: ACTION.into(),
        parameter: "amount".into(),
        fraction: 0.501,
    });
    let value = field_request(&mut editor, "amount").as_f64().unwrap();
    assert!(
        (value - 0.01).abs() < 1e-9,
        "fine step must not round back to zero: {value}"
    );
    assert!(editor.slider_draft.is_some());
    finish(editor, catalog);
}

#[test]
fn picker_and_curve_share_bounded_draft_and_commit_once() {
    for (parameter, message, expected) in [
        (
            "rgb",
            Message::ControlPicker {
                action: ACTION.into(),
                parameter: "rgb".into(),
                event: ColorPickerEvent::Hue(0.0),
            },
            json!([128, 32, 32]),
        ),
        (
            "master",
            Message::ControlCurve {
                action: ACTION.into(),
                parameter: "master".into(),
                event: CurveEditorEvent::Move {
                    index: 1,
                    position: [0.5, 0.75],
                },
            },
            json!([[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]]),
        ),
    ] {
        let (mut editor, catalog, asset) = editor();
        let log = attach_log(&mut editor);
        for _ in 0..3 {
            let _ = editor.update(message.clone());
            let _ = editor.update(Message::SliderDraftTick);
        }
        assert_eq!(field_request(&mut editor, parameter), expected);
        let _ = editor.update(Message::SliderDraftBegun(Ok(Box::new(Draft::new(
            ACTION,
            asset.clone(),
            4,
        )))));
        for _ in 0..3 {
            let _ = editor.update(Message::SliderDraftTick);
        }
        let current = editor.state.as_ref().unwrap().current_entry.clone();
        let mut draft = editor.session.draft.clone().unwrap();
        draft.draft_revision += 1;
        let job = refresh_for(&asset, &current, Vec::new(), &[&current], false).job;
        let now = std::time::Instant::now();
        let round_trip = crate::app::tasks::RoundTrip {
            queued: now,
            started: now,
            answered: now,
            planned: now,
        };
        let _ = editor.update(Message::SliderDraftSet(Ok(Box::new((
            draft, job, round_trip,
        )))));
        for _ in 0..2 {
            let _ = editor.update(Message::ControlReleased {
                action: ACTION.into(),
                parameter: parameter.into(),
            });
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
    let _ = editor.update(Message::ControlPicker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Hue(hue),
    });
    assert_eq!(
        editor.control_field_value(ACTION, "rgb"),
        Some(json!([128, 128, 128]))
    );
    assert!(editor.slider_draft.is_none());
    let _ = editor.update(Message::ControlPicker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Release,
    });
    assert!(!editor.busy, "a hue-only gray gesture changes no RGB field");
    let _ = editor.update(Message::ControlPicker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Plane([1.0, 0.5]),
    });
    let expected = lightwell_ui::hsv_to_rgb([f64::from(hue), 1.0, 0.5]);
    assert_eq!(field_request(&mut editor, "rgb"), json!(expected));
    assert!(editor.slider_draft.is_some());
    finish(editor, catalog);
}

#[test]
fn picker_rejects_cached_hsv_when_the_rgb_field_changes_elsewhere() {
    let (mut editor, catalog, _) = editor();
    editor.set_control_field_value(ACTION, "rgb", &json!([0, 0, 0]));
    let _ = editor.update(Message::ControlPicker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Hue(0.67),
    });
    editor.set_control_field_value(ACTION, "rgb", &json!([0, 255, 0]));
    let _ = editor.update(Message::ControlPicker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Plane([1.0, 0.5]),
    });
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
        let _ = editor.update(Message::ControlPicker {
            action: ACTION.into(),
            parameter: "rgb".into(),
            event,
        });
    }
    assert!(editor.slider_draft.is_none());
    let _ = editor.update(Message::ControlPicker {
        action: ACTION.into(),
        parameter: "rgb".into(),
        event: ColorPickerEvent::Plane([1.0, 0.5]),
    });
    assert_eq!(field_request(&mut editor, "rgb"), json!([0, 128, 128]));
    finish(editor, catalog);
}

#[test]
fn curve_channel_selection_changes_no_request_value_or_recipe() {
    let (mut editor, catalog, _) = editor();
    let fields = editor.fields.clone();
    let _ = editor.update(Message::ControlCurve {
        action: ACTION.into(),
        parameter: "master".into(),
        event: CurveEditorEvent::Channel(1),
    });
    assert_eq!(editor.fields, fields);
    assert!(editor.slider_draft.is_none());
    assert!(!editor.busy);
    assert_eq!(editor.state.as_ref().unwrap().revision, 4);
    finish(editor, catalog);
}

#[test]
fn escape_cancels_picker_and_curve_even_while_begin_is_in_flight() {
    for message in [
        Message::ControlPicker {
            action: ACTION.into(),
            parameter: "rgb".into(),
            event: ColorPickerEvent::Hue(0.0),
        },
        Message::ControlCurve {
            action: ACTION.into(),
            parameter: "master".into(),
            event: CurveEditorEvent::Move {
                index: 1,
                position: [0.5, 0.75],
            },
        },
    ] {
        let (mut editor, catalog, asset) = editor();
        let log = attach_log(&mut editor);
        let _ = editor.update(message);
        let _ = editor.update(Message::SliderDraftCancel);
        let _ = editor.update(Message::SliderDraftBegun(Ok(Box::new(Draft::new(
            ACTION, asset, 4,
        )))));
        assert!(editor.slider_draft.is_none());
        let records = logged(&mut editor, &log);
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "slider_draft_commit")
        );
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "slider_draft_set")
        );
        assert_eq!(editor.state.as_ref().unwrap().revision, 4);
        finish(editor, catalog);
    }
}

#[test]
fn stepper_button_is_one_complete_draft_gesture() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::ControlStep {
        action: ACTION.into(),
        parameter: "count".into(),
        direction: 1,
    });
    assert_eq!(field_request(&mut editor, "count"), json!(3));
    assert_eq!(
        editor.slider_draft.as_ref().unwrap().finish,
        Some(super::slider::Finish::Commit)
    );
    finish(editor, catalog);
}

#[test]
fn field_arrow_nudge_stays_local_until_enter() {
    let (mut editor, catalog, _) = editor();
    let _ = editor.update(Message::ControlFieldNudge {
        action: ACTION.into(),
        parameter: "coordinate".into(),
        direction: 1,
        shift: false,
        option: true,
    });
    assert!(!editor.busy);
    assert!(editor.slider_draft.is_none());
    assert_eq!(editor.editing, Some((ACTION.into(), "coordinate".into())));
    assert!((field_request(&mut editor, "coordinate").as_f64().unwrap() - 5.1).abs() < 1e-9);
    let _ = editor.update(Message::Submit {
        action: ACTION.into(),
        parameter: Some("coordinate".into()),
    });
    assert!(editor.busy);
    assert!(editor.slider_draft.is_none());
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
