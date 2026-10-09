//! The hue and saturation wheel of a generated control, driven through the one core draft: a drag
//! is one draft of both fields and one commit, Escape restores, the arrow keys draft like a drag,
//! and a double-click runs the wheel's declared reset.
use super::{
    message::{control::ControlMessage, draft::DraftMessage},
    testing::{attach_log, drafting, entry, events, finish, logged, refresh_for},
    *,
};
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

/// A double-click on the wheel runs its declared reset, the patch of exactly its three fields,
/// as one action.
#[test]
fn a_double_click_runs_the_wheels_reset() {
    let (mut editor, catalog, _, _, _, _) = drafting();
    let reset = controls::wheel_of(&editor.modules, ACTION, HUE)
        .and_then(|wheel| controls::wheel_reset(&editor.modules, wheel))
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

/// A real double-click on the disc: the first press moves the handle and opens the wheel's draft,
/// its release sends `draft.commit`, and the second press — the reset — arrives while that commit
/// is still answering. The reset waits rather than being refused, and goes out once, as the one
/// declared action, against the revision the commit produced.
#[test]
fn a_double_click_reset_waits_for_the_first_press_commit() {
    let (mut editor, catalog, log, asset, _, _) = drafting();
    let current = editor
        .document
        .state
        .as_ref()
        .expect("an open asset")
        .current_entry
        .clone();
    let revision = editor
        .document
        .state
        .as_ref()
        .expect("an open asset")
        .revision;
    let wheel = |editor: &mut Editor, event| {
        let _ = editor.update(Message::Control(ControlMessage::Wheel {
            action: ACTION.into(),
            hue: HUE.into(),
            event,
        }));
    };
    turn(&mut editor, 30.0, 0.25);
    wheel(&mut editor, WheelEvent::Release);
    assert_eq!(
        editor
            .core_gesture()
            .and_then(|gesture| gesture.draft.in_flight()),
        Some(draft::Round::Commit),
        "the release's commit is in flight"
    );
    wheel(&mut editor, WheelEvent::Reset);
    let records = logged(&mut editor, &log);
    let queued = events(&records, "field_reset_queued");
    assert_eq!(queued.len(), 1, "the reset waits: {records:?}");
    assert_eq!(queued[0]["revision"], json!(revision));
    assert!(events(&records, "field_reset_sent").is_empty());
    assert!(
        editor.controls.pending_reset.is_some(),
        "the reset is held, not refused: {}",
        editor.status.text
    );

    // The commit answers with the next revision; the reset goes out in the same update.
    let log = attach_log(&mut editor);
    let committed = entry(&asset, current.sequence + 1, Some(&current.id));
    let refresh = refresh_for(&asset, &committed, Vec::new(), &[&committed], false);
    testing::answer_commit(&mut editor, Ok(Some(refresh)));
    let records = logged(&mut editor, &log);
    let sent = events(&records, "field_reset_sent");
    assert_eq!(sent.len(), 1, "sent once: {records:?}");
    assert_eq!(sent[0]["revision"], json!(revision + 1));
    assert_eq!(sent[0]["action"], json!(ACTION));
    assert_eq!(
        sent[0]["preset"],
        json!({HUE: 0.0, SATURATION: 0.0, "wheel-luminance": 0.0})
    );
    assert_eq!(
        sent[0]["field"],
        json!({"action": ACTION, "parameter": HUE})
    );
    assert!(editor.busy && editor.controls.pending_reset.is_none());
    finish(editor, catalog);
}
