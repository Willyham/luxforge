//! The one refusal every start asks ([`Editor::gesture_refusal`]): which halves each start answers
//! to, and that a refused start writes its reason to the status bar and sends nothing.
use super::{
    gesture::Starting,
    message::{crop::CropMessage, pointer::PointerMessage},
    testing::{attach_log, boot, entry, finish, logged, open_crop, opened, picking, sample_mode},
    *,
};
use crate::state::{IN_FLIGHT, NO_PHOTOGRAPH, NOT_CURRENT};
use luxforge_core::HistorySelection;
use serde_json::Map;

const EVERY_START: [Starting; 13] = [
    Starting::Slider,
    Starting::Mask,
    Starting::MaskCommand,
    Starting::Crop,
    Starting::Mode,
    Starting::Pick,
    Starting::Gallery,
    Starting::Compare,
    Starting::Preset,
    Starting::Action,
    Starting::History,
    Starting::Refit,
    Starting::Export,
];

/// What each start is refused with, in order, for every start.
fn refusals(editor: &Editor) -> Vec<(Starting, Option<String>)> {
    EVERY_START
        .iter()
        .map(|&starting| (starting, editor.gesture_refusal(starting)))
        .collect()
}

/// `expected` for the starts named, and no refusal for every other one.
fn only(refused: &[Starting], reason: &str) -> Vec<(Starting, Option<String>)> {
    EVERY_START
        .iter()
        .map(|&starting| {
            (
                starting,
                refused.contains(&starting).then(|| reason.to_owned()),
            )
        })
        .collect()
}

/// Each start answers to the halves it declares and no others: a previewed history entry holds
/// back only what edits the current state, a request in flight only what would send one behind
/// it, and an open draft everything but an export. A refit takes the one-draft rule alone, and
/// neither Undo, Redo and Restore nor a refit ever takes the editable half.
#[test]
fn every_start_answers_to_the_halves_it_declares() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    assert_eq!(
        refusals(&editor),
        only(&[], ""),
        "nothing holds anything back"
    );

    let editing = [
        Starting::Slider,
        Starting::Mask,
        Starting::MaskCommand,
        Starting::Crop,
        Starting::Pick,
        Starting::Action,
    ];
    editor.session.preview.selection = HistorySelection::Entry(entry(&asset, 2, None).id);
    assert_eq!(refusals(&editor), only(&editing, NOT_CURRENT));

    editor.session.preview.selection = HistorySelection::Current;
    editor.busy = true;
    let waiting = [
        Starting::Mask,
        Starting::MaskCommand,
        Starting::Crop,
        Starting::Pick,
        Starting::Action,
        Starting::Gallery,
        Starting::History,
        Starting::Export,
    ];
    assert_eq!(refusals(&editor), only(&waiting, IN_FLIGHT));

    // Both at once: the editable half is asked first, and what takes only the busy half still
    // says so from a previewed entry.
    editor.session.preview.selection = HistorySelection::Entry(entry(&asset, 2, None).id);
    for (starting, reason) in refusals(&editor) {
        let expected = if editing.contains(&starting) {
            Some(NOT_CURRENT)
        } else if waiting.contains(&starting) {
            Some(IN_FLIGHT)
        } else {
            None
        };
        assert_eq!(reason.as_deref(), expected, "{starting:?}");
    }

    // An open draft refuses every start but an export, which writes the displayed entry the
    // draft does not change.
    editor.session.preview.selection = HistorySelection::Current;
    editor.busy = false;
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    assert!(editor.crop().is_some());
    for (starting, reason) in refusals(&editor) {
        assert_eq!(
            reason.is_none(),
            starting == Starting::Export,
            "{starting:?}: {reason:?}"
        );
    }
    finish(editor, catalog);
}

/// With no photograph open, the starts that edit say so; the others are answered by their own
/// sites, which have nothing to act on.
#[test]
fn a_start_that_edits_says_no_photograph_is_open() {
    let (editor, catalog) = boot();
    for (starting, reason) in refusals(&editor) {
        let edits = starting.halves().editable;
        assert_eq!(
            reason.as_deref(),
            edits.then_some(NO_PHOTOGRAPH),
            "{starting:?}"
        );
    }
    finish(editor, catalog);
}

/// The crop's start refused for a request in flight or a previewed entry says why and sends
/// nothing; it used to return without a word.
#[test]
fn a_refused_crop_start_says_why() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 4);
    for (case, reason) in [("busy", IN_FLIGHT), ("previewed", NOT_CURRENT)] {
        editor.busy = case == "busy";
        editor.session.preview.selection = if case == "previewed" {
            HistorySelection::Entry(entry(&asset, 2, None).id)
        } else {
            HistorySelection::Current
        };
        editor.status.text.clear();
        let task = editor.update(Message::Crop(CropMessage::Start));
        assert_eq!(task.units(), 0, "{case}: nothing is sent");
        assert!(editor.crop().is_none(), "{case}");
        assert_eq!(editor.status.text, reason, "{case}");
    }
    finish(editor, catalog);
}

/// A `draft.begin` the owner refuses opens nothing. It answers in the update of the press, so its
/// refusal is that update's answer: the status bar says why, what the start put up ends with it,
/// and nothing follows it — no `draft.set`, no input stage, no draft on the desktop.
#[test]
fn a_refused_begin_opens_nothing_and_says_why() {
    // A slider against the real owner, which does not hold this photograph and refuses the begin.
    let (mut editor, catalog, log, asset, action, parameter) = testing::drafting();
    editor.stand_in = None;
    let refusal = tasks::draft_begin_now(
        &editor.owner,
        editor.client,
        asset,
        &action,
        &Default::default(),
    )
    .expect_err("the owner does not hold the photograph");
    let _ = testing::slide(&mut editor, &action, &parameter, 1.0);
    assert!(editor.gesture.is_none(), "nothing opened");
    assert_eq!(editor.status.text, refusal, "the refusal is the answer");
    assert_eq!(editor.controls.dragging, None, "the drag ends with it");
    assert!(editor.session.draft.is_none());
    let records = logged(&mut editor, &log);
    assert_eq!(testing::events(&records, "slider_draft_begin").len(), 1);
    assert!(testing::events(&records, "slider_draft_set").is_empty());
    finish(editor, catalog);

    // The crop, whose start has already opened its frame and asked for its mode.
    let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
    let refusal = "conflict: this client already holds a draft";
    testing::stand_in(&mut editor)
        .begins
        .push_back(refusal.into());
    let mode = editor.session.workspace.mode.clone();
    let task = editor.update(Message::Crop(CropMessage::Start));
    assert_eq!(
        task.units(),
        0,
        "no input stage and no mode change are asked for"
    );
    assert!(editor.crop().is_none() && editor.gesture.is_none());
    assert_eq!(editor.status.text, refusal);
    assert_eq!(editor.sync.mode, None);
    assert_eq!(editor.session.workspace.mode, mode);
    // The next start opens as usual.
    let _ = editor.update(Message::Crop(CropMessage::Start));
    assert!(editor.crop().is_some(), "{}", editor.status.text);
    finish(editor, catalog);
}

/// A `draft.reapply` the owner refuses keeps the draft conflicted and says why, in the update of
/// the Reapply press, and a later Reapply can still rebase it.
#[test]
fn a_refused_reapply_keeps_the_draft_conflicted_and_says_why() {
    let (mut editor, catalog, _, _, action, parameter) = testing::drafting();
    let _ = testing::slide(&mut editor, &action, &parameter, 1.0);
    let state = editor.document.state.as_mut().expect("open");
    state.revision += 1;
    let newer = state.revision;
    editor.gesture_revision(newer);
    let refusal = "not-found: this client holds no such draft";
    testing::stand_in(&mut editor)
        .reapplies
        .push_back(refusal.into());
    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Reapply));
    assert_eq!(editor.status.text, refusal);
    let draft = &editor.core_gesture().expect("the draft is kept").draft;
    assert!(draft.conflicted && draft.base_revision == newer - 1);
    assert!(editor.gesture_conflicted(), "the notice stays up");

    let _ = editor.update(Message::Draft(message::draft::DraftMessage::Reapply));
    let draft = &editor.core_gesture().expect("the rebased draft").draft;
    assert!(!draft.conflicted && draft.base_revision == newer);
    finish(editor, catalog);
}

/// A pick from a previewed entry is refused in the one wording every edit uses.
#[test]
fn a_refused_pick_uses_the_one_wording() {
    let (mut editor, catalog, _) = picking();
    let asset = editor
        .document
        .state
        .as_ref()
        .expect("open")
        .asset
        .id
        .clone();
    editor.session.preview.selection = HistorySelection::Entry(entry(&asset, 2, None).id);
    let task = editor.update(Message::Pointer(PointerMessage::Picked { x: 3, y: 4 }));
    assert_eq!(task.units(), 0);
    assert_eq!(editor.status.text, NOT_CURRENT);
    finish(editor, catalog);
}

/// A sample-apply pick whose query answers after another request has started asks the pick's one
/// refusal again, says why and commits nothing.
#[test]
fn a_sample_answer_behind_a_request_in_flight_is_refused_with_its_reason() {
    let (mut editor, catalog) =
        super::testing::opened_with_modules(super::testing::descriptors(), 4);
    let (mode, _, action) = sample_mode(&editor);
    editor.session.workspace.mode = mode;
    let entry_id = editor.displayed_entry().expect("a displayed entry");
    let declared = crate::state::tools::declared_action(&editor.modules, &action)
        .expect("the declared action")
        .clone();
    let mut answer = Map::new();
    answer.insert(declared.parameters[0].name.clone(), json!(-12.0));
    let log = attach_log(&mut editor);
    editor.busy = true;
    let sequence = editor.sync.sequence;
    let _ = editor.update(Message::Pointer(PointerMessage::SampleQueried {
        entry: entry_id,
        action,
        point: (100, 42),
        result: Ok(Value::Object(answer)),
    }));
    assert_eq!(editor.status.text, IN_FLIGHT);
    assert_eq!(editor.sync.sequence, sequence, "nothing was sent");
    assert!(
        logged(&mut editor, &log)
            .iter()
            .all(|record| record["event"] != "canvas_sample"),
        "the refused answer is not recorded as applied"
    );
    finish(editor, catalog);
}
