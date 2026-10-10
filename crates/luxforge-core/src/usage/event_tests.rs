//! The session's numbering, timing, banks, drops and storage bounds.
use super::*;
use crate::usage::{
    BodyBuffer, DeviceProfile,
    registry::{MAX_BODY_BYTES, MAX_DEVICE_BYTES},
};
use std::{
    collections::HashSet,
    sync::{Barrier, atomic::AtomicBool},
    thread,
};

fn start() -> SessionStart {
    SessionStart::parse("2026-10-09T14:03:12Z").unwrap()
}

fn key(byte: u8) -> ImageKey {
    ImageKey::from_bytes([byte; 32])
}

fn idle() -> EventKind {
    EventKind::AppIdle {
        state: Idle::Active,
    }
}

fn manual_session() -> (Session, Arc<AtomicU32>) {
    let gate = Gate::new();
    let (clock, millis) = SessionClock::manual();
    (
        Session::new(gate.enable(), Trigger::Launch, start(), clock),
        millis,
    )
}

#[test]
fn usage_a_session_begins_with_session_start_at_zero_and_numbers_events_in_order() {
    let (session, millis) = manual_session();
    millis.store(40, Ordering::Release);
    assert_eq!(
        session.record(EventKind::WorkspaceEnter {
            workspace: Workspace::Develop
        }),
        Recorded::Stored(1)
    );
    millis.store(41, Ordering::Release);
    assert_eq!(session.record(idle()), Recorded::Stored(2));
    let window = session.detach().expect("a window");
    let seen: Vec<(u32, u32)> = window.events().iter().map(|e| (e.seq, e.t)).collect();
    assert_eq!(seen, [(0, 0), (1, 40), (2, 41)]);
    assert_eq!(
        window.events()[0].kind,
        EventKind::SessionStart {
            trigger: Trigger::Launch
        }
    );
    assert_eq!(window.batch_seq(), 0);
    assert_eq!(window.dropped(), 0);
}

#[test]
fn usage_a_full_bank_drops_and_counts_and_the_seq_is_still_consumed() {
    let (session, _) = manual_session();
    for _ in 1..MAX_EVENTS {
        assert!(matches!(session.record(idle()), Recorded::Stored(_)));
    }
    assert_eq!(session.record(idle()), Recorded::Dropped);
    assert_eq!(session.record(idle()), Recorded::Dropped);
    assert_eq!(session.window().dropped, 2);
    {
        let window = session.detach().unwrap();
        assert_eq!(window.events().len(), MAX_EVENTS);
        assert_eq!(window.dropped(), 2);
        // The next window's first event shows the gap: seq 512 and 513 were dropped.
        assert_eq!(session.record(idle()), Recorded::Stored(514));
    }
    let next = session.detach().unwrap();
    assert_eq!(next.events()[0].seq, 514);
    assert_eq!(next.dropped(), 0);
    assert_eq!(next.batch_seq(), 1);
    let diagnostics = session.diagnostics();
    assert_eq!(diagnostics.recorded, MAX_EVENTS as u64 + 1);
    assert_eq!(diagnostics.dropped, 2);
}

#[test]
fn usage_each_detached_window_holds_only_events_no_earlier_window_held() {
    let (session, _) = manual_session();
    let first: Vec<u32> = session
        .detach()
        .unwrap()
        .events()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert!(
        session.detach().is_none(),
        "an empty window is not detached"
    );
    session.record(idle());
    session.record(idle());
    let second: Vec<u32> = session
        .detach()
        .unwrap()
        .events()
        .iter()
        .map(|e| e.seq)
        .collect();
    session.record(idle());
    let third_window = session.detach().unwrap();
    let third: Vec<u32> = third_window.events().iter().map(|e| e.seq).collect();
    assert_eq!((first, second, third), (vec![0], vec![1, 2], vec![3]));
    assert_eq!(third_window.batch_seq(), 2);
}

#[test]
fn usage_a_window_discarded_unsent_leaves_a_batch_seq_gap() {
    let (session, _) = manual_session();
    drop(session.detach().unwrap()); // batch 0, discarded (expired, say)
    session.record(idle());
    assert_eq!(session.detach().unwrap().batch_seq(), 1);
}

#[test]
fn usage_a_producer_never_writes_the_window_the_worker_holds() {
    let (session, _) = manual_session();
    let window = session.detach().unwrap();
    // While held, producers write the other bank; a second detach is refused, not merged.
    assert_eq!(session.record(idle()), Recorded::Stored(1));
    assert!(session.detach().is_none());
    assert_eq!(window.events().len(), 1);
    drop(window);
    assert_eq!(session.detach().unwrap().events()[0].seq, 1);
}

#[test]
fn usage_concurrent_floods_stay_within_the_banks_and_every_seq_is_stored_or_dropped_once() {
    const THREADS: usize = 8;
    const EACH: usize = 20_000;
    let gate = Gate::new();
    let session = Session::new(
        gate.enable(),
        Trigger::Launch,
        start(),
        SessionClock::monotonic(),
    );
    let capacity_before = session.storage_bytes();
    let barrier = Barrier::new(THREADS + 1);
    let done = AtomicBool::new(false);
    let mut batches: Vec<(Vec<Event>, u32, u32)> = Vec::new();
    thread::scope(|scope| {
        let producers: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    let mut outcome = (0usize, 0usize);
                    for _ in 0..EACH {
                        match session.record(idle()) {
                            Recorded::Stored(_) => outcome.0 += 1,
                            Recorded::Dropped => outcome.1 += 1,
                            Recorded::Off => unreachable!(),
                        }
                    }
                    outcome
                })
            })
            .collect();
        barrier.wait();
        let worker = scope.spawn(|| {
            let mut held = Vec::new();
            while !done.load(Ordering::Acquire) {
                if let Some(window) = session.detach() {
                    held.push((
                        window.events().to_vec(),
                        window.dropped(),
                        window.batch_seq(),
                    ));
                }
            }
            held
        });
        let totals = producers
            .into_iter()
            .map(|p| p.join().unwrap())
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        done.store(true, Ordering::Release);
        batches = worker.join().unwrap();
        while let Some(window) = session.detach() {
            batches.push((
                window.events().to_vec(),
                window.dropped(),
                window.batch_seq(),
            ));
        }
        let stored: usize = batches.iter().map(|(events, _, _)| events.len()).sum();
        let dropped: u64 = batches
            .iter()
            .map(|(_, dropped, _)| u64::from(*dropped))
            .sum();
        assert_eq!(
            stored,
            totals.0 + 1,
            "every stored event is in exactly one batch"
        );
        // Drops while the active window is empty carry over into the next window, so the last
        // (empty) window may still hold some.
        assert_eq!(
            dropped + u64::from(session.window().dropped),
            totals.1 as u64
        );
        assert_eq!(session.diagnostics().dropped, totals.1 as u64);
    });
    let mut seen = HashSet::new();
    let mut last_seq = None;
    let mut last_t = 0;
    for (index, (events, _, batch_seq)) in batches.iter().enumerate() {
        assert_eq!(*batch_seq as usize, index);
        assert!(events.len() <= MAX_EVENTS);
        for event in events {
            assert!(
                last_seq.is_none_or(|last| event.seq > last),
                "seq increases across batches"
            );
            assert!(event.t >= last_t, "t never decreases");
            assert!(seen.insert(event.seq));
            last_seq = Some(event.seq);
            last_t = event.t;
        }
    }
    let total = (THREADS * EACH + 1) as u32;
    assert!(seen.iter().all(|seq| *seq < total));
    assert_eq!(
        session.storage_bytes(),
        capacity_before,
        "the banks never grew"
    );
}

#[test]
fn usage_a_collector_that_is_off_stores_nothing_and_owns_no_session() {
    let collector = Collector::new();
    assert_eq!(collector.record(idle()), Recorded::Off);
    assert!(collector.session().is_none());
    let session = collector.start(Trigger::OptIn, start(), SessionClock::manual().0, key(1));
    let generation = collector.gate().current().unwrap();
    assert_eq!(session.generation(), generation);
    assert_eq!(collector.record(idle()), Recorded::Stored(1));
    collector.stop();
    assert_eq!(collector.record(idle()), Recorded::Off);
    assert!(collector.session().is_none());
    // Work started in the ended generation records nothing after a new opt-in.
    let next = collector.start(Trigger::OptIn, start(), SessionClock::manual().0, key(1));
    assert_ne!(next.id(), session.id(), "a new session ID per off/on");
    assert_eq!(collector.record_in(generation, idle()), Recorded::Off);
    assert_eq!(collector.record(idle()), Recorded::Stored(1));
    drop(session);
}

#[test]
fn usage_bounded_values_clip_and_invalid_observations_are_dropped() {
    assert_eq!(DurationMs::from_millis(u64::MAX).get(), MAX_DURATION_MS);
    assert_eq!(
        DurationMs::from_duration(Duration::from_secs(90_000)).get(),
        MAX_DURATION_MS
    );
    assert_eq!(DurationMs::exact(86_400_001), None);
    assert_eq!(Percent::from_f64(f64::NAN), None);
    assert_eq!(Percent::from_f64(f64::INFINITY), None);
    assert_eq!(Percent::from_f64(-0.5), None);
    assert_eq!(Percent::from_f64(37.4).map(Percent::get), Some(37));
    assert_eq!(Percent::from_f64(1e12).map(Percent::get), Some(MAX_PERCENT));
    assert_eq!(MemoryMib::from_bytes(2140 << 20).get(), 2140);
    assert_eq!(MemoryMib::from_bytes(u64::MAX).get(), MAX_MEMORY_MIB);
    assert_eq!(AddedCount::new(0), None);
    assert_eq!(
        AddedCount::new(5_000_000).map(AddedCount::get),
        Some(MAX_COUNT)
    );
    let export = OperationEnd::export(
        Outcome::Completed,
        DurationMs::from_millis(1),
        Some(u64::MAX),
        None,
    );
    assert_eq!(export.count(), Some(MAX_COUNT));
    assert!(
        OperationEnd::checked(
            Operation::CatalogScan,
            Outcome::Completed,
            DurationMs::from_millis(1),
            None,
            Some(ImageId::from_bytes([1; 16]))
        )
        .is_none()
    );
    assert!(
        OperationEnd::checked(
            Operation::AutoTone,
            Outcome::Completed,
            DurationMs::from_millis(1),
            Some(1),
            None
        )
        .is_none()
    );
}

#[test]
fn usage_an_event_record_is_small_fixed_and_string_free() {
    // `Copy` (checked by the derive) means no heap: no String, Vec or Box can hide inside.
    fn is_copy<T: Copy>() {}
    is_copy::<Event>();
    assert!(size_of::<Event>() <= 48, "{} bytes", size_of::<Event>());
}

#[test]
fn usage_all_statistics_storage_fits_256_kib() {
    let (session, _) = manual_session();
    let body = BodyBuffer::new();
    let total = session.storage_bytes()
        + body.storage_bytes()
        + size_of::<DeviceProfile>().max(MAX_DEVICE_BYTES)
        + size_of::<Collector>();
    assert_eq!(body.storage_bytes(), MAX_BODY_BYTES);
    assert!(total <= 256 * 1024, "{total} bytes");
    assert!(size_of::<DeviceProfile>() <= MAX_DEVICE_BYTES);
}

#[test]
fn usage_the_image_id_is_keyed_by_the_session_and_absent_while_off() {
    let fingerprint = "a".repeat(64);
    let collector = Collector::new();
    assert_eq!(collector.image_id(&fingerprint), None, "off: no key, no ID");
    let first = collector.start(Trigger::OptIn, start(), SessionClock::manual().0, key(1));
    let id = collector.image_id(&fingerprint).expect("on: an ID");
    assert_eq!(Some(id), key(1).image_id_of_hex(&fingerprint));
    assert_eq!(
        collector.image_id(&fingerprint),
        Some(id),
        "the same photograph keeps its ID within one key"
    );
    assert_ne!(collector.image_id(&"b".repeat(64)), Some(id));
    assert_eq!(collector.image_id("not a fingerprint"), None);
    assert!(!format!("{first:?}").contains(&format!("{:?}", key(1).as_bytes())));
    collector.stop();
    assert_eq!(
        collector.image_id(&fingerprint),
        None,
        "the key ends with the session"
    );
    // Opting in again brings a new key, and so a different ID for the same photograph.
    collector.start(Trigger::OptIn, start(), SessionClock::manual().0, key(2));
    assert_ne!(collector.image_id(&fingerprint), Some(id));
}

#[test]
fn usage_the_last_image_id_is_remembered_within_its_session_only() {
    let fingerprint = "c".repeat(64);
    let other = "d".repeat(64);
    let collector = Collector::new();
    let session = collector.start(Trigger::OptIn, start(), SessionClock::manual().0, key(1));
    let id = collector.image_id(&fingerprint).expect("on: an ID");
    assert_eq!(Some(id), key(1).image_id_of_hex(&fingerprint));
    assert_eq!(collector.image_id(&fingerprint.to_uppercase()), Some(id));
    // The second ask is answered from the memo, not computed again: a planted answer comes back.
    let planted = ImageId::from_bytes([7; 16]);
    lock(&session.image_memo)
        .as_mut()
        .expect("remembered")
        .image = planted;
    assert_eq!(collector.image_id(&fingerprint), Some(planted));
    // Another photograph replaces it, and the first is computed again.
    let other_id = collector.image_id(&other).expect("an ID");
    assert_eq!(Some(other_id), key(1).image_id_of_hex(&other));
    assert_eq!(collector.image_id(&fingerprint), Some(id));
    // A memo another thread holds is passed by: the ID is computed, without waiting.
    let held = lock(&session.image_memo);
    assert_eq!(collector.image_id(&other), Some(other_id));
    drop(held);
    // A new session never answers from the last one's memo, with the same key or a new one.
    lock(&session.image_memo)
        .as_mut()
        .expect("remembered")
        .image = planted;
    collector.start(Trigger::Launch, start(), SessionClock::manual().0, key(1));
    assert_eq!(collector.image_id(&fingerprint), Some(id));
    collector.stop();
    collector.start(Trigger::OptIn, start(), SessionClock::manual().0, key(2));
    let renewed = collector.image_id(&fingerprint).expect("an ID");
    assert_ne!(renewed, id);
    assert_eq!(Some(renewed), key(2).image_id_of_hex(&fingerprint));
    assert!(!format!("{session:?}").contains(&fingerprint));
}
