use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp_catalog() -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightwell-json-cli-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn subprocess_client_edits_queries_and_exits_cleanly_on_eof() {
    let catalog = temp_catalog();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg")
        .canonicalize()
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightwell-json"))
        .args(["--catalog", catalog.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(
        input,
        "{}",
        json!({"id":"import","method":"catalog.import","params":{"path":fixture}})
    )
    .unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let imported: Value = serde_json::from_str(&line).unwrap();
    assert!(imported.get("error").is_none());
    let job_id = imported["result"]["job_id"].as_str().unwrap();
    let asset = loop {
        writeln!(
            input,
            "{}",
            json!({"id":"status","method":"job.status","params":{"job_id":job_id}})
        )
        .unwrap();
        input.flush().unwrap();
        line.clear();
        output.read_line(&mut line).unwrap();
        let status: Value = serde_json::from_str(&line).unwrap();
        assert!(status.get("error").is_none(), "{status}");
        match status["result"]["state"].as_str() {
            Some("ready") => break status["result"]["asset"]["asset"]["id"].clone(),
            Some("queued" | "preparing") => std::thread::sleep(std::time::Duration::from_millis(1)),
            other => panic!("unexpected source job {other:?}: {status}"),
        }
    };
    writeln!(
        input,
        "{}",
        json!({"id":"pixel","method":"edit.set-pixel","params":{"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"subprocess-pixel","actor":"subprocess-test"},"x":0,"y":0,"rgb":[12,34,56]}})
    )
    .unwrap();
    writeln!(
        input,
        "{}",
        json!({"id":"sample","method":"render.sample","params":{"asset_id":asset,"x":0,"y":0}})
    )
    .unwrap();
    input.flush().unwrap();
    for (id, expected) in [("pixel", None), ("sample", Some(json!([12, 34, 56, 255])))] {
        line.clear();
        output.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        assert!(response.get("error").is_none());
        if let Some(pixel) = expected {
            assert_eq!(response["result"]["rgba"], pixel);
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    std::fs::remove_file(catalog).unwrap();
}
