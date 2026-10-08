//! The Select workspace in the editor: the switch and its refusal, the keys, the grid laid out from
//! a summary, the rows window over a 10,000-file view, the `browse.select` request each gesture
//! builds and its one synchronous call, a collapsed burst, and a stale view evaluated again after a
//! wake. The owner's `browse.*` and `event.list` methods are answered here with synthetic values;
//! their real answers are exercised once they are registered.
use crate::app::{
    Editor,
    keymap::{KeyContext, keymap},
    message::{
        Message,
        crop::CropMessage,
        draft::DraftMessage,
        select::{SelectMessage, Step},
        sync::SyncMessage,
    },
    select::grid_blocks,
    tasks::owner_calls,
    testing::{boot, finish, opened},
};
use crate::state::select::{SelectGesture, SelectPanel, Shown, grid_content, select_params};
use iced::{
    Event, Size,
    event::Status,
    keyboard::{Event as Keys, Key, Modifiers, key::Named},
};
use luxforge_core::{
    ClientSession,
    catalog_types::{
        DayGroup, EventId, GroupLayout, LibraryChangeSeq, LocalDay, Moment, MomentKind,
        PositionRange, ViewQuery, ViewSelection, ViewSource, ViewSummary,
    },
};
use luxforge_ui::{GridBlock, GridPress, PressModifiers};
use serde_json::json;
use std::collections::BTreeSet;

fn event_source() -> ViewSource {
    ViewSource::Event {
        event_id: EventId::parse(format!("event-{}", "b".repeat(32))).unwrap(),
    }
}

/// A view of `count` frames at revision 7 over one day, with a burst of three at 1..4.
fn summary(count: u32) -> ViewSummary {
    ViewSummary {
        revision: 7,
        query: ViewQuery::of(event_source()),
        count,
        picked: 0,
        in_catalog: 0,
        unavailable: 0,
        groups: GroupLayout {
            days: vec![DayGroup {
                day: LocalDay::from_ymd(2026, 9, 12),
                start: 0,
                len: count,
                picked: 0,
            }],
            cameras: Vec::new(),
            moments: vec![Moment {
                kind: MomentKind::Burst,
                evidence: None,
                steps_ev: Vec::new(),
                span_ms: 1400,
                start: 1,
                len: 3,
                picked: 0,
            }],
        },
        library_sequence: LibraryChangeSeq(1),
        index_revision: 1,
    }
}

/// The editor's session with its view at revision 7, `selected` selected and `active` active.
fn browsing(editor: &Editor, selected: &[(u32, u32)], active: Option<u32>) -> ClientSession {
    let mut session = editor.session.clone();
    session.browse.revision = 7;
    session.browse.count = 20;
    session.browse.selection = ViewSelection {
        count: selected.iter().map(|(_, len)| len).sum(),
        ranges: selected
            .iter()
            .map(|&(start, len)| PositionRange { start, len })
            .collect(),
        active,
    };
    session
}

/// Select shown with `summary` evaluated from choosing its source, the session reporting
/// `selected`, and the grid 900 × 700 points.
fn viewing(
    summary: ViewSummary,
    selected: &[(u32, u32)],
    active: Option<u32>,
) -> (Editor, std::path::PathBuf) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let _ = editor.update(Message::Select(SelectMessage::Source(
        summary.query.source.clone(),
    )));
    assert!(editor.select.state.loading);
    let session = browsing(&editor, selected, active);
    let serial = editor.select.serial;
    let _ = editor.update(Message::Select(SelectMessage::Viewed {
        serial,
        result: Ok(Box::new((summary, session))),
    }));
    let _ = editor.update(Message::Select(SelectMessage::Viewport(Size::new(
        900.0, 700.0,
    ))));
    (editor, catalog)
}

/// An open crop draft refuses the switch with its reason, through the one start refusal, and
/// leaves the draft as it was; once it is cancelled the switch goes both ways, and each workspace
/// keeps what it had.
#[test]
fn the_select_switch_is_refused_by_an_open_draft_and_keeps_each_workspace() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 1);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    assert!(editor.crop().is_some());
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert_eq!(editor.select.state.shown, Shown::Develop);
    assert_eq!(
        editor.status.text,
        "Apply or Cancel the crop draft before switching to Select"
    );
    assert!(editor.crop().is_some(), "the draft is untouched");
    // `G` is the same switch, refused the same way.
    editor.status.text.clear();
    let _ = editor.update(Message::Key(
        pressed(Key::Character("g".into()), Modifiers::empty()),
        Status::Ignored,
    ));
    assert_eq!(editor.select.state.shown, Shown::Develop);
    assert_eq!(
        editor.status.text,
        "Apply or Cancel the crop draft before switching to Select"
    );

    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    let _ = editor.update(Message::Key(
        pressed(Key::Character("g".into()), Modifiers::empty()),
        Status::Ignored,
    ));
    assert!(editor.select_shown());
    assert_eq!(editor.workspace.select.shown, Shown::Select);
    assert!(
        editor.drawn_photo().is_none(),
        "Select does not draw the photograph"
    );
    assert!(
        editor.select.events_read,
        "the events are asked for once shown"
    );
    editor.select.state.search = "Konst".into();
    let _ = editor.update(Message::Select(SelectMessage::TogglePanel(
        SelectPanel::Info,
    )));
    let _ = editor.update(Message::Key(
        pressed(Key::Character("d".into()), Modifiers::empty()),
        Status::Ignored,
    ));
    assert!(!editor.select_shown());
    assert_eq!(editor.workspace.select, Default::default());
    assert_eq!(
        editor.document.state.as_ref().map(|state| &state.asset.id),
        Some(&asset),
        "Develop keeps its photograph"
    );
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert_eq!(editor.select.state.search, "Konst");
    assert!(!editor.select.state.info_panel, "Select keeps its panels");
    finish(editor, catalog);
}

fn pressed(key: Key, modifiers: Modifiers) -> Event {
    held(key, modifiers, false)
}

fn held(key: Key, modifiers: Modifiers, repeat: bool) -> Event {
    Event::Keyboard(Keys::KeyPressed {
        key: key.clone(),
        modified_key: key.clone(),
        physical_key: iced::keyboard::key::Physical::Unidentified(
            iced::keyboard::key::NativeCode::Unidentified,
        ),
        location: iced::keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat,
    })
}

/// With no loaded set or catalog folder, D explains the choice without developing a selection;
/// neither shortcut steals a letter from a text field or acts again when held.
#[test]
fn workspace_keys_respect_empty_develop_set_and_text_capture() {
    for (with_view, active) in [(false, None), (true, None), (true, Some(1))] {
        let (mut editor, catalog) = if with_view {
            viewing(
                summary(20),
                if active.is_some() { &[(1, 1)] } else { &[] },
                active,
            )
        } else {
            boot()
        };
        let press = |letter: &str, status| {
            Message::Key(
                pressed(Key::Character(letter.into()), Modifiers::empty()),
                status,
            )
        };
        let _ = editor.update(press("g", Status::Ignored));
        assert!(editor.select_shown());
        let _ = editor.update(press("d", Status::Captured));
        assert!(editor.select_shown(), "typing must keep Select shown");
        let _ = editor.update(Message::Key(
            held(Key::Character("d".into()), Modifiers::empty(), true),
            Status::Ignored,
        ));
        assert!(editor.select_shown(), "holding must not repeat the action");
        let _ = editor.update(press("d", Status::Ignored));
        assert!(editor.select_shown(), "D needs a folder or loaded set");
        assert!(editor.status.text.contains("Pick a catalog folder"));
        assert!(editor.develop.state.confirm.is_none());
        assert!(!editor.develop.state.planning);
        assert!(
            editor.select.library.is_none(),
            "D must not pick the active frame"
        );
        let _ = editor.update(press("g", Status::Captured));
        assert!(editor.select_shown(), "typing must keep Select shown");
        let _ = editor.update(press("g", Status::Ignored));
        assert!(editor.select_shown(), "G returns to Select");
        finish(editor, catalog);
    }
}

/// Select's keys map to its own messages and none of Develop's reaches it; `G` in Develop shows
/// Select, and `D` in Select returns to Develop.
#[test]
fn the_select_keys_are_its_own() {
    let letter = |value: &str| Key::Character(value.into());
    let develop = KeyContext::default();
    let select = KeyContext {
        select: true,
        ..KeyContext::default()
    };
    let menu = KeyContext {
        select_menu_open: true,
        ..select.clone()
    };
    let loupe = KeyContext {
        loupe_open: true,
        ..select.clone()
    };
    let command = Modifiers::COMMAND;
    let cases: Vec<(&str, Event, Status, &KeyContext, Option<&str>)> = vec![
        (
            "g in Develop",
            pressed(letter("g"), Modifiers::empty()),
            Status::Ignored,
            &develop,
            Some("Select(Switch(Select))"),
        ),
        (
            "g in a field",
            pressed(letter("g"), Modifiers::empty()),
            Status::Captured,
            &develop,
            None,
        ),
        (
            "shift g",
            pressed(letter("G"), Modifiers::SHIFT),
            Status::Ignored,
            &develop,
            None,
        ),
        (
            "g held",
            held(letter("g"), Modifiers::empty(), true),
            Status::Ignored,
            &develop,
            None,
        ),
        (
            "g in Select",
            pressed(letter("g"), Modifiers::empty()),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "g in the loupe",
            pressed(letter("g"), Modifiers::empty()),
            Status::Ignored,
            &loupe,
            Some("Select(Loupe(Close))"),
        ),
        (
            "g in a loupe field",
            pressed(letter("g"), Modifiers::empty()),
            Status::Captured,
            &loupe,
            None,
        ),
        (
            "g held in the loupe",
            held(letter("g"), Modifiers::empty(), true),
            Status::Ignored,
            &loupe,
            None,
        ),
        (
            "d in Select",
            pressed(letter("d"), Modifiers::empty()),
            Status::Ignored,
            &select,
            Some("Select(Switch(Develop))"),
        ),
        (
            "d in Develop",
            pressed(letter("d"), Modifiers::empty()),
            Status::Ignored,
            &develop,
            Some("Select(Switch(Develop))"),
        ),
        (
            "d in the loupe",
            pressed(letter("d"), Modifiers::empty()),
            Status::Ignored,
            &loupe,
            Some("Select(Switch(Develop))"),
        ),
        (
            "shift d",
            pressed(letter("D"), Modifiers::SHIFT),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "right",
            pressed(Key::Named(Named::ArrowRight), Modifiers::empty()),
            Status::Ignored,
            &select,
            Some("Select(Move { step: Right, extend: false })"),
        ),
        (
            "shift up",
            pressed(Key::Named(Named::ArrowUp), Modifiers::SHIFT),
            Status::Ignored,
            &select,
            Some("Select(Move { step: Up, extend: true })"),
        ),
        (
            "held left repeats",
            held(Key::Named(Named::ArrowLeft), Modifiers::empty(), true),
            Status::Ignored,
            &select,
            Some("Select(Move { step: Left, extend: false })"),
        ),
        (
            "down in the search field",
            pressed(Key::Named(Named::ArrowDown), Modifiers::empty()),
            Status::Captured,
            &select,
            None,
        ),
        (
            "select all",
            pressed(letter("a"), command),
            Status::Ignored,
            &select,
            Some("Select(SelectAll)"),
        ),
        (
            "select none",
            pressed(letter("d"), command),
            Status::Ignored,
            &select,
            Some("Select(SelectNone)"),
        ),
        (
            "select all in a field",
            pressed(letter("a"), command),
            Status::Captured,
            &select,
            None,
        ),
        (
            "tab",
            pressed(Key::Named(Named::Tab), Modifiers::empty()),
            Status::Ignored,
            &select,
            Some("Select(TogglePanels)"),
        ),
        (
            "sources panel",
            pressed(letter("["), command | Modifiers::ALT),
            Status::Ignored,
            &select,
            Some("Select(TogglePanel(Sources))"),
        ),
        (
            "info panel",
            pressed(letter("]"), command | Modifiers::ALT),
            Status::Ignored,
            &select,
            Some("Select(TogglePanel(Info))"),
        ),
        (
            "s",
            pressed(letter("s"), Modifiers::empty()),
            Status::Ignored,
            &select,
            Some("Select(Collapse)"),
        ),
        (
            "s held",
            held(letter("s"), Modifiers::empty(), true),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "escape closes a menu",
            pressed(Key::Named(Named::Escape), Modifiers::empty()),
            Status::Captured,
            &menu,
            Some("Select(Menu(None))"),
        ),
        (
            "escape without a menu",
            pressed(Key::Named(Named::Escape), Modifiers::empty()),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "space opens the loupe",
            pressed(Key::Named(Named::Space), Modifiers::empty()),
            Status::Ignored,
            &select,
            Some("Select(Loupe(Open))"),
        ),
        (
            "e opens the loupe",
            pressed(letter("e"), Modifiers::empty()),
            Status::Ignored,
            &select,
            Some("Select(Loupe(Open))"),
        ),
        (
            "e closes the loupe",
            pressed(letter("e"), Modifiers::empty()),
            Status::Ignored,
            &loupe,
            Some("Select(Loupe(Close))"),
        ),
        (
            "space closes the loupe",
            pressed(Key::Named(Named::Space), Modifiers::empty()),
            Status::Ignored,
            &loupe,
            Some("Select(Loupe(Close))"),
        ),
        (
            "escape closes the loupe",
            pressed(Key::Named(Named::Escape), Modifiers::empty()),
            Status::Ignored,
            &loupe,
            Some("Select(Loupe(Close))"),
        ),
        // Develop's keys never act on the photograph Select does not show.
        (
            "fit",
            pressed(letter("f"), Modifiers::empty()),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "clipping",
            pressed(letter("j"), Modifiers::empty()),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "mask",
            pressed(letter("m"), Modifiers::empty()),
            Status::Ignored,
            &select,
            None,
        ),
        // In Select, Cmd+Z and Shift+Cmd+Z are library undo and redo, never the history's.
        (
            "library undo",
            pressed(letter("z"), command),
            Status::Ignored,
            &select,
            Some("Select(Undo)"),
        ),
        (
            "library redo",
            pressed(letter("z"), command | Modifiers::SHIFT),
            Status::Ignored,
            &select,
            Some("Select(Redo)"),
        ),
        (
            "undo held",
            held(letter("z"), command, true),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "p picks",
            pressed(letter("p"), Modifiers::empty()),
            Status::Ignored,
            &select,
            Some("Select(Pick)"),
        ),
        (
            "p in the search field",
            pressed(letter("p"), Modifiers::empty()),
            Status::Captured,
            &select,
            None,
        ),
        (
            "p held",
            held(letter("p"), Modifiers::empty(), true),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "p in the loupe is the loupe's",
            pressed(letter("p"), Modifiers::empty()),
            Status::Ignored,
            &loupe,
            Some("Select(Loupe(Pick))"),
        ),
        // Cmd+O adds a folder in Select, and opens a single file in Develop.
        (
            "add a folder",
            pressed(letter("o"), command),
            Status::Ignored,
            &select,
            Some("Select(AddFolder)"),
        ),
        (
            "add a folder held",
            held(letter("o"), command, true),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "open in Develop",
            pressed(letter("o"), command),
            Status::Ignored,
            &develop,
            Some("Sync(Open)"),
        ),
        (
            "export",
            pressed(letter("e"), command),
            Status::Ignored,
            &select,
            None,
        ),
        (
            "palette",
            pressed(letter("k"), command),
            Status::Ignored,
            &select,
            None,
        ),
    ];
    for (name, event, status, context, expected) in cases {
        let got = keymap(&event, status, context).map(|message| format!("{message:?}"));
        assert_eq!(got.as_deref(), expected, "{name}");
    }
}

/// The grid is laid out from the summary: one item per position, the burst framed, a collapsed
/// burst one cell.
#[test]
fn the_select_grid_is_laid_out_from_the_summary() {
    let content = grid_content(&summary(20), &BTreeSet::from([0]));
    let blocks = grid_blocks(&content);
    assert!(
        matches!(&blocks[0], GridBlock::Day(heading) if heading.title == "Saturday 12 September 2026")
    );
    assert!(matches!(
        &blocks[2],
        GridBlock::Moment {
            frames: 3,
            collapsed: true,
            ..
        }
    ));
    let (editor, catalog) = viewing(summary(20), &[], None);
    let layout = &editor.select.layout;
    assert_eq!(layout.item_count(), 20);
    assert_eq!(layout.cell_count(), 20);
    assert_eq!(layout.width(), 900.0);
    assert_eq!(editor.select.state.rows.revision(), 7);
    assert!(!editor.select.state.loading);
    assert_eq!(editor.workspace.select.note, None);
    finish(editor, catalog);
}

/// A 10,000-file view reads only the block near the screen: scrolled far down, the one request in
/// flight is for the rows there, never the view's start.
#[test]
fn a_10000_file_select_view_reads_only_rows_near_the_screen() {
    let mut flat = summary(10_000);
    flat.groups = GroupLayout::default();
    let (mut editor, catalog) = viewing(flat, &[], None);
    let first = editor
        .select
        .state
        .rows
        .in_flight()
        .map(|request| request.from);
    assert_eq!(first, Some(0), "the first screen is read first");
    // The owner refuses that block; then the grid is scrolled far down.
    let _ = editor.update(Message::Select(SelectMessage::Rows {
        revision: 7,
        from: 0,
        result: Err("validation: unknown method browse.rows".into()),
    }));
    assert!(editor.status.text.starts_with("Rows unavailable"));
    let far = editor.select.layout.height() * 0.6;
    let _ = editor.update(Message::Select(SelectMessage::Scrolled(far)));
    let layout = &editor.select.layout;
    let visible = layout.visible_cells(editor.select.scroll, 700.0, 700.0);
    let expected = layout.cell(visible.start).item / crate::state::select::ROW_BLOCK;
    assert!(expected > 20, "far into the view: block {expected}");
    assert_eq!(
        editor
            .select
            .state
            .rows
            .in_flight()
            .map(|request| request.from),
        Some(expected * crate::state::select::ROW_BLOCK)
    );
    assert_eq!(editor.select.state.rows.len(), 0, "nothing else was read");
    finish(editor, catalog);
}

fn click(item: u32, shift: bool, command: bool) -> GridPress {
    GridPress {
        cell: item,
        item,
        span: 1,
        modifiers: PressModifiers { shift, command },
        double: false,
    }
}

/// Each gesture builds the request an independent JSON client would send against the view's
/// revision, and a click sends it once, synchronously, in its own update.
#[test]
fn every_select_gesture_builds_the_request_an_api_client_would_send() {
    let (mut editor, catalog) = viewing(summary(20), &[(4, 1)], Some(4));
    let request = |gesture: SelectGesture| select_params(gesture, Some(7));
    assert_eq!(
        request(editor.press_gesture(&click(9, false, false))),
        json!({"mode": "replace", "range": {"start": 9, "len": 1}, "active": 9, "revision": 7})
    );
    assert_eq!(
        request(editor.press_gesture(&click(4, false, true))),
        json!({"mode": "toggle", "range": {"start": 4, "len": 1}, "revision": 7}),
        "Cmd-click on the selected cell takes it out"
    );
    assert_eq!(
        request(editor.press_gesture(&click(9, false, true))),
        json!({"mode": "toggle", "range": {"start": 9, "len": 1}, "active": 9, "revision": 7})
    );
    assert_eq!(
        request(editor.press_gesture(&click(12, true, false))),
        json!({"mode": "replace", "range": {"start": 4, "len": 9}, "active": 12, "revision": 7}),
        "Shift-click extends from the active item"
    );
    let (gesture, target) = editor.move_gesture(Step::Right, false).unwrap();
    assert_eq!(target, (5, 1));
    assert_eq!(
        request(gesture),
        json!({"mode": "replace", "range": {"start": 5, "len": 1}, "active": 5, "revision": 7})
    );
    let (gesture, _) = editor.move_gesture(Step::Left, true).unwrap();
    assert_eq!(
        request(gesture),
        json!({"mode": "replace", "range": {"start": 3, "len": 2}, "active": 3, "revision": 7})
    );
    // The grid draws the session's selection.
    assert!(editor.workspace.select.selection.selected(4, 1));
    assert_eq!(editor.workspace.select.selection.active, Some(4));

    // A click, an arrow, Cmd+A and Cmd+D each make exactly one `browse.select` call, in their own
    // update.
    owner_calls::take();
    for message in [
        SelectMessage::Press(click(9, false, false)),
        SelectMessage::Move {
            step: Step::Down,
            extend: false,
        },
        SelectMessage::SelectAll,
        SelectMessage::SelectNone,
    ] {
        let _ = editor.update(Message::Select(message));
        assert_eq!(owner_calls::take(), vec!["browse.select".to_owned()]);
    }
    finish(editor, catalog);
}

/// With nothing active the first arrow selects the first cell; `S` collapses the active burst to
/// one cell and expands it again.
#[test]
fn select_arrows_start_at_the_first_cell_and_s_collapses_the_active_burst() {
    let (mut editor, catalog) = viewing(summary(20), &[], None);
    let (gesture, target) = editor.move_gesture(Step::Down, false).unwrap();
    assert_eq!(target, (0, 1));
    assert_eq!(gesture, SelectGesture::Only { item: 0, span: 1 });
    // The owner reports frame 2 of the burst active.
    let session = browsing(&editor, &[(2, 1)], Some(2));
    editor.adopt(session);
    let _ = editor.update(Message::Select(SelectMessage::Collapse));
    assert_eq!(editor.select.state.collapsed, BTreeSet::from([0]));
    assert_eq!(editor.select.layout.cell_count(), 18);
    assert!(editor.workspace.select.selection.active_in(1, 3));
    let _ = editor.update(Message::Select(SelectMessage::Collapse));
    assert!(editor.select.state.collapsed.is_empty());
    assert_eq!(editor.select.layout.cell_count(), 20);
    // A frame outside a burst has nothing to collapse.
    editor.adopt(browsing(&editor, &[(6, 1)], Some(6)));
    let _ = editor.update(Message::Select(SelectMessage::Collapse));
    assert!(editor.select.state.collapsed.is_empty());
    finish(editor, catalog);
}

/// A view evaluated again keeps its scroll: moved as little as keeps the active item in view when
/// it was on screen, and left where it was when the grid had been scrolled away from it.
#[test]
fn a_select_view_read_again_keeps_its_scroll() {
    let mut flat = summary(2000);
    flat.groups = GroupLayout::default();
    let (mut editor, catalog) = viewing(flat.clone(), &[(0, 1)], Some(0));
    let height = editor.select.viewport.height;
    let far = editor.select.layout.height() * 0.5;
    let again = |editor: &mut Editor| {
        let _ = editor.update(Message::Select(SelectMessage::Source(
            flat.query.source.clone(),
        )));
        let session = browsing(editor, &[(0, 1)], Some(0));
        let serial = editor.select.serial;
        let _ = editor.update(Message::Select(SelectMessage::Viewed {
            serial,
            result: Ok(Box::new((flat.clone(), session))),
        }));
    };
    // Scrolled away from the active first cell: the scroll stays.
    let _ = editor.update(Message::Select(SelectMessage::Scrolled(far)));
    let scrolled = editor.select.scroll;
    assert!(scrolled > height, "{scrolled}");
    again(&mut editor);
    assert_eq!(editor.select.scroll, scrolled);
    // With the active cell on screen, it stays on screen.
    let _ = editor.update(Message::Select(SelectMessage::Scrolled(0.0)));
    again(&mut editor);
    assert_eq!(
        editor.select.scroll,
        editor.select.layout.reveal(0, 0.0, height)
    );
    finish(editor, catalog);
}

/// The owner's wake reads the session while Select is shown; a stale view is evaluated again with
/// the query it was evaluated from, and the events read again. A wake while Develop is shown waits
/// for Select to be shown.
#[test]
fn a_stale_select_view_is_evaluated_again_after_a_wake() {
    let (mut editor, catalog) = viewing(summary(20), &[], None);
    let _ = editor.update(Message::Select(SelectMessage::Events(Ok(
        Default::default(),
    ))));
    assert!(!editor.select.events.in_flight());
    let serial = editor.select.serial;
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    assert!(editor.select.check.in_flight(), "the session is read");
    let mut stale = browsing(&editor, &[], None);
    stale.browse.stale = true;
    let _ = editor.update(Message::Select(SelectMessage::Checked(Ok(Box::new(stale)))));
    assert!(!editor.select.check.in_flight());
    assert!(editor.session.browse.stale);
    assert_eq!(editor.select.serial, serial + 1, "evaluated again");
    assert!(editor.select.state.loading);
    assert_eq!(
        editor.select.state.query.as_ref(),
        Some(&summary(20).query),
        "with the query it was evaluated from"
    );
    assert!(
        editor.select.events.in_flight(),
        "the events are read again"
    );
    // The same source evaluated again keeps the scroll near the active item.
    editor.select.scroll = 30.0;
    let session = browsing(&editor, &[(15, 1)], Some(15));
    let _ = editor.update(Message::Select(SelectMessage::Viewed {
        serial: serial + 1,
        result: Ok(Box::new((summary(20), session))),
    }));
    let reveal = editor.select.layout.reveal(15, 30.0, 700.0);
    assert_eq!(editor.select.scroll, reveal);

    // While Develop is shown a wake reads nothing until Select is shown again. This scene
    // supplies no development set, so set up that workspace directly.
    drop(editor.show_develop_workspace());
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    assert!(!editor.select.check.in_flight());
    assert!(editor.select.check_on_show);
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert!(editor.select.check.in_flight());
    finish(editor, catalog);
}

/// A method the owner does not have yet answers `validation`: the status bar says so, the panel and
/// the centre say what is unavailable, and the workspace stays usable.
#[test]
fn select_methods_the_owner_refuses_leave_the_workspace_usable() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let _ = editor.update(Message::Select(SelectMessage::Events(Err(
        "validation: unknown method event.list".into(),
    ))));
    assert_eq!(
        editor.status.text,
        "Events unavailable: validation: unknown method event.list"
    );
    assert_eq!(
        editor.workspace.select.sources.events_note.as_deref(),
        Some("Events unavailable")
    );
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::AllPhotographs,
    )));
    let serial = editor.select.serial;
    let _ = editor.update(Message::Select(SelectMessage::Viewed {
        serial,
        result: Err("validation: unknown method browse.view".into()),
    }));
    assert!(!editor.select.state.loading);
    assert_eq!(
        editor.workspace.select.note.as_deref(),
        Some("View unavailable: validation: unknown method browse.view")
    );
    assert_eq!(
        editor.status.text,
        "View unavailable: validation: unknown method browse.view"
    );
    // A press has no view to select in, so nothing is sent; the panels and the switch still work.
    owner_calls::take();
    let _ = editor.update(Message::Select(SelectMessage::SelectAll));
    assert!(owner_calls::take().is_empty());
    let _ = editor.update(Message::Select(SelectMessage::TogglePanels));
    assert!(!editor.select.state.sources_panel && !editor.select.state.info_panel);
    assert!(
        !editor.left_panel_shown(),
        "the Performance section stops sampling with its panel"
    );
    let _ = editor.update(Message::Select(SelectMessage::TogglePanels));
    assert!(editor.left_panel_shown());
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Develop)));
    assert!(editor.select_shown());
    assert!(editor.status.text.contains("Pick a catalog folder"));
    // An evaluation overtaken by a newer one is dropped.
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let _ = editor.update(Message::Select(SelectMessage::Source(ViewSource::Removed)));
    let session = editor.session.clone();
    let _ = editor.update(Message::Select(SelectMessage::Viewed {
        serial,
        result: Ok(Box::new((summary(20), session))),
    }));
    assert!(
        editor.select.state.summary.is_none(),
        "an older evaluation's answer"
    );
    finish(editor, catalog);
}
