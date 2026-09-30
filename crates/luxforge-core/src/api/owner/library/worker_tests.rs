//! Lane C's worker: a job that commits its result in parts keeps every part it committed, in
//! order, and a cancel between parts commits nothing more.
use super::{super::catalog::CatalogMessage, *};
use crate::{
    ModuleRegistry,
    api::{ApiRequest, OwnerHandle, owner::OwnerMessage},
    catalog_types::jobs::SOURCE_CHECK,
};
use luxforge_testbase::Gate;
use serde_json::json;
use std::sync::Mutex;

fn job(owner: &OwnerHandle, client: ClientId, job_id: &JobId) -> Value {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: "read".into(),
                method: "job.read".into(),
                params: json!({"job_id": job_id}),
                token: None,
            },
        )
        .unwrap();
    response.result.expect("the job")
}

fn settled(owner: &OwnerHandle, client: ClientId, job_id: &JobId) -> Value {
    luxforge_testbase::wait_for("the job to settle", || {
        let read = job(owner, client, job_id);
        (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
    })
}

/// Queue a job on the lane whose work commits `parts` parts, waiting at `gate` before each after
/// the first, and records on the owner what each part committed.
fn queue(
    owner: &OwnerHandle,
    parts: usize,
    gate: Arc<Gate>,
    committed: Arc<Mutex<Vec<usize>>>,
) -> JobId {
    let job_id = JobId::new();
    let queued = job_id.clone();
    let work: Work = Box::new(move |context| {
        let mut answers = Vec::new();
        for part in 0..parts {
            if part > 0 {
                gate.pass();
            }
            let committed = committed.clone();
            match context.commit(Box::new(move |_owner| {
                committed.lock().unwrap().push(part);
                Ok(json!(part))
            })) {
                Ok(answer) => answers.push(answer),
                Err(error) => return Box::new(move |_| Err(error)),
            }
        }
        Box::new(move |_| Ok(json!({"parts": answers})))
    });
    let task = Task {
        job_id: job_id.clone(),
        job: &SOURCE_CHECK,
        asset_id: None,
        detail: None,
        origin: Origin::new("test.parts", "parts"),
        work,
    };
    owner
        .sender
        .send(OwnerMessage::Catalog(CatalogMessage::Library(
            LibraryMessage::Run(Box::new(move |owner| {
                owner
                    .catalog
                    .library
                    .queue(&mut owner.jobs, task)
                    .expect("the lane takes the job");
            })),
        )))
        .unwrap();
    queued
}

#[test]
fn a_library_job_commits_its_parts_in_order_and_a_cancel_between_parts_keeps_the_rest_out() {
    let catalog = luxforge_testbase::paths::temp_catalog("library-worker");
    let (owner, join) =
        OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::builtin())).unwrap();
    let client = owner.register();

    let open = Arc::new(Gate::new());
    let committed = Arc::new(Mutex::new(Vec::new()));
    let whole = queue(&owner, 3, open, committed.clone());
    let read = settled(&owner, client, &whole);
    assert_eq!(read["status"], "ready", "{read}");
    assert_eq!(read["result"], json!({"parts": [0, 1, 2]}));
    assert_eq!(*committed.lock().unwrap(), [0, 1, 2]);

    // Held before its second part, the job is cancelled: its first part stays committed, and
    // nothing after it is.
    let gate = Arc::new(Gate::new());
    gate.shut();
    let committed = Arc::new(Mutex::new(Vec::new()));
    let cancelled = queue(&owner, 3, gate.clone(), committed.clone());
    luxforge_testbase::wait_until("the first part", || committed.lock().unwrap().len() == 1);
    let response = owner
        .call(
            client,
            ApiRequest {
                id: "cancel".into(),
                method: "job.cancel".into(),
                params: json!({"job_id": cancelled}),
                token: None,
            },
        )
        .unwrap();
    assert!(response.error.is_none(), "{:?}", response.error);
    gate.open();
    let read = settled(&owner, client, &cancelled);
    assert_eq!(read["status"], "cancelled", "{read}");
    assert_eq!(
        *committed.lock().unwrap(),
        [0],
        "the part before the cancel stays"
    );

    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
}

fn events_after(owner: &OwnerHandle, client: ClientId, after: u64) -> Value {
    call(owner, client, "events.since", json!({"after": after}))
}

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .unwrap();
    response.result.expect("an answer")
}

/// Every job records one event as it ends, however it ends, under the request that started it and
/// naming the job: a waiting job cancelled ends with its cancel, a running one when its worker
/// posts back.
#[test]
fn a_library_job_records_its_end_naming_the_job_however_it_ends() {
    let catalog = luxforge_testbase::paths::temp_catalog("library-job-end");
    let (owner, join) =
        OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::builtin())).unwrap();
    let client = owner.register();
    let ends = |after: u64| -> Vec<(Value, Value, Value, Option<Value>)> {
        events_after(&owner, client, after)["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["method"].clone(),
                    event["request_id"].clone(),
                    event["job_id"].clone(),
                    event.get("library_sequence").cloned(),
                )
            })
            .collect()
    };
    let start = events_after(&owner, client, 0)["current_sequence"]
        .as_u64()
        .unwrap();

    let gate = Arc::new(Gate::new());
    gate.shut();
    let committed = Arc::new(Mutex::new(Vec::new()));
    let running = queue(&owner, 2, gate.clone(), committed.clone());
    luxforge_testbase::wait_until("the first part", || committed.lock().unwrap().len() == 1);
    let waiting = queue(
        &owner,
        1,
        Arc::new(Gate::new()),
        Arc::new(Mutex::new(Vec::new())),
    );
    assert_eq!(job(&owner, client, &waiting)["status"], "queued");
    assert!(ends(start).is_empty(), "a part that announces nothing");

    call(&owner, client, "job.cancel", json!({"job_id": waiting}));
    assert_eq!(
        ends(start),
        [(json!("test.parts"), json!("parts"), json!(waiting), None)],
        "the waiting job ends with its cancel"
    );

    gate.open();
    assert_eq!(settled(&owner, client, &running)["status"], "ready");
    assert_eq!(
        ends(start),
        [
            (json!("test.parts"), json!("parts"), json!(waiting), None),
            (json!("test.parts"), json!("parts"), json!(running), None),
        ],
        "the running job ends as its worker posts back"
    );

    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
}
