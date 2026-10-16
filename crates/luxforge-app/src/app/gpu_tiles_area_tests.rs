//! The tile service's answer to `mask.sample-input-area` on this host's adapter, through a catalog
//! owner as the desktop's launch hands it the GPU tile worker, against the same query through an
//! owner holding the reference renderer's service.
//!
//! A range mask's area read is the input of the operation the mask modulates, at its input site
//! (`docs/design/range-masks.md#the-input-site`): for a mask no layer is bound to, the position a
//! first Basic adjustment through it would take; for a bound one, its first bound layer. Both are
//! read at a click's size (one pixel, no reduction) and over the whole frame (block means at the
//! reduction that leaves at most 4,096 points), over a pointwise stack and a Detail-led one, and
//! every answer is held to the reference's by the display limit of the stack's class, its stage,
//! rectangle, reduction and size equal and its renderer the GPU.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing: it is not GPU
//! evidence.
use super::{
    gpu_plan::install_output_encoding, gpu_tiles::GpuTiles, gpu_tiles_tests::host_adapter,
    tasks::call as owner_call, testing::import_and_adopt,
};
use luxforge_core::{
    AreaInput, AssetId, ClientId, HostConfig, ModuleRegistry, OwnerHandle, tiles::TileService,
};
use luxforge_reference::preview_error::{Class, ciede2000, lab_from_srgb8, statistics_of};
use luxforge_test_macros::slow;
use luxforge_testbase::paths;
use serde_json::{Value, json};
use std::sync::Arc;

/// The generated photograph's size: a whole-frame read reduces it by 16, to 57 × 38 points.
const WIDTH: u32 = 900;
const HEIGHT: u32 = 600;

/// A catalog owner over one imported photograph, its tiles answered by `tiles` (the reference's
/// service when `None`), with the range mask experiment forced on so the masks can be created.
struct Host {
    owner: OwnerHandle,
    join: Option<std::thread::JoinHandle<()>>,
    client: ClientId,
    asset: AssetId,
    dir: std::path::PathBuf,
}

impl Host {
    fn start(
        name: &str,
        tiles: Option<Arc<dyn TileService>>,
        photograph: &image::RgbImage,
    ) -> Self {
        let dir = paths::temp_dir(&format!("gpu-area-{name}"));
        let original = dir.join("original.jpg");
        let file = std::io::BufWriter::new(std::fs::File::create(&original).unwrap());
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 95)
            .encode_image(photograph)
            .expect("the photograph is written");
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                tiles,
                launch_flags: crate::app::testing::range_masks_config().launch_flags,
                ..HostConfig::unconfigured()
            },
        )
        .expect("a catalog owner");
        let client = owner.register();
        let asset = import_and_adopt(&owner, client, &original);
        Self {
            owner,
            join: Some(join),
            client,
            asset,
            dir,
        }
    }

    /// Commit `method` with `params` as the asset's next entry, answering its result.
    fn edit(&self, method: &str, mut params: Value) -> Value {
        let (state, _) = owner_call(
            &self.owner,
            self.client,
            "asset.state",
            json!({"asset_id": self.asset}),
        )
        .expect("the asset's state");
        let revision = state["revision"].clone();
        params["asset_id"] = json!(self.asset);
        params["mutation"] = json!({
            "expected_revision": revision,
            "request_id": format!("{method}-{revision}"),
            "actor": "test",
        });
        owner_call(&self.owner, self.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
            .0
    }

    /// `mask.sample-input-area` of `mask`, an identity, over `rect` (`x`, `y`, `width`, `height`).
    fn area(&self, mask: &Value, rect: [f64; 4]) -> (AreaInput, Value) {
        let answer = self.answer(mask, rect);
        let area = serde_json::from_value(answer.clone()).expect("an area answer");
        (area, answer)
    }

    /// The same answer as it is sent, which an [`AreaInput`] reads back only with a renderer a
    /// session carries.
    fn answer(&self, mask: &Value, rect: [f64; 4]) -> Value {
        let [x, y, width, height] = rect;
        let (answer, _) = owner_call(
            &self.owner,
            self.client,
            "mask.sample-input-area",
            json!({"asset_id": self.asset, "mask": mask,
                   "x": x, "y": y, "width": width, "height": height}),
        )
        .unwrap_or_else(|error| panic!("{mask} over {rect:?}: {error}"));
        answer
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A generated photograph with something for both range kinds to select: smooth gradients in every
/// channel, a hard diagonal edge, fine texture for Detail to act on and a flat grey patch.
fn photograph() -> image::RgbImage {
    image::RgbImage::from_fn(WIDTH, HEIGHT, |x, y| {
        if x < WIDTH / 6 && y > HEIGHT * 2 / 3 {
            return image::Rgb([128, 128, 128]);
        }
        let (u, v) = (x as f32 / WIDTH as f32, y as f32 / HEIGHT as f32);
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

/// A linear value's output code by the core's own thresholds, clamped to `[0, 1]`.
fn code(value: f64) -> u8 {
    let value = (value as f32).clamp(0.0, 1.0);
    luxforge_core::colour::srgb::output_thresholds()
        .partition_point(|threshold| *threshold <= value) as u8
}

/// Global edits committed before the masks are created: each a method and its parameters.
type Steps = Vec<(&'static str, Value)>;

/// The stacks the masks are read over, each with the class its limits come from: global steps
/// committed before the masks are created.
fn stacks() -> Vec<(&'static str, Class, Steps)> {
    vec![
        (
            "pointwise",
            Class::Pointwise,
            vec![(
                "edit.set-basic",
                json!({"exposure": 0.5, "contrast": 25.0, "shadows": 30.0, "saturation": 15.0}),
            )],
        ),
        (
            "detail-led",
            Class::Spatial,
            vec![
                (
                    "edit.set-detail",
                    json!({"luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0}),
                ),
                (
                    "edit.set-basic",
                    json!({"exposure": 0.5, "contrast": 25.0, "saturation": 15.0}),
                ),
            ],
        ),
    ]
}

/// The area reads, by name: one pixel near the middle, a click's size (a quarter of pixel
/// (333, 246) from inside it, so no rounding of the fraction reaches a neighbour), and the whole
/// frame.
fn reads() -> [(&'static str, [f64; 4]); 2] {
    let (width, height) = (f64::from(WIDTH), f64::from(HEIGHT));
    [
        (
            "click",
            [333.25 / width, 246.25 / height, 0.5 / width, 0.5 / height],
        ),
        ("whole frame", [0.0, 0.0, 1.0, 1.0]),
    ]
}

/// What the GPU tile worker answers `mask.sample-input-area` with, for bound and unbound luminance
/// and colour range masks at a click's size and over the whole frame, is within its stack's
/// display limit of the reference's answer, over the same stage, rectangle and reduction, and
/// names the GPU; and binding a first Basic adjustment through the first mask leaves the GPU's
/// answer for it as it was, since the layer lands at the unbound mask's input site.
#[test]
#[slow]
fn the_gpu_area_read_of_a_range_mask_is_within_the_limit_of_the_reference() {
    let test = "the_gpu_area_read_of_a_range_mask_is_within_the_limit_of_the_reference";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let tiles: Arc<dyn TileService> = Arc::new(GpuTiles::new(Some((backend, name.clone())), false));
    let photograph = photograph();
    let mut judged = 0;
    for (stack, class, steps) in stacks() {
        let gpu = Host::start(
            &format!("{stack}-gpu"),
            Some(Arc::clone(&tiles)),
            &photograph,
        );
        let reference = Host::start(&format!("{stack}-reference"), None, &photograph);
        // Each host's two masks, by identity: Mask 1 a luminance range, Mask 2 a colour range.
        let mut created = Vec::new();
        for host in [&gpu, &reference] {
            for (method, params) in &steps {
                host.edit(method, params.clone());
            }
            let luminance = host.edit(
                "mask.create-luminance-range",
                json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
            );
            let colour = host.edit("mask.create-colour-range", json!({"refine": 50.0}));
            created.push([luminance["mask"].clone(), colour["mask"].clone()]);
        }
        let mut unbound = Vec::new();
        for binding in ["unbound", "bound"] {
            if binding == "bound" {
                for (host, [first, second]) in [&gpu, &reference].into_iter().zip(&created) {
                    host.edit(
                        "edit.set-basic",
                        json!({"mask": first, "exposure": -0.6, "contrast": 30.0}),
                    );
                    host.edit(
                        "edit.set-basic",
                        json!({"mask": second, "exposure": 0.4, "vibrance": 40.0}),
                    );
                }
            }
            for (index, mask) in ["Mask 1", "Mask 2"].into_iter().enumerate() {
                for (size, rect) in reads() {
                    let what = format!("{stack}, {binding} {mask}, {size}");
                    let (ours, record) = gpu.area(&created[0][index], rect);
                    let (theirs, _) = reference.area(&created[1][index], rect);
                    assert_eq!(
                        record["renderer"],
                        json!({"record": "gpu", "reason": null}),
                        "{what}: the GPU answers"
                    );
                    assert_eq!(
                        (ours.stage, ours.rect, ours.reduction, ours.size),
                        (theirs.stage, theirs.rect, theirs.reduction, theirs.size),
                        "{what}: the same rectangle and reduction"
                    );
                    assert_eq!(ours.points.len(), theirs.points.len(), "{what}");
                    if size == "click" {
                        assert_eq!((ours.reduction, ours.points.len()), (1, 1), "{what}");
                    } else {
                        assert_eq!(
                            (ours.stage.width, ours.stage.height),
                            (WIDTH, HEIGHT),
                            "{what}"
                        );
                        assert!(ours.reduction > 1, "{what}: a reduced whole frame");
                    }
                    let (mut delta_e, mut delta_l) = (Vec::new(), Vec::new());
                    for (a, b) in ours.points.iter().zip(&theirs.points) {
                        let a = lab_from_srgb8(a.map(code));
                        let b = lab_from_srgb8(b.map(code));
                        delta_e.push(ciede2000(a, b));
                        delta_l.push(a[0] - b[0]);
                    }
                    let [columns, rows] = ours.size;
                    let errors = statistics_of(columns as usize, rows as usize, &delta_e, &delta_l)
                        .expect("the statistics");
                    let limits = class.limits();
                    // Each point is already a block mean, so every point is held to the worst
                    // block's limit.
                    assert!(
                        errors.mean <= limits.mean
                            && errors.max <= limits.worst_block
                            && errors.p99 <= limits.p99
                            && errors.mean_delta_l.abs() <= limits.mean_delta_l,
                        "{what}: {errors:?} against {limits:?}"
                    );
                    eprintln!(
                        "{test}: {name}: {what}: {} points at {}: {errors:?}",
                        ours.points.len(),
                        ours.reduction
                    );
                    if binding == "unbound" {
                        unbound.push((what, ours));
                    } else if mask == "Mask 1" {
                        let (_, before) = unbound.remove(0);
                        // Binding Basic puts the layer at the input site, so the GPU reads the
                        // same stage there as it did unbound. Mask 2's site moves past Mask 1's
                        // Basic once that is bound, so only the first mask is held to this.
                        assert_eq!(
                            (before.rect, before.reduction, &before.points),
                            (ours.rect, ours.reduction, &ours.points),
                            "{what}: binding a first Basic leaves the read unchanged"
                        );
                    }
                    judged += 1;
                }
            }
        }
    }
    assert_eq!(judged, 16, "every stack, binding, mask and size was judged");
}

/// A launch that refused the GPU (`--no-gpu-render`) still picks: its tile worker answers every
/// read from the reference, naming `refused`, a reason no session carries, and a click and a drag of
/// each kind each commit their one entry from that answer.
#[test]
#[slow]
fn a_pick_in_a_launch_that_refused_the_gpu_reads_the_reference() {
    let tiles: Arc<dyn TileService> = Arc::new(GpuTiles::pending(true));
    let host = Host::start("refused", Some(tiles), &photograph());
    for (create, pick, component) in [
        (
            "mask.create-luminance-range",
            "mask.pick-luminance-range",
            "Luminance range 1",
        ),
        (
            "mask.create-colour-range",
            "mask.pick-colour-range",
            "Colour range 1",
        ),
    ] {
        let created = host.edit(create, json!({}));
        let mask = created["mask"].clone();
        let answer = host.answer(&mask, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(
            answer["renderer"],
            json!({"record": "reference", "reason": "refused"}),
            "{create}"
        );
        for rect in [
            json!({"x": 0.37, "y": 0.41}),
            json!({"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0}),
        ] {
            let mut params = rect.clone();
            params["mask"] = mask.clone();
            params["component"] = created["component"].clone();
            let picked = host.edit(pick, params);
            assert!(
                picked["label"]
                    .as_str()
                    .is_some_and(|label| label.ends_with(&format!("Pick {component}"))),
                "{pick} over {rect}: {picked}"
            );
        }
    }
}
