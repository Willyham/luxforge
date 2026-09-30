use super::*;
use crate::app::{
    message::{performance::PerformanceMessage, select::SelectMessage},
    select::{Reading, refresh_now},
    testing::{boot, finish},
};
use crate::state::{
    long_work::LongWorkModel,
    select::{SelectPanel, Shown},
};
use luxforge_core::activity::Outcome as Ended;
use std::path::{Path, PathBuf};

/// A folder of `count` one-byte `.jpg` files beside the catalog, in folders of 250: work the index
/// lane lists and reads a header from for each, long enough to cancel while it runs.
fn files(catalog: &Path, name: &str, count: usize) -> PathBuf {
    let stem = catalog.file_stem().unwrap().to_string_lossy();
    let folder = catalog.with_file_name(format!("{stem}-{name}"));
    for index in 0..count {
        let dir = folder.join(format!("d{:02}", index / 250));
        if index % 250 == 0 {
            std::fs::create_dir_all(&dir).unwrap();
        }
        std::fs::write(dir.join(format!("IMG_{index:05}.jpg")), b"x").unwrap();
    }
    folder
}

/// The job's record once it has ended.
fn ended(editor: &Editor, job: &str) -> Value {
    luxforge_testbase::wait_for("the job to end", || {
        let record = job_now(&editor.owner, editor.client, job).unwrap();
        (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
    })
}

/// The refresh timer's tick, as it comes a second after the last read.
fn tick(editor: &mut Editor) {
    editor.long_work.read_at = Some(Instant::now() - REFRESH_INTERVAL);
    let _ = editor.update(Message::LongWork(LongWorkMessage::Tick));
}

/// With nothing running long work has no timer and takes no read: only the watch's wake, which the
/// board posts when catalog work changes, makes it read. The Performance section's tick reads the
/// board as the section's own read, which counts as the section's, not long work's.
#[test]
fn nothing_wakes_while_nothing_runs() {
    let (mut editor, catalog) = boot();
    assert!(editor.long_work.watch.is_some(), "the board is watched");
    assert_eq!(editor.long_work.timers(), Timers::default());
    assert_eq!(editor.workspace.long_work, LongWorkModel::default());
    let reads = editor.long_work.reads;
    for _ in 0..3 {
        let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
    }
    assert_eq!(editor.long_work.reads, reads);
    assert_eq!(editor.long_work.timers(), Timers::default());
    let summary = editor.snapshot()["long_work"].clone();
    assert_eq!(summary["busiest"], Value::Null);
    assert_eq!(summary["sheet"], Value::Null);
    assert_eq!(
        summary["timers"],
        json!({"throttle": false, "refresh": false})
    );
    finish(editor, catalog);
}

/// The status bar's job and the Performance section's rows are one read of the board: after each
/// read a wake asks for, with no sample of the section's between, the section's rows are that
/// board's, the running job the status bar shows is the section's row with Cancel, and once it has
/// ended neither shows it running. The section's own tick reads the board through the same watch.
/// Idle, nothing new wakes: no long-work timer or read, and none at all once the section is closed.
#[test]
fn the_section_and_the_status_bar_never_disagree_after_a_wake() {
    let (mut editor, catalog) = boot();
    assert!(editor.performance_sampling(), "the section is open");
    // The first message starts the section's sampling, whose first read goes out and stays out:
    // the section samples nothing more here, so whatever its rows show came from long work's reads.
    let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
    assert!(editor.performance.read.in_flight());
    let requested = editor.performance.requested;
    let agree = |editor: &Editor| {
        let work = crate::state::performance::Work {
            board: editor.long_work.state.board.as_ref(),
            rates: &editor.long_work.state.rates,
            home: editor.select.state.home.as_deref(),
        };
        let rows = crate::state::performance::jobs(work).rows;
        assert_eq!(
            editor.workspace.performance.board, editor.long_work.state.version,
            "the section is derived from long work's latest read"
        );
        assert_eq!(editor.workspace.performance.jobs, rows);
        match &editor.workspace.long_work.busiest {
            Some(busiest) => {
                let row = rows
                    .iter()
                    .find(|row| {
                        row.work
                            .as_ref()
                            .is_some_and(|work| work.job_id == busiest.job_id)
                    })
                    .expect("the status bar's job is a row of the section's with Cancel");
                assert!(row.running);
                assert_eq!(row.label, busiest.label);
            }
            None => assert!(
                rows.iter().all(|row| row.work.is_none()),
                "no catalog work runs in the section either: {rows:?}"
            ),
        }
    };
    let wake = |editor: &mut Editor| {
        editor.long_work.read_at = Some(Instant::now() - MIN_INTERVAL);
        let _ = editor.update(Message::LongWork(LongWorkMessage::Woken));
    };
    let folder = files(&catalog, "agree", 3_000);
    let job = refresh_now(&editor.owner, editor.client, &folder).unwrap();
    let mut shown = false;
    let record = luxforge_testbase::wait_for("the listing to end", || {
        wake(&mut editor);
        agree(&editor);
        shown |= editor
            .workspace
            .long_work
            .busiest
            .as_ref()
            .is_some_and(|busiest| busiest.job_id == job);
        let record = job_now(&editor.owner, editor.client, &job).unwrap();
        (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
    });
    assert_eq!(record["status"], "ready");
    assert!(shown, "the listing ran long enough to be shown");
    // The read that finds it ended.
    wake(&mut editor);
    agree(&editor);
    assert!(editor.long_work.state.job(&job).is_none());
    assert_eq!(editor.long_work.timers(), Timers::default());
    assert_eq!(
        editor.performance.requested, requested,
        "no sample of the section's"
    );

    // Idle with the section open, its tick is the only read, the section's own.
    let _ = editor.update(Message::Performance(PerformanceMessage::Sampled {
        epoch: editor.performance.epoch,
        result: Err("stand-in".into()),
    }));
    let (reads, wakes, version) = (
        editor.long_work.reads,
        editor.long_work.wakes,
        editor.long_work.state.version,
    );
    let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
    assert_eq!(editor.performance.requested, requested + 1);
    assert_eq!(
        editor.long_work.state.version,
        version + 1,
        "the section read the board"
    );
    agree(&editor);
    assert_eq!(
        (editor.long_work.reads, editor.long_work.wakes),
        (reads, wakes)
    );
    assert_eq!(editor.long_work.timers(), Timers::default());
    // Closed, nothing reads it at all: a tick already queued as it closed reads nothing.
    let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
    assert!(!editor.performance_sampling());
    for _ in 0..3 {
        let _ = editor.update(Message::Performance(PerformanceMessage::Tick));
    }
    assert_eq!(editor.long_work.state.version, version + 1);
    assert_eq!(editor.performance.requested, requested + 1);
    assert_eq!(editor.long_work.timers(), Timers::default());
    std::fs::remove_dir_all(folder).unwrap();
    finish(editor, catalog);
}

/// A wake inside the throttle's interval is read once that interval has passed, by the one timer
/// that exists only while a read is due; a later wake is read at once.
#[test]
fn a_wake_inside_the_throttle_is_read_when_its_interval_passes() {
    let (mut editor, catalog) = boot();
    editor.long_work.read_at = Some(Instant::now());
    let reads = editor.long_work.reads;
    let _ = editor.update(Message::LongWork(LongWorkMessage::Woken));
    let _ = editor.update(Message::LongWork(LongWorkMessage::Woken));
    assert_eq!(editor.long_work.wakes, 2);
    assert_eq!(editor.long_work.reads, reads, "throttled");
    assert!(editor.long_work.timers().throttle, "a read is due");
    tick(&mut editor);
    assert_eq!(editor.long_work.reads, reads + 1);
    assert_eq!(editor.long_work.timers(), Timers::default());
    editor.long_work.read_at = Some(Instant::now() - MIN_INTERVAL);
    let _ = editor.update(Message::LongWork(LongWorkMessage::Woken));
    assert_eq!(editor.long_work.reads, reads + 2, "read at once");
    assert!(!editor.long_work.timers().throttle);
    // The refresh timer's tick right after a read adds nothing, so the reads stay at the
    // throttle's rate.
    let _ = editor.update(Message::LongWork(LongWorkMessage::Tick));
    assert_eq!(editor.long_work.reads, reads + 2);
    finish(editor, catalog);
}

/// A running catalog job keeps the refresh timer while it runs; its Cancel is `job.cancel` with its
/// id, and the board then lists it cancelled and nothing keeps a timer.
#[test]
fn cancel_sends_job_cancel_for_the_job_and_the_board_says_it_was_cancelled() {
    let (mut editor, catalog) = boot();
    let folder = files(&catalog, "cancel", 3_000);
    let job = refresh_now(&editor.owner, editor.client, &folder).unwrap();
    // Long enough to be kept as recent work once it ends (the board keeps work of 250 ms or more).
    luxforge_testbase::wait_for("the listing to run on the board", || {
        tick(&mut editor);
        editor
            .long_work
            .state
            .job(&job)
            .filter(|running| running.elapsed_ms >= 300)
            .map(|_| ())
    });
    assert!(editor.long_work.timers().refresh, "a job runs");
    let _ = editor.update(Message::LongWork(LongWorkMessage::Cancel {
        job_id: job.clone(),
    }));
    assert_eq!(editor.long_work.cancels, vec![job.clone()]);
    // What the Cancel's owner task sends.
    cancel_now(&editor.owner, editor.client, &job).unwrap();
    assert_eq!(ended(&editor, &job)["status"], "cancelled");
    tick(&mut editor);
    assert!(editor.long_work.state.job(&job).is_none());
    let board = editor.long_work.state.board.as_ref().unwrap();
    let recent = board
        .recent
        .iter()
        .find(|entry| entry.entry.job_id.as_deref() == Some(job.as_str()))
        .expect("the cancelled job is recent");
    assert_eq!(recent.outcome, Ended::Cancelled);
    assert_eq!(editor.long_work.timers(), Timers::default());
    assert_eq!(editor.workspace.long_work.busiest, None);
    // A refused cancel says so.
    let _ = editor.update(Message::LongWork(LongWorkMessage::Cancelled {
        job_id: job,
        result: Err("validation: unknown job".into()),
    }));
    assert_eq!(
        editor.status.text,
        "Could not cancel: validation: unknown job"
    );
    std::fs::remove_dir_all(folder).unwrap();
    finish(editor, catalog);
}

/// A finished job's sentence reaches the status bar when its record answers.
#[test]
fn a_finished_job_leaves_its_sentence_in_the_status_bar() {
    let (mut editor, catalog) = boot();
    let job = RecentActivity {
        entry: luxforge_core::activity::ActivityEntry {
            id: 9,
            kind: "index.refresh".into(),
            label: "Indexing".into(),
            detail: Some("/Volumes/Archive/2026".into()),
            job_id: Some("job-9".into()),
            ..Default::default()
        },
        outcome: Ended::Completed,
        duration_ms: 4_000,
        ended_ms_ago: 5,
    };
    let _ = editor.update(Message::LongWork(LongWorkMessage::Ended {
        job: Box::new(job),
        result: Ok(json!({"status": "ready", "result": {"files": 12_408}})),
    }));
    assert_eq!(
        editor.status.text,
        "Indexed 12,408 files in /Volumes/Archive/2026"
    );
    finish(editor, catalog);
}

/// Select's first look at a folder is long work's waiting view; its end is heard as a board change,
/// after which Select reads the job's record and views the folder. No timer asks after it.
#[test]
fn a_first_look_is_followed_to_its_end_through_the_board() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let folder = files(&catalog, "look", 20);
    let _ = editor.update(Message::Select(SelectMessage::FolderPicked(Some(
        folder.clone(),
    ))));
    let job = refresh_now(&editor.owner, editor.client, &folder);
    let _ = editor.update(Message::Select(SelectMessage::Reading(job.clone())));
    let job = job.unwrap();
    let waiting = editor.long_work.state.waiting.clone().unwrap();
    assert_eq!(waiting.job_id, job);
    assert!(waiting.name.ends_with("-look") && !waiting.card);
    let record = ended(&editor, &job);
    assert_eq!(record["status"], "ready");
    // Named, the job's record was asked for at once; the board's news of its end asks again once
    // that answers, rather than beside it.
    tick(&mut editor);
    assert!(editor.select.read_in_flight);
    assert!(editor.long_work.state.job(&job).is_none());
    let _ = editor.update(Message::Select(SelectMessage::ReadAnswered(Ok(record))));
    assert!(editor.select.reading.is_none(), "the folder is read");
    assert!(editor.select.state.loading, "and viewed");
    assert_eq!(editor.long_work.state.waiting, None);
    assert_eq!(editor.workspace.long_work.sheet, None);
    assert_eq!(editor.long_work.timers(), Timers::default());
    std::fs::remove_dir_all(folder).unwrap();
    finish(editor, catalog);
}

/// Continue in background sends the waiting view's job to the background; the sheet's rules are the
/// model's (`state/long_work.rs`).
#[test]
fn continue_in_background_sends_the_waiting_job_to_the_background() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::LongWork(LongWorkMessage::ContinueInBackground));
    assert_eq!(editor.long_work.state.background, None, "nothing waits");
    editor.select.reading = Some(Reading {
        path: PathBuf::from("/Volumes/NIKON Z 8/DCIM"),
        job: Some("job-7".into()),
    });
    tick(&mut editor);
    let _ = editor.update(Message::LongWork(LongWorkMessage::ContinueInBackground));
    assert_eq!(
        editor
            .long_work
            .state
            .waiting
            .as_ref()
            .map(|waiting| waiting.name.as_str()),
        Some("DCIM")
    );
    assert_eq!(editor.long_work.state.background.as_deref(), Some("job-7"));
    editor.select.reading = None;
    finish(editor, catalog);
}

/// The status bar's job opens the Performance section, showing the panel that holds it.
#[test]
fn the_status_bar_job_opens_the_performance_section() {
    let (mut editor, catalog) = boot();
    let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
    assert!(!editor.performance.expanded);
    let _ = editor.update(Message::LongWork(LongWorkMessage::OpenPerformance));
    assert!(editor.performance.expanded && editor.performance_sampling());

    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let _ = editor.update(Message::Select(SelectMessage::TogglePanel(
        SelectPanel::Sources,
    )));
    let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
    assert!(!editor.performance_sampling());
    let _ = editor.update(Message::LongWork(LongWorkMessage::OpenPerformance));
    assert!(editor.select.state.sources_panel && editor.performance.expanded);
    assert!(editor.performance_sampling());
    finish(editor, catalog);
}
