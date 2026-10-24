//! A module's setups over `luxforge-json`'s pipe, as an agent sets one up: discover
//! `module.setup.install`, read every setup missing, ask for Object and be refused once with every
//! grant it needs, grant them, install, retry the same request and be answered the same jobs,
//! settle the installs, and find Object, Mask Object and Remove installed, with nothing left for
//! Mask Object to install.
use luxforge_testbase::paths;
use luxforge_testkit::JsonProcess;
use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_luxforge-json");
const MODULE: &str = "luxforge.ai";

fn mutation(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "cli-test"})
}

fn install(setup: &str, request_id: &str) -> Value {
    json!({"module_id": MODULE, "setup": setup, "mutation": mutation(request_id)})
}

/// Each setup's state in `module.status`, by id.
fn states(client: &mut JsonProcess) -> Vec<(String, String)> {
    let status = client.call("module.status", json!({"module_id": MODULE}));
    status["setups"]
        .as_array()
        .unwrap_or_else(|| panic!("{status}"))
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap().to_owned(),
                row["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn an_agent_sets_up_object_under_one_consent_and_its_parts_set_up_the_others() {
    let data_root = paths::temp_path("json-cli-setups");
    std::fs::create_dir_all(&data_root).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    let catalog = data_root.join("catalog.sqlite");
    let mut client = JsonProcess::start(
        BINARY,
        &[
            "--catalog",
            catalog.to_str().unwrap(),
            "--data-root",
            data_root.to_str().unwrap(),
            "--secret-store",
            "memory",
            "--permission-authority",
            "--developer",
        ],
        "setups",
    );
    let schema = client.call("schema.list", json!({}));
    assert!(
        schema["methods"]["module.setup.install"].is_object(),
        "{schema}"
    );
    let missing = |setup: &str| (setup.to_owned(), "missing".to_owned());
    assert_eq!(
        states(&mut client),
        [
            missing("remove"),
            missing("object"),
            missing("mask-object"),
            missing("replace")
        ]
    );

    // One refusal names every grant the setup needs, in its parts' order, and queues nothing.
    let refused = client.error("module.setup.install", install("object", "ask"));
    assert_eq!(refused["code"], "consent-required", "{refused}");
    let consent = &refused["data"]["consent"];
    assert_eq!(
        (&consent["module_id"], &consent["setup"], &consent["denied"]),
        (&json!(MODULE), &json!("object"), &json!(false)),
        "{consent}"
    );
    let grants = consent["grants"].as_array().unwrap();
    let resources: Vec<&Value> = grants
        .iter()
        .map(|grant| &grant["scope"]["resource"])
        .collect();
    assert_eq!(
        resources,
        [
            "fake-segmenter-encoder",
            "fake-segmenter-decoder",
            "fake-generator"
        ]
    );
    assert_eq!(consent["disclosure"]["title"], "Object", "{consent}");
    for (index, grant) in grants.iter().enumerate() {
        client.call(
            "module.permission.grant",
            json!({
                "module_id": MODULE, "capability": grant["capability"], "scope": grant["scope"],
                "mutation": mutation(&format!("grant-{index}")),
            }),
        );
    }

    // Allowed, it installs each missing resource in order; the same request answers the same jobs.
    let installed = client.call("module.setup.install", install("object", "install"));
    let jobs = installed["jobs"].as_array().unwrap();
    let order: Vec<&Value> = jobs.iter().map(|job| &job["resource_id"]).collect();
    assert_eq!(order, resources, "{installed}");
    let retried = client.call("module.setup.install", install("object", "install"));
    assert_eq!(retried["jobs"], installed["jobs"], "{retried}");
    for job in jobs {
        assert_eq!(client.settle("job.read", &job["job_id"])["status"], "ready");
    }
    let installed = |setup: &str| (setup.to_owned(), "installed".to_owned());
    assert_eq!(
        states(&mut client),
        [
            installed("remove"),
            installed("object"),
            installed("mask-object"),
            missing("replace")
        ]
    );
    let nothing = client.call("module.setup.install", install("mask-object", "mask"));
    assert_eq!(nothing["jobs"], json!([]), "{nothing}");
    client.finish();
    let _ = std::fs::remove_dir_all(data_root);
}
