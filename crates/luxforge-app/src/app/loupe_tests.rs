//! The loupe against a real owner over the Select tests' seeded catalog (a Nikon burst of three,
//! singles, a Leica bracket of three, and a second day of singles): opened over the grid, the keys
//! the loupe answers, stepping frames and moments and jumping within a moment through the session's
//! own selection, the look-ahead wanted in the direction of travel, compare and the focus check,
//! `P`'s hook and P7, and back to the grid.
use super::*;
use crate::app::select_owner_tests::{evaluate, finish, read_rows, selecting};
use crate::state::select::SourcePress;
use iced::{Size, event::Status, keyboard::Event as KeyEvent, keyboard::key::Physical};
use luxforge_core::catalog_types::MomentKind;
use std::path::PathBuf;

/// The editor with the seeded event viewed, every row read.
fn viewing() -> (Editor, PathBuf) {
    let (mut editor, catalog) = selecting();
    let SourcePress::View(source) = editor.workspace.select.sources.months[0].rows[0]
        .press
        .clone()
    else {
        panic!("an event row views its event");
    };
    let _ = editor.update(Message::Select(SelectMessage::Source(source)));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        1000.0, 700.0,
    ))));
    evaluate(&mut editor);
    read_rows(&mut editor);
    (editor, catalog)
}

fn send(editor: &mut Editor, message: LoupeMessage) {
    let _ = editor.update(loupe_message(message));
}

fn active(editor: &Editor) -> Option<u32> {
    editor.session.browse.selection.active
}

/// The first frame of the view's moment of `kind`, and its length.
fn moment(editor: &Editor, kind: MomentKind) -> (u32, u32) {
    let summary = editor.select.state.summary.as_ref().expect("a view");
    let moment = summary
        .groups
        .moments
        .iter()
        .find(|moment| moment.kind == kind)
        .unwrap_or_else(|| panic!("a {kind:?} in {:?}", summary.groups.moments));
    (moment.start, moment.len)
}

/// A key pressed through the key table, as the keyboard sends it.
fn key(editor: &Editor, key: Key, repeat: bool) -> Option<String> {
    let event = iced::Event::Keyboard(KeyEvent::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        location: iced::keyboard::Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat,
    });
    crate::app::keymap::keymap(&event, Status::Ignored, &editor.key_context())
        .map(|message| format!("{message:?}"))
}

/// The loupe opens over the view's first frame when none is active; the arrows step the view's
/// frames and moments through `browse.select`, each in its own update, as the session reports
/// them; `1`–`9` jump within a moment only; the look-ahead follows the direction of travel.
#[test]
fn loupe_steps_frames_and_moments_through_the_session() {
    let (mut editor, catalog) = viewing();
    let (burst, burst_len) = moment(&editor, MomentKind::Burst);
    let (bracket, bracket_len) = moment(&editor, MomentKind::Bracket);
    assert_eq!((burst_len, bracket_len), (3, 3));
    send(&mut editor, LoupeMessage::Open);
    assert!(editor.loupe_open());
    assert_eq!(active(&editor), Some(0), "the view's first frame, selected");
    let model = &editor.workspace.select.loupe;
    assert_eq!(model.subject.unwrap().position, 0);
    // The keys the loupe answers while it is open.
    for (pressed, expected) in [
        (Key::Named(Named::ArrowRight), "Frame(Forward)"),
        (Key::Named(Named::ArrowLeft), "Frame(Back)"),
        (Key::Named(Named::ArrowDown), "Moment(Forward)"),
        (Key::Named(Named::ArrowUp), "Moment(Back)"),
        (Key::Character("3".into()), "Jump(2)"),
        (Key::Character("z".into()), "ToggleFocus"),
        (Key::Character("c".into()), "ToggleCompare"),
        (Key::Character("p".into()), "Pick"),
    ] {
        assert_eq!(
            key(&editor, pressed, false).as_deref(),
            Some(format!("Select(Loupe({expected}))").as_str())
        );
    }
    assert!(
        key(&editor, Key::Named(Named::ArrowRight), true).is_some(),
        "a held arrow repeats"
    );
    assert_eq!(key(&editor, Key::Character("z".into()), true), None);
    assert_eq!(key(&editor, Key::Character("0".into()), false), None);

    // Down to the burst: every stop on the way is a moment or a single frame.
    let mut stops = Vec::new();
    while active(&editor) != Some(burst) {
        send(&mut editor, LoupeMessage::Moment(Travel::Forward));
        stops.push(active(&editor).unwrap());
        assert!(stops.len() < 12, "never reached the burst: {stops:?}");
    }
    let session = crate::app::select::session_now(&editor.owner, editor.client).unwrap();
    assert_eq!(
        session.browse.selection.active,
        Some(burst),
        "the owner's own"
    );
    let model = &editor.workspace.select.loupe;
    assert!(
        model.info.moment.ends_with("\u{b7} burst"),
        "{:?}",
        model.info
    );
    assert!(
        model.info.frame.starts_with("Frame 1 of 3"),
        "{:?}",
        model.info
    );
    assert_eq!(model.strip.as_ref().unwrap().frames.len(), 3);
    assert_eq!(model.frame.as_ref().unwrap().name, "DSC_0001.NEF");
    // The look-ahead: the next three frames, which reach the next moment's first, nearest first.
    let wants = editor.loupe_wants();
    let positions: Vec<u32> = model::wanted(&editor.select.state, &editor.session.browse)
        .iter()
        .map(|frame| frame.position)
        .collect();
    assert_eq!(positions, vec![burst, burst + 1, burst + 2, burst + 3]);
    assert!(wants[0].shown && wants[1..].iter().all(|want| !want.shown));
    // Frames across the burst's end, and a jump back within it.
    send(&mut editor, LoupeMessage::Frame(Travel::Forward));
    send(&mut editor, LoupeMessage::Frame(Travel::Forward));
    assert_eq!(active(&editor), Some(burst + 2));
    assert!(
        editor
            .workspace
            .select
            .loupe
            .info
            .frame
            .starts_with("Frame 3 of 3 \u{b7} +0.60 s"),
        "{:?}",
        editor.workspace.select.loupe.info
    );
    send(&mut editor, LoupeMessage::Jump(0));
    assert_eq!(
        active(&editor),
        Some(burst),
        "1 jumps to the moment's first frame"
    );
    send(&mut editor, LoupeMessage::Jump(5));
    assert_eq!(
        active(&editor),
        Some(burst),
        "past the moment's frames: nothing"
    );
    send(&mut editor, LoupeMessage::Frame(Travel::Back));
    assert_eq!(editor.select.state.loupe.travel, Travel::Back);
    let back: Vec<u32> = model::wanted(&editor.select.state, &editor.session.browse)
        .iter()
        .map(|frame| frame.position)
        .collect();
    assert!(back.windows(2).all(|pair| pair[0] > pair[1]), "{back:?}");
    // Up to the bracket and back down: moments either way.
    while active(&editor) != Some(bracket) {
        send(&mut editor, LoupeMessage::Moment(Travel::Forward));
    }
    assert!(
        editor
            .workspace
            .select
            .loupe
            .info
            .moment
            .ends_with("\u{b7} bracket")
    );
    send(&mut editor, LoupeMessage::Moment(Travel::Back));
    assert!(active(&editor) < Some(bracket));
    // Esc: back to the grid on the active frame, the loupe's frames released.
    let at = active(&editor);
    send(&mut editor, LoupeMessage::Close);
    assert!(!editor.loupe_open());
    assert_eq!(
        active(&editor),
        at,
        "the grid shows the frame the loupe left"
    );
    assert_eq!(editor.select.loupe.frames.summary()["wanted"], 0);
    finish(editor, catalog);
}

/// Compare shows up to four frames of a moment and nothing for a single, which the status bar
/// says; the focus check asks for the rectangle under the pointer in the frame's upright header
/// size, and the two are exclusive. `P` picks nothing until lane D's picks land, and says so;
/// P7 moves on from a picked burst frame to the next moment, and not from a bracket's.
#[test]
fn loupe_compares_checks_focus_and_moves_on_after_a_burst_pick() {
    let (mut editor, catalog) = viewing();
    let (burst, _) = moment(&editor, MomentKind::Burst);
    let (bracket, _) = moment(&editor, MomentKind::Bracket);
    send(&mut editor, LoupeMessage::Open);
    // A single frame has nothing to compare.
    let single = (0..12)
        .find(|at| {
            let summary = editor.select.state.summary.as_ref().unwrap();
            unit_of(summary, *at).moment.is_none()
        })
        .unwrap();
    assert!(editor.loupe_select(single));
    send(&mut editor, LoupeMessage::ToggleCompare);
    assert!(!editor.select.state.loupe.compare);
    assert!(
        editor.status.text.contains("single frame"),
        "{}",
        editor.status.text
    );
    // The burst's three frames side by side, the active one marked, each its own frame.
    assert!(editor.loupe_select(burst + 1));
    send(&mut editor, LoupeMessage::ToggleCompare);
    let compare = &editor.workspace.select.loupe.compare;
    assert_eq!(compare.len(), 3);
    assert_eq!(
        compare.iter().map(|cell| cell.position).collect::<Vec<_>>(),
        vec![burst, burst + 1, burst + 2]
    );
    assert!(compare[1].active && !compare[0].active);
    assert_eq!(
        editor
            .loupe_wants()
            .iter()
            .filter(|want| want.shown)
            .count(),
        3
    );
    // Z: the focus check replaces compare; its rectangle is in the header's 6000 × 4000 frame.
    send(&mut editor, LoupeMessage::ToggleFocus);
    assert!(editor.select.state.loupe.focus && !editor.select.state.loupe.compare);
    let request = editor
        .loupe_region()
        .expect("a rectangle under the pointer");
    assert_eq!(request.frame.width, 6000);
    assert_eq!(
        request.item,
        editor.workspace.select.loupe.frame.as_ref().unwrap().item
    );
    assert!(editor.select.loupe.focus.pending(), "the region is out");
    send(&mut editor, LoupeMessage::Pointer(Some((0.1, 0.1))));
    let moved = editor.loupe_region().unwrap();
    assert!(moved.rect.x < request.rect.x, "it follows the pointer");
    assert!(!editor.loupe_settled(), "the region has not landed");
    send(&mut editor, LoupeMessage::ToggleFocus);
    assert!(editor.loupe_region().is_none());
    // P: lane D's pick is not on this branch; nothing moves or changes.
    send(&mut editor, LoupeMessage::Pick);
    assert_eq!(active(&editor), Some(burst + 1));
    assert!(editor.status.text.contains("not yet available"));
    // P7, as the pick's answer calls it.
    editor.loupe_picked(&[burst + 1], true, true);
    assert_eq!(
        active(&editor),
        Some(burst + 3),
        "the next moment's first frame"
    );
    assert!(editor.loupe_select(bracket + 1));
    editor.loupe_picked(&[bracket + 1], true, true);
    assert_eq!(
        active(&editor),
        Some(bracket + 1),
        "a bracket's frame stays"
    );
    assert!(editor.loupe_select(burst));
    editor.loupe_picked(&[burst], false, true);
    editor.loupe_picked(&[burst], true, false);
    editor.loupe_picked(&[burst + 2], true, true);
    assert_eq!(
        active(&editor),
        Some(burst),
        "a clear, a failure, another frame's pick"
    );
    finish(editor, catalog);
}
