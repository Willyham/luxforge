//! Detail's numerical paths and stored-setting journeys through the real host.
use luxforge_core::{
    BASIC_EFFECT, ClientId, DETAIL_EFFECT, LinearSettings, ModuleRegistry, PRESENCE_EFFECT,
    SnapshotId, SourceImage,
};
use luxforge_reference::{
    SplitMix64,
    detail::{self, Image, Params},
    srgb,
};
use luxforge_testbase::paths;
use luxforge_testkit::{
    client::{self, Owner},
    fixtures,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc};

fn source() -> SourceImage {
    let mut rng = SplitMix64(83249);
    let pixels = (0..29 * 17)
        .map(|i| {
            let x = (i % 29) as f64;
            let mean = 0.08 + 0.4 * x / 29.0;
            std::array::from_fn(|c| {
                srgb::code(mean * (1.0 - c as f64 * 0.12) + 0.025 * rng.next_noise())
            })
        })
        .collect::<Vec<_>>();
    fixtures::source_of(29, 17, &pixels)
}

#[test]
fn detail_byte_and_linear_outputs_match_the_reference_and_samples() {
    let registry = ModuleRegistry::builtin();
    let source = source();
    let decoded = source
        .rgba
        .chunks_exact(4)
        .map(|p| std::array::from_fn(|c| srgb::decode(p[c])))
        .collect::<Vec<_>>();
    let linear = fixtures::linear_source_of(source.width, source.height, &decoded);
    for p in [
        Params {
            luminance: 40.0,
            colour: 40.0,
            sharpening: 50.0,
            ..Params::default()
        },
        Params {
            luminance: 100.0,
            colour: 100.0,
            sharpening: 150.0,
            radius: 3.0,
            sharpen_detail: 100.0,
            sharpen_masking: 100.0,
            ..Params::default()
        },
        Params {
            sharpening: 100.0,
            radius: 0.5,
            ..Params::default()
        },
    ] {
        let stack = fixtures::recipe(vec![fixtures::layer(
            DETAIL_EFFECT,
            json!({"sharpening":p.sharpening,"radius":p.radius,"sharpen-detail":p.sharpen_detail,"sharpen-masking":p.sharpen_masking,"luminance":p.luminance,"luminance-detail":p.luminance_detail,"colour":p.colour,"colour-detail":p.colour_detail}),
        )]);
        let reference = detail::apply(&Image::new(29, 17, decoded.clone()), p, [1.0; 2]);
        let frame = fixtures::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        let raw_pixels = decoded
            .iter()
            .map(|p| p.map(|v| f64::from(v as f32)))
            .collect();
        let raw_reference = detail::apply(&Image::new(29, 17, raw_pixels), p, [1.0; 2]);
        let raw = fixtures::render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap();
        for (i, pixel) in reference.pixels.iter().enumerate() {
            for (c, value) in pixel.iter().copied().enumerate() {
                fixtures::assert_code_near_threshold(
                    frame.rgba[4 * i + c],
                    srgb::code(value),
                    value,
                    1e-5,
                    "Detail JPEG",
                );
                let v = raw_reference.pixels[i][c];
                fixtures::assert_code_near_threshold(
                    raw.rgba[4 * i + c],
                    srgb::code(v),
                    v,
                    1e-5,
                    "Detail RAW",
                );
            }
        }
        for (x, y) in [(0, 0), (1, 1), (14, 8), (28, 16), (28, 0), (0, 16)] {
            assert_eq!(
                fixtures::sample(&registry, &source, &stack, x, y)
                    .unwrap()
                    .rgba,
                frame.pixel(x, y)
            );
            assert_eq!(
                fixtures::sample_linear(
                    &registry,
                    &linear,
                    &stack,
                    LinearSettings::default(),
                    x,
                    y
                )
                .unwrap()
                .rgba,
                raw.pixel(x, y)
            );
        }
    }
}

#[test]
fn detail_neutral_and_ancillary_only_share_the_source_buffer() {
    let registry = ModuleRegistry::builtin();
    let source = source();
    for payload in [json!({}), json!({"radius":2.0,"colour-detail":75.0})] {
        let frame = fixtures::render(
            &registry,
            &source,
            SnapshotId::new(),
            &fixtures::recipe(vec![fixtures::layer(DETAIL_EFFECT, payload)]),
        )
        .unwrap();
        assert!(Arc::ptr_eq(&frame.rgba, &source.rgba));
    }
}

#[test]
fn detail_before_basic_and_presence_keeps_sample_full_equality() {
    let registry = ModuleRegistry::builtin();
    let source = source();
    let stack = fixtures::recipe(vec![
        fixtures::layer(
            DETAIL_EFFECT,
            json!({"luminance":40,"colour":40,"sharpening":50}),
        ),
        fixtures::layer(BASIC_EFFECT, json!({"exposure":2.0,"shadows":100})),
        fixtures::layer(PRESENCE_EFFECT, json!({"texture":30,"clarity":20})),
    ]);
    let frame = fixtures::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    for (x, y) in [(0, 0), (14, 8), (28, 16), (28, 0), (0, 16)] {
        assert_eq!(
            fixtures::sample(&registry, &source, &stack, x, y)
                .unwrap()
                .rgba,
            frame.pixel(x, y)
        );
    }
}

struct Session {
    owner: Owner,
    client: ClientId,
    asset: Value,
    directory: PathBuf,
}
impl Session {
    fn open(tag: &str) -> Self {
        let directory = paths::temp_dir(&format!("detail-{tag}"));
        fs::create_dir_all(&directory).unwrap();
        let owner = Owner::start(
            &directory.join("catalog.sqlite"),
            ModuleRegistry::builtin(),
            "detail-test",
        )
        .unwrap();
        let client = owner.client();
        let asset = owner.open(client, &paths::jpeg()).unwrap()["asset"]["id"].clone();
        Self {
            owner,
            client,
            asset,
            directory,
        }
    }
    fn mutate(&self, method: &str, mut parameters: Value) -> Value {
        parameters["asset_id"] = self.asset.clone();
        parameters["mutation"] = client::mutation(
            self.owner.revision(self.client, &self.asset).unwrap(),
            &client::request_id(method),
            "detail-test",
        );
        self.owner.call(self.client, method, parameters).unwrap()
    }
    fn settings(&self) -> Value {
        self.owner
            .call(
                self.client,
                "preset.capture",
                json!({"asset_id":self.asset,"fields":{"set-detail":true}}),
            )
            .unwrap()["settings"]["set-detail"]
            .clone()
    }
    fn close(self) {
        self.owner.close().unwrap();
        fs::remove_dir_all(self.directory).unwrap();
    }
}
fn ancillary() -> Value {
    json!({"sharpening":0.0,"radius":1.7,"sharpen-detail":73.0,"sharpen-masking":22.0,"luminance":0.0,"luminance-detail":62.0,"colour":0.0,"colour-detail":84.0})
}
fn library_mutation(tag: &str) -> Value {
    json!({"request_id":client::request_id(tag),"actor":"detail-test"})
}

#[test]
fn detail_ancillary_commit_is_reported_and_later_activation_uses_its_radius() {
    let s = Session::open("ancillary-activation");
    let registry = ModuleRegistry::builtin();
    let source = luxforge_core::open_source(&paths::jpeg()).unwrap();
    let added = s.mutate("edit.set-detail", json!({"radius":2.2}));
    assert_eq!(added["outcome"], json!("applied"));
    assert!(!added["created_entry_id"].is_null());
    let inactive = s.owner.recipe(s.client, &s.asset).unwrap();
    let held = inactive
        .layers
        .iter()
        .find(|layer| layer.effect_id == DETAIL_EFFECT)
        .unwrap();
    assert_eq!(held.payload, json!({"radius":2.2}));
    let described = s.owner.describe(s.client, &s.asset).unwrap();
    let report = described["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["effect"] == json!(DETAIL_EFFECT))
        .unwrap();
    assert_eq!(report["neutral"], json!(false));
    assert_ne!(report["summary"], json!("Neutral"));
    assert_eq!(report["values"]["radius"], json!(2.2));
    assert_eq!(report["values"]["sharpening"], json!(0.0));
    let identity = fixtures::render(&registry, &source, SnapshotId::new(), &inactive).unwrap();
    assert!(Arc::ptr_eq(&identity.rgba, &source.rgba));

    s.mutate("edit.set-detail", json!({"sharpening":80.0}));
    let active = s.owner.recipe(s.client, &s.asset).unwrap();
    let layer = active
        .layers
        .iter()
        .find(|layer| layer.effect_id == DETAIL_EFFECT)
        .unwrap();
    assert_eq!(layer.id, held.id);
    assert_eq!(layer.payload, json!({"radius":2.2,"sharpening":80.0}));
    let actual = fixtures::render(&registry, &source, SnapshotId::new(), &active).unwrap();
    let render = |payload| {
        fixtures::render(
            &registry,
            &source,
            SnapshotId::new(),
            &fixtures::recipe(vec![fixtures::layer(DETAIL_EFFECT, payload)]),
        )
        .unwrap()
    };
    let configured = render(json!({"sharpening":80.0,"radius":2.2}));
    let default_radius = render(json!({"sharpening":80.0}));
    assert_eq!(actual.rgba, configured.rgba);
    assert_ne!(actual.rgba, default_radius.rgba);
    assert_ne!(actual.rgba, identity.rgba);
    for point in [(0, 0), (source.width / 2, source.height / 2)] {
        assert_eq!(
            s.owner.sample(s.client, &s.asset, point, None).unwrap(),
            json!(configured.pixel(point.0, point.1))
        );
    }
    s.close();
}

#[test]
fn detail_native_preset_round_trips_all_eight_fields_and_zero_strengths() {
    let s = Session::open("native-preset");
    s.mutate("edit.set-detail", ancillary());
    let mask = s.mutate(
        "mask.create-radial",
        json!({"x":0.5,"y":0.5,"radius_x":0.3,"radius_y":0.3,"angle":0.0,"feather":50.0}),
    )["mask"]
        .clone();
    s.mutate(
        "edit.set-detail",
        json!({"mask":mask,"luminance":30,"colour":25,"sharpening":45}),
    );
    let before = s.owner.recipe(s.client, &s.asset).unwrap();
    let masks = before.masks;
    let masked = before
        .layers
        .iter()
        .find(|l| l.mask.is_some())
        .unwrap()
        .clone();
    let captured = s.settings();
    assert_eq!(captured, ancillary());
    let created=s.owner.call(s.client,"preset.create",json!({"name":"Detail ancillary","settings":{"set-detail":captured},"mutation":library_mutation("create")})).unwrap();
    let exported = s
        .owner
        .call(
            s.client,
            "preset.export",
            json!({"preset_id":created["preset"]["id"]}),
        )
        .unwrap();
    let imported=s.owner.call(s.client,"preset.import",json!({"content":exported["content"],"file_name":exported["file_name"],"name":"Detail imported","mutation":library_mutation("import")})).unwrap();
    assert_eq!(imported["preset"]["settings"]["set-detail"], ancillary());
    s.mutate(
        "edit.set-detail",
        json!({"sharpening":80,"luminance":45,"colour":45,"radius":2.8}),
    );
    let head = s.owner.head(s.client, &s.asset).unwrap();
    s.mutate(
        "edit.apply-settings",
        json!({"origin":{"kind":"preset","name":"Detail imported"},"settings":imported["preset"]["settings"]}),
    );
    assert_eq!(s.owner.head(s.client, &s.asset).unwrap(), head + 1);
    assert_eq!(s.settings(), ancillary());
    assert_eq!(s.owner.recipe(s.client, &s.asset).unwrap().masks, masks);
    assert_eq!(
        s.owner
            .recipe(s.client, &s.asset)
            .unwrap()
            .layers
            .iter()
            .find(|l| l.id == masked.id),
        Some(&masked)
    );
    s.close();
}

#[test]
fn detail_history_undo_redo_restore_versions_and_reopen() {
    let s = Session::open("history");
    let original = s.owner.recipe(s.client, &s.asset).unwrap();
    let original_pixel = s.owner.sample(s.client, &s.asset, (0, 0), None).unwrap();
    assert!(!original.layers.iter().any(|l| l.effect_id == DETAIL_EFFECT));
    let added = s.mutate("edit.set-detail", json!({"radius":2.0}));
    let ancillary_recipe = s.owner.recipe(s.client, &s.asset).unwrap();
    let layer = ancillary_recipe
        .layers
        .iter()
        .find(|l| l.effect_id == DETAIL_EFFECT)
        .unwrap()
        .id
        .clone();
    s.owner
        .call(
            s.client,
            "version.create",
            json!({"asset_id":s.asset,"name":"Ancillary","mutation":library_mutation("version")}),
        )
        .unwrap();
    let changed = s.mutate(
        "edit.set-detail",
        json!({"sharpening":60,"luminance":40,"colour":40}),
    );
    let adjusted = s.owner.recipe(s.client, &s.asset).unwrap();
    let adjusted_pixel = s.owner.sample(s.client, &s.asset, (0, 0), None).unwrap();
    assert_eq!(
        adjusted
            .layers
            .iter()
            .find(|l| l.effect_id == DETAIL_EFFECT)
            .unwrap()
            .id,
        layer
    );
    s.mutate("history.undo", json!({}));
    assert_eq!(
        s.owner.recipe(s.client, &s.asset).unwrap(),
        ancillary_recipe
    );
    assert_eq!(
        s.owner.sample(s.client, &s.asset, (0, 0), None).unwrap(),
        original_pixel
    );
    s.mutate("history.redo", json!({}));
    assert_eq!(s.owner.recipe(s.client, &s.asset).unwrap(), adjusted);
    assert_eq!(
        s.owner.sample(s.client, &s.asset, (0, 0), None).unwrap(),
        adjusted_pixel
    );
    s.mutate("edit.reset-detail", json!({}));
    let reset = s.owner.recipe(s.client, &s.asset).unwrap();
    let reset = reset
        .layers
        .iter()
        .find(|l| l.effect_id == DETAIL_EFFECT)
        .unwrap();
    assert_eq!(reset.id, layer);
    assert_eq!(reset.payload, json!({}));
    s.mutate(
        "history.restore",
        json!({"entry_id":changed["created_entry_id"]}),
    );
    assert_eq!(s.owner.recipe(s.client, &s.asset).unwrap(), adjusted);
    // The saved catalog must be released before a new owner opens it.
    s.owner.close().unwrap();
    let reopened = Owner::start(
        &s.directory.join("catalog.sqlite"),
        ModuleRegistry::builtin(),
        "reopened",
    )
    .unwrap();
    let client = reopened.client();
    assert_eq!(reopened.recipe(client, &s.asset).unwrap(), adjusted);
    reopened.prepare(client, &s.asset).unwrap();
    assert_eq!(
        reopened.sample(client, &s.asset, (0, 0), None).unwrap(),
        adjusted_pixel
    );
    let versions = reopened
        .call(client, "version.list", json!({"asset_id":s.asset}))
        .unwrap();
    assert_eq!(
        versions["versions"][0]["entry_id"],
        added["created_entry_id"]
    );
    let restored = reopened
        .call(
            client,
            "history.restore",
            json!({
                "asset_id":s.asset,
                "entry_id":versions["versions"][0]["entry_id"],
                "mutation":client::mutation(
                    reopened.revision(client, &s.asset).unwrap(),
                    &client::request_id("restore-named-detail"),
                    "detail-test"
                )
            }),
        )
        .unwrap();
    assert_ne!(restored["created_entry_id"], changed["created_entry_id"]);
    assert_eq!(reopened.recipe(client, &s.asset).unwrap(), ancillary_recipe);
    assert_eq!(
        reopened.sample(client, &s.asset, (0, 0), None).unwrap(),
        original_pixel
    );
    let restored_settings = reopened
        .call(
            client,
            "preset.capture",
            json!({"asset_id":s.asset,"fields":{"set-detail":true}}),
        )
        .unwrap()["settings"]["set-detail"]
        .clone();
    assert_eq!(restored_settings["radius"], 2.0);
    for strength in ["sharpening", "luminance", "colour"] {
        assert_eq!(restored_settings[strength], 0.0);
    }
    assert_eq!(
        reopened
            .call(client, "version.list", json!({"asset_id":s.asset}))
            .unwrap(),
        versions,
        "restoring a named version preserves its saved reference"
    );
    reopened.close().unwrap();
    fs::remove_dir_all(s.directory).unwrap();
}

#[test]
fn detail_drafts_conflict_and_reapply() {
    let s = Session::open("conflict");
    let agent = s.owner.client();
    let draft = s
        .owner
        .call(
            s.client,
            "draft.begin",
            json!({"asset_id":s.asset,"action":"set-detail"}),
        )
        .unwrap()["draft_id"]
        .clone();
    s.owner
        .call(
            s.client,
            "draft.set",
            json!({"draft_id":draft,"fields":{"sharpening":60,"radius":2.0}}),
        )
        .unwrap();
    s.owner.call(agent,"edit.set-detail",json!({"asset_id":s.asset,"luminance":40,"mutation":client::mutation(0,&client::request_id("external"),"external")})).unwrap();
    let refused=s.owner.refused(s.client,"draft.commit",json!({"draft_id":draft,"mutation":client::mutation(0,&client::request_id("conflict"),"detail-test")})).unwrap();
    assert_eq!(refused.0, "conflict");
    let reapplied = s
        .owner
        .call(s.client, "draft.reapply", json!({"draft_id":draft}))
        .unwrap();
    assert_eq!(reapplied["conflicted"], false);
    s.owner.call(s.client,"draft.commit",json!({"draft_id":draft,"mutation":client::mutation(1,&client::request_id("commit"),"detail-test")})).unwrap();
    let settings = s.settings();
    assert_eq!(settings["sharpening"], 60.0);
    assert_eq!(settings["radius"], 2.0);
    assert_eq!(settings["luminance"], 40.0);
    s.close();
}

#[test]
fn detail_original_hashes_unchanged_and_exact_export_uses_the_saved_entry() {
    let before = fs::read(paths::jpeg()).unwrap();
    let s = Session::open("export");
    let entry = s.mutate(
        "edit.set-detail",
        json!({"luminance":40,"colour":40,"sharpening":50}),
    )["created_entry_id"]
        .clone();
    let exported=s.owner.call(s.client,"export.jpeg",json!({"asset_id":s.asset,"entry_id":entry,"destination":s.directory.join("detail.jpg"),"mutation":library_mutation("export")})).unwrap();
    s.mutate("edit.set-detail", json!({"sharpening":90}));
    let job = client::settle(&s.owner, s.client, &exported["job_id"]).unwrap();
    assert_eq!(job["status"], "ready");
    assert!(s.directory.join("detail.jpg").exists());
    assert_eq!(exported["entry_id"], entry);
    assert_eq!(fs::read(paths::jpeg()).unwrap(), before);
    s.close();
}

#[test]
fn detail_missing_provider_refuses_affected_outputs_and_keeps_data() {
    let s = Session::open("missing");
    s.mutate("edit.set-detail", json!({"luminance":40,"colour":40}));
    let recipe = s.owner.recipe(s.client, &s.asset).unwrap();
    s.owner.close().unwrap();
    let missing = Owner::start(
        &s.directory.join("catalog.sqlite"),
        client::registry_without("luxforge.detail").unwrap(),
        "missing",
    )
    .unwrap();
    let c = missing.client();
    missing.prepare(c, &s.asset).unwrap();
    assert_eq!(missing.recipe(c, &s.asset).unwrap(), recipe);
    for (method, params) in [
        ("render.sample", json!({"asset_id":s.asset,"x":0,"y":0})),
        (
            "export.jpeg",
            json!({"asset_id":s.asset,"destination":s.directory.join("missing.jpg"),"mutation":library_mutation("missing-export")}),
        ),
    ] {
        let error = missing.refused(c, method, params).unwrap();
        assert!(error.1.contains("luxforge.detail"), "{method}: {error:?}");
    }
    let analysis = missing
        .analyse(c, &s.asset, json!({"kind":"current"}))
        .unwrap();
    assert_eq!(analysis["status"], "failed");
    assert!(analysis.get("result").is_none());
    assert_eq!(missing.recipe(c, &s.asset).unwrap(), recipe);
    missing.close().unwrap();
    fs::remove_dir_all(s.directory).unwrap();
}

#[test]
fn detail_lightroom_keys_report_not_mapped_to_detail() {
    let registry = ModuleRegistry::builtin();
    let input = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Sharpness="40" crs:LuminanceSmoothing="20"/></rdf:RDF></x:xmpmeta>"#;
    let inspected = luxforge_core::inspect_preset(input, Some("detail.xmp"), &registry).unwrap();
    let reasons = inspected
        .report
        .unsupported
        .iter()
        .map(|r| r.reason.as_deref().unwrap())
        .collect::<Vec<_>>();
    assert!(reasons.contains(&"Lightroom sharpening is not mapped to Luxforge Detail"));
    assert!(reasons.contains(&"Lightroom noise reduction is not mapped to Luxforge Detail"));
    assert!(inspected.settings.is_empty());
}

/// Run with LUXFORGE_RAW_FIXTURE naming one authentic qualified NEF, RAF or DNG. This checks
/// development identity and the moving proxies without rendering a full photo again.
#[test]
#[ignore = "requires an authentic RAW fixture; run explicitly with LUXFORGE_RAW_FIXTURE"]
fn detail_raw_white_balance_invalidates_and_sliders_do_not_redevelop() {
    use luxforge_core::{
        Draft, EditorService, ErrorKind, LinearImage, Mutation, PreviewIntent, PreviewJob,
        PreviewPhase, PreviewQueue, PreviewResult, PreviewSource, ProxyBounds, RawPayload,
        SourceKind,
    };
    use sha2::{Digest, Sha256};

    fn image(job: &PreviewJob) -> &LinearImage {
        let PreviewSource::Raw { image, settings } = job.evaluation.source() else {
            panic!("an authentic RAW must use the linear source");
        };
        assert!(
            settings.white_balance.is_none(),
            "committed development is exact"
        );
        image
    }
    fn moving(queue: &mut PreviewQueue, mut job: PreviewJob) -> PreviewResult {
        let expected_entry = job.identity.entry_id.clone();
        job.intent = PreviewIntent::Interactive;
        let generation = queue.request(job);
        let result = luxforge_testbase::wait_for("a RAW Detail moving proxy", || queue.poll());
        assert_eq!(result.generation, generation);
        assert_eq!(result.entry_id, expected_entry);
        assert_eq!(result.phase(), PreviewPhase::Proxy);
        assert!(result.raster().is_ok());
        result
    }

    let path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("RAW fixture path"));
    let original_hash = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
    let directory = paths::temp_dir("detail-raw-development");
    let mut service = EditorService::open(&directory.join("catalog.sqlite")).unwrap();
    let imported = service.import(&path).unwrap();
    assert!(matches!(imported.asset.source, SourceKind::Raw { .. }));
    assert_eq!(imported.asset.fingerprint, original_hash);
    let asset = imported.asset.id;
    let source_layer = imported.current_entry.snapshot.recipe.layers[0].clone();
    let as_shot: RawPayload = serde_json::from_value(source_layer.payload.clone()).unwrap();
    let initial_job = service.preview_job(&asset, None, None, None, None).unwrap();
    // PartialEq includes the process-unique development identity, not merely pixel equality:
    // every adoption of newly developed planes receives a different identity.
    let original_development = image(&initial_job).clone();
    drop(initial_job);

    for fields in [
        json!({"luminance":40,"colour":40,"sharpening":50}),
        json!({"sharpening":75,"radius":2.1}),
        json!({"luminance-detail":75,"colour-detail":65}),
    ] {
        let state = service.state(&asset).unwrap();
        service
            .apply_action(
                &asset,
                Mutation {
                    expected_revision: state.revision,
                    request_id: client::request_id("raw-detail-slider"),
                    actor: "detail-test".into(),
                },
                "set-detail",
                fields,
            )
            .unwrap();
        let state = service.state(&asset).unwrap();
        assert_eq!(state.current_entry.snapshot.recipe.layers[0], source_layer);
        let job = service.preview_job(&asset, None, None, None, None).unwrap();
        assert!(
            image(&job) == &original_development,
            "Detail reused the held development"
        );
    }

    let state = service.state(&asset).unwrap();
    let detail_layer = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|layer| layer.effect_id == DETAIL_EFFECT)
        .unwrap()
        .clone();
    let bounds = Some(ProxyBounds {
        width: 96,
        height: 96,
    });
    let mut queue = PreviewQueue::default();
    let before = moving(
        &mut queue,
        service
            .preview_job(&asset, None, None, None, bounds)
            .unwrap(),
    );
    let reused = moving(
        &mut queue,
        service
            .preview_job(&asset, None, None, None, bounds)
            .unwrap(),
    );
    assert_eq!(before.raster().unwrap().rgba, reused.raster().unwrap().rgba);

    let mut detail_draft = Draft::new("set-detail", asset.clone(), state.revision);
    detail_draft.merge(json!({"sharpening":90}).as_object().unwrap().clone());
    let drafted_detail = service
        .preview_job(&asset, None, None, Some(&detail_draft), bounds)
        .unwrap();
    assert!(image(&drafted_detail) == &original_development);
    let detail_result = moving(&mut queue, drafted_detail);
    assert!(!detail_result.approximate_white_balance);

    let [kelvin, _] = as_shot.white_balance_controls();
    let temperature = if kelvin < 7000.0 { 9500.0 } else { 3000.0 };
    let mut wb_draft = Draft::new("set-raw", asset.clone(), state.revision);
    wb_draft.merge(
        json!({"temperature":temperature})
            .as_object()
            .unwrap()
            .clone(),
    );
    let drafted_wb = service
        .preview_job(&asset, None, None, Some(&wb_draft), bounds)
        .unwrap();
    let PreviewSource::Raw {
        image: held,
        settings,
    } = drafted_wb.evaluation.source()
    else {
        panic!("RAW white balance uses the held development");
    };
    assert!(held == &original_development);
    assert!(settings.white_balance.is_some());
    let wb_result = moving(&mut queue, drafted_wb);
    assert!(wb_result.approximate_white_balance);

    service
        .apply_action(
            &asset,
            Mutation {
                expected_revision: state.revision,
                request_id: client::request_id("detail-raw-wb"),
                actor: "detail-test".into(),
            },
            "set-raw",
            json!({"temperature":temperature}),
        )
        .unwrap();
    let changed = service.state(&asset).unwrap();
    assert_ne!(
        changed.current_entry.snapshot.recipe.layers[0],
        source_layer
    );
    assert_eq!(
        changed
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .find(|l| l.effect_id == DETAIL_EFFECT),
        Some(&detail_layer)
    );
    let refused = service
        .preview_job(&asset, None, None, None, bounds)
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::PreparationRequired);
    let Some(luxforge_core::Preparation::Needs(needs)) = refused.preparation.as_deref() else {
        panic!("the white-balance refusal must name its preparation");
    };
    assert_eq!(needs.entry_id, changed.current_entry.id);
    assert!(needs.gains.is_some());
    service.prepare(needs).unwrap();
    let prepared = service
        .preview_job(&asset, None, None, None, bounds)
        .unwrap();
    assert!(
        image(&prepared) != &original_development,
        "white balance adopts a new development"
    );
    let exact_wb = moving(&mut queue, prepared);
    assert!(!exact_wb.approximate_white_balance);
    assert_ne!(
        before.raster().unwrap().rgba,
        exact_wb.raster().unwrap().rgba
    );
    drop(original_development);
    drop(queue);
    drop(service);
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
        original_hash
    );
    fs::remove_dir_all(directory).unwrap();
}
