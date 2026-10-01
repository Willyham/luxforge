//! Lens selection across the process boundary agrees with the direct command service.
use luxforge_core::{EditorService, ErrorKind, Mutation};
use luxforge_testbase::{paths, wait_for};
use luxforge_testkit::JsonProcess;
use serde_json::json;

const ACTOR: &str = "lens-json-parity";
fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: ACTOR.into(),
    }
}

#[test]
fn lens_profile_selection_over_json_matches_service() {
    let source = paths::fixture("geometry/z6-24-70-35mm-grid.jpg");
    let original = std::fs::read(&source).unwrap();
    let direct_catalog = paths::temp_catalog("lens-direct");
    let pipe_catalog = paths::temp_catalog("lens-pipe");
    let mut service = EditorService::open(&direct_catalog).unwrap();
    let asset = service.import(&source).unwrap().asset.id;
    let rows = wait_for("the offline profile index", || {
        let entry = service.state(&asset).unwrap().current_entry.id;
        match service.run_query(
            &asset,
            &entry,
            "lens-profiles",
            json!({"assume-uncorrected":true}),
        ) {
            Ok(rows) => Some(rows),
            Err(error) if error.kind == ErrorKind::NotReady => None,
            Err(error) => panic!("{error}"),
        }
    });
    // No list without search text: the detected profile is the answer's suggestion.
    assert_eq!(rows["rows"], json!([]));
    let suggestion = &rows["status"]["suggestion"];
    assert_eq!(suggestion["eligible"], true, "{rows}");
    assert_eq!(
        suggestion["parameters"],
        json!({"assume-uncorrected": true})
    );
    let key = suggestion["key"].clone();
    let entry = service.state(&asset).unwrap().current_entry.id;
    let searched = service
        .run_query(
            &asset,
            &entry,
            "lens-profiles",
            json!({"text":"NIKKOR Z 24-70"}),
        )
        .unwrap();
    assert!(
        searched["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["key"] == key)
    );
    let parameters = json!({"profile":key,"assume-uncorrected":true});
    // Seed a neutral layer so both copies retain the same layer identity. Entry and snapshot
    // identities remain independently generated; compare their complete recipes and history rows.
    service
        .apply_action(
            &asset,
            mutation(0, "seed"),
            "select-lens-profile",
            parameters.clone(),
        )
        .unwrap();
    service
        .apply_action(
            &asset,
            mutation(1, "neutral"),
            "reset-lens-profile",
            json!({}),
        )
        .unwrap();
    drop(service);
    std::fs::copy(&direct_catalog, &pipe_catalog).unwrap();

    let mut client = JsonProcess::start(
        env!("CARGO_BIN_EXE_luxforge-json"),
        &["--catalog", pipe_catalog.to_str().unwrap()],
        "lens",
    );
    let prepared = client.call("source.prepare", json!({"asset_id":asset}));
    let ready = client.settle("job.read", &prepared["job_id"]);
    assert_eq!(ready["status"], "ready", "{ready}");
    let queried = wait_for("the pipe's offline index", || {
        let response = client.call_raw(
            "query.lens-profiles",
            json!({"asset_id":asset,"assume-uncorrected":true}),
        );
        match response["error"]["code"].as_str() {
            Some("not-ready") => None,
            None => Some(response["result"].clone()),
            _ => panic!("{response}"),
        }
    });
    // A neutral layer leaves the detected lens offered: the same answer as before any selection.
    assert_eq!(queried, rows);
    assert_eq!(
        client.call(
            "query.lens-profiles",
            json!({"asset_id":asset,"text":"NIKKOR Z 24-70"})
        ),
        searched
    );
    let refused = client.error(
        "edit.select-lens-profile",
        json!({"asset_id":asset,"profile":key,"mutation":mutation(2,"refused")}),
    );
    assert_eq!(refused["code"], "incompatible", "{refused}");
    assert_eq!(
        client.call("asset.state", json!({"asset_id":asset}))["revision"],
        2
    );
    let selected = client.call(
        "edit.select-lens-profile",
        json!({"asset_id":asset,"profile":key,"assume-uncorrected":true,
               "mutation":mutation(2,"select")}),
    );
    assert_eq!(selected["outcome"], "applied");
    let pipe_state = client.call("asset.state", json!({"asset_id":asset}));
    let pipe_history = client.call("history.list", json!({"asset_id":asset,"limit":100}));
    let same = client.call(
        "edit.select-lens-profile",
        json!({"asset_id":asset,"profile":key,"assume-uncorrected":true,
               "mutation":mutation(3,"same")}),
    );
    assert_eq!(same["outcome"], "no-op");
    client.finish();

    let mut service = EditorService::open(&direct_catalog).unwrap();
    service
        .prepare(&service.entry_needs(&asset, None).unwrap())
        .unwrap();
    service
        .apply_action(
            &asset,
            mutation(2, "select"),
            "select-lens-profile",
            parameters,
        )
        .unwrap();
    let direct = service.state(&asset).unwrap();
    assert_eq!(direct.revision, 3);
    assert_eq!(
        serde_json::to_value(&direct.current_entry.snapshot.recipe).unwrap(),
        pipe_state["current_entry"]["snapshot"]["recipe"]
    );
    let history = service.history(&asset, None, 100).unwrap();
    let pipe_rows = pipe_history["entries"].as_array().unwrap();
    assert_eq!(history.entries.len(), pipe_rows.len());
    for (direct, pipe) in history.entries.iter().zip(pipe_rows) {
        assert_eq!(direct.sequence, pipe["sequence"].as_u64().unwrap());
        assert_eq!(direct.action_id, pipe["action_id"].as_str().unwrap());
        assert_eq!(direct.label, pipe["label"].as_str().unwrap());
        assert_eq!(direct.actor, pipe["actor"].as_str().unwrap());
    }
    assert_eq!(std::fs::read(source).unwrap(), original);
    drop(service);
    std::fs::remove_file(direct_catalog).unwrap();
    std::fs::remove_file(pipe_catalog).unwrap();
}
