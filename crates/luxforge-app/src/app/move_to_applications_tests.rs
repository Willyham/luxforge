//! The move question on the desktop: who is asked, the classification once the workspace is usable,
//! the notice held back by the usage notice, Not now with and without Don't ask again, the
//! replace confirmation, a failed move, a finished move handing its relaunch to the quit path, and
//! an evidence run that shows the question and never moves. Off-loop answers are handed to the
//! update function as their tasks would hand them.
use super::{
    Editor,
    message::{
        Message,
        move_to_applications::{MoveDone, MoveFailure, MoveMessage},
    },
    settings_tests::{answer_preference, answer_read, finish, launch},
};
use crate::state::{
    move_to_applications::{
        Asker, BODY, CANCEL, DONT_ASK, MOVE, MoveAction, MoveQuestion, NOT_NOW, Phase, REPLACE,
        TITLE,
    },
    usage::Prompt,
};
use luxforge_install::{Cleanup, Location, Moved, Placement};
use std::path::PathBuf;

fn placement(location: Location) -> Placement {
    let bundle = PathBuf::from("/Users/someone/Downloads/Luxforge.app");
    Placement {
        original: bundle.clone(),
        cleanup: Cleanup::Trash(bundle.clone()),
        bundle,
        location,
    }
}

fn send(editor: &mut Editor, message: MoveMessage) {
    let _ = editor.update(Message::MoveToApplications(message));
}

/// An editor whose launch asks, as a person-launched release build's does, with its preferences
/// read and tool discovery answered.
fn asking(asker: Asker) -> (Editor, PathBuf) {
    let (mut editor, root) = launch();
    editor.move_question = MoveQuestion::new(asker);
    answer_read(&mut editor);
    editor.modules_ready = true;
    (editor, root)
}

/// Classified as `location`, through the hook that asks for it.
fn classified(editor: &mut Editor, location: Location) {
    send(editor, MoveMessage::DontAsk(editor.move_question.dont_ask));
    assert_eq!(editor.move_question.phase, Phase::Classifying);
    send(editor, MoveMessage::Classified(Some(placement(location))));
}

#[test]
fn move_to_applications_asks_only_a_person_launched_macos_release_build() {
    assert_eq!(Asker::of(true, true, false, false, false), Asker::Person);
    assert_eq!(Asker::of(false, true, false, false, false), Asker::Never);
    assert_eq!(Asker::of(true, false, false, false, false), Asker::Never);
    assert_eq!(Asker::of(true, true, true, false, false), Asker::Never);
    assert_eq!(Asker::of(true, true, false, true, false), Asker::Never);
    assert_eq!(Asker::of(false, true, true, true, true), Asker::Evidence);
    assert_eq!(Asker::of(false, false, false, false, true), Asker::Never);

    // A test launch is never a person's release launch: nothing is classified or shown.
    let (mut editor, root) = launch();
    editor.modules_ready = true;
    send(&mut editor, MoveMessage::DontAsk(false));
    assert_eq!(editor.move_question.phase, Phase::Waiting);
    assert!(editor.workspace.move_notice.is_none());
    finish(editor, root);
}

#[test]
fn move_to_applications_is_shown_after_the_workspace_is_usable_and_the_usage_notice() {
    let (mut editor, root) = launch();
    editor.move_question = MoveQuestion::new(Asker::Person);
    answer_read(&mut editor);
    send(&mut editor, MoveMessage::DontAsk(false));
    assert_eq!(
        editor.move_question.phase,
        Phase::Waiting,
        "nothing is classified before tool discovery answers"
    );
    editor.modules_ready = true;
    editor.usage.eligible = true;
    editor.usage.prompt = Prompt::Shown;
    classified(&mut editor, Location::Elsewhere);
    assert_eq!(editor.move_question.phase, Phase::Pending);
    assert!(
        editor.workspace.move_notice.is_none(),
        "the usage notice is answered first"
    );
    editor.usage.prompt = Prompt::Declined;
    send(&mut editor, MoveMessage::DontAsk(false));
    assert_eq!(editor.move_question.phase, Phase::Shown);
    let notice = editor.workspace.move_notice.clone().expect("the notice");
    assert_eq!(notice.title, TITLE);
    assert_eq!(notice.body, BODY);
    assert_eq!(
        notice.actions,
        vec![(MOVE, MoveAction::Move), (NOT_NOW, MoveAction::NotNow)]
    );
    assert_eq!(notice.dont_ask, Some(false));
    let summary = editor.move_summary();
    assert_eq!(summary["phase"], "shown");
    assert_eq!(summary["location"], "elsewhere");
    assert_eq!(summary["cleanup"], "trash");
    assert_eq!(summary["notice"]["buttons"][0], MOVE);
    assert_eq!(DONT_ASK, "Don't ask again");
    finish(editor, root);
}

#[test]
fn move_to_applications_is_not_asked_inside_applications_or_after_dont_ask() {
    for location in [Location::Applications, Location::UserApplications] {
        let (mut editor, root) = asking(Asker::Person);
        classified(&mut editor, location);
        assert_eq!(
            editor.move_question.phase,
            Phase::NotAsked("in-applications")
        );
        assert!(editor.workspace.move_notice.is_none());
        finish(editor, root);
    }
    let (mut editor, root) = asking(Asker::Person);
    send(&mut editor, MoveMessage::Classified(None));
    // Classified answers only a classification asked for; this one asks now.
    assert_eq!(editor.move_question.phase, Phase::Classifying);
    send(&mut editor, MoveMessage::Classified(None));
    assert_eq!(editor.move_question.phase, Phase::NotAsked("not-a-bundle"));
    finish(editor, root);
    // Translocated, on a disk image or a read-only volume: asked.
    for location in [
        Location::Translocated,
        Location::DiskImage,
        Location::ReadOnlyVolume,
    ] {
        let (mut editor, root) = asking(Asker::Person);
        classified(&mut editor, location);
        send(&mut editor, MoveMessage::DontAsk(false));
        assert_eq!(editor.move_question.phase, Phase::Shown, "{location:?}");
        finish(editor, root);
    }
}

#[test]
fn move_to_applications_not_now_with_dont_ask_stores_the_preference() {
    let (mut editor, root) = asking(Asker::Person);
    classified(&mut editor, Location::DiskImage);
    send(&mut editor, MoveMessage::DontAsk(true));
    assert_eq!(editor.move_question.phase, Phase::Shown);
    assert_eq!(
        editor.workspace.move_notice.as_ref().unwrap().dont_ask,
        Some(true)
    );
    send(&mut editor, MoveMessage::NotNow);
    assert_eq!(editor.move_question.phase, Phase::Declined);
    assert!(editor.workspace.move_notice.is_none());
    assert!(
        !editor.preferences.applied().unwrap().update_ask_to_move,
        "shown at once"
    );
    answer_preference(&mut editor);
    assert!(
        !editor.preferences.stored().unwrap().update_ask_to_move,
        "stored through preferences.set"
    );
    finish(editor, root);

    // The next launch reads it and does not ask.
    let (mut editor, root) = launch();
    editor.move_question = MoveQuestion::new(Asker::Person);
    let change = crate::state::preferences::PreferenceChange {
        update_ask_to_move: Some(false),
        ..Default::default()
    };
    let _ = editor.store_preferences(change);
    answer_preference(&mut editor);
    editor.modules_ready = true;
    send(&mut editor, MoveMessage::DontAsk(false));
    assert_eq!(editor.move_question.phase, Phase::NotAsked("dont-ask"));
    finish(editor, root);

    // Not now alone writes nothing.
    let (mut editor, root) = asking(Asker::Person);
    classified(&mut editor, Location::Elsewhere);
    send(&mut editor, MoveMessage::NotNow);
    assert_eq!(editor.move_question.phase, Phase::Declined);
    assert!(editor.preferences.idle(), "nothing written");
    assert!(editor.preferences.applied().unwrap().update_ask_to_move);
    finish(editor, root);
}

#[test]
fn move_to_applications_confirms_before_replacing_and_reports_a_failure() {
    let (mut editor, root) = asking(Asker::Person);
    classified(&mut editor, Location::Elsewhere);
    send(&mut editor, MoveMessage::DontAsk(false));
    // The move's own answer, as its task would hand it.
    editor.move_question.phase = Phase::Moving { replace: false };
    let existing = PathBuf::from("/Applications/Luxforge.app");
    send(
        &mut editor,
        MoveMessage::Answered(Err(MoveFailure::Existing(existing.clone()))),
    );
    assert_eq!(editor.move_question.phase, Phase::Confirming(existing));
    let notice = editor.workspace.move_notice.clone().unwrap();
    assert_eq!(
        notice.actions,
        vec![(REPLACE, MoveAction::Replace), (CANCEL, MoveAction::Cancel)]
    );
    assert!(notice.body.contains("/Applications/Luxforge.app"));
    send(&mut editor, MoveMessage::Cancel);
    assert_eq!(
        editor.move_question.phase,
        Phase::Shown,
        "back to the question"
    );

    editor.move_question.phase = Phase::Moving { replace: true };
    send(
        &mut editor,
        MoveMessage::Answered(Err(MoveFailure::Failed {
            reason: "verify-failed".into(),
            detail: "the copy's signature did not verify".into(),
        })),
    );
    assert_eq!(
        editor.move_question.phase,
        Phase::Failed("the copy's signature did not verify".into())
    );
    let summary = editor.move_summary();
    assert_eq!(summary["failure"], "the copy's signature did not verify");
    assert!(editor.workspace.move_notice.is_some(), "it says why");
    // A stale answer changes nothing.
    send(&mut editor, MoveMessage::Answered(Ok(MoveDone::Requested)));
    assert!(matches!(editor.move_question.phase, Phase::Failed(_)));
    finish(editor, root);
}

#[test]
fn move_to_applications_a_finished_move_relaunches_through_the_quit_path() {
    let (mut editor, root) = asking(Asker::Person);
    classified(&mut editor, Location::Elsewhere);
    editor.move_question.phase = Phase::Moving { replace: false };
    let moved = Moved {
        app: PathBuf::from("/nonexistent/Applications/Luxforge.app"),
        replaced: None,
        user: false,
        cleanup: Cleanup::Trash(PathBuf::from("/Users/someone/Downloads/Luxforge.app")),
        quarantine_cleared: 3,
    };
    send(
        &mut editor,
        MoveMessage::Answered(Ok(MoveDone::Moved(moved))),
    );
    assert_eq!(editor.move_question.phase, Phase::Relaunching);
    // The relaunch waits in the slot for the quit path, which the window's close runs; the test
    // takes it unrun, so nothing is launched or moved.
    let (name, _) = super::relaunch::take().expect("the relaunch waits for the quit path");
    assert_eq!(name, "relaunch");
    assert!(editor.owner_join.is_some(), "the catalog is still open");
    finish(editor, root);
}

#[test]
fn move_to_applications_an_evidence_run_shows_the_question_and_never_moves() {
    let (mut editor, root) = asking(Asker::Evidence);
    editor.evidence = Some(crate::app::testing::scripted_evidence("[]"));
    editor.evidence.as_mut().unwrap().step = 1;
    classified(&mut editor, Location::Applications);
    send(&mut editor, MoveMessage::DontAsk(false));
    assert_eq!(
        editor.move_question.phase,
        Phase::Shown,
        "asked wherever it runs"
    );
    send(&mut editor, MoveMessage::Move);
    assert_eq!(
        editor.move_question.phase,
        Phase::Shown,
        "a press is logged and nothing moves"
    );
    editor.evidence = None;
    finish(editor, root);
}
