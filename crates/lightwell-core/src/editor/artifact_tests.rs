//! Derived artifacts through the editor service: publish, commit and reopen, history reachability,
//! collection, missing and corrupt bytes, crash points, moving the catalog and binding.
use super::*;
use crate::artifacts::{
    ArtifactId, collect_files, object_path, prepared,
    testing::{APPLY_PLAIN, APPLY_TINT, TINT_EFFECT, TINT_MODULE, TintModule},
};
use std::{
    fs,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::SystemTime,
};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A directory of its own for one test, so the catalog file and its artifact root sit side by side.
fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "lightwell-artifacts-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "test".into(),
    }
}

fn open(catalog: &Path) -> EditorService {
    EditorService::open_with(catalog, TintModule::registry()).unwrap()
}

/// Publish three gains the way a module task will: a writer on any thread, then the owner records
/// what it wrote.
fn publish(service: &mut EditorService, gains: [f32; 3]) -> ArtifactId {
    let writer = service.artifact_writer().unwrap();
    let (record, prepared) = writer
        .write(&TintModule::bytes(gains), TintModule::meta(), TINT_MODULE)
        .unwrap();
    let id = record.id.clone();
    service.register_artifact(record, prepared, true).unwrap();
    id
}

fn tint(
    service: &mut EditorService,
    asset: &AssetId,
    revision: u64,
    request: &str,
    artifact: &ArtifactId,
) -> Result<MutationResult, Error> {
    service.apply_action(
        asset,
        mutation(revision, request),
        APPLY_TINT,
        json!({"artifact": artifact}),
    )
}

fn count(service: &EditorService, table: &str) -> i64 {
    service
        .connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// The artifacts one entry's references record.
fn references(service: &EditorService, entry: &EntryId) -> Vec<String> {
    let mut statement = service
        .connection
        .prepare("SELECT artifact_id FROM artifact_refs WHERE entry_id=?1 ORDER BY artifact_id")
        .unwrap();
    statement
        .query_map([entry.as_str()], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Every stored entry exactly as the catalog holds it.
fn stored_entries(service: &EditorService) -> Vec<String> {
    let mut statement = service
        .connection
        .prepare("SELECT entry_json FROM entries ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Run a collection the way the owner does: rows now, files on the worker.
fn collect(service: &mut EditorService) -> crate::artifacts::Collected {
    let collection = service.plan_collection().unwrap();
    collect_files(&collection, &AtomicBool::new(false)).unwrap()
}

#[test]
fn a_published_artifact_commits_renders_and_survives_reopen() {
    let directory = directory("journey");
    let catalog = directory.join("catalog.sqlite");
    let source_bytes = fs::read(source()).unwrap();
    let (asset, entry, artifact, rendered);
    {
        let mut service = open(&catalog);
        assert_eq!(service.artifact_root, directory.join("catalog.artifacts"));
        asset = service.import(&source()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap();
        artifact = publish(&mut service, [0.25, 1.0, 1.0]);
        let committed = tint(&mut service, &asset, 0, "tint", &artifact).unwrap();
        assert_eq!(committed.outcome, MutationOutcome::Applied);
        entry = committed.current_entry_id;
        let layers = service
            .entry(&asset, &entry)
            .unwrap()
            .snapshot
            .recipe
            .layers;
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].effect_id, TINT_EFFECT);
        assert_eq!(layers[0].artifacts, std::slice::from_ref(&artifact));
        // The bytes decide the pixels: red is scaled down in linear light, green and blue are not.
        rendered = service.render_current(&asset).unwrap();
        let mut lowered = 0;
        for (tinted, plain) in rendered
            .rgba
            .chunks_exact(4)
            .zip(original.rgba.chunks_exact(4))
        {
            assert!(tinted[0] <= plain[0]);
            assert_eq!(tinted[1..], plain[1..]);
            lowered += usize::from(tinted[0] < plain[0]);
        }
        assert!(lowered > 1000, "the gains reach the frame");
        let sampled = service.sample_entry(&asset, &entry, 5, 7).unwrap();
        assert_eq!(Some(sampled.rgba), rendered.pixel(5, 7));
        assert_eq!(references(&service, &entry), [artifact.as_str()]);
        assert_eq!(
            service.describe_entry(&asset, None).unwrap().layers[0].artifacts,
            std::slice::from_ref(&artifact),
            "recipe.describe lists the layer's artifacts"
        );
    }
    // A new service has nothing prepared, so the render reads and verifies the object again.
    let service = open(&catalog);
    assert_eq!(service.prepared_artifacts.borrow().len(), 0);
    let state = service.state(&asset).unwrap();
    assert_eq!(state.current_entry.id, entry);
    assert_eq!(
        state.current_entry.snapshot.recipe.layers[0].artifacts,
        std::slice::from_ref(&artifact)
    );
    assert_eq!(count(&service, "artifacts"), 1);
    assert_eq!(references(&service, &entry), [artifact.as_str()]);
    assert_eq!(service.render_current(&asset).unwrap().rgba, rendered.rgba);
    assert_eq!(service.prepared_artifacts.borrow().len(), 1);
    assert_eq!(fs::read(source()).unwrap(), source_bytes);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn branches_versions_previews_and_restores_keep_their_artifacts() {
    let directory = directory("branches");
    let mut service = open(&directory.join("catalog.sqlite"));
    let state = service.import(&source()).unwrap();
    let (asset, original) = (state.asset.id, state.current_entry.id);
    let a = publish(&mut service, [0.5, 0.9, 1.0]);
    let first = tint(&mut service, &asset, 0, "a", &a)
        .unwrap()
        .current_entry_id;
    service
        .create_version(&asset, "tinted", Some(&first), "test")
        .unwrap();
    let b = publish(&mut service, [1.0, 0.9, 0.5]);
    let second = tint(&mut service, &asset, 1, "b", &b)
        .unwrap()
        .current_entry_id;
    let first_frame = service.render_entry(&asset, &first).unwrap();
    let second_frame = service.render_entry(&asset, &second).unwrap();
    assert_ne!(
        first_frame.rgba, second_frame.rgba,
        "each entry binds its own"
    );
    // Undo and edit again: `second` is left on an abandoned branch.
    service.undo(&asset, mutation(2, "undo")).unwrap();
    let branch = service
        .apply_pixel(&asset, mutation(3, "branch"), 0, 0, [1, 2, 3])
        .unwrap()
        .current_entry_id;
    let lineage = service.lineage(&asset, None, 10).unwrap();
    assert!(lineage.steps.iter().all(|step| step.entry_id != second));
    // A historical preview of the abandoned entry still binds its artifact and renders it.
    let job = service
        .preview_job(&asset, Some(&second), None, None, None)
        .unwrap();
    assert_eq!(
        job.artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&b)
    );
    let previewed = job
        .source
        .render(&job.registry, job.entry.snapshot.id.clone(), &job.recipe)
        .unwrap();
    assert_eq!(previewed.rgba, second_frame.rgba);
    // Restoring it brings the same stack back as a new entry.
    let restored = service
        .restore(&asset, mutation(4, "restore"), &second)
        .unwrap()
        .current_entry_id;
    assert_eq!(
        service.render_current(&asset).unwrap().rgba,
        second_frame.rgba
    );
    // The version still names the first entry, and it still renders.
    assert_eq!(service.versions(&asset).unwrap()[0].entry_id, first);
    assert_eq!(
        service.render_entry(&asset, &first).unwrap().rgba,
        first_frame.rgba
    );
    let branch_frame = service.render_entry(&asset, &branch).unwrap();
    assert_eq!(branch_frame.pixel(0, 0), Some([1, 2, 3, 255]));
    assert_eq!(branch_frame.pixel(1, 0), first_frame.pixel(1, 0));
    // Every entry that holds the layer records what it references; Original records nothing.
    assert!(references(&service, &original).is_empty());
    assert_eq!(references(&service, &first), [a.as_str()]);
    assert_eq!(references(&service, &second), [b.as_str()]);
    assert_eq!(references(&service, &branch), [a.as_str()]);
    assert_eq!(references(&service, &restored), [b.as_str()]);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn collection_removes_only_unreferenced_rows_orphans_and_stale_staging() {
    let directory = directory("collect");
    let catalog = directory.join("catalog.sqlite");
    let root = directory.join("catalog.artifacts");
    let mut service = open(&catalog);
    let asset = service.import(&source()).unwrap().asset.id;
    // `saved` is referenced only by an entry a version names, `abandoned` only by an entry on an
    // abandoned branch and `current` by the current entry. `unused` is recorded and never
    // committed.
    let saved = publish(&mut service, [0.7, 1.0, 1.0]);
    let named = tint(&mut service, &asset, 0, "saved", &saved)
        .unwrap()
        .current_entry_id;
    service
        .create_version(&asset, "saved", Some(&named), "test")
        .unwrap();
    let abandoned = publish(&mut service, [0.7, 0.7, 1.0]);
    tint(&mut service, &asset, 1, "abandoned", &abandoned).unwrap();
    service.undo(&asset, mutation(2, "undo")).unwrap();
    let current = publish(&mut service, [0.7, 0.7, 0.7]);
    tint(&mut service, &asset, 3, "current", &current).unwrap();
    let unused = publish(&mut service, [0.7, 1.0, 0.7]);
    // An object an interrupted session left without a row, and staged files old and new.
    let orphan_bytes = TintModule::bytes([0.7, 0.5, 0.5]);
    let orphan = ArtifactId::for_hash(&format!("{:x}", Sha256::digest(&orphan_bytes))).unwrap();
    fs::write(object_path(&root, &orphan), &orphan_bytes).unwrap();
    let stale = root.join("tmp").join("stale");
    let recent = root.join("tmp").join("recent");
    fs::write(&stale, b"stale").unwrap();
    fs::write(&recent, b"recent").unwrap();
    File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
        .unwrap();
    let status = service.artifact_status().unwrap();
    assert_eq!(
        (
            &status["state"],
            &status["referenced"],
            &status["candidates"]
        ),
        (&json!("ready"), &json!(3), &json!(0)),
        "the unused artifact is live while this service is open: {status}"
    );
    let collected = collect(&mut service);
    assert_eq!(
        (collected.rows, collected.objects, collected.temporary),
        (0, 1, 1)
    );
    assert!(!object_path(&root, &orphan).exists());
    assert!(!stale.exists());
    assert!(recent.exists(), "a staged write that may still finish");
    for kept in [&saved, &abandoned, &current, &unused] {
        assert!(object_path(&root, kept).exists());
    }
    drop(service);
    // After a reopen nothing is live, so the unused artifact is a candidate and goes, row and file.
    let mut service = open(&catalog);
    assert_eq!(service.artifact_status().unwrap()["candidates"], json!(1));
    let collected = collect(&mut service);
    assert_eq!(
        (collected.rows, collected.objects, collected.temporary),
        (1, 1, 0)
    );
    assert!(!object_path(&root, &unused).exists());
    assert_eq!(count(&service, "artifacts"), 3);
    assert_eq!(
        service.inspect_artifact(&unused).unwrap_err().detail,
        format!("unknown artifact {unused}")
    );
    // Everything any entry references is untouched and still renders.
    for kept in [&saved, &abandoned, &current] {
        assert!(object_path(&root, kept).exists());
        let inspected = service.inspect_artifact(kept).unwrap();
        assert_eq!(inspected["file"], json!("present"));
        assert_eq!(inspected["references"], json!(1));
        assert_eq!(inspected["live"], json!(false));
    }
    for entry in service.history(&asset, None, 10).unwrap().entries {
        service.render_entry(&asset, &entry.id).unwrap();
    }
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_missing_artifact_fails_evaluation_explicitly_and_rewrites_nothing() {
    let directory = directory("missing");
    let root = directory.join("catalog.artifacts");
    let source_bytes = fs::read(source()).unwrap();
    let mut service = open(&directory.join("catalog.sqlite"));
    let asset = service.import(&source()).unwrap().asset.id;
    let artifact = publish(&mut service, [0.6, 1.0, 1.0]);
    let entry = tint(&mut service, &asset, 0, "tint", &artifact)
        .unwrap()
        .current_entry_id;
    let stored = stored_entries(&service);
    fs::remove_file(object_path(&root, &artifact)).unwrap();
    let missing = format!("artifact {artifact} is missing");
    let refusals = [
        service.render_current(&asset).unwrap_err(),
        service.sample_entry(&asset, &entry, 0, 0).unwrap_err(),
        service
            .analysis_plan(&asset, AnalysisSelection::Current)
            .unwrap_err(),
        service
            .preview_job(&asset, None, None, None, None)
            .unwrap_err(),
        service.locate_entry(&asset, &entry, 0, 0).unwrap_err(),
        // A new edit plans against the current stack, so it is refused too.
        service
            .apply_pixel(&asset, mutation(1, "pixel"), 0, 0, [1, 2, 3])
            .unwrap_err(),
    ];
    for error in refusals {
        assert_eq!(error.kind, ErrorKind::SourceUnavailable, "{error}");
        assert_eq!(error.detail, missing);
    }
    // History stays readable and navigable.
    assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 2);
    assert_eq!(
        service
            .entry(&asset, &entry)
            .unwrap()
            .snapshot
            .recipe
            .layers[0]
            .artifacts,
        std::slice::from_ref(&artifact)
    );
    assert_eq!(
        service.describe_entry(&asset, None).unwrap().layers[0].artifacts,
        std::slice::from_ref(&artifact)
    );
    assert_eq!(
        service.inspect_artifact(&artifact).unwrap()["file"],
        json!("missing")
    );
    service.undo(&asset, mutation(1, "undo")).unwrap();
    service.render_current(&asset).unwrap();
    service.redo(&asset, mutation(2, "redo")).unwrap();
    // A restore of a stack whose artifact is gone is refused and writes nothing.
    service.undo(&asset, mutation(3, "undo-again")).unwrap();
    let refused = service
        .restore(&asset, mutation(4, "restore"), &entry)
        .unwrap_err();
    assert_eq!(refused.detail, missing);
    assert_eq!(stored_entries(&service), stored, "no entry was rewritten");
    // Without its directory the stack names the directory and the way out.
    service.redo(&asset, mutation(4, "redo-again")).unwrap();
    fs::remove_dir_all(&root).unwrap();
    let gone = service.render_current(&asset).unwrap_err();
    assert_eq!(gone.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        gone.detail,
        format!(
            "artifact directory {} is missing; move it with the catalog or select it with artifact.relocate",
            root.display()
        )
    );
    assert_eq!(
        service.artifact_status().unwrap()["state"],
        json!("missing")
    );
    // A directory whose manifest names another catalog is refused as incompatible.
    fs::create_dir_all(root.join("objects")).unwrap();
    fs::write(
        root.join("manifest.json"),
        br#"{"format":1,"catalog_id":"another-catalog"}"#,
    )
    .unwrap();
    let foreign = service.render_current(&asset).unwrap_err();
    assert_eq!(foreign.kind, ErrorKind::Incompatible);
    assert!(
        foreign
            .detail
            .ends_with("belongs to catalog another-catalog")
    );
    assert_eq!(
        service.artifact_status().unwrap()["state"],
        json!("foreign")
    );
    assert_eq!(stored_entries(&service), stored);
    assert_eq!(fs::read(source()).unwrap(), source_bytes);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_corrupt_artifact_is_refused_when_read_and_is_not_rewritten() {
    let directory = directory("corrupt");
    let root = directory.join("catalog.artifacts");
    let mut service = open(&directory.join("catalog.sqlite"));
    let asset = service.import(&source()).unwrap().asset.id;
    let artifact = publish(&mut service, [0.6, 0.6, 1.0]);
    tint(&mut service, &asset, 0, "tint", &artifact).unwrap();
    let stored = stored_entries(&service);
    // The same length with other bytes: the changed file signature is a cache miss, and the read
    // that follows finds the hash wrong.
    let object = object_path(&root, &artifact);
    let damaged = TintModule::bytes([9.0, 9.0, 9.0]);
    fs::write(&object, &damaged).unwrap();
    let error = service.render_current(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(error.detail, format!("artifact {artifact} is corrupt"));
    assert_eq!(
        fs::read(&object).unwrap(),
        damaged,
        "nothing repairs it behind the caller"
    );
    assert_eq!(stored_entries(&service), stored);
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_commit_naming_an_unknown_or_missing_artifact_or_the_wrong_effect_writes_nothing() {
    let directory = directory("refused-commit");
    let root = directory.join("catalog.artifacts");
    let mut service = open(&directory.join("catalog.sqlite"));
    let asset = service.import(&source()).unwrap().asset.id;
    let before = service.state(&asset).unwrap();
    let tables = |service: &EditorService| {
        ["entries", "requests", "artifact_refs"].map(|table| count(service, table))
    };
    let untouched = tables(&service);
    let unknown = ArtifactId::for_hash(&"0".repeat(64)).unwrap();
    let error = tint(&mut service, &asset, 0, "unknown", &unknown).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(error.detail, format!("unknown artifact {unknown}"));
    let absent = publish(&mut service, [0.8, 1.0, 1.0]);
    fs::remove_file(object_path(&root, &absent)).unwrap();
    let error = tint(&mut service, &asset, 0, "absent", &absent).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(error.detail, format!("artifact {absent} is missing"));
    // Only an effect that declares artifacts may hold one.
    let present = publish(&mut service, [0.8, 0.8, 1.0]);
    let error = service
        .apply_action(
            &asset,
            mutation(0, "plain"),
            APPLY_PLAIN,
            json!({"artifact": present}),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(
        error.detail.ends_with(
            "of effect test.tint.plain references artifacts, which its effect does not declare"
        ),
        "{error}"
    );
    let mut pixel = Layer::pixel(0, 0, [1, 2, 3]);
    pixel.artifacts.push(present.clone());
    assert_eq!(
        service.registry().validate_layer(&pixel).unwrap_err().kind,
        ErrorKind::Validation
    );
    assert_eq!(service.state(&asset).unwrap(), before);
    assert_eq!(tables(&service), untouched, "nothing was written");
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn crash_points_between_publish_and_commit_leave_only_collectable_artifacts() {
    let directory = directory("crash");
    let catalog = directory.join("catalog.sqlite");
    let root = directory.join("catalog.artifacts");
    // A crash after the file is renamed and before the owner records it: an object with no row.
    let unrecorded = {
        let mut service = open(&catalog);
        service.import(&source()).unwrap();
        let (record, _) = service
            .artifact_writer()
            .unwrap()
            .write(
                &TintModule::bytes([0.9, 0.1, 0.1]),
                TintModule::meta(),
                TINT_MODULE,
            )
            .unwrap();
        record.id
    };
    assert!(object_path(&root, &unrecorded).exists());
    // A recorded artifact whose commit fails: an injected write failure, then a stale revision.
    let mut service = open(&catalog);
    let asset = service.assets().unwrap()[0].id.clone();
    let recorded = publish(&mut service, [0.9, 0.9, 0.1]);
    service
        .connection
        .execute_batch(
            "CREATE TRIGGER injected_reference_failure BEFORE INSERT ON artifact_refs
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
    let before = service.state(&asset).unwrap();
    let failed = tint(&mut service, &asset, 0, "fails", &recorded).unwrap_err();
    assert!(failed.detail.contains("injected"), "{failed}");
    service
        .connection
        .execute_batch("DROP TRIGGER injected_reference_failure")
        .unwrap();
    let stale = tint(&mut service, &asset, 7, "stale", &recorded).unwrap_err();
    assert_eq!(stale.kind, ErrorKind::Conflict);
    assert_eq!(service.state(&asset).unwrap(), before);
    assert_eq!(count(&service, "entries"), 1, "no snapshot references it");
    assert_eq!(count(&service, "artifact_refs"), 0);
    assert_eq!(count(&service, "requests"), 0);
    drop(service);
    // After a reopen the recorded one is an unreferenced candidate and the unrecorded one an
    // orphan; a collection removes both and nothing else.
    let mut service = open(&catalog);
    assert_eq!(service.artifact_status().unwrap()["candidates"], json!(1));
    let collected = collect(&mut service);
    assert_eq!(
        (collected.rows, collected.objects, collected.temporary),
        (1, 2, 0)
    );
    assert!(!object_path(&root, &unrecorded).exists());
    assert!(!object_path(&root, &recorded).exists());
    assert_eq!(count(&service, "artifacts"), 0);
    service.render_current(&asset).unwrap();
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn moving_the_catalog_with_its_artifact_directory_keeps_rendering() {
    let parent = directory("moved");
    let before = parent.join("before");
    fs::create_dir_all(&before).unwrap();
    let (asset, rendered) = {
        let mut service = open(&before.join("catalog.sqlite"));
        let asset = service.import(&source()).unwrap().asset.id;
        let artifact = publish(&mut service, [0.4, 1.0, 0.4]);
        tint(&mut service, &asset, 0, "tint", &artifact).unwrap();
        let rendered = service.render_current(&asset).unwrap();
        (asset, rendered)
    };
    let after = parent.join("after");
    fs::rename(&before, &after).unwrap();
    let service = open(&after.join("catalog.sqlite"));
    let status = service.artifact_status().unwrap();
    assert_eq!(status["root"], json!(after.join("catalog.artifacts")));
    assert_eq!(status["state"], json!("ready"));
    assert_eq!(service.render_current(&asset).unwrap().rgba, rendered.rgba);
    drop(service);
    fs::remove_dir_all(parent).unwrap();
}

#[test]
fn copying_only_the_catalog_fails_artifact_layers_while_history_reads() {
    let original = directory("copied-from");
    let (asset, entry) = {
        let mut service = open(&original.join("catalog.sqlite"));
        let asset = service.import(&source()).unwrap().asset.id;
        let artifact = publish(&mut service, [0.4, 0.4, 1.0]);
        let entry = tint(&mut service, &asset, 0, "tint", &artifact)
            .unwrap()
            .current_entry_id;
        (asset, entry)
    };
    let copy = directory("copied-to");
    fs::copy(original.join("catalog.sqlite"), copy.join("copy.sqlite")).unwrap();
    let service = open(&copy.join("copy.sqlite"));
    let error = service.render_current(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(
        error.detail,
        format!(
            "artifact directory {} is missing; move it with the catalog or select it with artifact.relocate",
            copy.join("copy.artifacts").display()
        )
    );
    assert_eq!(
        service.artifact_status().unwrap()["state"],
        json!("missing")
    );
    assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 2);
    assert_eq!(
        service.describe_entry(&asset, Some(&entry)).unwrap().layers[0].summary,
        "Tint"
    );
    drop(service);
    fs::remove_dir_all(original).unwrap();
    fs::remove_dir_all(copy).unwrap();
}

#[test]
fn identical_bytes_published_twice_yield_one_artifact_and_one_file() {
    let directory = directory("twice");
    let root = directory.join("catalog.artifacts");
    let mut service = open(&directory.join("catalog.sqlite"));
    let first = publish(&mut service, [0.3, 0.6, 0.9]);
    let second = publish(&mut service, [0.3, 0.6, 0.9]);
    assert_eq!(first, second);
    assert_eq!(count(&service, "artifacts"), 1);
    assert_eq!(fs::read_dir(root.join("objects")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(root.join("tmp")).unwrap().count(), 0);
    let manifest: Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest,
        json!({"format": 1, "catalog_id": service.catalog_id()})
    );
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn jobs_pin_their_artifacts_and_an_unprepared_one_needs_preparation() {
    let directory = directory("pins");
    let mut service = open(&directory.join("catalog.sqlite"));
    let asset = service.import(&source()).unwrap().asset.id;
    let artifact = publish(&mut service, [0.2, 0.4, 0.6]);
    tint(&mut service, &asset, 0, "tint", &artifact).unwrap();
    let expected = service.render_current(&asset).unwrap();
    let job = service.preview_job(&asset, None, None, None, None).unwrap();
    let plan = service
        .analysis_plan(&asset, AnalysisSelection::Current)
        .unwrap();
    assert_eq!(plan.artifacts.len(), 1);
    drop(plan);
    // Evicting everything the owner kept ready leaves the job's own pin: it still compiles.
    service.clear_prepared_artifacts();
    assert!(prepared(&artifact).is_some(), "the job holds it");
    let rendered = job
        .source
        .render(&job.registry, job.entry.snapshot.id.clone(), &job.recipe)
        .unwrap();
    assert_eq!(rendered.rgba, expected.rgba);
    drop(job);
    assert!(prepared(&artifact).is_none(), "nothing else held it");
    // The catalog owner never reads on a miss: it names what a source job must prepare.
    service.disable_sync_source();
    let error = service.render_current(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::PreparationRequired);
    assert_eq!(
        error.data.as_deref(),
        Some(&json!({"artifacts": [artifact.clone()]}))
    );
    let reads = service.artifact_preparation(&asset, None, &[]).unwrap();
    assert_eq!(reads.len(), 1);
    let verified = crate::artifacts::read_verified(&reads[0], &AtomicBool::new(false)).unwrap();
    service.adopt_artifacts(vec![verified]);
    assert_eq!(service.render_current(&asset).unwrap().rgba, expected.rgba);
    assert!(
        service
            .artifact_preparation(&asset, None, &[])
            .unwrap()
            .is_empty()
    );
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_stack_that_cannot_be_held_ready_at_once_is_a_resource_limit() {
    let directory = directory("oversized");
    let service = open(&directory.join("catalog.sqlite"));
    // Two rows recorded at 200 MiB each; the check reads rows only, so no file is needed.
    let ids: Vec<ArtifactId> = ["1", "2"]
        .map(|digit| ArtifactId::for_hash(&digit.repeat(64)).unwrap())
        .into();
    for id in &ids {
        service
            .connection
            .execute(
                "INSERT INTO artifacts VALUES (?1,?2,?3,'tint',NULL,NULL,NULL,'test.tint',0)",
                params![id.as_str(), id.sha256(), 200_i64 * 1024 * 1024],
            )
            .unwrap();
    }
    let recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        layers: vec![Layer {
            id: LayerId::new(),
            effect_id: TINT_EFFECT.into(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            artifacts: ids,
            mask: None,
        }],
        masks: Vec::new(),
        strokes: Default::default(),
    };
    let error = service.require_artifacts(&recipe).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(
        error
            .detail
            .starts_with("the stack binds 2 artifacts of 419430400 bytes"),
        "{error}"
    );
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}
