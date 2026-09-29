//! History selection, compare and navigation: a selection of the current entry returns to current,
//! compare restores the selection it replaced, and an open draft refuses Undo, Redo and Restore.
use super::{
    message::{crop::CropMessage, draft::DraftMessage, history::HistoryMessage, sync::SyncMessage},
    testing::{
        boot, descriptors, entry, finish, open_crop, opened, patch_control, refresh_for, stand_in,
    },
    *,
};
use luxforge_core::{AssetId, HistoryRow, HistorySelection};

#[test]
fn compare_remembers_the_selection_it_replaced() {
    let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 1);
    let original = luxforge_core::EntryId::new();
    editor.document.original_entry = Some(original.clone());
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(
        editor.document.compare_return,
        Some(HistorySelection::Current),
        "the selection Compare replaced is remembered"
    );
    let _ = editor.update(Message::History(HistoryMessage::CompareEnd));
    assert!(editor.document.compare_return.is_none());
    // From a historical preview Compare returns to that entry, not to current.
    editor.session.preview.selection = HistorySelection::Entry(entry_id.clone());
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(
        editor.document.compare_return,
        Some(HistorySelection::Entry(entry_id))
    );
    let _ = std::hint::black_box(&asset);
    finish(editor, catalog);
}

/// Shift+\ holds the whole, uncropped Original through the same hold: it remembers the selection
/// it replaced, a second press while held changes nothing, and the one release ends either hold.
#[test]
fn the_uncropped_compare_is_the_same_hold() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    editor.document.original_entry = Some(luxforge_core::EntryId::new());
    let _ = editor.update(Message::History(HistoryMessage::CompareUncropped));
    assert_eq!(
        editor.document.compare_return,
        Some(HistorySelection::Current)
    );
    assert!(editor.workspace.title.compare_held);
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(
        editor.document.compare_return,
        Some(HistorySelection::Current),
        "one hold at a time"
    );
    let _ = editor.update(Message::History(HistoryMessage::CompareEnd));
    assert!(editor.document.compare_return.is_none());
    finish(editor, catalog);
}

/// Compare selects the Original entry, which would pause an open draft. The rule is simple and
/// explicit: it is refused with a reason, and the release that follows a refused hold does
/// nothing at all rather than restoring a selection Compare never took.
#[test]
fn compare_is_refused_while_a_crop_draft_is_open() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    editor.document.original_entry = Some(luxforge_core::EntryId::new());
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let selection = editor.session.preview.selection.clone();

    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert!(
        editor.document.compare_return.is_none(),
        "no compare hold was taken: {}",
        editor.status.text
    );
    assert!(
        editor
            .status
            .text
            .contains("Apply or Cancel the crop draft"),
        "{}",
        editor.status.text
    );
    assert!(editor.crop().is_some(), "the draft is untouched");
    assert_eq!(editor.session.preview.selection, selection);
    assert!(!editor.workspace.title.compare_held);

    // The release of a refused hold changes nothing.
    let _ = editor.update(Message::History(HistoryMessage::CompareEnd));
    assert!(editor.document.compare_return.is_none());
    assert_eq!(editor.session.preview.selection, selection);
    assert!(!editor.busy, "nothing was sent");

    // With the draft gone, Compare works as before and reaches the title bar model.
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(
        editor.document.compare_return,
        Some(HistorySelection::Current)
    );
    assert!(editor.workspace.title.compare_held);
    assert_eq!(editor.snapshot()["compare"], json!(true));
    finish(editor, catalog);
}

#[test]
fn selecting_the_current_entry_returns_to_current_instead_of_previewing() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 1);
    let _ = editor.update(Message::History(HistoryMessage::Select(entry_id)));
    assert!(editor.busy);
    assert!(
        editor.status.text.starts_with("Returning to current"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// During a historical preview the status bar names the entry by its sequence, the panel keeps
/// Return to current and Restore, and the tools panel stays visible with nothing runnable.
#[test]
fn a_historical_preview_names_the_entry_and_keeps_the_panels_visible() {
    let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 4);
    let older = entry(&asset, 2, None);
    editor
        .document
        .history
        .entries
        .push(HistoryRow::from(&older));
    // The current state says what last happened, in words: no entry, snapshot or source identity.
    let current = editor.displayed_status(&older.id);
    assert!(!current.starts_with("Previewing"), "{current}");
    for identity in [entry_id.as_str(), older.id.as_str(), "snapshot", "source"] {
        assert!(!current.contains(identity), "{current}");
    }

    editor.session.preview.selection = HistorySelection::Entry(older.id.clone());
    assert_eq!(
        editor.displayed_status(&older.id),
        format!("Previewing entry 2 \u{b7} {}", older.label)
    );
    // An entry the loaded page does not hold is still reported, without inventing a number.
    assert_eq!(
        editor.displayed_status(&luxforge_core::EntryId::new()),
        "Previewing history"
    );

    editor.rederive();
    assert!(
        editor.workspace.title.tools_panel_open,
        "the tools panel stays visible during a preview"
    );
    assert_eq!(
        editor.workspace.panel.preview,
        Some(crate::state::panel::PreviewControls {
            can_return: true,
            can_restore: true
        })
    );
    let section = editor
        .workspace
        .tools
        .all()
        .next()
        .expect("the crop section");
    assert!(!section.enabled && section.reset.is_some());
    let _ = std::hint::black_box(&entry_id);
    finish(editor, catalog);
}

/// An editor on a photograph whose current entry has an entry to undo to and one to redo, with
/// every built-in module, so a slider and the crop can each open a draft on it.
fn navigable() -> (Editor, std::path::PathBuf, AssetId, luxforge_core::EntryId) {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    // The owner does not hold this photograph, so its draft requests are answered by the stand-in.
    stand_in(&mut editor);
    let asset = AssetId::new();
    let original = entry(&asset, 0, None);
    let current = entry(&asset, 1, Some(&original.id));
    let mut refresh = refresh_for(
        &asset,
        &current,
        vec![current.clone(), original.clone()],
        &[&current, &original],
        false,
    );
    refresh.state.redo.push(luxforge_core::EntryId::new());
    refresh.recipe = crate::app::testing::described_at(&current, crate::app::testing::CROP_SOURCE);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    assert!(editor.workspace.title.can_undo && editor.workspace.title.can_redo);
    (editor, catalog, asset, original.id)
}

/// Undo, Redo and Restore, each by the message every route to it sends — the title bar, Cmd+Z,
/// the palette and the panel's Restore — are refused with `reason` and send nothing, and the title
/// bar offers neither Undo nor Redo. Restore is tried from a historical preview of `entry`.
pub(super) fn history_refused(editor: &mut Editor, entry: &luxforge_core::EntryId, reason: &str) {
    assert!(
        !editor.workspace.title.can_undo && !editor.workspace.title.can_redo,
        "the title bar offers neither Undo nor Redo during a draft"
    );
    let held = editor.gesture.clone().map(|gesture| format!("{gesture:?}"));
    let revision = editor.document.state.as_ref().map(|state| state.revision);
    let selection = editor.session.preview.selection.clone();
    for (name, message) in [
        ("undo", HistoryMessage::Undo),
        ("redo", HistoryMessage::Redo),
        ("restore", HistoryMessage::Restore),
    ] {
        if name == "restore" {
            editor.session.preview.selection = HistorySelection::Entry(entry.clone());
        }
        editor.status.text.clear();
        assert!(!editor.busy, "{name}: nothing in flight before it");
        let _ = editor.update(Message::History(message));
        assert!(!editor.busy, "{name}: nothing was sent");
        assert_eq!(editor.status.text, reason, "{name}");
        assert_eq!(
            editor.gesture.clone().map(|gesture| format!("{gesture:?}")),
            held,
            "{name}: the draft is untouched"
        );
        assert_eq!(
            editor.document.state.as_ref().map(|state| state.revision),
            revision
        );
    }
    editor.session.preview.selection = selection;
}

/// Undo, Redo and Restore commit at once, so an open slider draft refuses them as it refuses every
/// other discrete commit; once the draft is gone they go out as before.
#[test]
fn history_navigation_is_refused_while_a_slider_draft_is_open() {
    let (mut editor, catalog, _, original) = navigable();
    let (action, parameter) = patch_control(&editor);
    let _ = testing::slide(&mut editor, &action, &parameter, 25.0);
    assert!(editor.slider_gesture().is_some());
    history_refused(
        &mut editor,
        &original,
        "Finish or discard the slider draft before undoing, redoing or restoring",
    );

    editor.gesture = None;
    editor.rederive();
    assert!(editor.workspace.title.can_undo && editor.workspace.title.can_redo);
    let _ = editor.update(Message::History(HistoryMessage::Undo));
    assert!(
        editor.busy,
        "with no draft Undo goes out: {}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// Undo, Redo and Restore also wait for a request in flight, and say so rather than being dropped
/// without a word; Restore still runs from the previewed entry it restores, since History never
/// takes the editable half.
#[test]
fn history_navigation_is_refused_while_a_request_is_in_flight() {
    let (mut editor, catalog, _, original) = navigable();
    editor.busy = true;
    let sequence = editor.sync.sequence;
    for message in [
        HistoryMessage::Undo,
        HistoryMessage::Redo,
        HistoryMessage::Restore,
    ] {
        editor.session.preview.selection = HistorySelection::Entry(original.clone());
        editor.status.text.clear();
        let task = editor.update(Message::History(message.clone()));
        assert_eq!(task.units(), 0, "{message:?}: nothing is sent");
        assert_eq!(editor.status.text, crate::state::IN_FLIGHT, "{message:?}");
    }
    assert_eq!(editor.sync.sequence, sequence);

    editor.busy = false;
    let _ = editor.update(Message::History(HistoryMessage::Restore));
    assert!(
        editor.busy,
        "Restore runs from the previewed entry: {}",
        editor.status.text
    );
    finish(editor, catalog);
}

/// The same for a crop draft; once it is cancelled, Redo goes out as before.
#[test]
fn history_navigation_is_refused_while_a_crop_draft_is_open() {
    let (mut editor, catalog, _, original) = navigable();
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    assert!(editor.crop().is_some());
    history_refused(
        &mut editor,
        &original,
        "Apply or Cancel the crop draft before undoing, redoing or restoring",
    );

    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    let _ = editor.update(Message::History(HistoryMessage::Redo));
    assert!(
        editor.busy,
        "with no draft Redo goes out: {}",
        editor.status.text
    );
    finish(editor, catalog);
}
