//! The shared admission boundary protects every source path without decoding or reading profiles.
use super::*;
use crate::editor::catalog::encode;
use crate::editor::test_support::synthetic_raw_metadata;
use crate::modules::lens::{index, payload, resolve};
use crate::{Layer, ModuleRegistry, MutationOutcome, OpticalIdentity, Recipe, SourceOptics};
use luxforge_raw::{
    DngCalibrationMetadata, DngCorrectionMetadata, DngOpcodeProvenance, OpticalStatus, RawMode,
};

fn asset(source: SourceKind) -> AssetRecord {
    AssetRecord {
        id: AssetId::new(),
        source_root: PathBuf::new(),
        locator: PathBuf::from("untouched.original"),
        fingerprint: "original".into(),
        file_identity: "original".into(),
        byte_len: 1,
        width: 600,
        height: 400,
        source,
    }
}
fn profile(ledger: luxforge_raw::OpticalLedger) -> Value {
    let index = index::LensIndex::parse(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/lensfun/index.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let input = resolve::ResolveInput {
        make: "NIKON CORPORATION".into(),
        model: "NIKON Z 6".into(),
        lens_model: Some("NIKKOR Z 24-70mm f/4 S".into()),
        focal_mm: Some(35.0),
        focal_35mm: Some(35),
        focal_override: None,
        stage: crate::Stage {
            width: 600,
            height: 400,
        },
    };
    let key = resolve::candidates(&index, &input)
        .into_iter()
        .find(|r| r.eligible)
        .unwrap()
        .key;
    let source = SourceOptics {
        identity: OpticalIdentity {
            make: None,
            model: None,
            lens_make: None,
            lens_model: None,
            focal_mm: None,
            focal_35mm: None,
        },
        ledger,
    };
    let resolved = resolve::resolve(&index, &key, &input).unwrap();
    serde_json::to_value(payload::Payload {
        profile: Some(payload::Profile::from_resolution(resolved, &source, true).unwrap()),
    })
    .unwrap()
}
fn recipe(source: &SourceKind, profile: Value) -> Recipe {
    let mut recipe = Recipe::default();
    if let SourceKind::Raw { metadata } = source {
        recipe.layers.push(
            crate::RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz)
                .unwrap()
                .layer(LayerId::new()),
        );
    }
    recipe.layers.push(Layer::new(crate::LENS_EFFECT, profile));
    recipe
}
fn dng(mode: &str) -> luxforge_raw::RawMetadata {
    let mut metadata = synthetic_raw_metadata();
    metadata.mode = RawMode::from_id(mode).unwrap();
    metadata.dng_corrections = Some(DngCorrectionMetadata {
        interpretation: "synthetic-stage3-v1".into(),
        applied: vec![DngOpcodeProvenance {
            list: 51022,
            id: 1,
            version: 0x0103_0000,
            flags: 0,
            payload_sha256: "0".repeat(64),
        }],
        skipped_optional: vec![],
        calibration: DngCalibrationMetadata {
            illuminants: [17, 21],
            color_matrix1_sha256: "0".repeat(64),
            color_matrix2_sha256: "0".repeat(64),
            selected: "test".into(),
        },
    });
    metadata
}
#[test]
fn lens_admission_rechecks_jpeg_acknowledgement_marker_and_status() {
    let registry = ModuleRegistry::builtin();
    let asset = asset(SourceKind::Jpeg);
    let frozen = profile(SourceOptics::jpeg_ledger());
    let recipe = recipe(&asset.source, frozen.clone());
    validate_source_recipe(&registry, &asset, &recipe).unwrap();
    for (field, value) in [
        ("acknowledged", Value::Null),
        (
            "source_interpretation",
            json!("raw-mosaic:NikonZ6Lossless14"),
        ),
        ("distortion", json!("known-unapplied")),
    ] {
        let mut recipe = recipe.clone();
        recipe.layers[0].payload["profile"]["optics"][field] = value;
        assert_eq!(
            validate_source_recipe(&registry, &asset, &recipe)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible,
            "{field}"
        );
    }
}
#[test]
fn lens_admission_accepts_raw_mosaic_and_declared_lateral_ca_but_refuses_undeclared_warp() {
    let registry = ModuleRegistry::builtin();
    for metadata in [synthetic_raw_metadata(), dng("DjiAir2sDng16")] {
        let ledger = luxforge_raw::optical_ledger(&metadata);
        assert_eq!(ledger.distortion.status, OpticalStatus::KnownUnapplied);
        let source = SourceKind::Raw {
            metadata: RawInterpretation::new(metadata).unwrap(),
        };
        let frozen = profile(ledger);
        let asset = asset(source.clone());
        let mut recipe = recipe(&source, frozen);
        recipe.layers[1].payload["profile"]["optics"]["acknowledged"] = Value::Null;
        validate_source_recipe(&registry, &asset, &recipe).unwrap();
    }
    let metadata = dng("DJIFC220Dng16");
    let ledger = luxforge_raw::optical_ledger(&metadata);
    assert_eq!(ledger.distortion.status, OpticalStatus::Applied);
    let source = SourceKind::Raw {
        metadata: RawInterpretation::new(metadata).unwrap(),
    };
    let asset = asset(source.clone());
    let mut frozen = profile(SourceOptics::jpeg_ledger());
    frozen["profile"]["optics"]["source_interpretation"] = json!(ledger.interpretation);
    frozen["profile"]["optics"]["distortion"] = json!("applied");
    let mut recipe = recipe(&source, frozen);
    let error = validate_source_recipe(&registry, &asset, &recipe).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert_eq!(error.data.unwrap()["reason"], "embedded-distortion-applied");
    recipe.layers[1].payload = json!({"profile":null});
    validate_source_recipe(&registry, &asset, &recipe).unwrap();
}
#[test]
fn lens_admission_requires_frozen_full_input_dimensions_and_accepts_quarter_turns() {
    let registry = ModuleRegistry::builtin();
    let mut asset = asset(SourceKind::Jpeg);
    let frozen = profile(SourceOptics::jpeg_ledger());
    let recipe = recipe(&asset.source, frozen);
    asset.width = 400;
    asset.height = 600;
    validate_source_recipe(&registry, &asset, &recipe).unwrap();
    asset.height = 601;
    let error = validate_source_recipe(&registry, &asset, &recipe).unwrap_err();
    assert_eq!(error.data.unwrap()["reason"], "lens-input-stage-changed");
}

#[test]
fn applied_distortion_refuses_lens_drafts_commits_restore_reopen_and_export() {
    use crate::editor::{history::CommittedAction, mutation, test_support::temp};
    use crate::{ActionInput, Draft, EditorService, Snapshot};
    use serde_json::Map;

    fn committed(label: &str) -> CommittedAction {
        CommittedAction {
            input: ActionInput {
                action_id: "test-optics-fixture".into(),
                parameters: Map::new(),
            },
            label: label.into(),
            touched: None,
            skipped: Vec::new(),
        }
    }
    fn stored_rows(service: &EditorService) -> Vec<(String, String)> {
        service
            .connection
            .prepare("SELECT id,entry_json FROM entries ORDER BY sequence")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    let catalog = temp("lens-applied-distortion-journey.sqlite");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg");
    let original = std::fs::read(&source).unwrap();
    let mut service = EditorService::open(&catalog).unwrap();
    let initial = service.import(&source).unwrap();
    let asset = initial.asset.id.clone();
    let frozen = profile(SourceOptics::jpeg_ledger());
    let selected = initial
        .current_entry
        .snapshot
        .with_recipe(recipe(&initial.asset.source, frozen))
        .unwrap();
    service
        .commit_snapshot(
            &asset,
            mutation(0, "fixture-lens"),
            json!({"fixture":"lens"}),
            selected,
            &initial.asset,
            committed("Fixture lens"),
        )
        .unwrap();
    let lens_entry = service.state(&asset).unwrap().current_entry.id;
    service
        .apply_action(
            &asset,
            mutation(1, "fixture-perspective"),
            "set-perspective",
            json!({"horizontal":40,"vertical":-25}),
        )
        .unwrap();
    let entries = stored_rows(&service);
    drop(service);

    // Fault injection into an isolated catalog: the original remains a JPEG. A synthetic source
    // interpretation declares an applied, undeclared WarpRectilinear, and each saved stack gets
    // valid RAW calibration plus a forged lens acknowledgement. Every tested path must stop at
    // source admission before attempting decode; stored rows remain readable and unchanged.
    let metadata = dng("DJIFC220Dng16");
    let ledger = luxforge_raw::optical_ledger(&metadata);
    assert_eq!(ledger.distortion.status, OpticalStatus::Applied);
    let source_kind = SourceKind::Raw {
        metadata: RawInterpretation::new(metadata.clone()).unwrap(),
    };
    let connection = rusqlite::Connection::open(&catalog).unwrap();
    connection
        .execute(
            "UPDATE assets SET source_json=?1,source_kind='raw' WHERE id=?2",
            rusqlite::params![encode(&source_kind).unwrap(), asset.as_str()],
        )
        .unwrap();
    connection
        .execute_batch("DROP TRIGGER entries_are_immutable;")
        .unwrap();
    for (id, json) in entries {
        let mut entry: crate::HistoryEntry = serde_json::from_str(&json).unwrap();
        let raw = crate::RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz)
            .unwrap()
            .layer(LayerId::new());
        entry.snapshot.recipe.layers.insert(0, raw);
        for layer in &mut entry.snapshot.recipe.layers {
            if layer.effect_id == crate::LENS_EFFECT {
                layer.payload["profile"]["optics"]["source_interpretation"] =
                    json!(ledger.interpretation);
                layer.payload["profile"]["optics"]["distortion"] = json!("applied");
                layer.payload["profile"]["optics"]["acknowledged"] = json!("assume-uncorrected");
            }
        }
        connection
            .execute(
                "UPDATE entries SET entry_json=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&entry).unwrap(), id],
            )
            .unwrap();
    }
    connection.execute_batch("CREATE TRIGGER entries_are_immutable BEFORE UPDATE ON entries BEGIN SELECT RAISE(ABORT, 'history entries are immutable'); END;").unwrap();
    drop(connection);

    for _ in 0..2 {
        let mut service = EditorService::open(&catalog).unwrap();
        let before = service.state(&asset).unwrap();
        let rows = stored_rows(&service);
        let profile = before
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|layer| layer.effect_id == crate::LENS_EFFECT)
            .unwrap()
            .payload["profile"]
            .clone();
        let parameters = json!({"profile":profile["key"],"assume-uncorrected":true});
        let mut draft = Draft::new("select-lens-profile", asset.clone(), before.revision);
        draft.fields = parameters.as_object().unwrap().clone();
        let snapshot = Snapshot {
            id: crate::SnapshotId::new(),
            asset_id: asset.clone(),
            recipe: before.current_entry.snapshot.recipe.clone(),
        };
        let errors = [
            ("draft", service.draft_recipe(&asset, &draft).unwrap_err()),
            (
                "selection commit",
                service
                    .apply_action(
                        &asset,
                        mutation(before.revision, "refuse-selection"),
                        "select-lens-profile",
                        parameters,
                    )
                    .unwrap_err(),
            ),
            (
                "direct commit",
                service
                    .commit_snapshot(
                        &asset,
                        mutation(before.revision, "refuse-direct"),
                        json!({"fixture":"direct"}),
                        snapshot,
                        &before.asset,
                        committed("Refused lens"),
                    )
                    .unwrap_err(),
            ),
            (
                "Restore",
                service
                    .restore(
                        &asset,
                        mutation(before.revision, "refuse-restore"),
                        &lens_entry,
                    )
                    .unwrap_err(),
            ),
            (
                "source preparation",
                service.entry_needs(&asset, None).unwrap_err(),
            ),
            ("render", service.render_current(&asset).unwrap_err()),
            (
                "export",
                service
                    .export_plan(&asset, None)
                    .err()
                    .expect("export refuses applied distortion"),
            ),
        ];
        for (path, error) in errors {
            assert_eq!(error.kind, ErrorKind::Incompatible, "{path}: {error}");
            assert_eq!(
                error.data.as_deref().unwrap()["reason"],
                "embedded-distortion-applied",
                "{path}"
            );
        }
        let after = service.state(&asset).unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.current_entry, before.current_entry);
        assert_eq!(stored_rows(&service), rows);
        assert_eq!(service.history(&asset, None, 64).unwrap().entries.len(), 3);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        drop(service);
    }
    std::fs::remove_file(catalog).unwrap();
}

/// The desktop's RAW pick journey: the output click is located through the optional recipe
/// geometry first; the RAW command then asks the retained mosaic at that content point. Air 2S
/// has mandatory per-channel DNG warp and gain corrections inside that final sensor query.
#[test]
#[ignore = "requires explicit local authentic DJI DNG"]
fn owner_dji_raw_neutral_pick_inverts_recipe_then_queries_corrected_sensor_once() {
    use crate::export::CaptureMetadata;
    use crate::source::PreparedSource;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, atomic::AtomicBool};

    struct IndexGuard;
    impl Drop for IndexGuard {
        fn drop(&mut self) {
            index::clear_for_test();
        }
    }
    let owner = std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner fixture directory");
    let path = PathBuf::from(owner).join("mavic_air_2s.DNG");
    let bytes = std::fs::read(&path).expect("read Air 2S DNG");
    let fingerprint = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        fingerprint,
        "aab79ce1795a7dd5f1c2e52ec7bd07345cb9bda0262d1d5aa3701db212b09e1d"
    );
    let capture = Arc::new(CaptureMetadata::from_raw(&bytes));
    let sensor = Arc::new(luxforge_raw::RawSource::decode(bytes, &AtomicBool::new(false)).unwrap());
    let metadata = sensor.metadata();
    let ledger = luxforge_raw::optical_ledger(metadata);
    assert_eq!(ledger.distortion.status, OpticalStatus::KnownUnapplied);
    assert_eq!(ledger.lateral_ca.status, OpticalStatus::Applied);
    assert_eq!(ledger.shading.status, OpticalStatus::Applied);
    let raw = RawPrepared {
        sensor: sensor.clone(),
        linear: None,
        gains: metadata.as_shot_gains,
        capture,
    };
    let (canonical, signature) = EditorService::request_signature(&path).unwrap();
    let catalog = crate::editor::test_support::temp("dji-neutral-recipe.sqlite");
    let mut service = EditorService::open(&catalog).unwrap();
    // The photograph is brought in as a Develop brings it in, at its Original; what that read of
    // the file is dropped, so the worker's decode below is what its preparation adopts.
    let photograph = service.develop_one(&path).unwrap();
    service.forget_read(&photograph);
    let index = index::LensIndex::parse(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/lensfun/index.json"
        ))
        .unwrap(),
    )
    .unwrap();
    // The index is ready before completion, as the source worker's wait guarantees.
    index::set_for_test(Ok(Arc::new(index)));
    let _index = IndexGuard;
    // The real worker's decoded sensor is adopted through its normal completion boundary. No
    // development is needed: locate compiles geometry and a neutral pick reads only 169 sites.
    let completion = service
        .complete_preparation(Prepared::File(
            PreparedFile {
                asset_id: photograph,
                canonical,
                signature,
                source: PreparedSource::Raw(raw),
                fingerprint: fingerprint.clone(),
            },
            Vec::new(),
        ))
        .unwrap();
    // The Air 2S DNG's distortion is known uncorrected, so its detected FC3411 profile is applied
    // once, as the system entry after the Original, when its first preparation completes.
    let initial = completion.state;
    assert_eq!(initial.revision, 1);
    assert_eq!(
        (
            initial.current_entry.action_id.as_str(),
            initial.current_entry.actor.as_str(),
            initial.current_entry.label.as_str()
        ),
        (
            "select-lens-profile",
            "system",
            "Lens profile FC3411 & compatibles"
        )
    );
    assert_eq!(completion.first_open.len(), 1);
    assert_eq!(
        completion.first_open[0].entry_id.as_ref(),
        Some(&initial.current_entry.id)
    );
    let asset = initial.asset.id;
    let answer = service
        .run_query(
            &asset,
            &initial.current_entry.id,
            "lens-profiles",
            json!({}),
        )
        .unwrap();
    assert_eq!(answer["status"]["state"], "applied");
    assert_eq!(
        answer["status"]["current"]["title"],
        "DJI Air 2S (FC3411) · built-in lens"
    );
    let key = answer["status"]["current"]["key"].clone();
    let same = service
        .apply_action(
            &asset,
            crate::editor::mutation(1, "lens"),
            "select-lens-profile",
            json!({"profile":key}),
        )
        .unwrap();
    assert_eq!(same.outcome, MutationOutcome::NoOp);
    service
        .apply_action(
            &asset,
            crate::editor::mutation(1, "perspective"),
            "set-perspective",
            json!({"horizontal":20,"vertical":-15}),
        )
        .unwrap();
    let state = service.state(&asset).unwrap();
    let map = service
        .transform_entry(&asset, &state.current_entry.id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&map).unwrap()["mapping"]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "the recipe map contains Lens and Perspective; mandatory DNG mapping belongs to the sensor query"
    );
    let geometry: Vec<_> = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .filter(|layer| layer.effect_id != "luxforge.raw")
        .cloned()
        .collect();
    let mut accepted = 0;
    let mut distinguishes_unlocated = false;
    let mut distinguishes_double_inverse = false;
    for (x, y) in [
        (1000, 500),
        (1800, 1200),
        (3500, 2000),
        (4500, 2800),
        (2700, 1800),
        (1200, 2300),
        (4100, 900),
    ] {
        let state = service.state(&asset).unwrap();
        let located = service
            .locate_entry(&asset, &state.current_entry.id, x, y)
            .unwrap();
        let (cx, cy) = map
            .to_content(f64::from(x) + 0.5, f64::from(y) + 0.5)
            .unwrap();
        assert_eq!(
            (located.content_x, located.content_y),
            (cx.floor() as u32, cy.floor() as u32)
        );
        let Ok(expected) = sensor.neutral_gains_at(located.content_x, located.content_y) else {
            continue;
        };
        if let Ok(wrong) = sensor.neutral_gains_at(x, y) {
            distinguishes_unlocated |= wrong != expected;
        }
        if let Ok((dx, dy)) = map.to_content(
            f64::from(located.content_x) + 0.5,
            f64::from(located.content_y) + 0.5,
        ) && let Ok(wrong) = sensor.neutral_gains_at(dx.floor() as u32, dy.floor() as u32)
        {
            distinguishes_double_inverse |= wrong != expected;
        }
        service
            .apply_action(
                &asset,
                crate::editor::mutation(state.revision, &format!("neutral-{accepted}")),
                "pick-raw-neutral",
                json!({"x":located.content_x,"y":located.content_y}),
            )
            .unwrap();
        let picked = service.state(&asset).unwrap();
        let raw: crate::RawPayload = serde_json::from_value(
            picked
                .current_entry
                .snapshot
                .recipe
                .layers
                .iter()
                .find(|layer| layer.effect_id == "luxforge.raw")
                .unwrap()
                .payload
                .clone(),
        )
        .unwrap();
        assert_eq!(
            raw.gains.map(f32::to_bits),
            expected.map(f32::to_bits),
            "the command samples the corrected content point exactly once"
        );
        assert_eq!(
            picked
                .current_entry
                .snapshot
                .recipe
                .layers
                .iter()
                .filter(|layer| layer.effect_id != "luxforge.raw")
                .cloned()
                .collect::<Vec<_>>(),
            geometry
        );
        accepted += 1;
    }
    assert!(
        accepted >= 3,
        "at least three usable authentic sensor patches"
    );
    assert!(
        distinguishes_unlocated,
        "the fixture detects skipping recipe inversion"
    );
    assert!(
        distinguishes_double_inverse,
        "the fixture detects repeating recipe inversion"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap())),
        fingerprint
    );
    drop(service);
    std::fs::remove_file(catalog).unwrap();
}
