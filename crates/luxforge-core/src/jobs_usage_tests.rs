//! A job's terminal usage event is recorded in the consent generation it began in: work begun
//! before an opt-in, or across a revocation, records nothing; a Develop counts only the
//! photographs its committed batches created, however it ends; a cancelled or failed export counts
//! nothing written.
use super::*;
use crate::usage::{
    Collector, EventKind, ImageKey, Operation, Outcome as Ended, SessionClock, SessionStart,
    Trigger,
};

fn table(collector: &Arc<Collector>) -> Jobs {
    let mut jobs = Jobs::new(Arc::new(|_, _| {}), ActivityBoard::new());
    jobs.set_usage(collector.clone());
    jobs
}

fn start(collector: &Collector) {
    collector.start(
        Trigger::OptIn,
        SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
        SessionClock::manual().0,
        ImageKey::from_bytes([1; 32]),
    );
}

fn open(jobs: &mut Jobs, kind: JobKind) -> JobId {
    let job_id = JobId::new();
    jobs.open_catalog(CatalogOpened {
        job_id: job_id.clone(),
        kind,
        asset_id: None,
        origin: None,
        control: JobControl::new(),
    });
    job_id
}

fn recorded(collector: &Collector) -> Vec<EventKind> {
    let Some(session) = collector.session() else {
        return Vec::new();
    };
    let Some(window) = session.detach() else {
        return Vec::new();
    };
    window
        .events()
        .iter()
        .map(|event| event.kind)
        .filter(|kind| !matches!(kind, EventKind::SessionStart { .. }))
        .collect()
}

fn develop_report() -> Output {
    let folder = crate::catalog_types::CatalogFolderId::new();
    Output::Value(json!({
        "developed": [
            {"path": "/a.jpg", "asset_id": AssetId::new(), "outcome": "created", "folder_id": folder},
            {"path": "/b.jpg", "asset_id": AssetId::new(), "outcome": "linked", "folder_id": folder},
            {"path": "/c.jpg", "asset_id": AssetId::new(), "outcome": "relinked", "folder_id": folder},
            {"path": "/d.jpg", "asset_id": AssetId::new(), "outcome": "created", "folder_id": folder},
        ],
        "failed": [{"path": "/e.jpg", "code": "decode", "message": "no"}],
        "changes": [],
    }))
}

#[test]
fn usage_a_job_begun_before_an_opt_in_or_across_a_revocation_records_nothing() {
    let collector = Arc::new(Collector::new());
    let mut jobs = table(&collector);
    // Begun while off, finished after an opt-in.
    let before = open(&mut jobs, JobKind::DevelopPicks);
    start(&collector);
    jobs.finish(&before, Ok(develop_report()));
    // Begun in one generation, finished in the next.
    let revoked = open(&mut jobs, JobKind::IndexRefresh);
    collector.stop();
    start(&collector);
    jobs.finish(&revoked, Ok(Output::Value(json!({}))));
    assert_eq!(recorded(&collector), []);
    // Begun and finished in the current one.
    let current = open(&mut jobs, JobKind::IndexRefresh);
    jobs.finish(&current, Err(Error::cancelled("stopped")));
    let events = recorded(&collector);
    let [EventKind::OperationEnd(end)] = events.as_slice() else {
        panic!("{events:?}");
    };
    assert_eq!(
        (end.operation(), end.outcome(), end.count(), end.image()),
        (Operation::CatalogScan, Ended::Cancelled, None, None)
    );
    jobs.shutdown();
}

fn added(count: u64) -> EventKind {
    EventKind::ImagesAdded {
        count: crate::usage::AddedCount::new(count).unwrap(),
    }
}

#[test]
fn usage_a_develop_counts_only_the_photographs_it_created_and_a_retry_counts_none() {
    let collector = Arc::new(Collector::new());
    start(&collector);
    let mut jobs = table(&collector);
    // Two batches committed: one created photograph each (the report's linked and relinked picks
    // were never counted).
    let developed = open(&mut jobs, JobKind::DevelopPicks);
    jobs.usage_added(&developed, 1);
    jobs.usage_added(&developed, 1);
    jobs.finish(&developed, Ok(develop_report()));
    let retried = open(&mut jobs, JobKind::DevelopPicks);
    jobs.forget_usage(&retried);
    jobs.usage_added(&retried, 2);
    jobs.finish(&retried, Ok(develop_report()));
    let failed = open(&mut jobs, JobKind::DevelopPicks);
    jobs.finish(&failed, Err(Error::internal("the lane stopped")));
    let nothing_new = open(&mut jobs, JobKind::DevelopPicks);
    jobs.usage_added(&nothing_new, 0);
    jobs.finish(
        &nothing_new,
        Ok(Output::Value(
            json!({"developed": [], "failed": [], "changes": []}),
        )),
    );
    assert_eq!(recorded(&collector), [added(2)]);
    jobs.shutdown();
}

#[test]
fn usage_a_cancelled_or_failed_develop_counts_the_photographs_its_committed_batches_created() {
    let collector = Arc::new(Collector::new());
    start(&collector);
    let mut jobs = table(&collector);
    let cancelled = open(&mut jobs, JobKind::DevelopPicks);
    jobs.usage_added(&cancelled, 3);
    jobs.finish(&cancelled, Err(Error::cancelled("stopped")));
    let failed = open(&mut jobs, JobKind::DevelopPicks);
    jobs.usage_added(&failed, 1);
    jobs.finish(&failed, Err(Error::internal("the lane stopped")));
    assert_eq!(recorded(&collector), [added(3), added(1)]);
    // Begun while off: its batches count nothing.
    collector.stop();
    let off = open(&mut jobs, JobKind::DevelopPicks);
    start(&collector);
    jobs.usage_added(&off, 2);
    jobs.finish(&off, Err(Error::cancelled("stopped")));
    assert_eq!(recorded(&collector), []);
    jobs.shutdown();
}

#[test]
fn usage_a_batch_export_counts_what_it_wrote_only_when_it_completes() {
    let collector = Arc::new(Collector::new());
    start(&collector);
    let mut jobs = table(&collector);
    let written = open(&mut jobs, JobKind::BatchExport);
    jobs.finish(
        &written,
        Ok(Output::Value(json!({
            "done": [AssetId::new(), AssetId::new()],
            "written": [{"path": "/x.jpg"}, {"path": "/y.jpg"}],
            "skipped": [],
        }))),
    );
    let cancelled = open(&mut jobs, JobKind::BatchExport);
    jobs.finish(&cancelled, Err(Error::cancelled("stopped")));
    let refused = open(&mut jobs, JobKind::BatchExport);
    jobs.finish(&refused, Err(Error::validation("no folder")));
    let ends: Vec<_> = recorded(&collector)
        .into_iter()
        .map(|kind| match kind {
            EventKind::OperationEnd(end) => {
                (end.operation(), end.outcome(), end.count(), end.image())
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        ends,
        [
            (Operation::Export, Ended::Completed, Some(2), None),
            (Operation::Export, Ended::Cancelled, None, None),
            (Operation::Export, Ended::Refused, None, None),
        ]
    );
    jobs.shutdown();
}
