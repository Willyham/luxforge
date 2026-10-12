//! Frozen patches in the repair layer through the editor service (`docs/design/corrections.md`,
//! "Frozen patches"): the composite against the rendered frame, the operation's answers, labels
//! and rows, stale reasons and what does not stale a patch, the artifacts the layer lists,
//! missing and corrupt bytes, a moved or copied catalog, a reopen without any producer, and a
//! refused accept that leaves only an unreferenced artifact. The owner's accept flow is tested in
//! `api/owner/accept_tests.rs`.
use super::{
    CORRECTIONS_EFFECT,
    input::{RepairInput, StaleReason, repair_input, source_identity},
    patch_testing::{PatchSpec, accept, add_patch, candidate, composited},
    payload::RepairPayload,
    tests::{Session, spot},
};
use crate::{
    EditorService, ErrorKind, Layer, MutationOutcome, Raster, Stage,
    artifacts::{collect_files, object_path},
    editor::mutation,
};
use luxforge_test_macros::slow;
use serde_json::{Value, json};
use std::{fs, path::Path, sync::atomic::AtomicBool};

fn pixel(raster: &Raster, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * raster.width + x) * 4) as usize;
    [raster.rgba[at], raster.rgba[at + 1], raster.rgba[at + 2]]
}

/// Every pixel outside the covered part of `spec` is exactly `before`'s; every covered one is the
/// frozen composite of `before`'s within one code. Returns how many pixels changed.
fn check_composite(before: &Raster, after: &Raster, spec: PatchSpec, opacity: u8) -> usize {
    assert_eq!((before.width, before.height), (after.width, after.height));
    let [x0, y0, width, height] = spec.rect;
    let mut changed = 0;
    for y in 0..after.height {
        for x in 0..after.width {
            let (input, output) = (pixel(before, x, y), pixel(after, x, y));
            let inside = (x0..x0 + width).contains(&x) && (y0..y0 + height).contains(&y);
            let covered = inside && spec.pixel(x - x0, y - y0).1 > 0.0 && opacity > 0;
            if !covered {
                assert_eq!(output, input, "({x}, {y}) is outside the coverage");
                continue;
            }
            let (rgb, alpha) = spec.pixel(x - x0, y - y0);
            let expected = composited(input, rgb, alpha, opacity);
            for channel in 0..3 {
                assert!(
                    (f64::from(output[channel]) - expected[channel]).abs() <= 1.0,
                    "({x}, {y}) channel {channel}: {} against {}",
                    output[channel],
                    expected[channel]
                );
            }
            changed += usize::from(output != input);
        }
    }
    changed
}

fn rows(session: &Session) -> Vec<Value> {
    session.corrections()["operations"]
        .as_array()
        .unwrap()
        .clone()
}

fn repair_layer(session: &Session) -> Layer {
    let state = session.service.state(&session.asset).unwrap();
    state
        .current_entry
        .snapshot
        .recipe
        .layers
        .into_iter()
        .find(|layer| layer.effect_id == CORRECTIONS_EFFECT)
        .unwrap()
}

fn stale(session: &Session) -> Value {
    let description = session
        .service
        .describe_entry(&session.asset, None)
        .unwrap();
    let row = description
        .layers
        .iter()
        .find(|row| row.effect == CORRECTIONS_EFFECT)
        .unwrap();
    serde_json::to_value(&row.stale).unwrap()
}

#[test]
fn an_accepted_patch_composites_its_coverage_and_keeps_every_other_pixel() {
    let mut session = Session::new("patch-composite.sqlite");
    let before = session.service.render_current(&session.asset).unwrap();
    let spec = PatchSpec {
        rect: [40, 30, 24, 16],
        seed: 5,
        opacity: Some(80),
    };
    let (result, artifact) =
        add_patch(&mut session.service, &session.asset, "accept", spec).unwrap();
    assert_eq!(result.mutation.outcome, MutationOutcome::Applied);
    assert_eq!(
        result.answer["correction"],
        json!({"operation": "correction-1", "name": "Patch 1", "index": 0})
    );
    assert_eq!(session.label(), "Add Patch 1");
    let layer = repair_layer(&session);
    assert_eq!(layer.artifacts, std::slice::from_ref(&artifact));
    let payload = RepairPayload::parse(&layer.payload).unwrap();
    assert!(payload.strokes.is_empty(), "a patch carries no stroke");
    let after = session.service.render_current(&session.asset).unwrap();
    let changed = check_composite(&before, &after, spec, 80);
    assert!(changed > 200, "the patch reaches the frame: {changed}");
    let row = &rows(&session)[0];
    assert_eq!(
        *row,
        json!({
            "operation": "correction-1", "name": "Patch 1", "kind": "patch", "index": 0,
            "label": "Patch", "artifact": artifact, "rect": [40, 30, 24, 16], "opacity": 80,
            "provenance": {"module": "luxforge.candidates", "task": "generate-proof-patches", "seed": 5},
        })
    );
    assert_eq!(stale(&session), json!([]));
    let description = session
        .service
        .describe_entry(&session.asset, None)
        .unwrap();
    assert_eq!(description.layers[0].summary, "1 correction");
    assert_eq!(
        description.layers[0].artifacts,
        std::slice::from_ref(&artifact)
    );
    // A sample of a covered pixel is the rendered one.
    let entry = session
        .service
        .state(&session.asset)
        .unwrap()
        .current_entry
        .id;
    let sampled = session
        .service
        .sample_entry(&session.asset, &entry, 41, 30)
        .unwrap();
    assert_eq!(Some(sampled.rgba), after.pixel(41, 30));
}

#[test]
fn a_patch_is_stale_only_when_its_input_changes_and_keeps_rendering() {
    let mut session = Session::new("patch-stale.sqlite");
    session.ok("add-correction", spot(0.3, 0.3, 0.6, 0.6));
    let spec = PatchSpec::at([100, 100, 20, 20]);
    add_patch(&mut session.service, &session.asset, "accept", spec).unwrap();
    assert_eq!(rows(&session)[1]["name"], "Patch 2");
    // Later operations, downstream edits and an undo/redo of them change nothing it read.
    session.ok("add-correction", spot(0.7, 0.2, 0.2, 0.7));
    session.ok("set-basic", json!({"exposure": 0.4}));
    assert!(rows(&session)[1].get("stale").is_none());
    assert_eq!(stale(&session), json!([]));
    // A fully covered pixel at opacity 100 is the patch's own colour, whatever lies beneath it.
    let full = (0..20u32)
        .flat_map(|row| (0..20u32).map(move |column| (column, row)))
        .find(|&(column, row)| spec.pixel(column, row).1 == 1.0)
        .unwrap();
    let rendered = session.service.render_current(&session.asset).unwrap();
    let held = pixel(&rendered, 100 + full.0, 100 + full.1);
    // An operation below it changes: the patch is stale and still drawn.
    session.ok(
        "update-correction",
        json!({"operation": "correction-1", "opacity": 50}),
    );
    assert_eq!(
        rows(&session)[1]["stale"],
        json!({"reasons": ["earlier-correction"]})
    );
    assert_eq!(
        stale(&session),
        json!([{"operation": "correction-2", "name": "Patch 2", "reasons": ["earlier-correction"]}])
    );
    let still = session.service.render_current(&session.asset).unwrap();
    assert_eq!(pixel(&still, 100 + full.0, 100 + full.1), held);
    // Undoing the change makes it current again; reordering a later operation below it stales it.
    session
        .service
        .undo(&session.asset, mutation(session.revision(), "undo-opacity"))
        .unwrap();
    assert!(rows(&session)[1].get("stale").is_none());
    session.ok(
        "reorder-correction",
        json!({"operation": "correction-3", "index": 0}),
    );
    let listed = rows(&session);
    assert_eq!(listed[2]["name"], "Patch 2");
    assert_eq!(
        listed[2]["stale"],
        json!({"reasons": ["earlier-correction"]})
    );
    // Deleting it leaves the stack without stale reports.
    session.ok("delete-correction", json!({"operation": "correction-2"}));
    assert_eq!(stale(&session), json!([]));
    assert!(repair_layer(&session).artifacts.is_empty());
}

/// The identity's components, against a stack the photograph cannot hold as a JPEG: a source
/// development before the repair layer stales a patch for `development`, a mask no earlier layer
/// reads does not, and the source identity ignores where the original is.
#[test]
fn the_input_identity_names_each_component_that_changed() {
    let session = Session::new("patch-identity.sqlite");
    let asset = session.service.state(&session.asset).unwrap().asset;
    let source = source_identity(&asset);
    let moved = crate::AssetRecord {
        locator: "/elsewhere/photo.jpg".into(),
        source_root: "/elsewhere".into(),
        file_identity: "unix:1:2".into(),
        ..asset.clone()
    };
    assert_eq!(
        source_identity(&moved),
        source,
        "Locate keeps the source identity"
    );
    let other = crate::AssetRecord {
        fingerprint: "f".repeat(64),
        ..asset.clone()
    };
    assert_ne!(source_identity(&other), source);
    let content = Stage {
        width: 480,
        height: 320,
    };
    // The development layer keeps its identity as its white balance changes.
    let development = Layer::new(
        "luxforge.raw",
        json!({"white_balance": {"temperature": 6500.0}}),
    );
    let raw = |temperature: f64| Layer {
        payload: json!({"white_balance": {"temperature": temperature}}),
        ..development.clone()
    };
    let repair = Layer::new(CORRECTIONS_EFFECT, RepairPayload::default().stored());
    let input = |layers: &[Layer], masks: &[crate::Mask]| -> RepairInput {
        repair_input(&source, content, layers, masks, 1, &[], 0).unwrap()
    };
    let warm = input(&[raw(6500.0), repair.clone()], &[]);
    let cool = input(&[raw(4000.0), repair.clone()], &[]);
    assert_eq!(warm.reasons(&cool), [StaleReason::Development]);
    // A downstream layer is not part of it.
    let graded = input(
        &[
            raw(6500.0),
            repair.clone(),
            Layer::new(crate::BASIC_EFFECT, json!({"exposure": 1.0})),
        ],
        &[],
    );
    assert!(warm.reasons(&graded).is_empty());
    let resized = repair_input(
        &source,
        Stage {
            width: 320,
            height: 480,
        },
        &[raw(6500.0), repair],
        &[],
        1,
        &[],
        0,
    )
    .unwrap();
    assert_eq!(warm.reasons(&resized), [StaleReason::Dimensions]);
}

#[test]
fn a_patch_changes_only_its_opacity_and_moves_like_any_correction() {
    let mut session = Session::new("patch-update.sqlite");
    add_patch(
        &mut session.service,
        &session.asset,
        "accept",
        PatchSpec::at([10, 10, 8, 8]),
    )
    .unwrap();
    let updated = session.ok(
        "update-correction",
        json!({"operation": "correction-1", "opacity": 40}),
    );
    assert_eq!(
        updated.answer["correction"],
        json!({"operation": "correction-1", "name": "Patch 1", "index": 0})
    );
    assert_eq!(session.label(), "Patch 1 · Opacity 40");
    assert_eq!(rows(&session)[0]["opacity"], 40);
    let same = session
        .run(
            "update-correction",
            json!({"operation": "correction-1", "opacity": 40}),
        )
        .unwrap();
    assert_eq!(same.mutation.outcome, MutationOutcome::NoOp);
    for field in [
        json!({"radius": 0.02}),
        json!({"points": [[0.5, 0.5]]}),
        json!({"mode": "heal"}),
        json!({"source-x": 0.2, "source-y": 0.2}),
    ] {
        let mut parameters = field.clone();
        parameters["operation"] = json!("correction-1");
        let error = session.refused("update-correction", parameters);
        assert_eq!(error.kind, ErrorKind::Validation, "{field}");
        assert!(error.detail.contains("frozen patch"), "{}", error.detail);
    }
    let error = session
        .service
        .run_query(
            &session.asset,
            &session
                .service
                .state(&session.asset)
                .unwrap()
                .current_entry
                .id,
            "correction-sources",
            json!({"operation": "correction-1"}),
        )
        .unwrap_err();
    assert!(error.detail.contains("no source"), "{}", error.detail);
    session.ok("add-correction", spot(0.6, 0.6, 0.3, 0.3));
    session.ok(
        "reorder-correction",
        json!({"operation": "correction-1", "index": 1}),
    );
    assert_eq!(session.label(), "Reorder Patch 1");
    session.ok("delete-correction", json!({"operation": "correction-1"}));
    assert_eq!(session.label(), "Delete Patch 1");
    assert!(repair_layer(&session).artifacts.is_empty());
    add_patch(
        &mut session.service,
        &session.asset,
        "accept-2",
        PatchSpec::at([10, 10, 8, 8]),
    )
    .unwrap();
    assert_eq!(session.label(), "Add Patch 3");
    session.ok("reset-corrections", json!({}));
    let layer = repair_layer(&session);
    assert!(layer.artifacts.is_empty());
    assert_eq!(RepairPayload::parse(&layer.payload).unwrap().next, 4);
    // An accept is never drafted: without a pinned candidate its plan is refused.
    let error = session.refused(
        "accept-correction-patch",
        json!({"job_id": crate::JobId::new(), "candidate": 0}),
    );
    assert!(
        error.detail.contains("cannot be drafted"),
        "{}",
        error.detail
    );
}

#[test]
fn the_layer_lists_each_distinct_patch_artifact_once_and_at_most_sixteen() {
    let mut session = Session::new("patch-artifacts.sqlite");
    let spec = PatchSpec::at([0, 0, 4, 4]);
    let (_, first) = add_patch(&mut session.service, &session.asset, "a", spec).unwrap();
    // The same bytes twice are one artifact, listed once.
    let (_, again) = add_patch(&mut session.service, &session.asset, "b", spec).unwrap();
    assert_eq!(first, again);
    assert_eq!(
        repair_layer(&session).artifacts,
        std::slice::from_ref(&first)
    );
    for seed in 1..15u64 {
        add_patch(
            &mut session.service,
            &session.asset,
            &format!("seed-{seed}"),
            PatchSpec { seed, ..spec },
        )
        .unwrap();
    }
    assert_eq!(repair_layer(&session).artifacts.len(), 15);
    add_patch(
        &mut session.service,
        &session.asset,
        "sixteenth",
        PatchSpec { seed: 99, ..spec },
    )
    .unwrap();
    let before = session.revision();
    let error = add_patch(
        &mut session.service,
        &session.asset,
        "seventeenth",
        PatchSpec { seed: 100, ..spec },
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit, "{}", error.detail);
    assert_eq!(session.revision(), before);
    // A stored stack whose layer lists other artifacts than its patches' is refused, not drawn.
    let layer = repair_layer(&session);
    let state = session.service.state(&session.asset).unwrap();
    let mut reversed = layer.clone();
    reversed.artifacts.reverse();
    let mut recipe = state.current_entry.snapshot.recipe.clone();
    let at = recipe
        .layers
        .iter()
        .position(|layer| layer.effect_id == CORRECTIONS_EFFECT)
        .unwrap();
    recipe.layers[at] = reversed;
    let Err(error) =
        session
            .service
            .registry()
            .compile(state.asset.width, state.asset.height, &recipe)
    else {
        panic!("a layer listing other artifacts compiles");
    };
    assert!(
        matches!(
            error.kind,
            ErrorKind::Incompatible | ErrorKind::SourceUnavailable
        ),
        "{error:?}"
    );
}

#[test]
fn a_refused_accept_leaves_only_an_artifact_that_a_collection_removes() {
    let mut session = Session::new("patch-refused.sqlite");
    let (job, number) = candidate(
        &session.service,
        &session.asset,
        PatchSpec::at([5, 5, 6, 6]),
    )
    .unwrap();
    // A correction added since the candidate was generated changes what a new top one reads.
    session.ok("add-correction", spot(0.5, 0.5, 0.2, 0.2));
    let revision = session.revision();
    let error = accept(
        &mut session.service,
        &session.asset,
        revision,
        "late",
        &job,
        number,
        None,
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(
        error.data.as_deref(),
        Some(&json!({"reasons": ["earlier-correction"]}))
    );
    assert_eq!(session.revision(), revision, "nothing was committed");
    assert_eq!(
        session.service.candidates().read_job(&job)[0].state,
        crate::CandidateState::Live,
        "the candidate is still there to release"
    );
    let catalog = session.catalog.clone();
    let asset = session.asset.clone();
    drop(session);
    // Once no process holds it live, the unreferenced artifact goes.
    let mut service = EditorService::open(&catalog).unwrap();
    let collection = service.plan_collection().unwrap();
    let collected = collect_files(&collection, &AtomicBool::new(false)).unwrap();
    assert_eq!((collected.rows, collected.objects), (1, 1), "{collected:?}");
    assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 2);
}

fn reopened_render(catalog: &Path, asset: &crate::AssetId) -> Result<Raster, crate::Error> {
    let mut service = EditorService::open(catalog).unwrap();
    let needs = service.entry_needs(asset, None)?;
    service.prepare(&needs)?;
    service.render_current(asset)
}

#[test]
#[slow]
fn a_reopened_patch_renders_without_a_producer_and_missing_or_corrupt_bytes_are_named() {
    let mut session = Session::new("patch-reopen.sqlite");
    let (_, artifact) = add_patch(
        &mut session.service,
        &session.asset,
        "accept",
        PatchSpec::at([20, 20, 12, 12]),
    )
    .unwrap();
    let rendered = session.service.render_current(&session.asset).unwrap();
    let (catalog, asset) = (session.catalog.clone(), session.asset.clone());
    drop(session);
    // The built-in registry has no producer at all: the patch renders from its bytes alone.
    assert_eq!(
        reopened_render(&catalog, &asset).unwrap().rgba,
        rendered.rgba
    );
    let root = catalog.with_extension("artifacts");
    let object = object_path(&root, &artifact);
    let bytes = fs::read(&object).unwrap();
    fs::write(&object, &bytes[..bytes.len() - 8]).unwrap();
    let corrupt = reopened_render(&catalog, &asset).unwrap_err();
    assert_eq!(corrupt.kind, ErrorKind::SourceUnavailable, "{corrupt:?}");
    assert!(corrupt.detail.contains("corrupt"), "{}", corrupt.detail);
    fs::remove_file(&object).unwrap();
    let missing = reopened_render(&catalog, &asset).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::SourceUnavailable, "{missing:?}");
    assert!(missing.detail.contains("missing"), "{}", missing.detail);
    let service = EditorService::open(&catalog).unwrap();
    assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 2);
    assert_eq!(
        service.describe_entry(&asset, None).unwrap().layers[0].summary,
        "1 correction"
    );
    // Put back, it renders again.
    fs::write(&object, &bytes).unwrap();
    drop(service);
    assert_eq!(
        reopened_render(&catalog, &asset).unwrap().rgba,
        rendered.rgba
    );
}

#[test]
fn a_catalog_moved_with_its_artifacts_keeps_its_patches_and_a_copied_database_does_not() {
    let directory = luxforge_testbase::paths::temp_dir("patch-moved")
        .canonicalize()
        .unwrap();
    let before = directory.join("before");
    fs::create_dir_all(&before).unwrap();
    let (asset, rendered) = {
        let mut service = EditorService::open(&before.join("catalog.sqlite")).unwrap();
        let asset = service
            .import(&luxforge_testbase::paths::jpeg())
            .unwrap()
            .asset
            .id;
        add_patch(
            &mut service,
            &asset,
            "accept",
            PatchSpec::at([30, 40, 16, 16]),
        )
        .unwrap();
        (asset.clone(), service.render_current(&asset).unwrap())
    };
    let after = directory.join("after");
    fs::rename(&before, &after).unwrap();
    assert_eq!(
        reopened_render(&after.join("catalog.sqlite"), &asset)
            .unwrap()
            .rgba,
        rendered.rgba
    );
    let copy = directory.join("copy");
    fs::create_dir_all(&copy).unwrap();
    fs::copy(after.join("catalog.sqlite"), copy.join("catalog.sqlite")).unwrap();
    let service = EditorService::open(&copy.join("catalog.sqlite")).unwrap();
    let error = service.render_current(&asset).unwrap_err();
    assert_eq!(error.kind, ErrorKind::SourceUnavailable, "{error:?}");
    assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 2);
    drop(service);
    let _ = fs::remove_dir_all(directory);
}

/// A frozen patch on the GPU plan: its words resident in the repair step, the patch's own half
/// floats at the stage it was placed in and box means at a reduced one, the same allocation on a
/// second plan of the same stack, so a tick writes them once; and its own pass when a later
/// operation reads its output.
#[test]
fn a_gpu_plan_holds_a_patch_resident_at_full_and_reduced_stages() {
    use super::qualification::{QualBrush, QualPatch, QualStep, repair_stack};
    let registry = crate::ModuleRegistry::builtin();
    let full = Stage {
        width: 400,
        height: 300,
    };
    let patch = || QualPatch {
        rect: [100, 80, 40, 30],
        opacity: 70,
        pixel: Box::new(|x, y| ([0.2, 0.4, 0.6], ((x + y) % 5) as f32 / 4.0)),
    };
    let stack = repair_stack(
        vec![
            QualStep::Patch(patch()),
            QualStep::Brush(QualBrush {
                heal: false,
                points: vec![[0.75, 0.75]],
                radius: 0.02,
                source: [0.3, 0.3],
                feather: 50,
                opacity: 100,
            }),
        ],
        Vec::new(),
    );
    let plan = |request: crate::GpuPlanRequest| match crate::gpu_plan(&registry, &stack, request)
        .unwrap()
    {
        crate::GpuAnswer::Plan(plan) => *plan,
        crate::GpuAnswer::Fallback(reason) => panic!("{reason}"),
    };
    let exact = plan(crate::GpuPlanRequest::exact(0, full).from_source());
    let step = &exact.spatial[0];
    assert_eq!(step.resident.blocks.len(), 1);
    assert_eq!(
        step.resident.len(),
        40 * 30 * 2,
        "two words a pixel at full resolution"
    );
    assert!(
        step.passes
            .iter()
            .any(|pass| pass.kernel == "lf_repair_patch_blend"),
        "the Clone reads the patch's output, so the patch is a pass of its own"
    );
    let again = plan(crate::GpuPlanRequest::exact(0, full).from_source());
    assert!(std::sync::Arc::ptr_eq(
        &step.resident.blocks[0],
        &again.spatial[0].resident.blocks[0]
    ));
    assert_eq!(step.resident, again.spatial[0].resident);
    let proxy = Stage {
        width: 200,
        height: 150,
    };
    let reduced = plan(crate::GpuPlanRequest::fit(0, proxy, full).from_source());
    let step = &reduced.spatial[0];
    assert_eq!(
        step.resident.len(),
        20 * 15 * 4,
        "four f32 words a pixel of the patch's region at half scale"
    );
}

/// The warm list of a stack holding a frozen patch — at open and after an accept — warms the
/// patch's own pipelines with the stack's repair links.
#[test]
fn the_warm_list_of_a_patched_stack_warms_its_patch() {
    use super::qualification::{QualPatch, QualStep, repair_stack};
    use crate::{AssetId, EntryId, HistoryEntry, Snapshot, SnapshotId};
    let (width, height) = (160u32, 120u32);
    let rgba: Vec<u8> = (0..width * height)
        .flat_map(|index| {
            [
                (index % 251) as u8,
                (index % 239) as u8,
                (index % 113) as u8,
                255,
            ]
        })
        .collect();
    let source = crate::SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:patch-warm".into(),
        orientation: 1,
        capture: Default::default(),
    };
    let recipe = repair_stack(
        vec![QualStep::Patch(QualPatch {
            rect: [20, 20, 30, 20],
            opacity: 100,
            pixel: Box::new(|_, _| ([0.5, 0.5, 0.5], 1.0)),
        })],
        vec![Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.2}))],
    );
    let asset = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "test".into(),
        label: "Test".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset,
            recipe: recipe.clone(),
        },
        undo_parent: None,
        restore_target: None,
    };
    let held = crate::Evaluation::new(
        std::sync::Arc::new(crate::ModuleRegistry::builtin()),
        crate::RenderContext::new(),
        crate::PreviewSource::Jpeg(source),
        entry,
        recipe,
        None,
    );
    let warm = crate::render::gpu::plan_warm_list(
        &held,
        crate::GpuView::Fit(crate::ProxyBounds { width, height }),
    )
    .unwrap();
    let functions: Vec<&str> = warm
        .plans
        .iter()
        .flat_map(|plan| &plan.spatial)
        .flat_map(|step| {
            step.applies
                .iter()
                .map(|apply| apply.function)
                .chain(step.passes.iter().map(|pass| pass.kernel))
        })
        .collect();
    assert!(
        functions
            .iter()
            .any(|name| name.starts_with("lf_repair_patch")),
        "{functions:?}"
    );
}
