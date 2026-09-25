//! History selection and compare: a selection of the current entry returns to current, and compare
//! restores the selection it replaced.
use super::{
    message::{CropMessage, HistoryMessage},
    tasks::Upload,
    testing::{entry, finish, opened},
    *,
};
use lightwell_core::{CropStage, HistoryRow};

#[test]
fn compare_remembers_the_selection_it_replaced() {
    let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 1);
    let original = lightwell_core::EntryId::new();
    editor.original_entry = Some(original.clone());
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(
        editor.compare_return,
        Some(HistorySelection::Current),
        "the selection Compare replaced is remembered"
    );
    let _ = editor.update(Message::History(HistoryMessage::CompareEnd));
    assert!(editor.compare_return.is_none());
    // From a historical preview Compare returns to that entry, not to current.
    editor.session.preview.selection = HistorySelection::Entry(entry_id.clone());
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(
        editor.compare_return,
        Some(HistorySelection::Entry(entry_id))
    );
    let _ = std::hint::black_box(&asset);
    finish(editor, catalog);
}

/// Compare selects the Original entry, which would pause an open draft. The rule is simple and
/// explicit: it is refused with a reason, and the release that follows a refused hold does
/// nothing at all rather than restoring a selection Compare never took.
#[test]
fn compare_is_refused_while_a_crop_draft_is_open() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    editor.original_entry = Some(lightwell_core::EntryId::new());
    let _ = editor.update(Message::Crop(CropMessage::Start));
    editor.open_draft(CropStage {
        width: 480,
        height: 320,
        angle: 0.0,
    });
    let selection = editor.session.preview.selection.clone();

    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert!(
        editor.compare_return.is_none(),
        "no compare hold was taken: {}",
        editor.status
    );
    assert!(
        editor.status.contains("Apply or Cancel the crop draft"),
        "{}",
        editor.status
    );
    assert!(editor.crop().is_some(), "the draft is untouched");
    assert_eq!(editor.session.preview.selection, selection);
    assert!(!editor.workspace.title.compare_held);

    // The release of a refused hold changes nothing.
    let _ = editor.update(Message::History(HistoryMessage::CompareEnd));
    assert!(editor.compare_return.is_none());
    assert_eq!(editor.session.preview.selection, selection);
    assert!(!editor.busy, "nothing was sent");

    // With the draft gone, Compare works as before and reaches the title bar model.
    let _ = editor.update(Message::Crop(CropMessage::Cancel));
    let _ = editor.update(Message::History(HistoryMessage::CompareBegin));
    assert_eq!(editor.compare_return, Some(HistorySelection::Current));
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
        editor.status.starts_with("Returning to current"),
        "{}",
        editor.status
    );
    finish(editor, catalog);
}

/// During a historical preview the status bar names the entry by its sequence, the panel keeps
/// Return to current and Restore, and the tools panel stays visible with nothing runnable.
#[test]
fn a_historical_preview_names_the_entry_and_keeps_the_panels_visible() {
    let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 4);
    let older = entry(&asset, 2, None);
    editor.history.entries.push(HistoryRow::from(&older));
    let upload = Upload {
        generation: 1,
        draft_revision: None,
        width: 480,
        height: 320,
        entry_id: older.id.clone(),
        snapshot_id: "snapshot-1".into(),
        source_fingerprint: "source-1".into(),
        proxy: false,
        proxy_dimensions: None,
        proxy_built: false,
        proxy_approximation: lightwell_core::ProxyApproximation::default(),
        approximate_white_balance: false,
        reason: None,
        render_ms: Some(3.0),
    };
    assert!(
        editor.displayed_status(&upload).starts_with("Current · "),
        "the current state is not a preview"
    );

    editor.session.preview.selection = HistorySelection::Entry(older.id.clone());
    assert!(
        editor
            .displayed_status(&upload)
            .starts_with("Previewing entry 2 · "),
        "{}",
        editor.displayed_status(&upload)
    );
    // An entry the loaded page does not hold is still reported, without inventing a number.
    let unknown = Upload {
        entry_id: lightwell_core::EntryId::new(),
        ..upload
    };
    assert!(
        editor
            .displayed_status(&unknown)
            .starts_with("Previewing history · ")
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
