use super::*;
use luxforge_core::activity::ActivityProgress;
use serde_json::json;
use std::borrow::Cow;

const HOME: &str = "/Users/someone";

fn home() -> Option<&'static Path> {
    Some(Path::new(HOME))
}

/// A running board entry of `kind`, with a job id when it is a catalog kind.
fn running(
    id: u64,
    kind: &'static str,
    detail: Option<&str>,
    elapsed_ms: u64,
    progress: Option<(Option<f64>, &str)>,
) -> ActiveActivity {
    let label = catalog_job(kind).map_or("Rendering preview", |job| job.label);
    ActiveActivity {
        entry: ActivityEntry {
            id,
            kind: Cow::Borrowed(kind),
            label: Cow::Borrowed(label),
            detail: detail.map(str::to_owned),
            job_id: catalog_job(kind).map(|_| format!("job-{id}")),
            progress: progress.map(|(fraction, message)| ActivityProgress {
                fraction,
                message: (!message.is_empty()).then(|| message.to_owned()),
            }),
            ..ActivityEntry::default()
        },
        elapsed_ms,
    }
}

fn indexing(id: u64, elapsed_ms: u64, fraction: Option<f64>, message: &str) -> ActiveActivity {
    running(
        id,
        "index.refresh",
        Some("/Users/someone/Pictures"),
        elapsed_ms,
        Some((fraction, message)),
    )
}

fn board(active: Vec<ActiveActivity>) -> ActivitySnapshot {
    ActivitySnapshot {
        active,
        ..ActivitySnapshot::default()
    }
}

fn finished(entry: ActiveActivity, outcome: Outcome, duration_ms: u64) -> RecentActivity {
    RecentActivity {
        entry: entry.entry,
        outcome,
        duration_ms,
        ended_ms_ago: 10,
    }
}

/// Reads of one job every `every_ms`, its fraction advancing by `step` each time from `from`.
fn steady_reads(state: &mut LongWorkState, reads: u64, every_ms: u64, from: f64, step: f64) {
    for read in 0..reads {
        let at = 600 + read * every_ms;
        state.observe(board(vec![indexing(
            1,
            at,
            Some(from + step * read as f64),
            "headers",
        )]));
    }
}

#[test]
fn nothing_shows_while_nothing_runs() {
    let state = LongWorkState::default();
    assert_eq!(model(&state, true, home()), LongWorkModel::default());
    let mut state = LongWorkState::default();
    state.observe(board(vec![running(1, "preview.render", None, 4_000, None)]));
    assert_eq!(
        model(&state, true, home()),
        LongWorkModel::default(),
        "the desktop's own renders are not long-running catalog work"
    );
    assert!(state.running().next().is_none());
}

/// A count is the work's own words, `48,210 of about 200,000 files` while a walk still finds its
/// extent; with no fraction the bar has no fill and the status bar says `working`.
#[test]
fn a_walk_still_finding_its_extent_shows_its_count_and_no_fraction() {
    let mut state = LongWorkState::default();
    let job = indexing(1, 3_000, None, "48,210 of about 200,000 files");
    state.observe(board(vec![job.clone()]));
    let busiest = model(&state, false, home()).busiest.unwrap();
    assert_eq!(busiest.label, "Indexing ~/Pictures");
    assert_eq!(busiest.fraction, None, "no fraction is invented");
    assert_eq!(busiest.jobs, 1);
    let work = work_info(&job, &state.rates).unwrap();
    assert_eq!(work.job_id, "job-1");
    assert_eq!(work.count.as_deref(), Some("48,210 of about 200,000 files"));
    assert_eq!(work.estimate, None);
    // Work that can say nothing reads `working` in the widget: no count at all.
    let silent = running(2, "preview.extract", None, 900, None);
    assert_eq!(work_info(&silent, &state.rates).unwrap().count, None);
    // Work that is not the catalog's has no work part: its row stays a plain job row.
    assert_eq!(
        work_info(&running(3, "source.develop", None, 900, None), &state.rates),
        None
    );
}

/// The estimate appears only after enough updates over enough time at a steady rate, and says how
/// long is left at that rate.
#[test]
fn an_estimate_appears_only_once_the_rate_is_steady() {
    let mut state = LongWorkState::default();
    // 2% every 250 ms: 8% a second.
    steady_reads(&mut state, STEADY_UPDATES as u64, 250, 0.10, 0.02);
    assert_eq!(
        state.rates.remaining_ms(1),
        None,
        "{STEADY_UPDATES} updates over 750 ms are not enough time"
    );
    let mut state = LongWorkState::default();
    steady_reads(&mut state, 9, 250, 0.10, 0.02);
    // The last read is 0.26 done at 2,600 ms: 0.74 left at 0.08 a second is 9.25 s.
    let left = state.rates.remaining_ms(1).unwrap();
    assert!((9_200..=9_300).contains(&left), "{left}");
    let job = state.job("job-1").unwrap().clone();
    assert_eq!(
        work_info(&job, &state.rates).unwrap().estimate.as_deref(),
        Some("about 10 s")
    );
}

/// A rate that is still settling — each update a different pace — shows no estimate.
#[test]
fn an_unsteady_rate_shows_no_estimate() {
    let mut state = LongWorkState::default();
    let mut fraction = 0.1;
    for (read, step) in [0.01, 0.01, 0.01, 0.01, 0.01, 0.06, 0.08, 0.10]
        .into_iter()
        .enumerate()
    {
        fraction += step;
        state.observe(board(vec![indexing(
            1,
            600 + read as u64 * 300,
            Some(fraction),
            "",
        )]));
    }
    assert!(!state.rates.steady(1));
    assert_eq!(state.rates.remaining_ms(1), None);
}

/// Once shown, the estimate survives jitter but is withdrawn when the rate collapses or the work
/// stalls; the bar still shows exactly the fraction reported, so a stuck job never reads as nearly
/// done.
#[test]
fn the_estimate_is_withdrawn_when_the_rate_collapses_or_the_work_stalls() {
    let mut state = LongWorkState::default();
    steady_reads(&mut state, 9, 250, 0.10, 0.02);
    assert!(state.rates.steady(1));
    // Jitter within the collapse tolerance keeps it.
    state.observe(board(vec![indexing(1, 2_900, Some(0.275), "")]));
    assert!(state.rates.steady(1), "a slightly slower update keeps it");
    // The rate collapses to a fifth for a few updates.
    let mut at = 2_900;
    let mut fraction = 0.275;
    for _ in 0..4 {
        at += 250;
        fraction += 0.004;
        state.observe(board(vec![indexing(1, at, Some(fraction), "")]));
    }
    assert!(!state.rates.steady(1), "a collapsed rate withdraws it");
    assert_eq!(state.rates.remaining_ms(1), None);

    // A steady job that stops reporting: once it has been quiet for the stall time, no estimate,
    // and its bar stays exactly where it stopped.
    let mut state = LongWorkState::default();
    steady_reads(&mut state, 9, 250, 0.10, 0.02);
    assert!(state.rates.remaining_ms(1).is_some());
    state.observe(board(vec![indexing(1, 2_600 + 1_000, Some(0.26), "")]));
    assert!(
        state.rates.remaining_ms(1).is_some(),
        "a second's quiet is not a stall"
    );
    state.observe(board(vec![indexing(
        1,
        2_600 + STALL_MIN_MS + 1,
        Some(0.26),
        "",
    )]));
    assert_eq!(state.rates.remaining_ms(1), None, "stalled");
    assert_eq!(
        model(&state, false, home()).busiest.unwrap().fraction,
        Some(0.26)
    );
    // Minutes later, still no estimate: the samples age out of the window.
    state.observe(board(vec![indexing(1, 600_000, Some(0.26), "")]));
    assert_eq!(state.rates.remaining_ms(1), None);
}

/// A new phase whose fraction starts again from zero is measured from its own start, and a job
/// that stops stating a fraction has no estimate.
#[test]
fn a_new_phase_is_measured_from_its_own_start() {
    let mut state = LongWorkState::default();
    steady_reads(&mut state, 9, 250, 0.10, 0.02);
    assert!(state.rates.steady(1));
    state.observe(board(vec![indexing(1, 2_850, Some(0.01), "")]));
    assert!(!state.rates.steady(1));
    state.observe(board(vec![indexing(1, 3_100, None, "1,204 files found")]));
    assert_eq!(state.rates.remaining_ms(1), None);
    // An entry that is no longer running is forgotten.
    state.observe(board(vec![]));
    assert_eq!(state.rates, Rates::default());
}

/// The busiest job is the one that began first among those shown; the count is every shown job;
/// work under the display threshold and the desktop's own work are neither.
#[test]
fn the_busiest_job_is_the_longest_running_and_the_count_is_every_shown_job() {
    let mut state = LongWorkState::default();
    state.observe(board(vec![
        running(1, "preview.render", None, 9_000, None),
        indexing(2, 8_000, Some(0.24), "12 of 50 headers read"),
        running(
            3,
            "preview.extract",
            None,
            5_000,
            Some((Some(0.8), "640 of 800")),
        ),
        running(4, "preview.extract", None, LONG_JOB_MS - 1, None),
    ]));
    let busiest = model(&state, false, home()).busiest.unwrap();
    assert_eq!(
        busiest,
        BusiestJob {
            job_id: "job-2".into(),
            label: "Indexing ~/Pictures".into(),
            fraction: Some(0.24),
            jobs: 2,
        }
    );
    // The first job ending hands the status bar to the next.
    state.observe(board(vec![running(
        3,
        "preview.extract",
        None,
        6_000,
        Some((Some(0.9), "720 of 800")),
    )]));
    let busiest = model(&state, false, home()).busiest.unwrap();
    assert_eq!(
        (busiest.label.as_str(), busiest.jobs, busiest.fraction),
        ("Reading previews", 1, Some(0.9))
    );
}

fn waiting(job_id: &str) -> Option<Waiting> {
    Some(Waiting {
        job_id: job_id.into(),
        name: "images".into(),
        card: false,
    })
}

/// The sheet covers only a Select view waiting on a running job, once it has run long enough to be
/// worth one, until it ends or is sent to the background.
#[test]
fn the_sheet_shows_only_over_a_view_with_nothing_to_show_yet() {
    let mut state = LongWorkState::default();
    let reading = indexing(1, 1_200, Some(0.51), "312 of 612 headers read");
    state.observe(board(vec![reading.clone()]));
    assert_eq!(model(&state, true, home()).sheet, None, "no view waits");

    state.waiting = waiting("job-1");
    let sheet = model(&state, true, home()).sheet.unwrap();
    assert_eq!(
        sheet,
        ProgressSheet {
            job_id: "job-1".into(),
            card: false,
            title: "Reading images".into(),
            note: SHEET_NOTE.into(),
            count: Some("312 of 612 headers read".into()),
            fraction: Some(0.51),
            estimate: None,
        }
    );
    assert_eq!(model(&state, false, home()).sheet, None, "Develop is shown");

    // Too young: the first look at a known folder is over before a sheet would be worth it.
    let mut young = state.clone();
    young.observe(board(vec![indexing(1, LONG_JOB_MS - 1, None, "")]));
    assert_eq!(model(&young, true, home()).sheet, None);

    // A card's sheet names the card.
    let mut card = state.clone();
    card.waiting = Some(Waiting {
        job_id: "job-1".into(),
        name: "NIKON Z 8".into(),
        card: true,
    });
    let sheet = model(&card, true, home()).sheet.unwrap();
    assert_eq!(
        (sheet.title.as_str(), sheet.card),
        ("Reading the NIKON Z 8 card", true)
    );

    // Continue in background: the sheet goes, the job stays in the status bar.
    let mut background = state.clone();
    background.background = Some("job-1".into());
    let shown = model(&background, true, home());
    assert_eq!(shown.sheet, None);
    assert_eq!(shown.busiest.unwrap().job_id, "job-1");

    // The job ended: the view can draw, so the sheet goes by itself.
    let mut ended = state.clone();
    ended.observe(board(vec![]));
    assert_eq!(model(&ended, true, home()).sheet, None);

    // With a steady rate, the sheet says how long is left.
    let mut steady = LongWorkState {
        waiting: waiting("job-1"),
        ..LongWorkState::default()
    };
    steady_reads(&mut steady, 9, 250, 0.10, 0.02);
    assert_eq!(
        model(&steady, true, home())
            .sheet
            .unwrap()
            .estimate
            .as_deref(),
        Some("about 10 s left")
    );
}

#[test]
fn estimates_read_in_seconds_then_minutes_then_hours() {
    for (ms, text) in [
        (0, "about 1 s"),
        (400, "about 1 s"),
        (4_000, "about 4 s"),
        (4_001, "about 5 s"),
        (59_000, "about 59 s"),
        (59_500, "about 1 min"),
        (100_000, "about 1 min 40 s"),
        (96_000, "about 1 min 40 s"),
        (94_000, "about 1 min 30 s"),
        (599_000, "about 10 min"),
        (840_000, "about 14 min"),
        (3_570_000, "about 1 h"),
        (4_800_000, "about 1 h 20 min"),
    ] {
        assert_eq!(format_remaining(ms), text, "{ms} ms");
    }
}

#[test]
fn places_read_from_home_and_long_ones_by_their_name() {
    assert_eq!(place("/Users/someone/Pictures", home()), "~/Pictures");
    assert_eq!(place("/Users/someone", home()), "~");
    assert_eq!(
        place("/Volumes/Archive/2026", home()),
        "/Volumes/Archive/2026"
    );
    assert_eq!(
        place(
            "/Users/someone/projects/archive/target/smoke/generated/images",
            home()
        ),
        "\u{2026}/images"
    );
    assert_eq!(place("the NIKON Z 8 card", home()), "the NIKON Z 8 card");
    assert_eq!(place("3 indexed folders", home()), "3 indexed folders");
    let rendering = running(1, "preview.photo", Some("DSC_0412.NEF"), 900, None);
    assert_eq!(
        work_label(&rendering.entry, home()),
        "Rendering previews \u{b7} DSC_0412.NEF"
    );
}

/// A finished job's sentence: what it did, from its record when it has one, and nothing for short
/// work or work that is not the catalog's.
#[test]
fn a_finished_job_leaves_a_sentence() {
    let index = indexing(1, 0, Some(1.0), "12,408 of 12,408 headers read");
    let record = json!({"status": "ready", "result": {"files": 12_408, "roots": []}});
    assert_eq!(
        finished_sentence(
            &finished(index.clone(), Outcome::Completed, 4_000),
            Some(&record),
            home()
        )
        .as_deref(),
        Some("Indexed 12,408 files in ~/Pictures")
    );
    let one = json!({"result": {"files": 1}});
    assert_eq!(
        finished_sentence(
            &finished(index.clone(), Outcome::Completed, 4_000),
            Some(&one),
            home()
        )
        .as_deref(),
        Some("Indexed 1 file in ~/Pictures")
    );
    let card = running(2, "index.refresh", Some("the NIKON Z 8 card"), 0, None);
    assert_eq!(
        finished_sentence(
            &finished(card, Outcome::Completed, 2_000),
            Some(&record),
            home()
        )
        .as_deref(),
        Some("Indexed 12,408 files on the NIKON Z 8 card")
    );
    assert_eq!(
        finished_sentence(
            &finished(index.clone(), Outcome::Cancelled, 4_000),
            None,
            home()
        )
        .as_deref(),
        Some("Cancelled indexing ~/Pictures")
    );
    let failed = json!({"status": "failed", "error": {"code": "read-error", "message": "the card was removed"}});
    assert_eq!(
        finished_sentence(
            &finished(index.clone(), Outcome::Failed, 4_000),
            Some(&failed),
            home()
        )
        .as_deref(),
        Some("Indexing ~/Pictures failed: the card was removed")
    );
    let previews = running(
        3,
        "preview.extract",
        None,
        0,
        Some((Some(1.0), "1,042 of 1,042")),
    );
    assert_eq!(
        finished_sentence(&finished(previews, Outcome::Completed, 9_000), None, home()).as_deref(),
        Some("Read previews \u{b7} 1,042 of 1,042")
    );
    assert_eq!(
        finished_sentence(
            &finished(index, Outcome::Completed, FINISHED_SENTENCE_MS - 1),
            Some(&record),
            home()
        ),
        None,
        "work under a second leaves nothing"
    );
    assert_eq!(
        finished_sentence(
            &finished(
                running(4, "source.develop", None, 0, None),
                Outcome::Completed,
                9_000
            ),
            None,
            home()
        ),
        None
    );
}

/// A running catalog job's Performance row is a work row: its place in its label, the job its
/// Cancel stops, its count and its estimate once steady; other work keeps its plain row.
/// A batch preset or export leaves no sentence of the status bar's own: the catalog's batch form
/// words its end.
#[test]
fn a_batch_job_leaves_its_sentence_to_its_view() {
    for kind in ["batch.apply-settings", "batch.export"] {
        let batch = running(7, kind, Some("12 photographs"), 0, None);
        for outcome in [Outcome::Completed, Outcome::Cancelled, Outcome::Failed] {
            assert_eq!(
                finished_sentence(&finished(batch.clone(), outcome, 9_000), None, home()),
                None,
                "{kind} {outcome:?}"
            );
        }
    }
    let develop = running(8, "pick.develop", Some("18 picks"), 0, None);
    assert!(
        finished_sentence(&finished(develop, Outcome::Completed, 9_000), None, home()).is_some()
    );
}

#[test]
fn a_performance_row_of_catalog_work_names_its_job_count_and_estimate() {
    use crate::state::performance::{Work, jobs};
    let mut state = LongWorkState::default();
    steady_reads(&mut state, 9, 250, 0.10, 0.02);
    let mut snapshot = state.board.clone().unwrap();
    snapshot.active.insert(
        0,
        running(7, "source.develop", Some("DSC_0412.NEF"), 3_000, None),
    );
    let listed = jobs(Work {
        board: Some(&snapshot),
        rates: &state.rates,
        home: home(),
    });
    assert_eq!(listed.caption.as_deref(), Some("2 jobs"));
    let [plain, indexing] = listed.rows.as_slice() else {
        panic!("two rows: {:?}", listed.rows);
    };
    assert_eq!(plain.work, None);
    assert_eq!(plain.detail.as_deref(), Some("DSC_0412.NEF"));
    assert_eq!(indexing.label, "Indexing ~/Pictures");
    assert_eq!(indexing.progress, Some(0.26));
    assert_eq!(
        indexing.work,
        Some(WorkInfo {
            job_id: "job-1".into(),
            count: Some("headers".into()),
            estimate: Some("about 10 s".into()),
        })
    );
    assert!(indexing.running && indexing.detail.is_none());
}

/// A cancelled catalog job's row, once nothing long runs, keeps its place and says it was
/// cancelled: what happened is the row's recent state.
#[test]
fn a_cancelled_catalog_job_reads_cancelled_in_the_section() {
    use crate::state::performance::{Work, jobs};
    let mut cancelled = finished(
        indexing(1, 0, None, "5,120 files found"),
        Outcome::Cancelled,
        2_400,
    );
    cancelled.ended_ms_ago = 1_200;
    let snapshot = ActivitySnapshot {
        recent: vec![cancelled],
        ..ActivitySnapshot::default()
    };
    let listed = jobs(Work {
        board: Some(&snapshot),
        rates: &Rates::EMPTY,
        home: home(),
    });
    let [row] = listed.rows.as_slice() else {
        panic!("one row: {:?}", listed.rows);
    };
    assert_eq!(row.label, "Indexing ~/Pictures");
    assert_eq!(row.detail.as_deref(), Some("Cancelled 1 s ago"));
    assert!(
        !row.running && row.work.is_none(),
        "a finished job has no Cancel"
    );
}
