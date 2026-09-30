//! Developing picks through `luxforge-json`, as an independent process: a JSON client picks three
//! files in a scratch folder of copied fixtures, plans them, develops them as a job it reads with
//! `job.read`, undoes the Develop's last batch (which sends its photographs back and picks their
//! files again) and sends the last photograph back, with every file unchanged throughout.
use luxforge_testbase::paths;
use luxforge_testkit::JsonProcess;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::SystemTime};

const ACTOR: &str = "develop-json-cli";

/// What a file is compared by before and after: its bytes, length, modification time and path.
fn untouched(path: &PathBuf) -> (Vec<u8>, u64, SystemTime, PathBuf) {
    let metadata = fs::metadata(path).unwrap();
    (
        fs::read(path).unwrap(),
        metadata.len(),
        metadata.modified().unwrap(),
        path.canonicalize().unwrap(),
    )
}

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": ACTOR})
}

fn photographs(client: &mut JsonProcess) -> u64 {
    client.call("catalog.info", json!({}))["counts"]["photographs"]
        .as_u64()
        .unwrap()
}

fn picks(client: &mut JsonProcess) -> Vec<Value> {
    client.call("pick.list", json!({}))["picks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pick| pick["path"].clone())
        .collect()
}

#[test]
fn develop_picks_plans_develops_undoes_and_sends_back_over_the_pipe() {
    let dir = paths::temp_dir("develop-json-cli").canonicalize().unwrap();
    let folder = dir.join("Konstanz");
    fs::create_dir_all(&folder).unwrap();
    let files: Vec<PathBuf> = ["orientation-1.jpg", "orientation-2.jpg", "portrait.jpg"]
        .iter()
        .map(|name| {
            let path = folder.join(name);
            fs::copy(paths::fixture(&format!("s0/{name}")), &path).unwrap();
            path.canonicalize().unwrap()
        })
        .collect();
    let before: Vec<_> = files.iter().map(untouched).collect();
    let catalog = dir.join("catalog.sqlite");
    let mut client = JsonProcess::start(
        env!("CARGO_BIN_EXE_luxforge-json"),
        &["--catalog", catalog.to_str().unwrap()],
        "develop",
    );
    let targets = json!({"kind": "paths", "paths": files});

    // Pick the three files.
    let picked = client.call(
        "pick.set",
        json!({"targets": targets, "picked": true, "mutation": envelope("pick")}),
    );
    assert_eq!(picked["outcome"], "applied", "{picked}");
    assert_eq!(picks(&mut client).len(), 3);

    // Plan them: one event (the folder's undated files, since nothing indexed them), a new folder
    // named after it, nothing removable or offline.
    let plan = client.call("pick.plan", json!({"targets": targets}));
    assert_eq!(plan["count"], 3, "{plan}");
    assert_eq!(plan["offline"], 0, "{plan}");
    let events = plan["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{plan}");
    assert_eq!(events[0]["name"], "Undated · Konstanz");
    assert_eq!(
        events[0]["folder"],
        json!({"kind": "new", "name": "Undated · Konstanz"})
    );

    // Develop them into the plan's folder, as a job read with job.read.
    let started = client.call(
        "pick.develop",
        json!({"targets": targets, "into": [], "mutation": envelope("develop")}),
    );
    assert_eq!(started["deduplicated"], false);
    let job = client.settle("job.read", &started["job_id"]);
    assert_eq!(job["status"], "ready", "{job}");
    assert_eq!(job["kind"], "develop-picks");
    assert_eq!(
        job["progress"],
        json!({"fraction": 1.0, "message": "3 of 3"}),
        "its progress, as the activity board showed it: {job}"
    );
    let report = &job["result"];
    let developed = report["developed"].as_array().unwrap();
    assert_eq!(developed.len(), 3, "{report}");
    assert!(
        developed
            .iter()
            .all(|pick| pick["outcome"] == "created" && pick["asset_id"].is_string())
    );
    assert_eq!(report["failed"], json!([]));
    let changes = report["changes"].as_array().unwrap().clone();
    assert_eq!(
        changes.len(),
        2,
        "the first file alone, then the rest: {report}"
    );
    assert!(
        picks(&mut client).is_empty(),
        "the committed picks are cleared"
    );
    assert_eq!(photographs(&mut client), 3);
    let folders = client.call("folder.list", json!({}))["folders"].clone();
    assert_eq!(folders[0]["name"], "Undated · Konstanz");
    assert_eq!(folders[0]["count"], 3);
    // Retried, it answers the first job and changes nothing.
    let retried = client.call(
        "pick.develop",
        json!({"targets": targets, "into": [], "mutation": envelope("develop")}),
    );
    assert_eq!(retried["job_id"], started["job_id"]);
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(photographs(&mut client), 3);

    // Each batch is a library change of the request.
    let journal = client.call("library.journal", json!({}))["changes"].clone();
    let developing: Vec<&Value> = journal
        .as_array()
        .unwrap()
        .iter()
        .filter(|change| change["method"] == "pick.develop")
        .collect();
    assert_eq!(developing.len(), 2);
    assert_eq!(developing[0]["label"], "Developed orientation-1.jpg");
    assert_eq!(developing[1]["label"], "Developed 2");

    // Undo sends the last batch's photographs back and picks their files again.
    let undone = client.call("library.undo", json!({"mutation": envelope("undo")}));
    assert_eq!(undone["outcome"], "applied", "{undone}");
    assert_eq!(photographs(&mut client), 1);
    assert_eq!(picks(&mut client), [json!(files[1]), json!(files[2])]);

    // Send the first photograph back: its record goes and its file is picked again.
    let first = developed
        .iter()
        .find(|pick| pick["path"] == json!(files[0]))
        .unwrap()["asset_id"]
        .clone();
    let sent = client.call(
        "asset.send-back",
        json!({"targets": {"kind": "assets", "asset_ids": [first]}, "mutation": envelope("send")}),
    );
    assert_eq!(sent["outcome"], "applied", "{sent}");
    let refused = client.error("asset.state", json!({"asset_id": first}));
    assert_eq!(refused["code"], "validation");
    assert_eq!(photographs(&mut client), 0);
    assert_eq!(picks(&mut client).len(), 3);

    client.finish();
    let after: Vec<_> = files.iter().map(untouched).collect();
    assert!(after == before, "no file changed");
    fs::remove_dir_all(dir).unwrap();
}
