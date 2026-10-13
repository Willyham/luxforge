//! Model selections through the editor service: a mask component that binds a published alpha
//! raster is admitted, rendered, sampled and listed with the raster bound; history references it
//! like a layer's artifact, so retention keeps it across entries, branches, versions and restore; a
//! reopened catalog renders it from the stored raster without any model; a missing, corrupt or
//! wrong artifact fails by name; and a stack past the bind budget is refused at `mask.add-model`.
use super::test_support::{fixture, mutation, temp};
use super::*;
use crate::{
    HistoryEntry, Mask, Recipe, Snapshot, SnapshotId,
    artifacts::{ArtifactId, ArtifactMeta, collect_files, object_path},
    mask::ALPHA_KIND,
    mask::commands::{MaskTarget, ModelStale},
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{fs, sync::atomic::AtomicBool};

const MODEL: &str = "select-test";

/// Publish `codes` as a `width × height` selection raster, the way a selection task does: a writer,
/// then the owner records what it wrote.
fn publish(service: &mut EditorService, width: u32, height: u32, codes: &[u8]) -> ArtifactId {
    let (record, prepared) = service
        .artifact_writer()
        .unwrap()
        .write(
            codes,
            ArtifactMeta {
                kind: ALPHA_KIND.into(),
                width: Some(width),
                height: Some(height),
                colour: None,
            },
            "luxforge.ai",
        )
        .unwrap();
    let id = record.id.clone();
    service.register_artifact(record, prepared, true).unwrap();
    id
}

/// A 32 × 24 raster whose left half is fully selected and whose right half is not.
fn half() -> Vec<u8> {
    (0..24 * 32)
        .map(|k| if k % 32 < 16 { 255 } else { 0 })
        .collect()
}

/// A model component's parameters over `artifact`, recorded against `input`.
fn selection(artifact: &ArtifactId, input: &(String, String)) -> Value {
    json!({
        "artifact": artifact,
        "class": "sky",
        "model": MODEL,
        "model_version": "1",
        "input_fingerprint": input.0,
        "prefix_hash": input.1,
    })
}

fn run(
    service: &mut EditorService,
    asset: &AssetId,
    method: &str,
    target: MaskTarget,
    parameters: Value,
    request: &str,
) -> Result<ActionResult, Error> {
    let revision = service.state(asset).unwrap().revision;
    service.run_action(
        asset,
        mutation(revision, request),
        method,
        target.request(parameters),
    )
}

/// The current selection input of `asset`'s current stack.
fn input_now(service: &EditorService, asset: &AssetId) -> (String, String) {
    let state = service.state(asset).unwrap();
    service
        .selection_input(&state.asset, &state.current_entry.snapshot.recipe)
        .unwrap()
}

/// A new mask holding one model selection of `artifact`, and a masked Basic exposure through it:
/// the mask's identity and the entry the exposure committed.
fn select_and_brighten(
    service: &mut EditorService,
    asset: &AssetId,
    artifact: &ArtifactId,
) -> (MaskId, EntryId) {
    let input = input_now(service, asset);
    let created = run(
        service,
        asset,
        "mask.create-model",
        MaskTarget::default(),
        selection(artifact, &input),
        "select",
    )
    .unwrap();
    let mask = created.mask.expect("a created mask");
    let revision = service.state(asset).unwrap().revision;
    let entry = service
        .apply_action(
            asset,
            mutation(revision, "brighten"),
            "set-basic",
            json!({"exposure": 1.5, "mask": mask}),
        )
        .unwrap()
        .current_entry_id;
    (mask, entry)
}

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

#[test]
fn a_model_selection_commits_renders_samples_lists_and_survives_reopen() {
    let catalog = temp("model-mask.sqlite");
    let (asset, entry, artifact, rendered);
    {
        let mut service = EditorService::open(&catalog).unwrap();
        asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap();
        artifact = publish(&mut service, 32, 24, &half());
        let mask;
        (mask, entry) = select_and_brighten(&mut service, &asset, &artifact);
        let stored = &service.state(&asset).unwrap().current_entry.snapshot.recipe;
        let component = &stored.masks[0].components[0];
        assert_eq!(component.kind, "model");
        assert_eq!(component.payload["threshold"], json!(50.0));
        assert_eq!(component.payload["feather"], json!(100.0));
        assert_eq!(stored.layers.last().unwrap().mask.as_ref(), Some(&mask));
        // The raster decides the pixels: the selected half is brightened, the other half is the
        // original byte for byte.
        rendered = service.render_current(&asset).unwrap();
        let width = rendered.width;
        let (mut brighter, mut unchanged) = (0usize, 0usize);
        for (index, (edited, plain)) in rendered
            .rgba
            .chunks_exact(4)
            .zip(original.rgba.chunks_exact(4))
            .enumerate()
        {
            let x = index as u32 % width;
            if x < width * 2 / 5 {
                brighter += usize::from(edited[..3] > plain[..3]);
            } else if x > width * 3 / 5 {
                assert_eq!(edited, plain, "outside the selection at x {x}");
                unchanged += 1;
            }
        }
        assert!(
            brighter > 1000 && unchanged > 1000,
            "{brighter} {unchanged}"
        );
        // A sampled byte is the rendered byte, inside, outside and across the edge.
        let row = rendered.height / 2;
        for x in [
            0,
            width / 4,
            width / 2 - 2,
            width / 2,
            width / 2 + 1,
            width - 1,
        ] {
            assert_eq!(
                Some(service.sample_entry(&asset, &entry, x, row).unwrap().rgba),
                rendered.pixel(x, row),
                "({x}, {row})"
            );
        }
        // An export is planned with the raster bound and draws the same frame.
        let plan = service.export_plan(&asset, None).unwrap();
        assert_eq!(
            plan.evaluation
                .recipe()
                .artifacts
                .iter()
                .map(|bound| bound.id.clone())
                .collect::<Vec<_>>(),
            std::slice::from_ref(&artifact)
        );
        let exported = plan
            .evaluation
            .source()
            .render(
                plan.evaluation.registry(),
                plan.evaluation.entry().snapshot.id.clone(),
                plan.evaluation.recipe(),
            )
            .unwrap();
        assert_eq!(exported.rgba, rendered.rgba);
        // Both entries since the selection reference its raster.
        assert_eq!(references(&service, &entry), [artifact.as_str()]);
        // The listing says what computed it, when, and that it is current.
        let listing = service.mask_listing(&asset, &entry).unwrap();
        let model = listing.masks[0].components[0]
            .model
            .clone()
            .expect("a model row");
        assert_eq!(
            (
                model.class.as_str(),
                model.model.as_str(),
                model.model_version.as_str()
            ),
            ("sky", MODEL, "1")
        );
        assert_eq!(model.artifact, artifact);
        assert!(model.computed_ms.is_some_and(|ms| ms > 0));
        assert_eq!(model.stale, Some(Vec::new()));
    }
    // A new service has nothing prepared and no model: the render names the raster, preparation
    // reads and verifies it, and the selection renders exactly as before.
    let mut service = EditorService::open(&catalog).unwrap();
    let refused = service.render_current(&asset).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::PreparationRequired);
    assert_eq!(
        refused.needs().unwrap().artifacts,
        std::slice::from_ref(&artifact)
    );
    service.prepare(refused.needs().unwrap()).unwrap();
    assert_eq!(service.render_current(&asset).unwrap().rgba, rendered.rgba);
    assert_eq!(service.state(&asset).unwrap().current_entry.id, entry);
}

#[test]
fn a_selection_is_stale_for_a_changed_repair_prefix_and_a_changed_model_version() {
    let catalog = temp("model-mask-stale.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&fixture()).unwrap().asset.id;
    let artifact = publish(&mut service, 32, 24, &half());
    let input = input_now(&service, &asset);
    let mut recorded = selection(&artifact, &input);
    recorded["input_fingerprint"] = json!("c".repeat(64));
    recorded["prefix_hash"] = json!("d".repeat(64));
    run(
        &mut service,
        &asset,
        "mask.create-model",
        MaskTarget::default(),
        recorded,
        "stale",
    )
    .unwrap();
    let entry = service.state(&asset).unwrap().current_entry.id;
    let listing = service.mask_listing(&asset, &entry).unwrap();
    assert_eq!(
        listing.masks[0].components[0].model.as_ref().unwrap().stale,
        Some(vec![ModelStale::Source, ModelStale::RepairPrefix])
    );
    // A colour edit after the repair stage changes neither identity.
    let revision = service.state(&asset).unwrap().revision;
    service
        .apply_action(
            &asset,
            mutation(revision, "colour"),
            "set-basic",
            json!({"exposure": 0.4}),
        )
        .unwrap();
    assert_eq!(input_now(&service, &asset), input);
    // A declared model resource of another version is a third reason.
    assert!(
        service
            .declared_model_version("generative-klein-4b")
            .is_some()
    );
    let mut payload = selection(&artifact, &input);
    payload["model"] = json!("generative-klein-4b");
    let component = service
        .state(&asset)
        .unwrap()
        .current_entry
        .snapshot
        .recipe
        .masks[0]
        .components[0]
        .id
        .clone();
    let mask = service
        .state(&asset)
        .unwrap()
        .current_entry
        .snapshot
        .recipe
        .masks[0]
        .id
        .clone();
    run(
        &mut service,
        &asset,
        "mask.set-model",
        MaskTarget {
            mask: Some(mask),
            component: Some(component),
            ..MaskTarget::default()
        },
        payload,
        "recompute",
    )
    .unwrap();
    let entry = service.state(&asset).unwrap().current_entry.id;
    let listing = service.mask_listing(&asset, &entry).unwrap();
    assert_eq!(
        listing.masks[0].components[0].model.as_ref().unwrap().stale,
        Some(vec![ModelStale::ModelVersion])
    );
}

#[test]
fn retention_keeps_a_selection_across_entries_branches_versions_and_restore() {
    let catalog = temp("model-mask-retention.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&fixture()).unwrap().asset.id;
    let artifact = publish(&mut service, 32, 24, &half());
    let (mask, selected) = select_and_brighten(&mut service, &asset, &artifact);
    service
        .create_version(&asset, "selected", Some(&selected), "test")
        .unwrap();
    let selected_frame = service.render_entry(&asset, &selected).unwrap();
    // Delete the mask, then branch: no current entry references the raster any more.
    let deleted = run(
        &mut service,
        &asset,
        "mask.delete",
        MaskTarget {
            mask: Some(mask),
            ..MaskTarget::default()
        },
        Value::Null,
        "delete",
    )
    .unwrap()
    .mutation
    .current_entry_id;
    assert!(references(&service, &deleted).is_empty());
    let revision = service.state(&asset).unwrap().revision;
    service.undo(&asset, mutation(revision, "undo")).unwrap();
    let revision = service.state(&asset).unwrap().revision;
    let branch = service
        .apply_exposure(&asset, mutation(revision, "branch"), 0.2)
        .unwrap()
        .current_entry_id;
    assert_eq!(references(&service, &branch), [artifact.as_str()]);
    let revision = service.state(&asset).unwrap().revision;
    service
        .apply_exposure(&asset, mutation(revision, "plain"), 0.0)
        .unwrap();
    // A collection keeps a raster any entry references, and the version and a restore render it.
    let collection = service.plan_collection().unwrap();
    collect_files(&collection, &AtomicBool::new(false)).unwrap();
    assert!(object_path(&service.artifact_root, &artifact).exists());
    assert_eq!(
        service.render_entry(&asset, &selected).unwrap().rgba,
        selected_frame.rgba
    );
    let revision = service.state(&asset).unwrap().revision;
    let restored = service
        .restore(&asset, mutation(revision, "restore"), &selected)
        .unwrap()
        .current_entry_id;
    assert_eq!(references(&service, &restored), [artifact.as_str()]);
    assert_eq!(
        service.render_current(&asset).unwrap().rgba,
        selected_frame.rgba
    );
}

#[test]
fn a_missing_corrupt_unknown_or_foreign_raster_fails_by_name() {
    let catalog = temp("model-mask-missing.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&fixture()).unwrap().asset.id;
    let input = input_now(&service, &asset);
    // An artifact the catalog never recorded, and one that is not an alpha raster, are refused at
    // the commit, naming the artifact and the component; nothing is written.
    let unknown = ArtifactId::parse(format!("artifact-{}", "e".repeat(64))).unwrap();
    let refused = run(
        &mut service,
        &asset,
        "mask.create-model",
        MaskTarget::default(),
        selection(&unknown, &input),
        "unknown",
    )
    .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Validation);
    assert_eq!(refused.detail, format!("unknown artifact {unknown}"));
    let (record, prepared) = service
        .artifact_writer()
        .unwrap()
        .write(
            &[1u8; 12],
            ArtifactMeta {
                kind: "tint".into(),
                width: None,
                height: None,
                colour: None,
            },
            "luxforge.ai",
        )
        .unwrap();
    let foreign = record.id.clone();
    service.register_artifact(record, prepared, true).unwrap();
    let refused = run(
        &mut service,
        &asset,
        "mask.create-model",
        MaskTarget::default(),
        selection(&foreign, &input),
        "foreign",
    )
    .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Validation);
    assert_eq!(
        refused.detail,
        format!(
            "artifact {foreign} of component Model 1 is a tint artifact, not a mask-alpha raster"
        )
    );
    assert_eq!(service.state(&asset).unwrap().revision, 0);

    // A committed selection whose raster is then damaged or removed fails every render by name,
    // and its history and listing still read.
    let artifact = publish(&mut service, 32, 24, &half());
    let (_, entry) = select_and_brighten(&mut service, &asset, &artifact);
    let object = object_path(&service.artifact_root, &artifact);
    fs::write(&object, [7u8; 5]).unwrap();
    service.clear_prepared_artifacts();
    let error = service.render_current(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable);
    assert_eq!(error.detail, format!("artifact {artifact} is corrupt"));
    fs::remove_file(&object).unwrap();
    let error = service.render_current(&asset).unwrap_err();
    assert_eq!(error.detail, format!("artifact {artifact} is missing"));
    assert!(service.mask_listing(&asset, &entry).is_ok());
    assert!(service.history(&asset, None, 10).is_ok());
}

/// Seventeen full-size selections are more than the 256 MiB the prepared cache binds at once, so the
/// seventeenth `mask.add-model` is refused by name before anything is written. The rows and files
/// are planted: sixteen recorded 16 MiB rasters as sparse files of their recorded length, which is
/// everything the budget check reads.
#[test]
fn a_stack_past_the_bind_budget_is_refused_at_add_model_by_name() {
    let catalog = temp("model-mask-budget.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let state = service.import(&fixture()).unwrap();
    let asset = state.asset.id.clone();
    // A real publish creates the root and its manifest.
    publish(&mut service, 32, 24, &half());
    let input = input_now(&service, &asset);
    let side = crate::mask::ALPHA_SIDE_MAX;
    let bytes = i64::from(side) * i64::from(side);
    let planted: Vec<ArtifactId> = (0..17u32)
        .map(|index| {
            let id = ArtifactId::for_hash(&format!("{index:064x}")).unwrap();
            service
                .connection
                .execute(
                    "INSERT INTO artifacts VALUES (?1,?2,?3,?4,?5,?5,NULL,'luxforge.ai',1)",
                    params![id.as_str(), id.sha256(), bytes, ALPHA_KIND, i64::from(side)],
                )
                .unwrap();
            let object = object_path(&service.artifact_root, &id);
            fs::create_dir_all(object.parent().unwrap()).unwrap();
            fs::File::create(&object)
                .unwrap()
                .set_len(bytes as u64)
                .unwrap();
            id
        })
        .collect();
    let mut mask = Mask::new("Mask 1");
    for id in &planted[..16] {
        let name = mask.next_component_name("model");
        mask.components.push(crate::Component::new(
            name,
            crate::ComponentMode::Add,
            "model",
            json!({"artifact": id, "class": "sky", "model": MODEL, "model_version": "1",
                   "input_fingerprint": input.0, "prefix_hash": input.1,
                   "threshold": 50.0, "feather": 100.0}),
        ));
    }
    let mask_id = mask.id.clone();
    let current = state.current_entry.clone();
    let entry = HistoryEntry {
        id: EntryId::new(),
        sequence: current.sequence + 1,
        label: "Planted selections".into(),
        undo_parent: Some(current.id.clone()),
        base_revision: state.revision,
        result_revision: state.revision + 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset.clone(),
            recipe: Recipe {
                format: crate::RECIPE_FORMAT,
                masks: vec![mask],
                ..Recipe::default()
            },
        },
        ..current
    };
    drop(service);
    let connection = rusqlite::Connection::open(&catalog).unwrap();
    connection
        .execute(
            "INSERT INTO entries (id,asset_id,sequence,action_id,label,actor,timestamp_ms,
                                  undo_parent_id,restore_target_id,entry_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                entry.id.as_str(),
                entry.asset_id.as_str(),
                entry.sequence as i64,
                entry.action_id,
                entry.label,
                entry.actor,
                entry.timestamp_ms,
                entry.undo_parent.as_ref().map(EntryId::as_str),
                Option::<&str>::None,
                serde_json::to_string(&entry).unwrap(),
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
            params![
                entry.id.as_str(),
                entry.result_revision as i64,
                asset.as_str()
            ],
        )
        .unwrap();
    drop(connection);
    let mut service = EditorService::open(&catalog).unwrap();
    let mut parameters = selection(&planted[16], &input);
    parameters["mode"] = json!("add");
    let refused = run(
        &mut service,
        &asset,
        "mask.add-model",
        MaskTarget {
            mask: Some(mask_id),
            ..MaskTarget::default()
        },
        parameters,
        "one-too-many",
    )
    .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::ResourceLimit);
    assert!(
        refused
            .detail
            .starts_with("the stack binds 17 artifacts of 285212672 bytes"),
        "{refused}"
    );
    assert_eq!(
        refused.data.as_deref().unwrap()["limit"],
        json!("artifacts")
    );
    assert_eq!(service.state(&asset).unwrap().current_entry.id, entry.id);
}
