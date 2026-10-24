//! Removing a picked object over `luxforge-json`'s pipe, as an agent drives it: turn the analysis
//! flag on, install the developer segmenter and generator under their grants, analyse, select the
//! object under a point, keep it as a model mask with its prompt, Recompute it from the stored
//! prompt to the identical artifact, then fill with the selection's artifact and accept the
//! candidate, which changes the object and nothing outside its region.
use luxforge_test_macros::slow;
use luxforge_testbase::paths;
use luxforge_testkit::JsonProcess;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

const BINARY: &str = env!("CARGO_BIN_EXE_luxforge-json");
const DEVELOPER_ORIGIN: &str = "https://developer.luxforge.invalid";
/// What a model mask records of a selection, the prompt included.
const SELECTION_FIELDS: [&str; 7] = [
    "artifact",
    "class",
    "model",
    "model_version",
    "input_fingerprint",
    "prefix_hash",
    "prompt",
];

fn mutation(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "cli-test"})
}

fn edit_mutation(client: &mut JsonProcess, asset: &Value, request_id: &str) -> Value {
    let revision = client.call("asset.state", json!({"asset_id": asset}))["revision"].clone();
    json!({"expected_revision": revision, "request_id": request_id, "actor": "cli-test"})
}

/// Answer `method`, preparing the photograph first whenever the process asks.
fn prepared(client: &mut JsonProcess, method: &str, params: Value) -> Value {
    for _ in 0..4 {
        let response = client.call_raw(method, params.clone());
        match response["error"]["code"].as_str() {
            None => return response["result"].clone(),
            Some("preparation-required") => {
                let job = response["error"]["job_id"].clone();
                assert_eq!(client.settle("job.read", &job)["status"], "ready");
            }
            Some(_) => panic!("{method}: {response}"),
        }
    }
    panic!("{method} kept asking for preparation");
}

/// A task's finished job, under a request of its own unless `params` names one.
fn task(client: &mut JsonProcess, method: &str, mut params: Value) -> Value {
    static REQUESTS: AtomicU64 = AtomicU64::new(0);
    if params.get("mutation").is_none() {
        let n = REQUESTS.fetch_add(1, Ordering::Relaxed);
        params["mutation"] = mutation(&format!("{method}-{n}"));
    }
    let queued = prepared(client, method, params);
    let job = client.settle("job.read", &queued["job_id"]);
    assert_eq!(job["status"], "ready", "{method}: {job}");
    job
}

fn install(client: &mut JsonProcess, resource: &str) {
    client.call(
        "module.permission.grant",
        json!({
            "module_id": "luxforge.ai", "capability": format!("download-{resource}"),
            "scope": {"resource": resource, "version": "1", "origin": DEVELOPER_ORIGIN},
            "mutation": mutation(&format!("grant-{resource}")),
        }),
    );
    let installed = client.call(
        "module.resource.install",
        json!({"module_id": "luxforge.ai", "resource_id": resource,
               "mutation": mutation(&format!("install-{resource}"))}),
    );
    assert_eq!(
        client.settle("job.read", &installed["job_id"])["status"],
        "ready",
        "{resource}"
    );
}

fn sample(client: &mut JsonProcess, asset: &Value, (x, y): (u64, u64)) -> Value {
    prepared(
        client,
        "render.sample",
        json!({"asset_id": asset, "x": x, "y": y}),
    )["rgba"]
        .clone()
}

#[test]
#[slow]
fn an_agent_selects_an_object_recomputes_its_mask_and_removes_it() {
    let data_root = paths::temp_path("json-cli-objects");
    std::fs::create_dir_all(&data_root).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    let catalog = data_root.join("catalog.sqlite");
    let args = [
        "--catalog",
        catalog.to_str().unwrap(),
        "--data-root",
        data_root.to_str().unwrap(),
        "--secret-store",
        "memory",
        "--permission-authority",
        "--developer",
    ];
    let mut client = JsonProcess::start(BINARY, &args, "objects");
    let schema = client.call("schema.list", json!({}));
    let fill = &schema["methods"]["task.fill-generative"];
    assert!(fill.is_object(), "{fill}");
    let asset =
        client.open(&paths::jpeg().canonicalize().unwrap(), "cli-test")["asset"]["id"].clone();
    client.call("flags.set", json!({"flag": "ai.analysis", "value": true}));
    for resource in [
        "fake-segmenter-encoder",
        "fake-segmenter-decoder",
        "fake-generator",
    ] {
        install(&mut client, resource);
    }
    task(&mut client, "task.analyse", json!({"asset_id": asset}));

    // Select the object under a point: the selection names the prompt it was picked with.
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    let selected = task(&mut client, "task.select-object", point);
    let selection = selected["result"]["result"].clone();
    assert_eq!(
        selection["prompt"],
        json!({"point": [[0.5, 0.5]]}),
        "{selection}"
    );
    let artifact = selection["artifact"].clone();

    // Keep it as a model mask; the listing reports the prompt.
    let mut create =
        json!({"asset_id": asset, "mutation": edit_mutation(&mut client, &asset, "mask")});
    for field in SELECTION_FIELDS {
        create[field] = selection[field].clone();
    }
    let created = client.call("mask.create-model", create);
    let listing = client.call("mask.list", json!({"asset_id": asset}));
    let component = listing["masks"][0]["components"][0].clone();
    assert_eq!(
        component["model"]["prompt"], selection["prompt"],
        "{component}"
    );

    // Recompute: the stored prompt selects the identical artifact, which mask.set-model keeps.
    let mut again = json!({"asset_id": asset});
    for (key, value) in component["model"]["prompt"].as_object().unwrap() {
        again[key] = value.clone();
    }
    let recomputed = task(&mut client, "task.select-object", again)["result"]["result"].clone();
    assert_eq!(recomputed["artifact"], artifact, "{recomputed}");
    let mut set = json!({
        "asset_id": asset,
        "mutation": edit_mutation(&mut client, &asset, "recompute"),
        "mask": created["mask"],
        "component": component["id"],
    });
    for field in SELECTION_FIELDS {
        set[field] = recomputed[field].clone();
    }
    client.call("mask.set-model", set);
    let listing = client.call("mask.list", json!({"asset_id": asset}));
    assert_eq!(
        listing["masks"][0]["components"][0]["payload"]["artifact"],
        artifact
    );

    // Remove it: the candidate names the object and its prompt.
    let filled = task(
        &mut client,
        "task.fill-generative",
        json!({"asset_id": asset, "object-artifacts": [artifact], "model": "fake-generator",
               "seed": 5, "mutation": mutation("remove")}),
    );
    let candidate = &filled["candidates"][0];
    assert_eq!(
        candidate["provenance"]["objects"],
        json!([{"artifact": artifact, "prompt": selection["prompt"]}]),
        "{candidate}"
    );
    let rect: Vec<u64> = filled["result"]["result"]["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    let centre = (rect[0] + rect[2] / 2, rect[1] + rect[3] / 2);
    let outside = if rect[0] > 2 { (1, 1) } else { (479, 319) };
    let before = (
        sample(&mut client, &asset, outside),
        sample(&mut client, &asset, centre),
    );
    let accept = edit_mutation(&mut client, &asset, "accept");
    let accepted = client.call(
        "edit.accept-correction-patch",
        json!({"asset_id": asset, "job_id": filled["job_id"], "candidate": 0, "mutation": accept}),
    );
    assert_eq!(accepted["outcome"], "applied", "{accepted}");
    assert_eq!(sample(&mut client, &asset, outside), before.0);
    assert_ne!(sample(&mut client, &asset, centre), before.1);
    client.finish();
    let _ = std::fs::remove_dir_all(data_root);
}
