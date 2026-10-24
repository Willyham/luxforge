//! Removing the objects `task.select-object` picked, through the catalog owner as a client drives
//! it: `task.fill-generative` over `object-artifacts` alone and beside a painted selection and an
//! effect stroke, each candidate naming the objects and their prompts, an accept, the refusals of
//! a stale, foreign, unknown or excess selection before anything is queued, and a model mask that
//! keeps the prompt a Recompute selects with again.
use super::{
    analysis::{ANALYSE, Analysis, SELECT, segmenter_ready},
    *,
};
use crate::modules::ai::analysis::models::{FAKE_SEGMENTER_DECODER, FAKE_SEGMENTER_ENCODER};

/// The selection a finished `task.select-object` answers.
fn select(owner: &Owner, params: Value) -> Value {
    let job = owner.task(SELECT, params);
    assert_eq!(job["status"], "ready", "{job}");
    job["result"]["result"].clone()
}

fn analyse(owner: &Owner, asset: &AssetId) {
    let job = owner.task(ANALYSE, json!({"asset_id": asset}));
    assert_eq!(job["status"], "ready", "{job}");
}

/// A fill of `artifacts` alone.
fn objects(asset: &AssetId, artifacts: Value) -> Value {
    json!({
        "asset_id": asset,
        "object-artifacts": artifacts,
        "model": FAKE_GENERATOR.resource,
    })
}

/// `[x0, y0, width, height]` of a result's `rect`.
fn rect(result: &Value) -> [u64; 4] {
    let rect = result["rect"].as_array().expect("a rect");
    [0, 1, 2, 3].map(|i| rect[i].as_u64().unwrap())
}

fn holds(outer: [u64; 4], inner: [u64; 4]) -> bool {
    outer[0] <= inner[0]
        && outer[1] <= inner[1]
        && outer[0] + outer[2] >= inner[0] + inner[2]
        && outer[1] + outer[3] >= inner[1] + inner[3]
}

fn refused(owner: &Owner, params: Value, code: &str, words: &str) -> ApiFailure {
    let error = owner.fill(params).expect_err("refused");
    assert_eq!(error.code, code, "{error:?}");
    assert!(error.message.contains(words), "{error:?}");
    error
}

#[test]
#[slow]
fn a_fill_removes_the_objects_picked_on_the_photograph_as_it_is_and_names_them() {
    let fixture = Fixture::new("objects-fill");
    let [asset, other] = fixture.assets.clone();
    let owner = segmenter_ready(&fixture);
    owner.install(&FAKE_GENERATOR);
    analyse(&owner, &asset);
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    let picked = select(&owner, point.clone());
    assert_eq!(picked["prompt"], json!({"point": [[0.5, 0.5]]}), "{picked}");
    let artifact = picked["artifact"].clone();
    let fraction = picked["candidate"]["bounds"].as_array().expect("bounds");
    let at = |i: usize, side: f64| fraction[i].as_f64().unwrap() * side;
    let (x0, y0) = (at(0, 480.0).floor() as u64, at(1, 320.0).floor() as u64);
    let (x1, y1) = (at(2, 480.0).ceil() as u64, at(3, 320.0).ceil() as u64);
    let object = [x0, y0, x1 - x0, y1 - y0];

    // The object alone: its region holds it, and every candidate names it and its prompt.
    let alone = owner.generate(with(
        objects(&asset, json!([artifact])),
        json!({"seed": 3, "candidates": 2}),
    ));
    let result = &alone["result"]["result"];
    let region = rect(result);
    assert!(holds(region, object), "{region:?} does not hold {object:?}");
    for row in alone["candidates"].as_array().unwrap() {
        assert_eq!(
            row["provenance"]["objects"],
            json!([{"artifact": artifact, "prompt": picked["prompt"]}]),
            "{row}"
        );
    }
    // Beside an effect stroke and a painted selection, the region is the union.
    let effect = owner.generate(with(
        objects(&asset, json!([artifact])),
        json!({"seed": 3, "effect-points": [[0.1, 0.9], [0.2, 0.9]], "effect-radius": 0.03}),
    ));
    let joined = rect(&effect["result"]["result"]);
    assert!(holds(joined, region) && joined != region, "{joined:?}");
    let painted = owner.generate(with(
        objects(&asset, json!([artifact])),
        json!({"seed": 3, "selection-points": [[0.1, 0.1], [0.2, 0.1]], "selection-radius": 0.03}),
    ));
    let joined = rect(&painted["result"]["result"]);
    assert!(holds(joined, region) && joined != region, "{joined:?}");

    // Accepting a candidate changes the region and nothing outside it.
    let outside = if region[0] > 2 { (1, 1) } else { (479, 319) };
    assert!(!holds(region, [outside.0, outside.1, 1, 1]));
    let centre = (
        (region[0] + region[2] / 2) as u32,
        (region[1] + region[3] / 2) as u32,
    );
    let outside = (outside.0 as u32, outside.1 as u32);
    let before = (
        owner.sample(&asset, outside.0, outside.1),
        owner.sample(&asset, centre.0, centre.1),
    );
    let accepted = owner
        .accept_as(owner.edit, &asset, &alone["job_id"], 1)
        .unwrap();
    assert_eq!(accepted["outcome"], "applied", "{accepted}");
    assert_eq!(owner.sample(&asset, outside.0, outside.1), before.0);
    assert_ne!(owner.sample(&asset, centre.0, centre.1), before.1);

    // The patch is a repair, so the selection is of the photograph as it was: refused by name
    // before anything is queued.
    let generations = fixture.controls().generations();
    let stale = refused(
        &owner,
        objects(&asset, json!([artifact])),
        "conflict",
        "repaired",
    );
    assert_eq!(
        stale.data,
        Some(json!({"reason": "stale-selection", "artifact": artifact, "asset_id": asset})),
    );
    // Selecting it again on the photograph as it is now removes it again.
    analyse(&owner, &asset);
    let again = select(&owner, point);
    let fill = owner.generate(objects(&asset, json!([again["artifact"]])));
    assert_eq!(fill["status"], "ready");
    let generations = generations + 1;
    // Another photograph's, an unknown artifact, nine and one twice are refused too.
    refused(
        &owner,
        objects(&other, json!([again["artifact"]])),
        "conflict",
        "another photograph",
    );
    let unknown = format!("artifact-{}", "0".repeat(64));
    refused(
        &owner,
        objects(&asset, json!([unknown])),
        "validation",
        "unknown artifact",
    );
    let nine: Vec<Value> = (0..9).map(|_| again["artifact"].clone()).collect();
    refused(&owner, objects(&asset, json!(nine)), "validation", "9");
    refused(
        &owner,
        objects(&asset, json!([again["artifact"], again["artifact"]])),
        "validation",
        "twice",
    );
    assert_eq!(fixture.controls().generations(), generations);
    owner.stop();
}

#[test]
#[slow]
fn a_model_mask_keeps_the_prompt_that_selects_its_object_again() {
    let fixture = Fixture::new("objects-prompt");
    let [asset, _] = fixture.assets.clone();
    let owner = segmenter_ready(&fixture);
    analyse(&owner, &asset);
    let picked = select(&owner, json!({"asset_id": asset, "point": [[0.5, 0.5]]}));
    let fields = |selection: &Value, extra: Value| {
        let mut params = extra;
        for field in [
            "artifact",
            "class",
            "model",
            "model_version",
            "input_fingerprint",
            "prefix_hash",
            "prompt",
        ] {
            params[field] = selection[field].clone();
        }
        params
    };
    let mutation = |label: &str| mutation_json(owner.edit_revision(&asset), label);
    let created = owner.ok(
        "mask.create-model",
        fields(
            &picked,
            json!({"asset_id": asset, "mutation": mutation("select")}),
        ),
    );
    let mask = created["mask"].clone();
    let listed = |index: usize| {
        owner.ok("mask.list", json!({"asset_id": asset}))["masks"][0]["components"][index].clone()
    };
    let component = listed(0);
    assert_eq!(
        component["model"]["prompt"], picked["prompt"],
        "{component}"
    );
    assert_eq!(
        component["payload"]["prompt"], picked["prompt"],
        "{component}"
    );

    // Recompute: the stored prompt is the request that selects the same object again.
    let mut request = json!({"asset_id": asset});
    for (key, value) in component["model"]["prompt"].as_object().unwrap() {
        request[key] = value.clone();
    }
    let again = select(&owner, request);
    assert_eq!(again["artifact"], picked["artifact"]);
    owner.ok(
        "mask.set-model",
        fields(
            &again,
            json!({"asset_id": asset, "mutation": mutation("recompute"), "mask": mask,
                   "component": component["id"]}),
        ),
    );
    assert_eq!(listed(0)["model"]["prompt"], picked["prompt"]);

    // A stroke's prompt is kept with its radius and candidate; null clears a prompt.
    let stroke = json!({"stroke": [[0.3, 0.5], [0.7, 0.5]], "radius": 0.03, "candidate": 1});
    let mut add = fields(
        &picked,
        json!({"asset_id": asset, "mutation": mutation("add"), "mask": mask, "mode": "add"}),
    );
    add["prompt"] = stroke.clone();
    owner.ok("mask.add-model", add);
    assert_eq!(listed(1)["model"]["prompt"], stroke);
    owner.ok(
        "mask.set-model",
        json!({"asset_id": asset, "mutation": mutation("clear"), "mask": mask,
               "component": component["id"], "prompt": null}),
    );
    let cleared = listed(0);
    assert!(cleared["model"]["prompt"].is_null(), "{cleared}");
    assert!(cleared["payload"].get("prompt").is_none(), "{cleared}");

    // A prompt is an object's alone, and a malformed one is refused.
    for (class, prompt) in [
        ("subject", picked["prompt"].clone()),
        ("object", json!({"point": [[0.5, 0.5]], "radius": 0.1})),
        (
            "object",
            json!({"point": [[0.5, 0.5]], "stroke": [[0.5, 0.5]]}),
        ),
        ("object", json!({"stroke": [[0.5, 0.5]]})),
    ] {
        let mut params = fields(
            &picked,
            json!({"asset_id": asset, "mutation": mutation("refused")}),
        );
        params["class"] = json!(class);
        params["prompt"] = prompt.clone();
        let response = owner.call(owner.edit, "mask.create-model", params);
        let error = response.error.expect("refused");
        assert_eq!(error.code, "validation", "{class} {prompt}: {error:?}");
    }
    owner.stop();
}

#[test]
#[slow]
fn an_object_artifact_not_kept_ready_is_prepared_by_the_source_work_first() {
    // One module instance across two owners: the second owner's catalog keeps nothing ready, but
    // the module still holds its record of the selection, as a long-lived process whose prepared
    // cache evicted the artifact would.
    let fixture = Fixture::new("objects-prepare");
    let [asset, _] = fixture.assets.clone();
    let registry = registry(&fixture.gate);
    let catalog = fixture.root.join("catalog.sqlite");
    let owner = Owner::start(&catalog, registry.clone(), fixture.host(true));
    owner.analysis_flag(true);
    owner.install(&FAKE_SEGMENTER_ENCODER.file);
    owner.install(&FAKE_SEGMENTER_DECODER.file);
    owner.install(&FAKE_GENERATOR);
    analyse(&owner, &asset);
    let picked = select(&owner, json!({"asset_id": asset, "point": [[0.5, 0.5]]}));
    // Kept as a mask, so the catalog keeps the artifact past this session.
    let mut create = json!({
        "asset_id": asset,
        "mutation": mutation_json(owner.edit_revision(&asset), "keep"),
    });
    for field in [
        "artifact",
        "class",
        "model",
        "model_version",
        "input_fingerprint",
        "prefix_hash",
    ] {
        create[field] = picked[field].clone();
    }
    owner.ok("mask.create-model", create);
    owner.stop();

    let owner = Owner::start(&catalog, registry, fixture.host(true));
    let params = objects(&asset, json!([picked["artifact"]]));
    let response = owner.call(owner.edit, TASK, params.clone());
    let error = response.error.expect("a preparation first");
    assert_eq!(error.code, "preparation-required", "{error:?}");
    let job = owner.finished(&json!(error.job_id.expect("a preparation job")));
    assert_eq!(job["status"], "ready", "{job}");
    let filled = owner.generate(params);
    assert_eq!(
        filled["candidates"][0]["provenance"]["objects"][0]["artifact"],
        picked["artifact"]
    );
    owner.stop();
}
