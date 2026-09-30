//! Opening a file through the catalog owner as every client does, for the core's tests: the file is
//! picked and developed at once (`pick.develop` of its path, `into: []`, `confirm_removable`, as a
//! person who opens a file chose it), the photograph the Develop answers is prepared
//! (`source.prepare`) and adopted as the client's current photograph (`job.adopt`).
use crate::api::{ApiFailure, ApiRequest, ApiResponse, ClientId, OwnerHandle};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

/// Write the JPEG at `fixture` to `to`, making its folder, with a comment segment naming `to`
/// after its start marker, and answer its canonical path: the fixture's pixels in bytes no other
/// copy has, all copies of one fixture the same length. A Develop links a file whose bytes a
/// photograph already has, so a test that wants several photographs of one fixture writes them
/// this way.
pub(in crate::api) fn distinct_copy(fixture: &Path, to: &Path) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(fixture).expect("the fixture");
    assert!(bytes.starts_with(&[0xff, 0xd8]), "a JPEG fixture");
    let tag = format!("{:x}", Sha256::digest(to.as_os_str().as_encoded_bytes()));
    let mut copy = bytes[..2].to_vec();
    copy.extend([0xff, 0xfe]);
    copy.extend(u16::try_from(tag.len() + 2).unwrap().to_be_bytes());
    copy.extend(tag.as_bytes());
    copy.extend(&bytes[2..]);
    std::fs::create_dir_all(to.parent().expect("a folder")).unwrap();
    std::fs::write(to, copy).unwrap();
    to.canonicalize().unwrap()
}

/// A request identity no other test call repeats.
fn request_id(what: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("open-{what}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn send(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiResponse {
    owner
        .call(
            client,
            ApiRequest {
                id: request_id(method),
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

/// Read `job` until it leaves `queued` and `running`, blocking on the owner in between.
pub(in crate::api) fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Value {
    let id = crate::JobId::parse(job.as_str().expect("a job id")).expect("a job id");
    loop {
        let read = ok(owner, client, "job.read", json!({"job_id": job}));
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            return read;
        }
        owner
            .wait_source(client, Some(&id))
            .expect("the owner answers a wait");
    }
}

/// Develop the file at `path` at once, answering the Develop's `pick.develop` request identity
/// and the photograph it made, linked or relinked, or the refusal of the file.
pub(in crate::api) fn develop(
    owner: &OwnerHandle,
    client: ClientId,
    path: &Path,
) -> Result<(String, Value), ApiFailure> {
    let request_id = request_id("develop");
    let response = send(
        owner,
        client,
        "pick.develop",
        json!({
            "targets": {"kind": "paths", "paths": [path]},
            "into": [],
            "confirm_removable": true,
            "mutation": {"request_id": request_id, "actor": "test"},
        }),
    );
    let started = match (response.error, response.result) {
        (Some(error), _) => return Err(error),
        (None, result) => result.expect("a result"),
    };
    let job = settle(owner, client, &started["job_id"]);
    if job["status"] != "ready" {
        return Err(serde_json::from_value(job["error"].clone()).expect("a job's error"));
    }
    let report = &job["result"];
    if let Some(failed) = report["failed"]
        .as_array()
        .and_then(|failed| failed.first())
    {
        return Err(ApiFailure {
            code: failed["code"].as_str().unwrap_or_default().to_owned(),
            message: failed["message"].as_str().unwrap_or_default().to_owned(),
            data: None,
            job_id: None,
        });
    }
    Ok((request_id, report["developed"][0]["asset_id"].clone()))
}

/// Prepare `asset` and adopt it as the client's current photograph, answering `job.adopt`'s
/// answer: `{asset, session}`.
pub(in crate::api) fn adopt(owner: &OwnerHandle, client: ClientId, asset: &Value) -> Value {
    let started = ok(owner, client, "source.prepare", json!({"asset_id": asset}));
    let prepared = settle(owner, client, &started["job_id"]);
    assert_eq!(prepared["status"], "ready", "{prepared}");
    ok(
        owner,
        client,
        "job.adopt",
        json!({"job_id": started["job_id"]}),
    )
}

/// Develop the file at `path` and prepare its photograph, as a client does before it works on it,
/// answering the photograph's identity.
pub(in crate::api) fn import(owner: &OwnerHandle, client: ClientId, path: &Path) -> Value {
    let (_, asset) = develop(owner, client, path)
        .unwrap_or_else(|error| panic!("{} was not developed: {error:?}", path.display()));
    let started = ok(owner, client, "source.prepare", json!({"asset_id": asset}));
    let prepared = settle(owner, client, &started["job_id"]);
    assert_eq!(prepared["status"], "ready", "{prepared}");
    asset
}

/// Open the file at `path` as a client does — develop, prepare, adopt — answering the adopted
/// photograph's state (`{asset, revision, current_entry, redo}`).
pub(in crate::api) fn open(owner: &OwnerHandle, client: ClientId, path: &Path) -> Value {
    let (_, asset) = develop(owner, client, path)
        .unwrap_or_else(|error| panic!("{} was not developed: {error:?}", path.display()));
    adopt(owner, client, &asset)["asset"].clone()
}
