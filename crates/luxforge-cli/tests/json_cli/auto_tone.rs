//! Auto tone across the `luxforge-json` process boundary: `edit.auto-tone` answers the report its
//! values came from under `analysis.auto-tone`, and that report equals what `query.auto-tone`
//! answers for the same entry, before the action and, as Auto tone is idempotent, after it. What the
//! algorithm computes is proved in-process by the core's own tests.

use luxforge_core::auto_tone::FIELDS;
use luxforge_testbase::paths;
use luxforge_testkit::{JsonProcess, client::request_id};
use serde_json::{Value, json};

const ACTOR: &str = "auto-tone-json-cli";

fn mutation(revision: u64, tag: &str) -> Value {
    json!({"expected_revision": revision, "request_id": request_id(tag), "actor": ACTOR})
}

#[test]
fn the_actions_auto_tone_analysis_equals_the_querys_report_for_the_same_entry() {
    let catalog = paths::temp_catalog("auto-tone-json-cli");
    let mut client = JsonProcess::start(
        env!("CARGO_BIN_EXE_luxforge-json"),
        &["--catalog", catalog.to_str().expect("catalog is UTF-8")],
        "auto-tone",
    );
    let fixture = paths::jpeg().canonicalize().expect("the fixture");
    let asset = client.open(&fixture, ACTOR)["asset"]["id"].clone();
    let entry =
        client.call("asset.state", json!({"asset_id": asset}))["current_entry"]["id"].clone();

    let predicted = client.call("query.auto-tone", json!({"asset_id": asset}));
    assert_eq!(predicted["algorithm"], "auto-tone/1", "{predicted}");

    let applied = client.call(
        "edit.auto-tone",
        json!({"asset_id": asset, "mutation": mutation(0, "auto")}),
    );
    assert_eq!(applied["outcome"], "applied", "{applied}");
    assert_eq!(
        applied["analysis"]["auto-tone"], predicted,
        "the action answers the report the query gave for the entry it analysed"
    );
    assert_eq!(
        applied["analysis"].as_object().map(|map| map.len()),
        Some(1)
    );

    // The committed entry carries exactly the values that report predicted.
    let state = client.call("asset.state", json!({"asset_id": asset}));
    assert_eq!(state["current_entry"]["id"], applied["created_entry_id"]);
    assert_ne!(state["current_entry"]["id"], entry);
    assert_eq!(state["current_entry"]["label"], "Auto tone");
    let layers = state["current_entry"]["snapshot"]["recipe"]["layers"]
        .as_array()
        .expect("layers");
    let basic = layers
        .iter()
        .find(|layer| layer["effect_id"] == "luxforge.basic.adjust")
        .expect("a Basic layer");
    for field in FIELDS {
        assert_eq!(
            basic["payload"][field].as_f64().unwrap_or(0.0),
            predicted["values"][field]
                .as_f64()
                .expect("a predicted value"),
            "{field}"
        );
    }

    // Asked again on the new entry, the query reports the same analysis.
    assert_eq!(
        client.call("query.auto-tone", json!({"asset_id": asset})),
        predicted
    );
    client.finish();
    std::fs::remove_file(catalog).expect("the catalog is removed");
}
