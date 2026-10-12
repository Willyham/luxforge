use super::{
    worker::{CancelAt, progress_of, reads, stages},
    *,
};
use luxforge_test_macros::slow;

fn at(pairs: &[(&str, f64)]) -> Vec<(String, f64)> {
    pairs
        .iter()
        .map(|(name, at)| ((*name).to_owned(), *at))
        .collect()
}

#[test]
fn hdr_merge_measure_workloads_parse_and_name_their_brackets() {
    let workload = Workload::parse("60mp-9").unwrap();
    assert_eq!(workload, Workload::new(PhotoSize::Mp60, 9));
    assert_eq!(workload.name(), "60mp-9");
    let spec = workload.spec();
    assert_eq!((spec.frames, spec.step_ev, spec.handheld), (9, 1, true));
    assert_eq!(Workload::parse("24mp-3").unwrap().spec().step_ev, 2);
    for wrong in ["24mp", "30mp-3", "24mp-1", "24mp-10", "24mp-x"] {
        assert!(Workload::parse(wrong).is_err(), "{wrong}");
    }
    assert_eq!(
        Workload::DEFAULT.map(Workload::name),
        ["24mp-3", "60mp-9", "24mp-9", "60mp-3"]
    );
}

#[test]
fn hdr_merge_measure_stages_run_from_each_phase_to_the_next() {
    let phases = at(&[
        ("reading", 2.0),
        ("aligning", 900.0),
        ("merging", 1000.0),
        ("saving", 2500.0),
    ]);
    let found = stages(&phases, 2600.0);
    assert_eq!(found["reading"], 900.0, "reading counts from the request");
    assert_eq!(found["aligning"], 100.0);
    assert_eq!(found["merging"], 1500.0);
    assert_eq!(found["saving"], 100.0);

    // A phase never seen is null, and the one before it holds its time.
    let found = stages(&at(&[("reading", 1.0), ("merging", 50.0)]), 80.0);
    assert_eq!(found["reading"], 50.0);
    assert!(found["aligning"].is_null());
    assert_eq!(found["merging"], 30.0);
    assert!(found["saving"].is_null());
}

#[test]
fn hdr_merge_measure_reads_each_input_from_its_progress() {
    let messages = at(&[
        ("Reading 1 of 3", 5.0),
        ("Reading 2 of 3", 105.0),
        ("Reading 3 of 3", 225.0),
        ("Aligning", 400.0),
        ("Merging 1 of 4", 500.0),
    ]);
    let phases = at(&[("reading", 5.0), ("aligning", 400.0)]);
    assert_eq!(reads(&messages, &phases), [100.0, 120.0, 175.0]);
    // An input the board never showed leaves the reads untold.
    let missing = at(&[("Reading 1 of 3", 5.0), ("Reading 3 of 3", 225.0)]);
    assert!(reads(&missing, &phases).is_empty());
    assert!(reads(&messages, &at(&[("reading", 5.0)])).is_empty());
}

#[test]
fn hdr_merge_measure_cancels_where_the_board_shows_the_stage() {
    assert_eq!(progress_of("Merging 12 of 70", "Merging"), Some((12, 70)));
    assert_eq!(progress_of("Reading 2 of 9", "Merging"), None);
    assert!(CancelAt::Reading.due("reading", "Reading 2 of 3"));
    assert!(!CancelAt::Reading.due("reading", "Reading 1 of 3"));
    assert!(CancelAt::Aligning.due("aligning", "Aligning"));
    assert!(!CancelAt::Merging.due("merging", "Merging 34 of 70"));
    assert!(CancelAt::Merging.due("merging", "Merging 35 of 70"));
    assert!(CancelAt::Saving.due("saving", "Merging 70 of 70"));
    assert!(!CancelAt::Saving.due("merging", "Merging 70 of 70"));
}

#[test]
fn hdr_merge_measure_reports_a_failed_or_unmeasured_row() {
    let measured = row("a", "ms", [1.0, 2.0], "scope");
    assert_eq!(measured["status"], MEASURED);
    assert_eq!(measured["distribution"]["count"], 2);
    let empty = row("b", "ms", Vec::new(), "scope");
    assert_eq!(empty["status"], "not_measured");
    assert_eq!(status(std::slice::from_ref(&measured)), "passed");
    assert_eq!(status(&[measured.clone(), empty]), "incomplete");
    assert_eq!(status(&[measured, failed("c", "why")]), "failed");
    let left = spool_row("w", &[&json!({"spool_files_left": 1})]);
    assert_eq!(left["status"], FAILED);
    let none = spool_row("w", &[&json!({"spool_files_left": 0})]);
    assert_eq!(none["status"], MEASURED);
}

/// A photo-sized synthetic frame is admitted by the camera catalog as the synthetic body's own
/// recording mode at that size, with its default crop the frame the merge sees.
#[test]
#[slow]
fn hdr_merge_measure_photo_brackets_are_admitted_at_their_size() {
    luxforge_raw::admit_test_cameras();
    let bracket = Bracket::new(&BracketSpec::default());
    for size in PhotoSize::ALL {
        let (width, height) = size.sensor();
        let dng = bracket.photo_dng(0, size, vec![0; width as usize * height as usize * 2]);
        let raw = luxforge_raw::RawSource::decode(dng, &std::sync::atomic::AtomicBool::new(false))
            .unwrap_or_else(|error| panic!("{size:?}: {error}"));
        let metadata = raw.metadata();
        assert_eq!(
            metadata.mode.id(),
            format!("LuxforgeSyntheticBracketDng16W{width}H{height}")
        );
    }
}
