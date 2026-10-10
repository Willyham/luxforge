//! Corrections through the catalog owner, as clients call them: the automatic source search read
//! off the owner — `query.correction-sources` answered by the tile service and the analysis worker,
//! an `edit.add-correction` without a source parked while its read is answered and replayed once
//! against its revision, and a drafted add whose preview reads nothing and whose commit searches.
use super::OwnerHandle;
use crate::api::{ApiRequest, ApiResponse, ClientId};
use crate::editor::mutation_json;
use luxforge_testbase::paths::{jpeg as fixture, temp_path as temp};
use serde_json::{Value, json};

fn send(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiResponse {
    owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered")
}

fn ok(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    let response = send(owner, client, method, params);
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("a result")
}

fn revision(owner: &OwnerHandle, client: ClientId, asset: &Value) -> u64 {
    ok(owner, client, "asset.state", json!({"asset_id": asset}))["revision"]
        .as_u64()
        .unwrap()
}

#[test]
fn an_automatic_source_is_searched_off_the_owner_and_committed_against_its_revision() {
    let catalog = temp("owner-corrections.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let asset = super::library::opening::open(&owner, client, &fixture())["asset"]["id"].clone();
    let path = json!([[0.5, 0.5], [0.52, 0.51]]);

    let ranked = ok(
        &owner,
        client,
        "query.correction-sources",
        json!({"asset_id": asset, "points": path, "radius": 0.02}),
    );
    assert_eq!(ranked["outcome"], "found", "{ranked}");
    assert!(
        ranked["renderer"].is_object(),
        "the read names its renderer: {ranked}"
    );
    let best = ranked["candidates"][0].clone();

    let added = ok(
        &owner,
        client,
        "edit.add-correction",
        json!({"asset_id": asset, "mutation": mutation_json(0, "add"),
               "points": path, "radius": 0.02}),
    );
    assert_eq!(added["correction"]["operation"], "correction-1", "{added}");
    assert_eq!(added["correction"]["source-origin"], "auto");
    assert_eq!(added["correction"]["source-x"], best["source-x"]);
    assert_eq!(added["correction"]["source-y"], best["source-y"]);
    // A retry answers the same correction from the request, reading nothing.
    let retried = ok(
        &owner,
        client,
        "edit.add-correction",
        json!({"asset_id": asset, "mutation": mutation_json(0, "add"),
               "points": path, "radius": 0.02}),
    );
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(retried["correction"], added["correction"]);

    // Reset: the next search reads the repair layer's input with no operation over it.
    let before = revision(&owner, client, &asset);
    ok(
        &owner,
        client,
        "edit.reset-corrections",
        json!({"asset_id": asset, "mutation": mutation_json(before, "reset")}),
    );
    // A drafted automatic add previews the current stack, and its commit searches once.
    let begun = ok(
        &owner,
        client,
        "draft.begin",
        json!({"asset_id": asset, "action": "add-correction"}),
    );
    let draft = begun["draft_id"].clone();
    ok(
        &owner,
        client,
        "draft.set",
        json!({"draft_id": draft, "fields": {"points": [[0.2, 0.3]], "radius": 0.01}}),
    );
    let before = revision(&owner, client, &asset);
    let committed = ok(
        &owner,
        client,
        "draft.commit",
        json!({"draft_id": draft, "mutation": mutation_json(before, "commit")}),
    );
    assert_eq!(
        committed["correction"]["operation"], "correction-2",
        "{committed}"
    );
    assert_eq!(committed["correction"]["source-origin"], "auto");
    // A search over an earlier correction reads the repair layer's output: the operation it
    // samples is drawn first.
    let before = revision(&owner, client, &asset);
    let stacked = ok(
        &owner,
        client,
        "edit.add-correction",
        json!({"asset_id": asset, "mutation": mutation_json(before, "stacked"),
               "points": [[0.21, 0.31]], "radius": 0.01}),
    );
    assert_eq!(
        stacked["correction"]["operation"], "correction-3",
        "{stacked}"
    );
    let by_operation = ok(
        &owner,
        client,
        "query.correction-sources",
        json!({"asset_id": asset, "operation": "correction-3", "count": 1}),
    );
    assert_eq!(
        by_operation["candidates"][0]["source-x"],
        stacked["correction"]["source-x"]
    );
    let listed = ok(
        &owner,
        client,
        "query.corrections",
        json!({"asset_id": asset}),
    );
    assert_eq!(listed["operations"].as_array().unwrap().len(), 2);
    let labels: Vec<Value> =
        ok(&owner, client, "history.list", json!({"asset_id": asset}))["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["label"].clone())
            .collect();
    assert_eq!(
        labels[..3],
        [
            json!("Add Heal 2"),
            json!("Reset corrections"),
            json!("Add Heal 1")
        ]
    );

    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(catalog);
}
