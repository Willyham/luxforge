//! The job reader's decisions and its stream: what a read is worth sending, what a pass makes of
//! it, and how the stream the subscription runs yields, ends and stays quiet. The export and
//! capability readers' own passes are tested with their seams.
use crate::app::{
    job_reads::{Pass, Verdict, Watch, reads},
    testing::Followed,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const FAST: Duration = Duration::from_micros(200);

/// A record that is final when it reaches 100.
fn ended(record: &u32) -> bool {
    *record >= 100
}

/// The first read and every record that differs from the last one sent are sent; a repeat is not.
/// What is compared is the last record *sent*, so a record that returns to an earlier value after
/// another was sent is news.
#[test]
fn a_watch_sends_the_first_read_and_every_change_and_nothing_else() {
    let mut watch = Watch::default();
    assert_eq!(watch.observe(&Ok(1), ended), Verdict::Yield, "the first");
    for _ in 0..50 {
        assert_eq!(watch.observe(&Ok(1), ended), Verdict::Skip, "the same");
    }
    assert_eq!(watch.observe(&Ok(2), ended), Verdict::Yield, "a change");
    assert_eq!(watch.observe(&Ok(2), ended), Verdict::Skip);
    assert_eq!(watch.observe(&Ok(1), ended), Verdict::Yield, "back again");
    assert!(!watch.finished());
}

/// An ended job and a failed read are each sent once, and the watch is finished with the job:
/// whatever it reads afterwards, however it differs, is not sent.
#[test]
fn a_watch_sends_an_end_or_a_failure_once_and_is_then_finished() {
    let mut watch = Watch::default();
    assert_eq!(watch.observe(&Ok(1), ended), Verdict::Yield);
    assert_eq!(watch.observe(&Ok(100), ended), Verdict::Last);
    assert!(watch.finished());
    for read in [Ok(100), Ok(7), Err("late".to_owned())] {
        assert_eq!(watch.observe(&read, ended), Verdict::Skip);
    }

    // A job that is already over at its first read is sent, once.
    let mut watch = Watch::default();
    assert_eq!(watch.observe(&Ok(100), ended), Verdict::Last);
    assert_eq!(watch.observe(&Ok(100), ended), Verdict::Skip);

    // A failed read is the end, wherever it falls.
    for before in [None, Some(1)] {
        let mut watch = Watch::default();
        if let Some(record) = before {
            assert_eq!(watch.observe(&Ok(record), ended), Verdict::Yield);
        }
        assert_eq!(
            watch.observe(&Err("the owner stopped".into()), ended),
            Verdict::Last
        );
        assert!(watch.finished());
        assert_eq!(watch.observe(&Ok(1), ended), Verdict::Skip);
    }
}

#[test]
fn a_pass_builds_its_message_only_when_there_is_one() {
    let built = AtomicUsize::new(0);
    let message = || {
        let _ = built.fetch_add(1, Ordering::Relaxed);
        "message"
    };
    assert!(matches!(Pass::of(Verdict::Skip, message), Pass::Quiet));
    assert_eq!(built.load(Ordering::Relaxed), 0);
    assert!(matches!(
        Pass::of(Verdict::Yield, message),
        Pass::Send("message")
    ));
    assert!(matches!(
        Pass::of(Verdict::Last, message),
        Pass::Last("message")
    ));
    assert_eq!(built.load(Ordering::Relaxed), 2);
}

/// The stream yields a message only for a pass that sends one: the first pass is made at once, the
/// ninety-nine quiet ones after it yield nothing and wake nobody, and the stream ends after its
/// last message with no pass beyond it.
#[test]
fn a_reader_yields_only_the_passes_that_send_and_ends_after_its_last() {
    let passes = Arc::new(AtomicUsize::new(0));
    let counted = passes.clone();
    let mut stream = Followed::new(reads(FAST, move || {
        match counted.fetch_add(1, Ordering::Relaxed) {
            0 => Pass::Send("first"),
            1..=99 => Pass::Quiet,
            100 => Pass::Send("changed"),
            101 => Pass::Last("last"),
            more => panic!("a pass after the last message: {more}"),
        }
    }));
    assert_eq!(stream.next(), Some("first"));
    assert_eq!(passes.load(Ordering::Relaxed), 1);
    assert_eq!(stream.next(), Some("changed"));
    assert_eq!(
        passes.load(Ordering::Relaxed),
        101,
        "ninety-nine passes found nothing and yielded nothing"
    );
    assert_eq!(stream.next(), Some("last"));
    assert_eq!(stream.next(), None, "the stream ends after the last");
    assert_eq!(stream.next(), None, "and stays ended");
    assert_eq!(passes.load(Ordering::Relaxed), 102);
}

/// A reader reads at once, not after its first interval: with an interval nothing in the test
/// outlasts, its first message still arrives, and no second pass is made until a tick is due.
#[test]
fn a_reader_reads_at_once_and_then_waits_for_its_interval() {
    let passes = Arc::new(AtomicUsize::new(0));
    let counted = passes.clone();
    let mut stream = Followed::new(reads(Duration::from_secs(3600), move || {
        Pass::Send(counted.fetch_add(1, Ordering::Relaxed))
    }));
    assert_eq!(stream.next(), Some(0));
    assert_eq!(passes.load(Ordering::Relaxed), 1);
}

/// A reader only reads while it is polled: a desktop that is behind holds the one read it has,
/// and one that has dropped the subscription makes no more.
#[test]
fn a_reader_reads_only_while_it_is_polled() {
    let passes = Arc::new(AtomicUsize::new(0));
    let counted = passes.clone();
    let mut stream = Followed::new(reads(FAST, move || {
        Pass::Send(counted.fetch_add(1, Ordering::Relaxed))
    }));
    assert_eq!(stream.next(), Some(0));
    assert_eq!(stream.next(), Some(1));
    assert_eq!(
        passes.load(Ordering::Relaxed),
        2,
        "a read is made for each message pulled and no more"
    );
    drop(stream);
    assert_eq!(passes.load(Ordering::Relaxed), 2, "nothing reads after");
}
