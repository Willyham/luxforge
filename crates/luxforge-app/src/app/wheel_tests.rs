//! The hue and saturation wheel of a generated control, driven through the one core draft: a drag
//! is one draft of both fields and one commit, Escape restores, the arrow keys draft like a drag,
//! and a double-click runs the wheel's declared reset.
use super::{
    message::{control::ControlMessage, draft::DraftMessage},
    testing::{attach_log, drafting, events, finish, logged},
    *,
};
use crate::state::number::NumberSpec;
use luxforge_ui::WheelEvent;

const ACTION: &str = "set-controls";
const HUE: &str = "wheel-hue";
const SATURATION: &str = "wheel-saturation";

fn turn(editor: &mut Editor, hue: f32, radius: f32) {
    let _ = editor.update(Message::Control(ControlMessage::Wheel {
        action: ACTION.into(),
        hue: HUE.into(),
        event: WheelEvent::Moved { hue, radius },
    }));
}

fn text<'a>(editor: &'a Editor, parameter: &str) -> &'a str {
    editor
        .controls
        .fields
        .get(ACTION, parameter)
        .expect("a seeded wheel field")
}

/// A drag moves both fields together: one `draft.begin`, one `draft.set` of the hue and the
/// saturation per position, and on release one commit, so the gesture is one history entry.
#[test]
fn a_wheel_drag_is_one_draft_of_both_fields_and_one_commit() {
    let (mut editor, catalog, log, _, _, _) = drafting();
    turn(&mut editor, 120.0, 0.5);
    turn(&mut editor, 200.0, 0.75);
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "slider_draft_begin").len(), 1);
    let sent: Vec<Value> = events(&records, "slider_draft_set")
        .iter()
        .map(|record| record["fields"].clone())
        .collect();
    assert_eq!(
        sent,
        [
            json!({HUE: 120.0, SATURATION: 50.0}),
            json!({HUE: 200.0, SATURATION: 75.0}),
        ],
        "each position is one patch of both fields"
    );
    assert_eq!(editor.drafting_control(), Some((ACTION, HUE)));
    assert_eq!(
        (text(&editor, HUE), text(&editor, SATURATION)),
        ("200", "75")
    );

    let log = attach_log(&mut editor);
    let release = |editor: &mut Editor| {
        let _ = editor.update(Message::Control(ControlMessage::Wheel {
            action: ACTION.into(),
            hue: HUE.into(),
            event: WheelEvent::Release,
        }));
    };
    release(&mut editor);
    release(&mut editor);
    assert_eq!(
        events(&logged(&mut editor, &log), "slider_draft_commit").len(),
        1,
        "one release commits once; a second has nothing to commit"
    );
    finish(editor, catalog);
}

/// Escape discards the wheel's draft: nothing is committed and both fields show their
/// authoritative values again.
#[test]
fn escape_restores_the_wheel() {
    let (mut editor, catalog, log, _, _, _) = drafting();
    let before = (
        text(&editor, HUE).to_owned(),
        text(&editor, SATURATION).to_owned(),
    );
    turn(&mut editor, 90.0, 1.0);
    assert_eq!(text(&editor, SATURATION), "100");
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "slider_draft_cancelled").len(), 1);
    assert!(events(&records, "slider_draft_commit").is_empty());
    assert!(editor.slider_gesture().is_none());
    assert_eq!(
        (
            text(&editor, HUE).to_owned(),
            text(&editor, SATURATION).to_owned()
        ),
        before
    );
    finish(editor, catalog);
}

/// The widget reports the centre with the hue it kept, and the host sends that hue unchanged with
/// a saturation of 0, so a hue chosen before survives a drag through the centre.
#[test]
fn the_centre_keeps_the_hue_the_wheel_had() {
    let (mut editor, catalog, log, _, _, _) = drafting();
    turn(&mut editor, 240.0, 0.4);
    turn(&mut editor, 240.0, 0.0);
    let sent: Vec<Value> = events(&logged(&mut editor, &log), "slider_draft_set")
        .iter()
        .map(|record| record["fields"].clone())
        .collect();
    assert_eq!(sent.last(), Some(&json!({HUE: 240.0, SATURATION: 0.0})));
    finish(editor, catalog);
}

/// The arrow keys draft like a drag: Left turns the hue across the seam from 0 to 359 and Up
/// raises the saturation, keyed by the wheel's hue so the key's release commits once.
#[test]
fn arrow_keys_turn_the_hue_across_the_seam_and_step_the_saturation() {
    let (mut editor, catalog, log, _, _, _) = drafting();
    let nudge = |editor: &mut Editor, saturation: bool, direction: i8, shift: bool| {
        let _ = editor.update(Message::Control(ControlMessage::WheelNudge {
            action: ACTION.into(),
            hue: HUE.into(),
            saturation,
            direction,
            shift,
            option: false,
        }));
    };
    nudge(&mut editor, false, -1, false);
    assert_eq!(text(&editor, HUE), "359");
    nudge(&mut editor, true, 1, true);
    assert_eq!(text(&editor, SATURATION), "10");
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "slider_draft_begin").len(), 1);
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Control(ControlMessage::Released {
        action: ACTION.into(),
        parameter: HUE.into(),
    }));
    assert_eq!(
        events(&logged(&mut editor, &log), "slider_draft_commit").len(),
        1
    );
    finish(editor, catalog);
}

/// A hue nudge wraps across the seam in either direction, at every step size.
#[test]
fn a_hue_nudge_wraps_across_the_seam() {
    let spec = NumberSpec::of(
        &luxforge_core::ParameterDescriptor::number(HUE, 0.0, 360.0)
            .step(1.0)
            .precision(0),
    )
    .unwrap();
    let wrapped = |current, direction, shift, option| {
        controls::wrapped_hue(&spec, current, direction, shift, option)
    };
    assert_eq!(wrapped(0.0, -1, false, false), 359.0);
    assert_eq!(wrapped(359.0, 1, false, false), 0.0);
    assert_eq!(
        wrapped(360.0, 1, false, false),
        1.0,
        "360 is the same direction as 0"
    );
    assert_eq!(wrapped(355.0, 1, true, false), 5.0);
    assert_eq!(wrapped(0.0, -1, false, true), 359.9);
    assert_eq!(wrapped(180.0, 1, false, false), 181.0);
}

/// A double-click on the wheel runs its declared reset, the patch of exactly its three fields,
/// as one action.
#[test]
fn a_double_click_runs_the_wheels_reset() {
    let (mut editor, catalog, _, _, _, _) = drafting();
    let reset = controls::wheel_of(&editor.modules, ACTION, HUE)
        .and_then(|wheel| wheel.reset.clone())
        .expect("the proof's wheel declares its reset");
    assert_eq!(
        Value::Object(reset.preset.clone()),
        json!({HUE: 0.0, SATURATION: 0.0, "wheel-luminance": 0.0})
    );
    assert!(!editor.busy);
    let _ = editor.update(Message::Control(ControlMessage::Wheel {
        action: ACTION.into(),
        hue: HUE.into(),
        event: WheelEvent::Reset,
    }));
    assert!(editor.busy, "the reset is sent as one action");
    finish(editor, catalog);
}
