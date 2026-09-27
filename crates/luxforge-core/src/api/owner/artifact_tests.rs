//! Derived artifacts through the catalog owner and the JSON methods: preparation as a source job,
//! pinned preview jobs, corrupt bytes, the collection queued automatically when a catalog opens
//! and the explicit `artifact.collect` method.
use super::*;
use crate::{
    Mutation,
    api::ApiFailure,
    artifacts::{
        ArtifactId, object_path,
        testing::{APPLY_TINT, TINT_MODULE, TintModule},
    },
};
use sha2::Digest;
use std::{
    fs::{self, File},
    time::{Duration, Instant, SystemTime},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "luxforge-owner-artifacts-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

fn call(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiResponse {
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
    let response = call(owner, client, method, params);
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("a result")
}

fn failure(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiFailure {
    call(owner, client, method, params)
        .error
        .unwrap_or_else(|| panic!("{method} was expected to fail"))
}

/// Wait for a source job to leave `queued` and `running`. Nothing in the owner polls; this is the
/// test standing in for a client, bounded by a deadline.
fn settled(owner: &OwnerHandle, client: ClientId, job_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = ok(owner, client, "job.status", json!({"job_id": job_id}));
        match status["status"].as_str() {
            Some("queued" | "running") => {
                assert!(Instant::now() < deadline, "the job never settled: {status}");
                thread::sleep(Duration::from_millis(1));
            }
            _ => return status,
        }
    }
}

/// Retry a request through every preparation job it asks for, as a client does.
fn prepared(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Value {
    for _ in 0..4 {
        let response = call(owner, client, method, params.clone());
        match response.error {
            None => return response.result.expect("a result"),
            Some(error) if error.code == "preparation-required" => {
                let job = error.job_id.expect("a preparation names its job");
                assert_eq!(settled(owner, client, &job)["status"], "ready");
            }
            Some(error) => panic!("{method}: {error:?}"),
        }
    }
    panic!("{method} kept asking for preparation");
}

/// A catalog whose current entry tints the fixture with a published artifact, built through a
/// direct service and closed, so the owner that opens it next has nothing prepared. Returns the
/// asset, the entry, the artifact and the pixel the entry shows at (3, 4).
fn tinted_catalog(catalog: &Path, gains: [f32; 3]) -> (AssetId, EntryId, ArtifactId, [u8; 4]) {
    let mut service = EditorService::open_with(catalog, TintModule::registry()).unwrap();
    let asset = service.import(&source()).unwrap().asset.id;
    let (record, bytes) = service
        .artifact_writer()
        .unwrap()
        .write(&TintModule::bytes(gains), TintModule::meta(), TINT_MODULE)
        .unwrap();
    let artifact = record.id.clone();
    service.register_artifact(record, bytes, true).unwrap();
    let entry = service
        .apply_action(
            &asset,
            Mutation {
                expected_revision: 0,
                request_id: "tint".into(),
                actor: "test".into(),
            },
            APPLY_TINT,
            json!({"artifact": artifact}),
        )
        .unwrap()
        .current_entry_id;
    let pixel = service.sample_entry(&asset, &entry, 3, 4).unwrap().rgba;
    (asset, entry, artifact, pixel)
}

fn events(owner: &OwnerHandle, client: ClientId) -> Vec<String> {
    ok(owner, client, "events.since", json!({"after": 0}))["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["method"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn an_unprepared_artifact_is_prepared_by_a_source_job_and_the_retry_succeeds() {
    let directory = directory("prepare");
    let catalog = directory.join("catalog.sqlite");
    let (asset, _, artifact, expected) = tinted_catalog(&catalog, [0.35, 1.0, 1.0]);
    let (owner, join) = OwnerHandle::start_with(&catalog, TintModule::registry()).unwrap();
    let client = owner.register();
    let sample = json!({"asset_id": asset, "x": 3, "y": 4});
    // Neither the source nor the artifact is prepared: one source job readies both.
    let refused = failure(&owner, client, "render.sample", sample.clone());
    assert_eq!(refused.code, "preparation-required");
    let job = refused.job_id.expect("a job to wait for");
    assert_eq!(settled(&owner, client, &job)["status"], "ready");
    assert_eq!(
        ok(&owner, client, "render.sample", sample.clone())["rgba"],
        json!(expected)
    );
    // A preview job holds what it binds and renders the same bytes.
    let preview = owner
        .preview_job(PreviewRequest::new(client, asset.clone()))
        .unwrap();
    assert_eq!(preview.recipe.artifacts.iter().next().unwrap().id, artifact);
    let frame = preview
        .source
        .render(
            &preview.registry,
            preview.entry.snapshot.id.clone(),
            &preview.recipe,
        )
        .unwrap();
    assert_eq!(frame.pixel(3, 4), Some(expected));
    drop(preview);
    // An analysis of the tinted stack is evaluated with the same bytes.
    let requested = ok(
        &owner,
        client,
        "analysis.request",
        json!({"asset_id": asset, "target": {"kind": "current"}}),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let read = loop {
        let read = ok(
            &owner,
            client,
            "analysis.read",
            json!({"job_id": requested["job_id"]}),
        );
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            break read;
        }
        assert!(Instant::now() < deadline, "the analysis never settled");
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(read["status"], "ready", "{read}");
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_missing_artifact_fails_sampling_and_analysis_while_history_reads() {
    let directory = directory("missing");
    let catalog = directory.join("catalog.sqlite");
    let source_bytes = fs::read(source()).unwrap();
    let (asset, entry, artifact, _) = tinted_catalog(&catalog, [0.35, 0.6, 0.6]);
    let stored = {
        let service = EditorService::open_with(&catalog, TintModule::registry()).unwrap();
        serde_json::to_value(service.entry(&asset, &entry).unwrap()).unwrap()
    };
    fs::remove_file(object_path(&directory.join("catalog.artifacts"), &artifact)).unwrap();
    let (owner, join) = OwnerHandle::start_with(&catalog, TintModule::registry()).unwrap();
    let client = owner.register();
    // The file is checked before anything is prepared, so the refusal is immediate.
    for (method, params) in [
        ("render.sample", json!({"asset_id": asset, "x": 0, "y": 0})),
        (
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        ),
        ("source.prepare", json!({"asset_id": asset})),
    ] {
        let refused = failure(&owner, client, method, params);
        assert_eq!(refused.code, "source-unavailable", "{method}");
        assert_eq!(refused.message, format!("artifact {artifact} is missing"));
    }
    assert_eq!(
        ok(
            &owner,
            client,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": entry})
        ),
        stored,
        "the stored entry reads exactly as it was written"
    );
    assert_eq!(
        ok(
            &owner,
            client,
            "recipe.describe",
            json!({"asset_id": asset})
        )["layers"][0]["artifacts"],
        json!([artifact])
    );
    assert_eq!(
        ok(
            &owner,
            client,
            "artifact.inspect",
            json!({"artifact_id": artifact})
        )["file"],
        "missing"
    );
    assert_eq!(fs::read(source()).unwrap(), source_bytes);
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_corrupt_artifact_fails_its_preparation_job_and_nothing_is_rewritten() {
    let directory = directory("corrupt");
    let catalog = directory.join("catalog.sqlite");
    let source_bytes = fs::read(source()).unwrap();
    let (asset, entry, artifact, _) = tinted_catalog(&catalog, [0.35, 0.8, 1.0]);
    let object = object_path(&directory.join("catalog.artifacts"), &artifact);
    let damaged = TintModule::bytes([7.0, 7.0, 7.0]);
    fs::write(&object, &damaged).unwrap();
    let (owner, join) = OwnerHandle::start_with(&catalog, TintModule::registry()).unwrap();
    let client = owner.register();
    let inspect = json!({"asset_id": asset, "entry_id": entry});
    let stored = ok(&owner, client, "history.inspect", inspect.clone());
    for (method, params) in [
        ("render.sample", json!({"asset_id": asset, "x": 0, "y": 0})),
        (
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        ),
    ] {
        let refused = failure(&owner, client, method, params);
        assert_eq!(refused.code, "preparation-required", "{method}");
        let failed = settled(&owner, client, &refused.job_id.unwrap());
        assert_eq!(failed["status"], "failed", "{method}: {failed}");
        assert_eq!(failed["error"]["code"], "source-unavailable");
        assert_eq!(
            failed["error"]["message"],
            format!("artifact {artifact} is corrupt")
        );
    }
    assert_eq!(ok(&owner, client, "history.inspect", inspect), stored);
    assert_eq!(
        ok(
            &owner,
            client,
            "recipe.describe",
            json!({"asset_id": asset})
        )["layers"][0]["artifacts"],
        json!([artifact])
    );
    assert_eq!(fs::read(&object).unwrap(), damaged, "nothing repairs it");
    assert_eq!(fs::read(source()).unwrap(), source_bytes);
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

/// A catalog with a row and object no entry references, an object with no row at all and a stale
/// staged file, all recorded by a session that wrote them and closed before this one opens. What
/// still references `kept` is the tinted asset's current entry.
fn orphaned_catalog(
    catalog: &Path,
    root: &Path,
) -> (
    AssetId,
    ArtifactId,
    ArtifactId,
    PathBuf,
    ArtifactId,
    [u8; 4],
) {
    let (asset, _, kept, expected) = tinted_catalog(catalog, [0.35, 0.8, 0.8]);
    let unused = {
        let mut service = EditorService::open_with(catalog, TintModule::registry()).unwrap();
        let (record, bytes) = service
            .artifact_writer()
            .unwrap()
            .write(
                &TintModule::bytes([0.35, 0.8, 0.35]),
                TintModule::meta(),
                TINT_MODULE,
            )
            .unwrap();
        let id = record.id.clone();
        service.register_artifact(record, bytes, true).unwrap();
        id
    };
    let orphan_bytes = TintModule::bytes([0.35, 0.35, 0.8]);
    let orphan =
        ArtifactId::for_hash(&format!("{:x}", sha2::Sha256::digest(&orphan_bytes))).unwrap();
    fs::write(object_path(root, &orphan), &orphan_bytes).unwrap();
    let stale = root.join("tmp").join("stale");
    fs::write(&stale, b"stale").unwrap();
    File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
        .unwrap();
    (asset, unused, orphan, stale, kept, expected)
}

/// Wait for a path to stop existing, as a client would wait for the source worker to finish a
/// collection it has no job id for. Nothing polls this in production; the test stands in.
fn removed_eventually(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} was never removed",
            path.display()
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn opening_a_catalog_collects_what_a_closed_session_left_and_keeps_what_is_referenced() {
    let directory = directory("open-collect");
    let catalog = directory.join("catalog.sqlite");
    let root = directory.join("catalog.artifacts");
    let (asset, unused, orphan, stale, kept, expected) = orphaned_catalog(&catalog, &root);

    // No client calls `artifact.collect` anywhere in this test.
    let (owner, join) = OwnerHandle::start_with(&catalog, TintModule::registry()).unwrap();
    let client = owner.register();

    // The row query and deletion already ran on the thread that opened the catalog, before the
    // owner answered its first call: the orphaned row is already gone, the referenced one is not.
    assert_eq!(
        ok(&owner, client, "artifact.status", json!({}))["candidates"],
        json!(0)
    );
    assert_eq!(
        failure(
            &owner,
            client,
            "artifact.inspect",
            json!({"artifact_id": unused})
        )
        .message,
        format!("unknown artifact {unused}")
    );
    assert_eq!(
        ok(
            &owner,
            client,
            "artifact.inspect",
            json!({"artifact_id": kept})
        )["references"],
        json!(1)
    );

    // The source worker removes the files once it runs; wait for it the way a client without a
    // job id would.
    removed_eventually(&object_path(&root, &unused));
    removed_eventually(&object_path(&root, &orphan));
    removed_eventually(&stale);
    assert!(
        object_path(&root, &kept).exists(),
        "a referenced artifact's file survives"
    );

    // The tinted stack still samples the same bytes.
    assert_eq!(
        prepared(
            &owner,
            client,
            "render.sample",
            json!({"asset_id": asset, "x": 3, "y": 4})
        )["rgba"],
        json!(expected)
    );

    // Nothing announced a mutation: no client asked for this collection.
    assert_eq!(events(&owner, client), Vec::<String>::new());
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn artifact_collect_through_the_api_answers_once_the_open_has_already_swept_everything() {
    let directory = directory("collect");
    let catalog = directory.join("catalog.sqlite");
    let root = directory.join("catalog.artifacts");
    let (asset, _, kept, expected) = tinted_catalog(&catalog, [0.35, 0.8, 0.8]);
    let (owner, join) = OwnerHandle::start_with(&catalog, TintModule::registry()).unwrap();
    let client = owner.register();
    // Nothing is left for the explicit method to find: the automatic collection at open already
    // swept anything a prior session could have left.
    assert_eq!(
        ok(&owner, client, "artifact.status", json!({}))["candidates"],
        json!(0)
    );
    assert_eq!(
        failure(
            &owner,
            client,
            "artifact.collect",
            json!({"everything": true, "mutation": {"request_id": "everything", "actor": "test"}})
        )
        .code,
        "validation"
    );
    let queued = ok(
        &owner,
        client,
        "artifact.collect",
        json!({"mutation": {"request_id": "collect", "actor": "test"}}),
    );
    assert_eq!(queued["status"], "queued");
    let done = settled(&owner, client, queued["job_id"].as_str().unwrap());
    assert_eq!(done["status"], "ready", "{done}");
    assert_eq!(
        done["result"],
        json!({"rows": 0, "objects": 0, "temporary": 0})
    );
    assert!(object_path(&root, &kept).exists());
    assert_eq!(
        prepared(
            &owner,
            client,
            "render.sample",
            json!({"asset_id": asset, "x": 3, "y": 4})
        )["rgba"],
        json!(expected)
    );
    assert_eq!(events(&owner, client), ["artifact.collect"]);
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}
