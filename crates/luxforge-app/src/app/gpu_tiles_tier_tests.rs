//! A developed photograph's catalog tiers drawn by the desktop's GPU tile worker on this host's
//! adapter, through a catalog owner's preview lane as the desktop's launch hands it the worker
//! (`docs/design/catalog.md`, "Rendered previews"), against the reference's tiers of the same
//! entry from an owner with no GPU provider:
//!
//! - **Within the display limit.** Each grid and large tier the GPU draws, decoded, is within its
//!   class's display limit of the reference's tier decoded, over every pixel: a colour stack by
//!   the pointwise limits and Presence by the spatial limits, on a generated photograph whose tiers
//!   are both reduced and on the corpus's Presence fixture, whose large tier is its whole stage;
//!   and, with the corpus RAWs, on the Nikon Z 6 and the Air 2S. Each names the GPU, in its job's
//!   result and in the read answered from its row.
//! - **Without a GPU, by name.** A launch that refused the GPU and one whose window has not named
//!   its adapter draw the reference's tiers, byte for byte, naming `refused` and `surface-pending`,
//!   and their worker starts nothing.
//! - **Batch export names each file's renderer.** Through the worker every file names the GPU;
//!   through a launch that refused it, the reference naming `refused`.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing: it is not GPU
//! evidence.
use super::{
    gpu_tiles::GpuTiles, gpu_tiles_tests::host_adapter, tasks::call as owner_call,
    testing::import_and_adopt,
};
use luxforge_core::{
    AssetId, ClientId, EditorService, HostConfig, ModuleRegistry, Mutation, OwnerHandle, Raster,
    RendererRecord, catalog_types::RenderedBy, qualification, tiles::ReferenceTiles,
};
use luxforge_reference::preview_error::{Class, Rgb8, Statistics, compare, verdict};
use luxforge_testbase::paths;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

/// A catalog owner over one photograph, copied into a directory of its own and developed, whose
/// preview lane draws through `tiles`, or the reference's service when there is none.
struct Tiers {
    owner: OwnerHandle,
    join: Option<std::thread::JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
    asset: AssetId,
}

impl Tiers {
    fn new(name: &str, tiles: Option<Arc<GpuTiles>>, original: &Path) -> Self {
        let dir = paths::temp_dir(&format!("gpu-tiers-{name}"));
        let copied = dir.join(original.file_name().expect("a file name"));
        std::fs::copy(original, &copied).expect("the original is copied");
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                tiles: tiles.map(|tiles| tiles as Arc<dyn luxforge_core::tiles::TileService>),
                ..HostConfig::unconfigured()
            },
        )
        .expect("a catalog owner");
        let client = owner.register();
        let asset = import_and_adopt(&owner, client, &copied);
        Self {
            owner,
            join: Some(join),
            client,
            dir,
            asset,
        }
    }

    fn call(&self, method: &str, params: Value) -> Value {
        owner_call(&self.owner, self.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
            .0
    }

    /// Commit `method` with `params` as the asset's next entry.
    fn edit(&self, method: &str, mut params: Value) {
        let state = self.call("asset.state", json!({"asset_id": self.asset}));
        let revision = state["revision"].clone();
        params["asset_id"] = json!(self.asset);
        params["mutation"] = json!({
            "expected_revision": revision,
            "request_id": format!("{method}-{revision}"),
            "actor": "test",
        });
        self.call(method, params);
    }

    /// The job's record once it has ended, read through `job.wait`, which holds until it changes.
    fn settled(&self, job: &Value) -> Value {
        let mut after = Value::Null;
        loop {
            let waited = self.call("job.wait", json!({"job_id": job, "after": after}));
            let record = &waited["job"];
            if !matches!(record["status"].as_str(), Some("queued" | "running")) {
                return record.clone();
            }
            after = waited["change"].clone();
        }
    }

    /// Both tiers of the current entry, read until ready: each tier's preview and its JPEG, and
    /// how long the reads took until both were ready.
    fn tiers(&self) -> ([(Value, Vec<u8>); 2], Duration) {
        let started = Instant::now();
        let read = |tier: &str| {
            self.call(
                "preview.read",
                json!({"item": {"kind": "photo", "asset_id": self.asset}, "tier": tier,
                       "priority": "visible"}),
            )
        };
        let asked = [read("grid"), read("large")];
        for answer in &asked {
            if answer["state"] != "ready" {
                let record = self.settled(&answer["job_id"]);
                assert_eq!(record["status"], "ready", "{record}");
            }
        }
        let elapsed = started.elapsed();
        let tiers = ["grid", "large"].map(|tier| {
            let answer = read(tier);
            assert_eq!(answer["state"], "ready", "{answer}");
            let preview = answer["preview"].clone();
            let bytes = std::fs::read(preview["path"].as_str().expect("a path")).expect("a tier");
            (preview, bytes)
        });
        (tiers, elapsed)
    }
}

impl Drop for Tiers {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `candidate` against `reference`, two tiers' JPEGs each decoded independently, by the preview
/// error statistics over every pixel.
fn decoded_against(candidate: &[u8], reference: &[u8]) -> Statistics {
    let decode = |bytes: &[u8]| {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
            .expect("a tier decodes")
            .to_rgb8()
    };
    let (candidate, reference) = (decode(candidate), decode(reference));
    assert_eq!(candidate.dimensions(), reference.dimensions());
    let (width, height) = reference.dimensions();
    compare(
        Rgb8::new(width, height, candidate.as_raw()).unwrap(),
        Rgb8::new(width, height, reference.as_raw()).unwrap(),
        [0, 0, width, height],
    )
    .expect("the statistics")
}

/// `original` written as a JPEG at quality 95 into a scratch directory of its own.
fn written(name: &str, photograph: &image::RgbImage) -> PathBuf {
    let path = paths::temp_dir(&format!("gpu-tiers-original-{name}")).join(format!("{name}.jpg"));
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    image::codecs::jpeg::JpegEncoder::new_with_quality(file, 95)
        .encode_image(photograph)
        .expect("the photograph is written");
    path
}

/// A generated photograph of `width` × `height`: gradients, a hard diagonal edge, fine texture
/// and a flat grey patch.
fn generated(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        if x < width / 6 && y > height * 2 / 3 {
            return image::Rgb([128, 128, 128]);
        }
        let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
        let texture = 18.0 * ((x as f32 * 0.9).sin() * (y as f32 * 0.7).cos());
        let edge = if u > 0.6 + (v - 0.5) * 0.4 { 40.0 } else { 0.0 };
        let code = |base: f32| (base + texture + edge).clamp(0.0, 255.0).round() as u8;
        image::Rgb([
            code(30.0 + 190.0 * u),
            code(40.0 + 170.0 * v),
            code(200.0 - 150.0 * u * v),
        ])
    })
}

/// The corpus's Presence fixture as its generator draws it: a smooth gradient, a hard step edge,
/// a low-amplitude checker and a flat grey, one in each quadrant.
fn presence_fixture() -> image::RgbImage {
    let (width, height) = (1440u32, 960u32);
    let (hw, hh) = (width / 2, height / 2);
    image::RgbImage::from_fn(width, height, |x, y| {
        let level = match (x < hw, y < hh) {
            (true, true) => (40.0 + x as f32 / hw as f32 * (255.0 - 40.0)).round() as u8,
            (false, true) if x - hw < hw / 2 => 70,
            (false, true) => 210,
            (true, false) if (x / 4 + (y - hh) / 4).is_multiple_of(2) => 118,
            (true, false) => 138,
            (false, false) => 128,
        };
        image::Rgb([level; 3])
    })
}

/// `candidate` against `reference`, two tiers as the render worker draws them before encoding,
/// by the preview error statistics over every pixel.
fn against(candidate: &Raster, reference: &Raster) -> Statistics {
    assert_eq!(
        (candidate.width, candidate.height),
        (reference.width, reference.height)
    );
    let rgb = |raster: &Raster| -> Vec<u8> {
        raster
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect()
    };
    let (candidate_rgb, reference_rgb) = (rgb(candidate), rgb(reference));
    let (width, height) = (reference.width, reference.height);
    compare(
        Rgb8::new(width, height, &candidate_rgb).unwrap(),
        Rgb8::new(width, height, &reference_rgb).unwrap(),
        [0, 0, width, height],
    )
    .expect("the statistics")
}

/// `original` imported into a new catalog of its own, as the editor service a catalog owner holds.
fn service_of(name: &str, original: &Path) -> (EditorService, AssetId) {
    let mut service =
        EditorService::open(&paths::temp_catalog(&format!("gpu-tiers-{name}"))).unwrap();
    let asset = service.import(original).unwrap().asset.id;
    (service, asset)
}

/// After each of `edits` in turn, both tiers of the photograph's current entry drawn by the GPU
/// tile worker `worker`, before encoding, against the reference's: each within its class's
/// display limit over every pixel, and drawn by the GPU. The time each took to draw both tiers is
/// printed as an indication only.
fn held_to_the_reference(
    test: &str,
    name: &str,
    original: &Path,
    worker: &GpuTiles,
    edits: &[(&str, Value, Class)],
) {
    let (mut service, asset) = service_of(name, original);
    let mut revision = service.state(&asset).unwrap().revision;
    for (action, params, class) in edits {
        revision = service
            .apply_action(
                &asset,
                Mutation {
                    expected_revision: revision,
                    request_id: format!("{action}-{revision}"),
                    actor: "test".into(),
                },
                action,
                params.clone(),
            )
            .unwrap()
            .revision;
        let started = Instant::now();
        let (gpu, drawn) = qualification::photo_tiers(&service, &asset, worker).unwrap();
        let gpu_time = started.elapsed();
        let started = Instant::now();
        let (reference, by) =
            qualification::photo_tiers(&service, &asset, &ReferenceTiles::new()).unwrap();
        let reference_time = started.elapsed();
        assert_eq!(drawn, RenderedBy::gpu(), "{name}, {action}");
        assert_eq!(by.record, RendererRecord::Reference);
        for ((tier, candidate), expected) in ["grid", "large"].iter().zip(&gpu).zip(&reference) {
            let statistics = against(candidate, expected);
            eprintln!(
                "{test}: {name}, {action}, the {tier} tier ({}x{}) against the reference's: \
                 {statistics:?}",
                candidate.width, candidate.height
            );
            assert!(
                verdict(&statistics, *class).passed(),
                "{name}, {action}, {tier}: {statistics:?}"
            );
        }
        eprintln!(
            "{test}: {name}, {action}: both tiers drawn on the GPU in {gpu_time:?}, on the \
             reference in {reference_time:?}, each from its own preparation of the original (an \
             indication only, not a measurement)"
        );
    }
}

/// Each grid and large tier the GPU tile worker draws is within its class's display limit of the
/// reference's tier of the same entry, before encoding, over every pixel, after a Basic edit
/// (pointwise) and after Presence (spatial), on a generated 2600 × 1700 photograph whose tiers are
/// both reduced and on the corpus's Presence fixture, whose large tier is its whole 1440 × 960
/// stage.
#[test]
fn a_gpu_tier_is_within_the_display_limit_of_the_reference_tier() {
    let test = "a_gpu_tier_is_within_the_display_limit_of_the_reference_tier";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    for (name, photograph) in [
        ("generated", generated(2600, 1700)),
        ("presence", presence_fixture()),
    ] {
        let original = written(name, &photograph);
        let worker = GpuTiles::new(Some(adapter.clone()), false);
        held_to_the_reference(
            test,
            name,
            &original,
            &worker,
            &[
                (
                    "set-basic",
                    json!({"exposure": 0.6, "contrast": 25, "saturation": 15}),
                    Class::Pointwise,
                ),
                (
                    "set-presence",
                    json!({"texture": 30, "clarity": 25, "dehaze": 20}),
                    Class::Spatial,
                ),
            ],
        );
        let figures = worker.figures();
        assert_eq!(figures.streams, 2, "one stream a render: {figures:?}");
        assert_eq!(figures.references, 0, "{figures:?}");
    }
}

/// The corpus's Nikon Z 6 and Air 2S RAWs, edited with Basic and then Presence: each tier the GPU
/// tile worker draws is within its class's display limit of the reference's, before encoding.
/// The time each took to draw both tiers is printed as an indication only.
///
/// `LUXFORGE_RAW_MANIFEST=MANIFEST cargo test --release -p luxforge-app --bin luxforge
/// a_gpu_tier_of_a_raw -- --ignored --nocapture`
#[test]
#[ignore = "the corpus RAWs: set LUXFORGE_RAW_MANIFEST to the private RAW manifest"]
fn a_gpu_tier_of_a_raw_is_within_the_display_limit_of_the_reference_tier() {
    let test = "a_gpu_tier_of_a_raw_is_within_the_display_limit_of_the_reference_tier";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("LUXFORGE_RAW_MANIFEST").expect("a manifest"))
            .unwrap(),
    )
    .unwrap();
    for id in ["nikon-z6", "dji-air2s"] {
        let path = manifest["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["id"] == id)
            .and_then(|source| source["path"].as_str())
            .map(PathBuf::from)
            .expect("the RAW in the manifest");
        let worker = GpuTiles::new(Some(adapter.clone()), false);
        held_to_the_reference(
            test,
            id,
            &path,
            &worker,
            &[
                (
                    "set-basic",
                    json!({"exposure": 0.3, "contrast": 20}),
                    Class::Pointwise,
                ),
                (
                    "set-presence",
                    json!({"clarity": 30, "dehaze": 15}),
                    Class::Spatial,
                ),
            ],
        );
    }
}

/// Through a catalog owner's preview lane, as the desktop's launch hands it the GPU tile worker,
/// a photograph's grid and large tiers are drawn by the GPU: each names it, in its job's result
/// and in the read answered from its row, at the reference's tiers' sizes. Their JPEGs, decoded,
/// are compared with the reference's for the record only: the display limit is held before
/// encoding (above), since two encodings of pixels a code apart can differ by a 16 × 16 block.
#[test]
fn the_preview_lane_draws_a_photographs_tiers_on_the_gpu() {
    let test = "the_preview_lane_draws_a_photographs_tiers_on_the_gpu";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    let original = written("lane", &generated(2600, 1700));
    let worker = Arc::new(GpuTiles::new(Some(adapter), false));
    let gpu = Tiers::new("lane-gpu", Some(Arc::clone(&worker)), &original);
    let reference = Tiers::new("lane-reference", None, &original);
    for tiers in [&gpu, &reference] {
        tiers.edit("edit.set-basic", json!({"exposure": 0.6, "contrast": 25}));
    }
    let (drawn, _) = gpu.tiers();
    let (expected, _) = reference.tiers();
    for ((preview, bytes), (expected, expected_bytes)) in drawn.iter().zip(&expected) {
        assert_eq!(
            preview["renderer"],
            json!({"record": "gpu", "reason": null}),
            "{preview}"
        );
        assert_eq!(
            expected["renderer"],
            json!({"record": "reference", "reason": null})
        );
        assert_eq!(
            (&preview["width"], &preview["height"]),
            (&expected["width"], &expected["height"])
        );
        eprintln!(
            "{test}: the {} tier's JPEG against the reference's, decoded (for the record): {:?}",
            preview["tier"],
            decoded_against(bytes, expected_bytes)
        );
    }
    let figures = worker.figures();
    assert!(figures.streams >= 1 && figures.bands >= 1, "{figures:?}");
}

/// A launch that refused the GPU (`--no-gpu-render`) and one whose window has not named its
/// adapter yet draw the reference's tiers, byte for byte, naming `refused` and `surface-pending`;
/// neither worker starts a thread or opens a device. Neither needs an adapter on this host.
#[test]
fn a_no_gpu_launch_draws_the_reference_tiers_naming_why() {
    let original = written("no-gpu", &generated(900, 600));
    let reference = Tiers::new("no-gpu-reference", None, &original);
    reference.edit("edit.set-basic", json!({"exposure": 0.5}));
    let (expected, _) = reference.tiers();
    for (refused, reason) in [(true, "refused"), (false, "surface-pending")] {
        let worker = Arc::new(GpuTiles::pending(refused));
        let tiers = Tiers::new(reason, Some(Arc::clone(&worker)), &original);
        tiers.edit("edit.set-basic", json!({"exposure": 0.5}));
        let (drawn, _) = tiers.tiers();
        for ((preview, bytes), (_, expected)) in drawn.iter().zip(&expected) {
            assert_eq!(
                preview["renderer"],
                json!({"record": "reference", "reason": reason}),
                "{preview}"
            );
            assert!(bytes == expected, "{reason}: the reference's own tier");
        }
        assert!(!worker.started(), "{reason}: nothing started");
        assert_eq!(worker.figures().adapter, None, "{reason}: nothing opened");
    }
}

/// A batch export names each file's renderer as a single export does: through the GPU worker,
/// the GPU; through a launch that refused it, the reference naming `refused`.
#[test]
fn a_batch_export_names_each_files_renderer() {
    let test = "a_batch_export_names_each_files_renderer";
    let original = written("batch", &generated(600, 400));
    let mut workers = vec![(
        Arc::new(GpuTiles::pending(true)),
        json!({"record": "reference", "reason": "refused"}),
    )];
    if let Some(adapter) = host_adapter(test) {
        workers.push((
            Arc::new(GpuTiles::new(Some(adapter), false)),
            json!({"record": "gpu", "reason": null}),
        ));
    }
    for (worker, renderer) in workers {
        let tiers = Tiers::new("batch", Some(worker), &original);
        tiers.edit("edit.set-basic", json!({"exposure": 0.3}));
        let out = tiers.dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let started = tiers.call(
            "batch.export",
            json!({"targets": {"kind": "assets", "asset_ids": [tiers.asset]}, "destination": out,
                   "mutation": {"request_id": "batch", "actor": "test"}}),
        );
        let record = tiers.settled(&started["job_id"]);
        assert_eq!(record["status"], "ready", "{record}");
        assert_eq!(
            record["result"]["written"],
            json!([{"asset_id": tiers.asset, "path": out.join("batch-edited.jpg"), "renderer": renderer}]),
            "{record}"
        );
    }
}
