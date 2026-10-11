//! The desktop's statistics supervisor seams that run without a process supervisor: the evidence
//! clock, and the feeds being no-ops where none was started, as in every unit test.
use super::usage_supervisor::{self, EvidenceClock, TEST_RELEASE};
use luxforge_core::usage::{Clock, Release};
use std::time::Duration;

#[test]
fn usage_the_evidence_clock_moves_only_the_schedule() {
    let clock = EvidenceClock::new();
    let session = clock.session_clock();
    let (before, wall) = (clock.monotonic(), clock.wall());
    clock.advance(Duration::from_secs(660));
    assert!(clock.monotonic() >= before + Duration::from_secs(660));
    // The wall clock and a session's event times stay real, so the move is never a sleep.
    assert!(clock.wall() < wall + Duration::from_secs(60));
    assert!(session.now_ms() < 60_000);
}

#[test]
fn usage_the_test_release_is_a_reportable_release() {
    assert!(Release::parse(TEST_RELEASE).is_some());
}

#[test]
fn usage_without_a_started_supervisor_every_feed_is_a_no_op() {
    usage_supervisor::interacted();
    assert!(!usage_supervisor::advance(Duration::from_secs(1)));
    assert_eq!(
        usage_supervisor::summary(),
        serde_json::json!({"started": false})
    );
}
