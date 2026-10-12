//! The Generative tier over `luxforge-json`'s pipe, as an agent drives it: discover the task, turn
//! the flag on, install the developer generator under its grant, Generate, list and read the
//! candidate, Regenerate, accept one, undo and redo, then reopen the catalog in a process without
//! `--developer`, which serves no developer model and offers no installed generator, and render the
//! accepted patch.
use luxforge_testbase::paths;
use luxforge_testkit::JsonProcess;
use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_luxforge-json");
const TASK: &str = "task.fill-generative";

fn mutation(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "cli-test"})
}

fn edit_mutation(client: &mut JsonProcess, asset: &Value, request_id: &str) -> Value {
    let revision = client.call("asset.state", json!({"asset_id": asset}))["revision"].clone();
    json!({"expected_revision": revision, "request_id": request_id, "actor": "cli-test"})
}

/// `render.sample` of one pixel, preparing the photograph first when the process asks.
fn sample(client: &mut JsonProcess, asset: &Value, x: u32, y: u32) -> Value {
    let params = json!({"asset_id": asset, "x": x, "y": y});
    for _ in 0..4 {
        let response = client.call_raw("render.sample", params.clone());
        match response["error"]["code"].as_str() {
            None => return response["result"]["rgba"].clone(),
            Some("preparation-required") => {
                let job = response["error"]["job_id"].clone();
                assert_eq!(client.settle("job.read", &job)["status"], "ready");
            }
            Some(_) => panic!("{response}"),
        }
    }
    panic!("the sample kept asking for preparation");
}

#[test]
fn slow_an_agent_generates_regenerates_accepts_and_the_patch_renders_without_the_model() {
    let data_root = paths::temp_path("json-cli-generative");
    std::fs::create_dir_all(&data_root).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    let catalog = data_root.join("catalog.sqlite");
    let args = |developer: bool| {
        let mut args = vec![
            "--catalog".to_owned(),
            catalog.to_str().unwrap().to_owned(),
            "--data-root".to_owned(),
            data_root.to_str().unwrap().to_owned(),
            "--secret-store".to_owned(),
            "memory".to_owned(),
            "--permission-authority".to_owned(),
        ];
        if developer {
            args.push("--developer".to_owned());
        }
        args
    };
    let start = |developer: bool| {
        let args = args(developer);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        JsonProcess::start(BINARY, &args, "generative")
    };

    let mut client = start(true);
    let schema = client.call("schema.list", json!({}));
    for method in [TASK, "job.read-candidate", "edit.accept-correction-patch"] {
        assert!(schema["methods"][method].is_object(), "{method}");
    }
    let asset =
        client.open(&paths::jpeg().canonicalize().unwrap(), "cli-test")["asset"]["id"].clone();
    client.call("flags.set", json!({"flag": "ai.generative", "value": true}));
    client.call(
        "module.permission.grant",
        json!({
            "module_id": "luxforge.ai", "capability": "download-fake-generator",
            "scope": {"resource": "fake-generator", "version": "1",
                      "origin": "https://developer.luxforge.invalid"},
            "mutation": mutation("grant-generator"),
        }),
    );
    let installed = client.call(
        "module.resource.install",
        json!({"module_id": "luxforge.ai", "resource_id": "fake-generator",
               "mutation": mutation("install-generator")}),
    );
    assert_eq!(
        client.settle("job.read", &installed["job_id"])["status"],
        "ready"
    );
    let status = client.call("module.status", json!({"module_id": "luxforge.ai"}));
    assert_eq!(status["fill_tiers"][0]["offered"], true, "{status}");

    let fill = |seed: u64, request: &str| {
        json!({
            "asset_id": asset,
            "selection-points": [[0.4, 0.5], [0.6, 0.5]],
            "selection-radius": 0.08,
            "model": "fake-generator",
            "seed": seed,
            "mutation": mutation(request),
        })
    };
    let (outside, centre) = ((20, 20), (240, 160));
    let before = (
        sample(&mut client, &asset, outside.0, outside.1),
        sample(&mut client, &asset, centre.0, centre.1),
    );

    // Generate: one candidate (owner, 2026-10-11).
    let queued = client.call(TASK, fill(7, "generate"));
    let first = client.settle("job.read", &queued["job_id"]);
    assert_eq!(first["status"], "ready", "{first}");
    assert_eq!(first["result"]["result"]["seeds"], json!([7]));
    let candidates = first["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(
        candidates
            .iter()
            .all(|row| row["label"] == "Generative fill" && row["state"] == "live")
    );
    let preview = client.call(
        "job.read-candidate",
        json!({"job_id": queued["job_id"], "candidate": 0, "part": "preview"}),
    );
    assert!(preview["length"].as_u64().unwrap() > 0, "{preview}");

    // Regenerate from the next seed: a new job; the first one's candidates stay.
    let next = first["result"]["result"]["next_seed"].as_u64().unwrap();
    let again = client.call(TASK, fill(next, "regenerate"));
    let second = client.settle("job.read", &again["job_id"]);
    assert_eq!(second["result"]["result"]["seeds"], json!([8]));
    let first = client.call("job.read", json!({"job_id": queued["job_id"]}));
    assert!(
        first["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "live")
    );

    // Accept, undo and redo.
    let accept = edit_mutation(&mut client, &asset, "accept");
    let accepted = client.call(
        "edit.accept-correction-patch",
        json!({"asset_id": asset, "job_id": again["job_id"], "candidate": 0, "mutation": accept}),
    );
    assert_eq!(accepted["outcome"], "applied", "{accepted}");
    let after = sample(&mut client, &asset, centre.0, centre.1);
    assert_ne!(after, before.1);
    assert_eq!(sample(&mut client, &asset, outside.0, outside.1), before.0);
    let history = client.call("history.list", json!({"asset_id": asset, "limit": 2}));
    assert_eq!(history["entries"][0]["label"], "Add Generative fill 1");
    for method in ["history.undo", "history.redo"] {
        let mutation = edit_mutation(&mut client, &asset, method);
        client.call(method, json!({"asset_id": asset, "mutation": mutation}));
    }
    assert_eq!(sample(&mut client, &asset, centre.0, centre.1), after);
    client.finish();

    // A process without --developer serves no developer model, so no generator is installed, and
    // it renders the patch.
    let mut reopened = start(false);
    let status = reopened.call("module.status", json!({"module_id": "luxforge.ai"}));
    assert_eq!(status["fill_tiers"][0]["offered"], false, "{status}");
    assert!(
        status["resources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["id"] != "fake-generator"),
        "{status}"
    );
    assert_eq!(sample(&mut reopened, &asset, centre.0, centre.1), after);
    reopened.finish();
    let _ = std::fs::remove_dir_all(data_root);
}
