//! What one `draft.set` tick of a long brush stroke costs the catalog owner, counted in
//! allocations.
//!
//! A posted path is a JSON array with one small array per position, so every copy of it the owner
//! makes is about one allocation per position, while its typed reads (a path parsed, decimated or
//! hashed) are a few allocations whatever its length. Counting every allocation of the process
//! across one call, at two path lengths, therefore measures how many times the owner copies the
//! path per tick: the difference between the two counts, against the difference one copy of the
//! same two paths makes. The one copy a tick must make is its answer, which repeats the draft; the
//! session's draft takes the posted path itself.
//!
//! Over a stack with a spatial layer a complete tick is also planned, to seed its reads before it
//! is accepted, and that plan still copies the path twice more: the request a commit would send
//! (`Draft::request`), and the values the mask command planner splits from its identities.
//!
//! The counter sees every thread, so this binary holds one test and keeps the owner otherwise idle:
//! the source is prepared before any tick is counted, no tick reads a pixel, and each figure is the
//! least of several identical ticks.
use luxforge_core::{ApiRequest, ClientId, ModuleRegistry, OwnerHandle};
use luxforge_process::allocations::{self, Counting};
use luxforge_testbase::paths;
use serde_json::{Value, json};
use std::sync::Arc;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The posted lengths compared: a short stroke and the longest one a stroke stores.
const SHORT: usize = 64;
const LONG: usize = luxforge_core::path::POINTS_PER_STROKE;

/// Identical ticks per length; the least is the figure, so a stray wake-up elsewhere in the process
/// cannot raise it.
const TICKS: usize = 5;

/// Allocations a path's length may add to a tick beyond one copy's, for the typed reads whose
/// buffers grow by doubling: a few per read at most between these two lengths.
const GROWTH_ALLOWANCE: u64 = 32;

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("{method}-{}", allocations::allocations()),
                method: method.into(),
                params,
                token: None,
            },
        )
        .unwrap_or_else(|error| panic!("{method}: the owner did not answer: {error}"));
    match response.error {
        Some(error) => panic!("{method} failed: {} {}", error.code, error.message),
        None => response.result.expect("an answer"),
    }
}

fn wait(owner: &OwnerHandle, client: ClientId, job: &Value) {
    if job.is_null() {
        return;
    }
    luxforge_testbase::wait_until("a source job settling", || {
        let status = call(owner, client, "job.read", json!({"job_id": job}));
        !matches!(status["status"].as_str(), Some("queued" | "running"))
    });
}

/// A wave across the frame in `length` distinct positions, as a long drag posts it.
fn path(length: usize) -> Value {
    let points: Vec<[f64; 2]> = (0..length)
        .map(|index| {
            let along = index as f64 / length as f64;
            [0.1 + 0.8 * along, 0.5 + 0.3 * (along * 12.0).sin()]
        })
        .collect();
    json!(points)
}

/// What one `draft.set` carrying a path of `length` positions costs, as the least of [`TICKS`]
/// identical ticks after one that grows the draft to that length.
fn tick(owner: &OwnerHandle, client: ClientId, draft: &Value, length: usize) -> u64 {
    let request = |flow: f64| {
        json!({"draft_id": draft["draft_id"], "fields": {
            "points": path(length), "size": 0.05, "feather": 50.0, "flow": flow,
            "erase": false, "limit_to_colour": false, "colour_refine": 50.0,
        }})
    };
    call(owner, client, "draft.set", request(100.0));
    (0..TICKS)
        .map(|index| {
            let params = request(90.0 - index as f64);
            let before = allocations::allocations();
            let answer = call(owner, client, "draft.set", params);
            let counted = allocations::allocations() - before;
            assert_eq!(
                answer["fields"]["points"].as_array().map(Vec::len),
                Some(length),
                "the answer repeats the posted path"
            );
            counted
        })
        .min()
        .expect("ticks were counted")
}

/// What one copy of a path of `length` positions costs.
fn copy(length: usize) -> u64 {
    let path = path(length);
    let before = allocations::allocations();
    let copied = path.clone();
    let counted = allocations::allocations() - before;
    drop(copied);
    counted
}

#[test]
fn a_brush_tick_copies_its_path_once_whatever_its_length() {
    let catalog = paths::temp_catalog("draft-tick");
    let (owner, join) =
        OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::builtin())).unwrap();
    let client = owner.register();
    let queued = call(
        &owner,
        client,
        "catalog.import",
        json!({"path": paths::jpeg(), "mutation": {"request_id": "import", "actor": "test"}}),
    );
    wait(&owner, client, &queued["job_id"]);
    let status = call(
        &owner,
        client,
        "job.read",
        json!({"job_id": queued["job_id"]}),
    );
    let asset = status["result"]["asset"]["id"].clone();
    let prepared = call(&owner, client, "source.prepare", json!({"asset_id": asset}));
    wait(&owner, client, &prepared["job_id"]);
    let created = call(
        &owner,
        client,
        "mask.add-stroke",
        json!({"asset_id": asset, "points": [[0.5, 0.5]], "size": 0.05, "feather": 50.0,
        "flow": 100.0, "erase": false, "limit_to_colour": false, "colour_refine": 50.0,
        "mutation": {"expected_revision": 0, "request_id": "stroke", "actor": "test"}}),
    );
    let draft = call(
        &owner,
        client,
        "draft.begin",
        json!({"asset_id": asset, "action": "mask.add-stroke",
        "mask": created["mask"], "component": created["component"]}),
    );

    let short = tick(&owner, client, &draft, SHORT);
    let long = tick(&owner, client, &draft, LONG);
    let (copy_short, copy_long) = (copy(SHORT), copy(LONG));
    let per_tick = long.saturating_sub(short);
    let per_copy = copy_long - copy_short;
    eprintln!(
        "draft.set allocations: {short} at {SHORT} positions, {long} at {LONG}; one copy of the \
         path: {copy_short} and {copy_long}; the longer path adds {per_tick} to a tick and \
         {per_copy} to a copy"
    );
    assert!(
        per_tick <= per_copy + GROWTH_ALLOWANCE,
        "a tick of {LONG} positions makes {per_tick} more allocations than one of {SHORT}, more \
         than the {per_copy} of one copy of the path: the owner copies it more than once"
    );

    call(
        &owner,
        client,
        "draft.cancel",
        json!({"draft_id": draft["draft_id"]}),
    );

    // Over a spatial layer a complete tick is also planned, which copies the path twice more.
    call(
        &owner,
        client,
        "edit.set-detail",
        json!({"asset_id": asset, "luminance": 30.0,
        "mutation": {"expected_revision": 1, "request_id": "detail", "actor": "test"}}),
    );
    let draft = call(
        &owner,
        client,
        "draft.begin",
        json!({"asset_id": asset, "action": "mask.add-stroke",
        "mask": created["mask"], "component": created["component"]}),
    );
    let short = tick(&owner, client, &draft, SHORT);
    let long = tick(&owner, client, &draft, LONG);
    let per_planned = long.saturating_sub(short);
    eprintln!(
        "planned draft.set allocations: {short} at {SHORT} positions, {long} at {LONG}; the longer \
         path adds {per_planned}"
    );
    assert!(
        per_planned <= 3 * per_copy + GROWTH_ALLOWANCE,
        "a planned tick of {LONG} positions makes {per_planned} more allocations than one of \
         {SHORT}, more than its answer and its plan's two copies of the path: {per_copy} each"
    );
    call(
        &owner,
        client,
        "draft.cancel",
        json!({"draft_id": draft["draft_id"]}),
    );
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(&catalog);
}
