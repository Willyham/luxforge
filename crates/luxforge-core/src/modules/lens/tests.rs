use super::*;
use crate::editor::mutation;
use crate::{EditorService, ErrorKind, ModuleRegistry, MutationOutcome, Stage};
use luxforge_testbase::paths::temp_path as temp;
use std::{fs, path::PathBuf, sync::Arc};
fn grid() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg")
}
fn committed_index() -> Arc<index::LensIndex> {
    Arc::new(
        index::LensIndex::parse(
            &fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/data/lensfun/index.json"
            ))
            .unwrap(),
        )
        .unwrap(),
    )
}
struct IndexGuard;
impl IndexGuard {
    fn ready() -> Self {
        index::set_for_test(Ok(committed_index()));
        Self
    }
}
impl Drop for IndexGuard {
    fn drop(&mut self) {
        index::clear_for_test();
    }
}
fn query(service: &EditorService, asset: &crate::AssetId, assume: bool) -> Value {
    let entry = service.state(asset).unwrap().current_entry.id;
    service
        .run_query(asset, &entry, QUERY, json!({"assume-uncorrected":assume}))
        .unwrap()
}
/// The detected profile's Apply request: its key and the parameters the answer says it sends.
fn selection(service: &EditorService, asset: &crate::AssetId) -> Value {
    let answer = query(service, asset, false);
    let suggestion = &answer["status"]["suggestion"];
    assert_eq!(suggestion["eligible"], true, "{answer}");
    assert_eq!(suggestion["match"], "lens-model");
    let mut parameters = suggestion["parameters"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    parameters.insert("profile".into(), suggestion["key"].clone());
    Value::Object(parameters)
}
#[test]
fn lens_module_declares_nonpreset_actions_and_query_choice() {
    let module = LensModule::new();
    module.descriptor().validate().unwrap();
    assert!(
        module
            .descriptor
            .actions
            .iter()
            .all(|a| !a.patch && !a.preset)
    );
    assert!(module.descriptor.collapsed);
    assert_eq!(module.descriptor.effects[0].order, 2);
    assert!(module.descriptor.effects[0].single);
    assert_eq!(module.descriptor.controls.len(), 1);
    assert!(
        matches!(&module.descriptor.controls[0],Control::QueryChoice(control) if control.shared==["focal"])
    );
    let registry = ModuleRegistry::builtin();
    assert!(registry.resolve_action(SELECT).is_some());
    assert!(registry.resolve_query(QUERY).is_some());
}
#[test]
fn lens_select_reset_and_same_selection_keep_identity_and_noop() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-select.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let initial = service.import(&grid()).unwrap();
    let asset = initial.asset.id;
    let rows = query(&service, &asset, false);
    assert_eq!(rows["status"]["distortion"]["status"], "unknown");
    assert_eq!(rows["status"]["visible_shared"], json!([]));
    assert_eq!(
        rows["status"]["suggestion"]["parameters"],
        json!({"assume-uncorrected": true})
    );
    let parameters = selection(&service, &asset);
    assert_eq!(parameters["assume-uncorrected"], true);
    let refused = service
        .apply_action(
            &asset,
            mutation(0, "refuse"),
            SELECT,
            json!({"profile":parameters["profile"]}),
        )
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Incompatible);
    assert_eq!(service.state(&asset).unwrap().revision, 0);
    let selected = service
        .apply_action(&asset, mutation(0, "select"), SELECT, parameters.clone())
        .unwrap();
    assert_eq!(selected.outcome, MutationOutcome::Applied);
    let state = service.state(&asset).unwrap();
    let layer = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|l| l.effect_id == LENS_EFFECT)
        .unwrap()
        .clone();
    assert_eq!(
        state.current_entry.label,
        "Lens profile NIKKOR Z 24-70mm f/4 S at 35 mm"
    );
    let applied = query(&service, &asset, false);
    assert_eq!(applied["status"]["state"], "applied");
    assert_eq!(applied["status"]["current"]["key"], parameters["profile"]);
    assert_eq!(
        applied["status"]["current"]["title"],
        "NIKKOR Z 24-70mm f/4 S"
    );
    assert_eq!(
        applied["status"]["current"]["subtitle"],
        "Nikon Z 6 · 35 mm"
    );
    assert!(
        applied["status"]["current"]["note"]
            .as_str()
            .unwrap()
            .contains("assuming")
    );
    assert!(applied["status"].get("suggestion").is_none());
    let same = service
        .apply_action(
            &asset,
            mutation(state.revision, "same"),
            SELECT,
            parameters.clone(),
        )
        .unwrap();
    assert_eq!(same.outcome, MutationOutcome::NoOp);
    let reset = service
        .apply_action(&asset, mutation(same.revision, "reset"), RESET, json!({}))
        .unwrap();
    assert_eq!(reset.outcome, MutationOutcome::Applied);
    let state = service.state(&asset).unwrap();
    let reset_layer = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|l| l.effect_id == LENS_EFFECT)
        .unwrap();
    assert_eq!(reset_layer.id, layer.id);
    assert_eq!(reset_layer.payload, json!({"profile":null}));
    // A reset leaves the detected lens offered again, never applied.
    let after_reset = query(&service, &asset, false);
    assert_eq!(after_reset["status"]["state"], "detected");
    assert_eq!(
        after_reset["status"]["suggestion"]["key"],
        parameters["profile"]
    );
    let reset_again = service
        .apply_action(
            &asset,
            mutation(state.revision, "reset-again"),
            RESET,
            json!({}),
        )
        .unwrap();
    assert_eq!(reset_again.outcome, MutationOutcome::NoOp);
    drop(service);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn lens_reset_without_layer_needs_no_index_or_source_optics() {
    index::set_for_test(Err(Error::not_ready("test unavailable")));
    let guard = IndexGuard;
    let registry = ModuleRegistry::builtin();
    let fixed = super::super::FixedStage::new(Stage {
        width: 600,
        height: 400,
    });
    let context = StageContext {
        layers: &[],
        registry: &registry,
        target: None,
        kind: crate::SourceTag::Jpeg,
        masks: &[],
        questions: &fixed,
    };
    assert_eq!(
        LensModule::new()
            .plan(
                &ActionInput {
                    action_id: RESET.into(),
                    parameters: Map::new()
                },
                &context
            )
            .unwrap(),
        ActionPlan::NoOp
    );
    drop(guard);
}
#[test]
fn evaluation_never_reads_the_lens_index() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-frozen.sqlite");
    let asset;
    let (entry, rendered) = {
        let mut service = EditorService::open(&catalog).unwrap();
        asset = service.import(&grid()).unwrap().asset.id;
        let parameters = selection(&service, &asset);
        service
            .apply_action(&asset, mutation(0, "select"), SELECT, parameters)
            .unwrap();
        let state = service.state(&asset).unwrap();
        (
            state.current_entry.id,
            service.render_current(&asset).unwrap(),
        )
    };
    index::set_for_test(Err(Error::not_ready("test index unavailable")));
    let mut reopened = EditorService::open(&catalog).unwrap();
    reopened
        .prepare(&reopened.entry_needs(&asset, None).unwrap())
        .unwrap();
    let again = reopened.render_entry(&asset, &entry).unwrap();
    assert_eq!(rendered.rgba, again.rgba);
    let refused = reopened
        .run_query(&asset, &entry, QUERY, json!({}))
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::NotReady);
    let frozen = reopened
        .state(&asset)
        .unwrap()
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|l| l.effect_id == LENS_EFFECT)
        .unwrap()
        .payload
        .clone();
    LensModule::new()
        .compile(
            LENS_EFFECT,
            EFFECT_FORMAT,
            &frozen,
            crate::CompileStage::exact(Stage {
                width: 300,
                height: 200,
            }),
        )
        .unwrap();
    drop(reopened);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn lens_history_survives_undo_redo_restore_reopen() {
    let _index = IndexGuard::ready();
    let original = fs::read(grid()).unwrap();
    let catalog = temp("lens-history.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "select"), SELECT, parameters)
        .unwrap();
    let selected = service.state(&asset).unwrap();
    let snapshot = selected.current_entry.snapshot.clone();
    let entry = selected.current_entry.id.clone();
    let undone = service
        .undo(&asset, mutation(selected.revision, "undo"))
        .unwrap();
    let redone = service
        .redo(&asset, mutation(undone.revision, "redo"))
        .unwrap();
    assert_eq!(
        service.state(&asset).unwrap().current_entry.snapshot,
        snapshot
    );
    let reset = service
        .apply_action(&asset, mutation(redone.revision, "reset"), RESET, json!({}))
        .unwrap();
    service
        .restore(&asset, mutation(reset.revision, "restore"), &entry)
        .unwrap();
    let restored = service
        .state(&asset)
        .unwrap()
        .current_entry
        .snapshot
        .recipe
        .clone();
    assert_eq!(restored, snapshot.recipe);
    drop(service);
    let reopened = EditorService::open(&catalog).unwrap();
    assert_eq!(
        reopened
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe,
        restored
    );
    assert_eq!(fs::read(grid()).unwrap(), original);
    drop(reopened);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn lens_payload_refuses_unknown_fields_and_forged_normalization() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-validation.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "select"), SELECT, parameters)
        .unwrap();
    let state = service.state(&asset).unwrap();
    let value = state.current_entry.snapshot.recipe.layers[0]
        .payload
        .clone();
    for change in [
        "unknown",
        "interpretation",
        "hash",
        "normalization",
        "model",
        "overflow",
    ] {
        let mut bad = value.clone();
        match change {
            "unknown" => bad["profile"]["unknown"] = json!(true),
            "interpretation" => bad["profile"]["interpretation"] = json!("obsolete"),
            "hash" => bad["profile"]["database"]["record_sha256"] = json!("a".repeat(64)),
            "normalization" => bad["profile"]["normalization"]["unit_scale"] = json!(1.2),
            "overflow" => {
                bad["profile"]["lens"]["aspect_ratio"] = json!(f64::MAX);
                bad["profile"]["lens"]["crop_factor"] = json!(f64::MAX);
                bad["profile"]["camera"]["crop_factor"] = json!(f64::MAX);
            }
            _ => bad["profile"]["model"] = json!("poly3"),
        };
        assert!(payload::parse(&bad).is_err(), "{change}");
    }
    drop(service);
    fs::remove_file(catalog).unwrap();
}

#[test]
fn crop_payload_and_id_unchanged_by_lens_or_perspective_change() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-crop.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    service
        .apply_action(
            &asset,
            mutation(0, "crop"),
            "crop",
            json!({"angle":2.5,"x":0.1,"y":0.1,"width":0.8,"height":0.8}),
        )
        .unwrap();
    let before = service.state(&asset).unwrap();
    let crop = before
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|l| l.effect_id == crate::CROP_EFFECT)
        .unwrap()
        .clone();
    let base = service.render_current(&asset).unwrap();
    let parameters = selection(&service, &asset);
    service
        .apply_action(
            &asset,
            mutation(before.revision, "lens"),
            SELECT,
            parameters.clone(),
        )
        .unwrap();
    let revision = service.state(&asset).unwrap().revision;
    service
        .apply_action(
            &asset,
            mutation(revision, "perspective"),
            "set-perspective",
            json!({"horizontal":40,"vertical":-25}),
        )
        .unwrap();
    let changed = service.state(&asset).unwrap();
    assert_eq!(
        changed
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|l| l.effect_id == crate::CROP_EFFECT),
        Some(&crop)
    );
    service
        .apply_action(
            &asset,
            mutation(changed.revision, "clear-perspective"),
            "reset-perspective",
            json!({}),
        )
        .unwrap();
    let revision = service.state(&asset).unwrap().revision;
    let mut wide = parameters;
    wide["focal"] = json!(24.0);
    service
        .apply_action(&asset, mutation(revision, "wide"), SELECT, wide)
        .unwrap();
    let revision = service.state(&asset).unwrap().revision;
    service
        .apply_action(&asset, mutation(revision, "lens-reset"), RESET, json!({}))
        .unwrap();
    let final_state = service.state(&asset).unwrap();
    assert_eq!(
        final_state
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|l| l.effect_id == crate::CROP_EFFECT),
        Some(&crop)
    );
    assert_eq!(base.rgba, service.render_current(&asset).unwrap().rgba);
    drop(service);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn crop_reset_restores_covered_canvas() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-canvas.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "lens"), SELECT, parameters)
        .unwrap();
    let covered = service.render_current(&asset).unwrap();
    let rev = service.state(&asset).unwrap().revision;
    service
        .apply_action(
            &asset,
            mutation(rev, "crop"),
            "crop",
            json!({"angle":2.5,"x":0.1,"y":0.1,"width":0.8,"height":0.8}),
        )
        .unwrap();
    let rev = service.state(&asset).unwrap().revision;
    service
        .apply_action(&asset, mutation(rev, "crop-reset"), "crop-reset", json!({}))
        .unwrap();
    let reset = service.render_current(&asset).unwrap();
    assert_eq!((reset.width, reset.height), (600, 400));
    assert_eq!(covered.rgba, reset.rgba);
    drop(service);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn export_of_warped_entry_matches_exact_render_and_dimensions_are_fixed() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-export-buffer.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "lens"), SELECT, parameters)
        .unwrap();
    let rev = service.state(&asset).unwrap().revision;
    service
        .apply_action(
            &asset,
            mutation(rev, "perspective"),
            "set-perspective",
            json!({"horizontal":40,"vertical":-25}),
        )
        .unwrap();
    let plan = service.export_plan(&asset, None).unwrap();
    assert_eq!((plan.identity.width, plan.identity.height), (600, 400));
    let exported = plan
        .evaluation
        .exact(&crate::Cancel::never())
        .unwrap()
        .frame(plan.identity.snapshot_id.clone())
        .unwrap();
    assert_eq!(exported.rgba, service.render_current(&asset).unwrap().rgba);
    index::set_for_test(Err(Error::not_ready("missing index")));
    let frozen = service.export_plan(&asset, None).unwrap();
    assert_eq!(
        frozen
            .evaluation
            .exact(&crate::Cancel::never())
            .unwrap()
            .frame(frozen.identity.snapshot_id.clone())
            .unwrap()
            .rgba,
        exported.rgba
    );
    drop(service);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn failed_catalog_write_leaves_lens_selection_and_original_intact() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-write-fail.sqlite");
    let original = fs::read(grid()).unwrap();
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let before = service.state(&asset).unwrap();
    let parameters = selection(&service, &asset);
    service.connection.execute_batch("CREATE TRIGGER fail_lens_write BEFORE INSERT ON entries BEGIN SELECT RAISE(FAIL, 'forced test write failure'); END;").unwrap();
    assert_eq!(
        service
            .apply_action(&asset, mutation(0, "refused-write"), SELECT, parameters)
            .unwrap_err()
            .kind,
        ErrorKind::Catalog
    );
    let after = service.state(&asset).unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.current_entry, before.current_entry);
    assert_eq!(fs::read(grid()).unwrap(), original);
    drop(service);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn lens_described_fields_preserve_exif_focal_source_after_refresh() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-focal-fields.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "select"), SELECT, parameters.clone())
        .unwrap();
    let state = service.state(&asset).unwrap();
    let layer = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|l| l.effect_id == LENS_EFFECT)
        .unwrap();
    let report = LensModule::new()
        .describe(LENS_EFFECT, EFFECT_FORMAT, &layer.payload)
        .unwrap();
    assert!(!report.values.contains_key("focal"));
    assert_eq!(layer.payload["profile"]["focal"]["source"], "exif");
    let same = service
        .apply_action(
            &asset,
            mutation(state.revision, "select-described"),
            SELECT,
            Value::Object(report.values),
        )
        .unwrap();
    assert_eq!(same.outcome, MutationOutcome::NoOp);
    let mut override_parameters = parameters;
    override_parameters["focal"] = json!(35.0);
    service
        .apply_action(
            &asset,
            mutation(same.revision, "override"),
            SELECT,
            override_parameters,
        )
        .unwrap();
    let state = service.state(&asset).unwrap();
    let layer = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|l| l.effect_id == LENS_EFFECT)
        .unwrap();
    let report = LensModule::new()
        .describe(LENS_EFFECT, EFFECT_FORMAT, &layer.payload)
        .unwrap();
    assert_eq!(report.values["focal"], 35.0);
    assert_eq!(layer.payload["profile"]["focal"]["source"], "override");
    drop(service);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn fixed_camera_profile_history_uses_the_record_name_without_focal() {
    let index = committed_index();
    let request = resolve::ResolveInput {
        make: "DJI".into(),
        model: "FC3411".into(),
        lens_model: None,
        focal_mm: Some(8.38),
        focal_35mm: Some(22),
        focal_override: None,
        stage: Stage {
            width: 600,
            height: 400,
        },
    };
    let row = resolve::candidates(&index, &request)
        .into_iter()
        .find(|row| row.eligible)
        .unwrap();
    let resolution = resolve::resolve(&index, &row.key, &request).unwrap();
    assert!(resolution.camera.fixed_mount);
    let record_name = resolution.lens.model.clone();
    let optics = SourceOptics {
        identity: crate::OpticalIdentity {
            make: None,
            model: None,
            lens_make: None,
            lens_model: None,
            focal_mm: None,
            focal_35mm: None,
        },
        ledger: SourceOptics::jpeg_ledger(),
    };
    let frozen = serde_json::to_value(payload::Payload {
        profile: Some(payload::Profile::from_resolution(resolution, &optics, true).unwrap()),
    })
    .unwrap();
    let layer = Layer::new(LENS_EFFECT, frozen);
    assert_eq!(
        LensModule::new().planned_label(
            &ActionInput {
                action_id: SELECT.into(),
                parameters: Map::new()
            },
            &[layer],
            "fallback"
        ),
        format!("Lens profile {record_name}")
    );
}
#[test]
fn missing_or_disabled_lens_provider_preserves_edits_and_refuses_render_and_export() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-provider-unavailable.sqlite");
    let original = fs::read(grid()).unwrap();
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "lens"), SELECT, parameters)
        .unwrap();
    for neutral in [false, true] {
        if neutral {
            let revision = service.state(&asset).unwrap().revision;
            service
                .apply_action(&asset, mutation(revision, "reset"), RESET, json!({}))
                .unwrap();
        }
        let stored = service.state(&asset).unwrap();
        drop(service);
        for disabled in [false, true] {
            let mut registry = ModuleRegistry::new();
            for module in crate::modules::registry::linked_modules(false) {
                if module.descriptor().id == "luxforge.lens" {
                    if disabled {
                        registry
                            .register_unavailable(module, "disabled for test")
                            .unwrap();
                    }
                } else {
                    registry.register(module).unwrap();
                }
            }
            let mut reopened = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
            assert_eq!(
                reopened.state(&asset).unwrap().current_entry,
                stored.current_entry
            );
            reopened
                .prepare(&reopened.entry_needs(&asset, None).unwrap())
                .unwrap();
            for error in [
                reopened.render_current(&asset).unwrap_err(),
                reopened
                    .export_plan(&asset, None)
                    .err()
                    .expect("export refuses the unavailable provider"),
            ] {
                assert_eq!(error.kind, ErrorKind::Incompatible);
                assert!(error.detail.contains(LENS_EFFECT), "{}", error.detail);
            }
            assert_eq!(
                reopened.state(&asset).unwrap().current_entry,
                stored.current_entry
            );
            drop(reopened);
        }
        service = EditorService::open(&catalog).unwrap();
    }
    assert_eq!(fs::read(grid()).unwrap(), original);
    drop(service);
    fs::remove_file(catalog).unwrap();
}
#[test]
fn missing_original_after_lens_and_perspective_preserves_saved_edits() {
    let _index = IndexGuard::ready();
    let catalog = temp("lens-missing-source.sqlite");
    let source = temp("lens-missing-original.jpg");
    let held = source.with_extension("held.jpg");
    let original = fs::read(grid()).unwrap();
    fs::write(&source, &original).unwrap();
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&source).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "lens"), SELECT, parameters)
        .unwrap();
    let revision = service.state(&asset).unwrap().revision;
    service
        .apply_action(
            &asset,
            mutation(revision, "perspective"),
            "set-perspective",
            json!({"horizontal":40,"vertical":-25}),
        )
        .unwrap();
    let stored = service.state(&asset).unwrap();
    let rendered = service.render_current(&asset).unwrap();
    assert!(
        stored
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .any(|layer| layer.effect_id == LENS_EFFECT)
    );
    assert!(
        stored
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .any(|layer| layer.effect_id == crate::PERSPECTIVE_EFFECT)
    );
    fs::rename(&source, &held).unwrap();
    // A prepared source must still be signature-verified; removing the original refuses the
    // cached render as well as a fresh owner after reopening the same catalog.
    for reopened in [false, true] {
        if reopened {
            drop(service);
            service = EditorService::open(&catalog).unwrap();
        }
        let needs = service.entry_needs(&asset, None).unwrap();
        for error in [
            service.prepare(&needs).unwrap_err(),
            service.render_current(&asset).unwrap_err(),
            service
                .export_plan(&asset, None)
                .err()
                .expect("export refuses a missing original"),
        ] {
            assert_eq!(error.kind, ErrorKind::SourceUnavailable, "{error}");
        }
        let after = service.state(&asset).unwrap();
        assert_eq!(after.revision, stored.revision);
        assert_eq!(after.current_entry, stored.current_entry);
        assert_eq!(fs::read(&held).unwrap(), original);
    }
    fs::rename(&held, &source).unwrap();
    service
        .prepare(&service.entry_needs(&asset, None).unwrap())
        .unwrap();
    assert_eq!(service.render_current(&asset).unwrap().rgba, rendered.rgba);
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::read(grid()).unwrap(), original);
    drop(service);
    fs::remove_file(catalog).unwrap();
    fs::remove_file(source).unwrap();
}
#[test]
fn integrated_recipe_with_masks_lens_perspective_crop_and_finish_in_both_domains() {
    use crate::render::testing::{frame_in, linear, sample_in};
    use crate::{
        Component, ComponentMode, LinearImage, LinearSettings, Mask, Orientation, Recipe,
        RenderContext, RenderOptions, SnapshotId,
    };
    let _index = IndexGuard::ready();
    let catalog = temp("lens-integrated.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    let asset = service.import(&grid()).unwrap().asset.id;
    let parameters = selection(&service, &asset);
    service
        .apply_action(&asset, mutation(0, "lens"), SELECT, parameters)
        .unwrap();
    let profile = service
        .state(&asset)
        .unwrap()
        .current_entry
        .snapshot
        .recipe
        .layers[0]
        .payload
        .clone();
    let mut mask = Mask::new("Radial exposure");
    mask.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        json!({"x":0.45,"y":0.5,"radius_x":0.2,"radius_y":0.25,"angle":20.0,"feather":40.0}),
    ));
    let mut masked = Layer::new(
        crate::BASIC_EFFECT,
        json!({"exposure":0.25,"saturation":15.0}),
    );
    masked.mask = Some(mask.id.clone());
    let mut recipe = Recipe::default();
    recipe.masks.push(mask);
    recipe.layers = vec![
        Layer::new(crate::BASIC_EFFECT, json!({"exposure":-0.1})),
        masked,
        Layer::new(crate::PRESENCE_EFFECT, json!({"clarity":10.0})),
        Layer::orientation(Orientation {
            turns: 1,
            mirror: true,
        }),
        Layer::new(LENS_EFFECT, profile),
        Layer::new(
            crate::PERSPECTIVE_EFFECT,
            json!({"horizontal":40,"vertical":-25}),
        ),
        Layer::new(
            crate::CROP_EFFECT,
            json!({"angle":2.5,"x":0.1,"y":0.1,"width":0.8,"height":0.8}),
        ),
        Layer::new(crate::VIGNETTE_EFFECT, json!({"amount":-20.0})),
    ];
    let source = crate::open_source(&grid()).unwrap();
    let original_rgba = source.rgba.clone();
    let area = (source.width * source.height) as usize;
    let planes: Vec<_> = (0..3)
        .flat_map(|channel| {
            (0..area).map(move |i| 0.05 + ((i + channel * 97) % 1000) as f32 / 1300.0)
        })
        .collect();
    let linear_source = LinearImage::new(source.width, source.height, planes).unwrap();
    let original_planes = linear_source.planes().to_vec();
    let registry = ModuleRegistry::builtin();
    index::set_for_test(Err(Error::not_ready(
        "missing index during frozen evaluation",
    )));
    for raw in [false, true] {
        let context = RenderContext::new();
        let input = if raw {
            linear(&linear_source, LinearSettings::default())
        } else {
            crate::RenderSource::from(&source)
        };
        let raster = frame_in(
            &context,
            &registry,
            input,
            SnapshotId::new(),
            &recipe,
            RenderOptions::default(),
        )
        .unwrap();
        assert!(raster.width < 400 && raster.height < 600);
        for (x, y) in [
            (0, 0),
            (raster.width / 2, raster.height / 2),
            (raster.width - 1, raster.height - 1),
            (raster.width / 3, raster.height / 4),
        ] {
            let point = sample_in(
                &context,
                &registry,
                input,
                &recipe,
                RenderOptions::default(),
                x,
                y,
            )
            .unwrap()
            .rgba
            .unwrap();
            let start = ((y * raster.width + x) * 4) as usize;
            assert_eq!(&point, &raster.rgba[start..start + 4], "raw={raw}, {x},{y}");
        }
        let cancel = crate::Cancel::never();
        cancel.cancel();
        assert_eq!(
            frame_in(
                &context,
                &registry,
                input,
                SnapshotId::new(),
                &recipe,
                RenderOptions::exact(&cancel)
            )
            .unwrap_err()
            .kind,
            ErrorKind::Cancelled
        );
    }
    assert_eq!(source.rgba, original_rgba);
    assert_eq!(linear_source.planes(), original_planes);
    drop(service);
    fs::remove_file(catalog).unwrap();
}

/// A stack's questions with fixed optics and stage: what a prepared source answers.
struct Optical {
    stage: Stage,
    optics: SourceOptics,
}
impl super::super::StageQuestions for Optical {
    fn optics(&self) -> Result<SourceOptics, Error> {
        Ok(self.optics.clone())
    }
    fn stage_before(&self, _: usize) -> Result<Stage, Error> {
        Ok(self.stage)
    }
    fn sample_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[u8; 4]>, Error> {
        Ok(None)
    }
}
fn air2s(ledger: luxforge_raw::OpticalLedger) -> Optical {
    Optical {
        stage: Stage {
            width: 5472,
            height: 3648,
        },
        optics: SourceOptics {
            identity: crate::OpticalIdentity {
                make: Some("DJI".into()),
                model: Some("FC3411".into()),
                lens_make: None,
                lens_model: None,
                focal_mm: Some(8.38),
                focal_35mm: Some(22),
            },
            ledger,
        },
    }
}
fn mosaic_ledger(distortion: luxforge_raw::OpticalStatus) -> luxforge_raw::OpticalLedger {
    let entry = |status| luxforge_raw::OpticalEntry {
        status,
        provenance: "raw-mosaic".into(),
    };
    luxforge_raw::OpticalLedger {
        distortion: entry(distortion),
        lateral_ca: entry(luxforge_raw::OpticalStatus::KnownUnapplied),
        shading: entry(luxforge_raw::OpticalStatus::KnownUnapplied),
        interpretation: "raw-mosaic:test".into(),
    }
}

#[test]
fn first_open_selects_only_a_known_uncorrected_photos_eligible_detected_profile() {
    let _index = IndexGuard::ready();
    let registry = ModuleRegistry::builtin();
    let module = LensModule::new();
    let ask = |questions: &Optical, layers: &[Layer], kind| {
        let context = StageContext {
            layers,
            registry: &registry,
            target: None,
            kind,
            masks: &[],
            questions,
        };
        module.first_open(&context).inspect(|proposed| {
            // What it proposes is an ordinary selection that plans as it stands.
            if let Some(input) = proposed {
                assert!(matches!(
                    module.plan(input, &context).unwrap(),
                    ActionPlan::Commit(_)
                ));
            }
        })
    };
    let raw = air2s(mosaic_ledger(luxforge_raw::OpticalStatus::KnownUnapplied));
    let proposed = ask(&raw, &[], crate::SourceTag::Raw).unwrap().unwrap();
    assert_eq!(proposed.action_id, SELECT);
    let expected = resolve::detect(
        &committed_index(),
        &resolve::ResolveInput {
            make: "DJI".into(),
            model: "FC3411".into(),
            lens_model: None,
            focal_mm: Some(8.38),
            focal_35mm: Some(22),
            focal_override: None,
            stage: raw.stage,
        },
    )
    .detected
    .unwrap();
    assert_eq!(
        proposed.parameters,
        Map::from_iter([("profile".to_owned(), json!(expected.key))]),
        "no acknowledgement and no focal override: the profile applies as detected"
    );
    // A layer of its own, neutral or not, means a person or agent already decided.
    let neutral = Layer::new(LENS_EFFECT, json!({"profile": null}));
    assert_eq!(ask(&raw, &[neutral], crate::SourceTag::Raw).unwrap(), None);
    // A JPEG's in-camera correction is unknown and an embedded warp already corrects: neither is
    // corrected on first open.
    let jpeg = air2s(SourceOptics::jpeg_ledger());
    assert_eq!(ask(&jpeg, &[], crate::SourceTag::Jpeg).unwrap(), None);
    let corrected = air2s(mosaic_ledger(luxforge_raw::OpticalStatus::Applied));
    assert_eq!(ask(&corrected, &[], crate::SourceTag::Raw).unwrap(), None);
    // A camera the database does not hold detects nothing.
    let mut unknown = air2s(mosaic_ledger(luxforge_raw::OpticalStatus::KnownUnapplied));
    unknown.optics.identity.model = Some("FC9999".into());
    assert_eq!(ask(&unknown, &[], crate::SourceTag::Raw).unwrap(), None);
    // An unavailable index is an explicit refusal, never a silent skip.
    index::set_for_test(Err(Error::not_ready("test index unavailable")));
    assert_eq!(
        ask(&raw, &[], crate::SourceTag::Raw).unwrap_err().kind,
        ErrorKind::NotReady
    );
}
