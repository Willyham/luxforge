//! Owner answers: which the desktop adopts, what a commit reads back, and answers overtaken by a
//! newer selection or revision dropped on arrival.
use super::{
    message::SyncMessage,
    testing::{boot, entry, finish, opened, refresh_for},
    *,
};
use luxforge_core::{AssetId, HistoryRow, Zoom};
use std::{path::PathBuf, sync::atomic::Ordering};

#[test]
fn initial_open_keeps_its_prequeue_clock_and_normal_generation() {
    let (mut editor, catalog) = boot();
    let started = Instant::now() - std::time::Duration::from_millis(25);
    let _ = editor.open_queued(
        PathBuf::from("missing-startup-photo.jpg"),
        Some(tasks::StartupImport {
            started,
            result: Err("read-error: missing source".into()),
        }),
    );
    assert_eq!(editor.activity.request_started, started);
    assert_eq!(editor.activity.requested, 1);
    assert_eq!(editor.open_generation.load(Ordering::Acquire), 1);
    assert!(editor.activity.pending && editor.busy);
    finish(editor, catalog);
}

#[test]
fn a_late_open_result_cannot_replace_the_newer_selected_asset() {
    let (mut editor, catalog, current_asset, _) = opened(Vec::new(), 2);
    let displayed = editor.display_entry.clone();
    let preview_generation = editor.preview_generation;
    let session = editor.session.clone();
    editor.activity.requested = 5;
    editor.open_generation.store(5, Ordering::Release);
    let old_asset = AssetId::new();
    let old_entry = entry(&old_asset, 0, None);
    let stale = refresh_for(
        &old_asset,
        &old_entry,
        vec![old_entry.clone()],
        &[&old_entry],
        false,
    );
    let _ = editor.update(Message::Sync(SyncMessage::ImportRefreshed(
        4,
        Ok(Box::new(stale)),
    )));
    assert_eq!(editor.state.as_ref().unwrap().asset.id, current_asset);
    assert_eq!(editor.display_entry, displayed);
    assert_eq!(editor.preview_generation, preview_generation);
    assert_eq!(editor.session, session);
    finish(editor, catalog);
}

#[test]
fn stale_session_responses_are_not_adopted() {
    let (mut editor, catalog) = boot();
    let mut newer = ClientSession {
        revision: 5,
        ..ClientSession::default()
    };
    newer
        .preview
        .view
        .set_zoom(Zoom::Percent { value: 200.0 })
        .unwrap();
    let older = ClientSession {
        revision: 3,
        ..ClientSession::default()
    };
    let _ = editor.update(Message::View(ViewMessage::SessionUpdated(
        Ok(newer.clone()),
    )));
    let _ = editor.update(Message::View(ViewMessage::PanSynced(Ok(older))));
    assert_eq!(editor.session, newer);
    let mut same = newer.clone();
    same.preview.view.pan_to(4.0, 5.0).unwrap();
    let _ = editor.update(Message::View(ViewMessage::SessionUpdated(Ok(same.clone()))));
    assert_eq!(
        editor.session, same,
        "an equal revision may replace the copy"
    );
    finish(editor, catalog);
}

#[test]
fn refresh_replaces_or_merges_history_and_marks_abandoned_branches() {
    let (mut editor, catalog) = boot();
    let asset = AssetId::new();
    let original = entry(&asset, 0, None);
    let a = entry(&asset, 1, Some(&original.id));
    let b = entry(&asset, 2, Some(&a.id));
    let c = entry(&asset, 3, Some(&a.id));
    editor.busy = true;
    let full = refresh_for(
        &asset,
        &c,
        vec![c.clone(), b.clone(), a.clone(), original.clone()],
        &[&c, &a, &original],
        false,
    );
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(full)))));
    assert!(!editor.busy);
    assert_eq!(editor.api_sequence, 0, "only a poll moves the event cursor");
    assert_eq!(editor.history.entries.len(), 4);
    assert_eq!(editor.display_entry, Some(c.id.clone()));
    fn branch(editor: &Editor, id: &luxforge_core::EntryId) -> Option<bool> {
        editor
            .workspace
            .panel
            .history
            .iter()
            .find(|row| &row.entry_id == id)
            .map(|row| row.branch)
    }
    assert_eq!(branch(&editor, &c.id), Some(false));
    assert_eq!(branch(&editor, &original.id), Some(false));
    assert_eq!(
        branch(&editor, &b.id),
        Some(true),
        "b was undone and is a branch"
    );
    let d = entry(&asset, 4, Some(&c.id));
    let merged = refresh_for(&asset, &d, Vec::new(), &[&d, &c], true);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(merged)))));
    assert_eq!(editor.history.entries.len(), 5);
    assert_eq!(editor.history.entries[0].id, d.id);
    assert_eq!(branch(&editor, &d.id), Some(false));
    assert_eq!(
        branch(&editor, &b.id),
        Some(false),
        "below a truncated lineage nothing is marked as a branch"
    );
    finish(editor, catalog);
}

/// This desktop's own commit reads no page, lineage or versions: its entry's row merges into
/// the loaded page, the entry joins the loaded lineage beside the one it continues, the floor
/// below a truncated walk stays, and the versions stay as they were.
#[test]
fn a_commit_merges_its_row_and_lineage_without_reading_them() {
    let (mut editor, catalog) = boot();
    let asset = AssetId::new();
    let original = entry(&asset, 0, None);
    let a = entry(&asset, 1, Some(&original.id));
    let b = entry(&asset, 2, Some(&a.id));
    let c = entry(&asset, 3, Some(&a.id));
    let mut opened = refresh_for(
        &asset,
        &c,
        vec![c.clone(), b.clone(), a.clone(), original.clone()],
        &[&c, &a],
        true,
    );
    let version = luxforge_core::Version {
        asset_id: asset.clone(),
        name: "Keep".into(),
        entry_id: a.id.clone(),
        entry_sequence: 1,
        actor: "test".into(),
        created_ms: 0,
    };
    opened.versions = Some(vec![version.clone()]);
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(opened)))));
    let floor = editor.lineage_floor;
    assert_eq!(floor, Some(1));

    let d = entry(&asset, 4, Some(&c.id));
    let mut committed = refresh_for(&asset, &d, Vec::new(), &[], false);
    committed.lineage = None;
    committed.versions = None;
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        committed,
    )))));
    assert_eq!(editor.history.entries[0], HistoryRow::from(&d));
    assert_eq!(editor.history.entries.len(), 5);
    assert!(editor.lineage.contains(&d.id) && editor.lineage.contains(&c.id));
    assert!(!editor.lineage.contains(&b.id));
    assert_eq!(editor.lineage_floor, floor);
    assert_eq!(editor.versions, [version]);
    let branch = |id: &luxforge_core::EntryId| {
        editor
            .workspace
            .panel
            .history
            .iter()
            .find(|row| &row.entry_id == id)
            .map(|row| row.branch)
    };
    assert_eq!(branch(&d.id), Some(false));
    assert_eq!(branch(&b.id), Some(true), "b is still an abandoned branch");
    finish(editor, catalog);
}

/// With no currency request, an answer overtaken by what the desktop already holds is dropped
/// when it arrives: a refresh read before a newer selection or a newer revision of the asset,
/// and a selection's frame planned before a newer selection or showing a current entry that is
/// not the one held. Nothing of it is adopted and nothing is rendered; a poll whose state read
/// was overtaken leaves the event sequence where it was, so the next poll reads it again.
#[test]
fn answers_overtaken_by_a_newer_selection_or_revision_are_dropped() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    let current = editor.state.as_ref().unwrap().current_entry.clone();
    let older = entry(&asset, 2, None);
    let mut selected = editor.session.clone();
    selected
        .preview
        .select(HistorySelection::Entry(older.id.clone()));
    selected.revision += 1;
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload {
            job: refresh_for(&asset, &older, Vec::new(), &[&older], false).job,
            session: selected.clone(),
        },
    )))));
    assert_eq!(editor.display_entry, Some(older.id.clone()));
    let held = (
        editor.display_entry.clone(),
        editor.preview_generation,
        editor.session.clone(),
        editor.api_sequence,
    );
    let unchanged = |editor: &Editor, case: &str| {
        assert_eq!(
            (
                editor.display_entry.clone(),
                editor.preview_generation,
                editor.session.clone(),
                editor.api_sequence,
            ),
            held,
            "{case}"
        );
        assert_eq!(
            editor.state.as_ref().unwrap().current_entry.id,
            current.id,
            "{case}"
        );
    };

    // A commit's refresh that read the session before the selection.
    let next = entry(&asset, 5, Some(&current.id));
    let before_selection = refresh_for(&asset, &next, Vec::new(), &[&next], false);
    editor.busy = true;
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        before_selection.clone(),
    )))));
    unchanged(&editor, "a refresh behind the selection");
    assert!(editor.busy, "the answer that overtook it ends the request");
    let mut polled = tasks::SyncResult::changed(before_selection);
    polled.sequence = 20;
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
    unchanged(&editor, "a poll behind the selection");

    // A refresh of an older revision than the one held.
    let mut stale = refresh_for(&asset, &older, Vec::new(), &[&older], false);
    stale.state.revision = 3;
    stale.session = selected.clone();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(stale)))));
    unchanged(&editor, "a refresh of an older revision");

    // A selection's frame planned before the newer selection.
    let mut earlier = editor.session.clone();
    earlier.preview.generation -= 1;
    earlier.preview.selection = HistorySelection::Current;
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload {
            job: refresh_for(&asset, &current, Vec::new(), &[&current], false).job,
            session: earlier,
        },
    )))));
    unchanged(&editor, "a frame behind the selection");

    // Return to current whose frame shows a current entry this desktop does not hold.
    let mut returned = editor.session.clone();
    returned.preview.return_current();
    returned.revision += 1;
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload {
            job: refresh_for(&asset, &next, Vec::new(), &[&next], false).job,
            session: returned.clone(),
        },
    )))));
    unchanged(&editor, "a current frame of another entry");

    // The same return with the entry held is shown.
    let _ = editor.update(Message::Preview(PreviewMessage::Loaded(Ok(Box::new(
        tasks::PreviewPayload {
            job: refresh_for(&asset, &current, Vec::new(), &[&current], false).job,
            session: returned,
        },
    )))));
    assert_eq!(editor.display_entry, Some(current.id.clone()));
    finish(editor, catalog);
}

/// A poll that read nothing, as the tests hand it back.
fn nothing_new() -> tasks::SyncResult {
    tasks::SyncResult {
        sequence: 0,
        refresh: None,
        presets: None,
        capabilities: false,
        own: Vec::new(),
    }
}

/// Nothing polls the event log on a timer. The owner's wake asks for one poll, which starts once
/// nothing is in flight: a request of the desktop's own is answered first, so the poll can skip
/// the event that request left, and a wake that lands during a poll is read right after it.
#[test]
fn the_event_sync_polls_only_when_woken_and_never_across_a_request_in_flight() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
    assert!(!editor.syncing && !editor.sync_wanted);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(!editor.syncing, "a message that is no wake starts no poll");

    editor.busy = true;
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    assert!(
        !editor.syncing && editor.sync_wanted,
        "a wake waits for the request in flight"
    );
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Err(
        "conflict: stale revision".into(),
    ))));
    assert!(
        editor.syncing && !editor.sync_wanted,
        "the poll starts as the request is answered"
    );

    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    assert!(editor.syncing && editor.sync_wanted, "one poll at a time");
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(nothing_new()))));
    assert!(
        editor.syncing && !editor.sync_wanted,
        "the wake that landed during the poll is read right after it"
    );
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(nothing_new()))));
    assert!(
        !editor.syncing && !editor.sync_wanted,
        "and then nothing runs"
    );
    finish(editor, catalog);
}
