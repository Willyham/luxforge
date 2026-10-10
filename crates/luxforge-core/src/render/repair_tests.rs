//! The repair boundary against the independent `f64` reference (`luxforge_reference::repair`):
//! exact JPEG bytes and exact RAW-precision frame values over overlap, distant and far sources,
//! the frame's edges and both kinds; its place in a stack (crop anchoring, a resampling crop, the
//! quantization hand-off to colour), its declared reads, its refusals and its cancellation.
use super::{
    RenderContext, RenderOptions, SpatialMode,
    repair_entry::{RepairEntry, regions_of},
    repair_testing::{RepairTestModule, TEST_REPAIR_EFFECT, table_of},
    testing::{frame_in, linear_evaluation, point_evaluated, render},
    tests::{crop_layer, fitted_crop, geometry_registry, image},
};
use crate::{
    Cancel, EFFECT_FORMAT, ErrorKind, Layer, LayerId, ModuleRegistry, Recipe, SnapshotId,
    SourceImage,
    modules::{Region, RepairKind, Stage},
    path,
};
use luxforge_reference::{SplitMix64, repair as reference, srgb};
use serde_json::{Value, json};

pub(crate) fn repair_registry() -> ModuleRegistry {
    let mut registry = geometry_registry();
    registry.register(RepairTestModule::shared()).unwrap();
    registry
}

/// One operation as a test writes it, in pixels of a `stage`: put on the grid exactly as a stored
/// operation is.
#[derive(Clone, Debug)]
pub(crate) struct Op {
    pub(crate) id: u32,
    pub(crate) kind: RepairKind,
    pub(crate) points: Vec<[i32; 2]>,
    pub(crate) radius: i32,
    pub(crate) feather: u8,
    pub(crate) opacity: u8,
    pub(crate) offset: [i32; 2],
}

impl Op {
    pub(crate) fn new(
        id: u32,
        kind: RepairKind,
        stage: Stage,
        points_px: &[[f64; 2]],
        radius_px: f64,
        source_px: [f64; 2],
    ) -> Self {
        let (w, h) = (f64::from(stage.width), f64::from(stage.height));
        Self {
            id,
            kind,
            points: points_px
                .iter()
                .map(|[x, y]| [path::quantize(x / w), path::quantize(y / h)])
                .collect(),
            radius: path::quantize(radius_px / h),
            feather: 50,
            opacity: 100,
            offset: [path::quantize(source_px[0] / w), path::quantize(source_px[1] / h)],
        }
    }

    pub(crate) fn feather(mut self, feather: u8) -> Self {
        self.feather = feather;
        self
    }

    pub(crate) fn opacity(mut self, opacity: u8) -> Self {
        self.opacity = opacity;
        self
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": match self.kind { RepairKind::Clone => "clone", RepairKind::Heal => "heal" },
            "points": self.points,
            "radius": self.radius,
            "feather": self.feather,
            "opacity": self.opacity,
            "offset": self.offset,
        })
    }

    fn reference(&self) -> reference::Operation {
        reference::Operation {
            kind: match self.kind {
                RepairKind::Clone => reference::Kind::Clone,
                RepairKind::Heal => reference::Kind::Heal,
            },
            paths: vec![
                self.points
                    .iter()
                    .map(|[x, y]| [path::steps_to_units(*x), path::steps_to_units(*y)])
                    .collect(),
            ],
            radius: path::steps_to_units(self.radius),
            feather: f64::from(self.feather),
            opacity: f64::from(self.opacity),
            offset: [
                path::steps_to_units(self.offset[0]),
                path::steps_to_units(self.offset[1]),
            ],
        }
    }
}

pub(crate) fn repair_layer(ops: &[Op]) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: TEST_REPAIR_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"operations": ops.iter().map(Op::json).collect::<Vec<_>>()}),
        mask: None,
        artifacts: Vec::new(),
    }
}

pub(crate) fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        layers,
        ..Recipe::default()
    }
}

/// An opaque textured JPEG-like source: noise over a gradient, so a wrong source pixel, a wrong
/// order or a wrong blend changes bytes.
pub(crate) fn textured(width: u32, height: u32, seed: u64) -> SourceImage {
    let mut random = SplitMix64(seed);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let noise = (random.next_u64() % 48) as u32;
            rgba.extend([
                ((x * 200 / width) + noise).min(255) as u8,
                ((y * 180 / height) + noise / 2).min(255) as u8,
                ((x * 5 + y * 3 + noise) % 256) as u8,
                255,
            ]);
        }
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: format!("sha256:textured-{seed}"),
        orientation: 1,
        capture: Default::default(),
    }
}

/// The reference bytes of `ops` over `source`: the source decoded to the `f32` the stage holds,
/// every operation applied by the independent reference, each output encoded once.
fn reference_bytes(source: &SourceImage, ops: &[Op]) -> Vec<u8> {
    let pixels: Vec<[f64; 3]> = source
        .rgba
        .chunks_exact(4)
        .map(|p| [0, 1, 2].map(|c| f64::from(srgb::decode(p[c]) as f32)))
        .collect();
    let image = reference::Image::new(source.width as usize, source.height as usize, pixels);
    let ops: Vec<_> = ops.iter().map(Op::reference).collect();
    let out = reference::apply(&image, &ops);
    out.pixels
        .iter()
        .flat_map(|p| {
            let [r, g, b] = p.map(|v| srgb::code(f64::from(v as f32)));
            [r, g, b, 255]
        })
        .collect()
}

fn stage_of(source: &SourceImage) -> Stage {
    Stage {
        width: source.width,
        height: source.height,
    }
}

/// Overlap (a Clone over a Heal, reading the Heal's output), a far source, sources clipped by
/// every edge, a hard edge and partial opacity, a self-crossing stroke and a tiny spot.
fn varied_ops(stage: Stage) -> Vec<Op> {
    use RepairKind::{Clone, Heal};
    let (w, h) = (f64::from(stage.width), f64::from(stage.height));
    vec![
        Op::new(5, Heal, stage, &[[0.30 * w, 0.40 * h]], 9.0, [24.0, -6.0]),
        Op::new(2, Clone, stage, &[[0.25 * w, 0.40 * h], [0.36 * w, 0.44 * h]], 5.0, [-20.0, 30.0])
            .feather(0)
            .opacity(60),
        Op::new(9, Heal, stage, &[[0.85 * w, 0.20 * h]], 7.0, [-0.75 * w, 0.65 * h]),
        Op::new(3, Clone, stage, &[[2.0, 3.0]], 6.0, [w - 6.0, h - 8.0]),
        Op::new(4, Heal, stage, &[[w - 4.0, h - 3.0]], 6.0, [10.0, 8.0]),
        Op::new(
            7,
            Heal,
            stage,
            &[[0.6 * w, 0.6 * h], [0.7 * w, 0.75 * h], [0.7 * w, 0.6 * h], [0.6 * w, 0.75 * h]],
            4.0,
            [0.0, -0.3 * h],
        )
        .feather(20),
        Op::new(8, Clone, stage, &[[0.5 * w, 0.1 * h]], 1.0, [3.0, 0.0]),
    ]
}

#[test]
fn a_jpeg_repair_is_the_independent_reference_byte_for_byte() {
    let source = textured(160, 120, 11);
    let ops = varied_ops(stage_of(&source));
    let registry = repair_registry();
    let raster = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![repair_layer(&ops)]),
    )
    .unwrap();
    let expected = reference_bytes(&source, &ops);
    assert_eq!(raster.rgba.len(), expected.len());
    let differing = raster
        .rgba
        .iter()
        .zip(&expected)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(differing, 0, "bytes that differ from the reference");
    // The repair changed the frame, and left every pixel outside its destinations as it was.
    assert_ne!(&raster.rgba[..], &source.rgba[..]);
    let entry = RepairEntry::new(
        table_of(&repair_layer(&ops).payload).unwrap(),
        stage_of(&source),
    );
    for y in 0..source.height {
        for x in 0..source.width {
            if entry.regions().iter().any(|r| r.writes.contains(x, y)) {
                continue;
            }
            let at = ((y * source.width + x) * 4) as usize;
            assert_eq!(raster.rgba[at..at + 4], source.rgba[at..at + 4], "({x}, {y})");
        }
    }
}

#[test]
fn a_raw_repair_keeps_continuous_values_exactly() {
    let (width, height) = (96, 72);
    let stage = Stage { width, height };
    let mut random = SplitMix64(3);
    let pixels: Vec<[f32; 3]> = (0..width * height)
        .map(|_| {
            [0, 1, 2].map(|_| (random.next_u64() % 20_000) as f32 / 10_000.0 - 0.3)
        })
        .collect();
    let source = image(width, height, &pixels);
    let ops = varied_ops(stage);
    let registry = repair_registry();
    let context = RenderContext::new();
    let evaluation = linear_evaluation(
        &context,
        &registry,
        &source,
        &recipe(vec![repair_layer(&ops)]),
        Default::default(),
        super::spatial::Tiling::Halo,
        SpatialMode::Frames,
    )
    .unwrap();
    let frame = evaluation.frame.as_ref().expect("the repair frame");
    let plane = (width * height) as usize;
    let input = reference::Image::new(
        width as usize,
        height as usize,
        pixels.iter().map(|p| p.map(f64::from)).collect(),
    );
    let expected = reference::apply(&input, &ops.iter().map(Op::reference).collect::<Vec<_>>());
    let mut beyond = 0;
    for (index, value) in expected.pixels.iter().enumerate() {
        for (channel, expected) in value.iter().enumerate() {
            let held = frame.planes[channel * plane + index];
            assert_eq!(
                held.to_bits(),
                (*expected as f32).to_bits(),
                "pixel {index} channel {channel}"
            );
            beyond += usize::from(!(0.0..=1.0).contains(&held));
        }
    }
    assert!(beyond > 0, "the test holds values outside [0, 1]");
}

#[test]
fn a_sample_through_a_repair_is_the_frame_byte() {
    let source = textured(64, 48, 5);
    let ops = varied_ops(stage_of(&source));
    let registry = repair_registry();
    let recipe = recipe(vec![
        repair_layer(&ops),
        Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.4})),
    ]);
    let context = RenderContext::new();
    let raster = frame_in(
        &context,
        &registry,
        &source,
        SnapshotId::new(),
        &recipe,
        RenderOptions::default(),
    )
    .unwrap();
    let (_, _, points) =
        point_evaluated(&context, &registry, &source, &recipe, SpatialMode::Frames).unwrap();
    assert_eq!(raster.rgba[..], points[..]);
}

#[test]
fn a_repair_stays_anchored_to_content_through_a_straight_crop() {
    let source = textured(80, 60, 7);
    let stage = stage_of(&source);
    let ops = varied_ops(stage);
    let registry = repair_registry();
    let whole = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![repair_layer(&ops)]),
    )
    .unwrap();
    // Moved, grown and shrunk crops of the same repaired content.
    for rect in [
        [0.1, 0.1, 0.6, 0.6],
        [0.2, 0.15, 0.6, 0.6],
        [0.0, 0.0, 0.9, 0.95],
        [0.3, 0.3, 0.3, 0.4],
    ] {
        let crop = fitted_crop(stage.width, stage.height, 0.0, rect);
        let cropped = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![repair_layer(&ops), crop_layer(crop)]),
        )
        .unwrap();
        let (ox, oy) = (
            (rect[0] * f64::from(stage.width)).round() as u32,
            (rect[1] * f64::from(stage.height)).round() as u32,
        );
        for y in 0..cropped.height {
            for x in 0..cropped.width {
                let at = ((y * cropped.width + x) * 4) as usize;
                let from = (((y + oy) * stage.width + x + ox) * 4) as usize;
                assert_eq!(
                    cropped.rgba[at..at + 4],
                    whole.rgba[from..from + 4],
                    "{rect:?} ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn a_resampling_crop_reads_the_repaired_content() {
    let source = textured(90, 70, 9);
    let stage = stage_of(&source);
    let ops = varied_ops(stage);
    let registry = repair_registry();
    let repaired = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![repair_layer(&ops)]),
    )
    .unwrap();
    // The repair's frame is a narrow quantization boundary before a resample, so the rotated crop
    // of the repaired stack is the rotated crop of the repaired bytes.
    let crop = fitted_crop(stage.width, stage.height, 4.5, [0.1, 0.1, 0.7, 0.7]);
    let through = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![repair_layer(&ops), crop_layer(crop)]),
    )
    .unwrap();
    let bytes = SourceImage {
        rgba: repaired.rgba.clone(),
        fingerprint: "sha256:repaired".into(),
        ..source.clone()
    };
    let expected = render(
        &registry,
        &bytes,
        SnapshotId::new(),
        &recipe(vec![crop_layer(crop)]),
    )
    .unwrap();
    assert_eq!(through.rgba, expected.rgba);
}

#[test]
fn a_repair_before_colour_hands_off_wide_and_is_the_point_evaluation() {
    let source = textured(70, 50, 13);
    let ops = varied_ops(stage_of(&source));
    let registry = repair_registry();
    let context = RenderContext::new();
    for layers in [
        vec![
            repair_layer(&ops),
            Layer::new(crate::BASIC_EFFECT, json!({"exposure": -0.7, "contrast": 30.0})),
        ],
        vec![repair_layer(&ops), Layer::new(crate::DETAIL_EFFECT, json!({"sharpening": 60.0}))],
    ] {
        let recipe = recipe(layers);
        let compiled = registry.compile(source.width, source.height, &recipe).unwrap();
        let widths = super::byte::byte_frame_widths(&compiled);
        assert!(widths[1].input, "the repair frame is wide before colour or Detail");
        let raster = frame_in(
            &context,
            &registry,
            &source,
            SnapshotId::new(),
            &recipe,
            RenderOptions::default(),
        )
        .unwrap();
        let (_, _, points) =
            point_evaluated(&context, &registry, &source, &recipe, SpatialMode::Frames).unwrap();
        assert_eq!(raster.rgba[..], points[..]);
    }
}

#[test]
fn a_repair_outside_the_leading_run_is_refused_and_kept() {
    let source = textured(40, 30, 1);
    let ops = vec![Op::new(1, RepairKind::Heal, stage_of(&source), &[[10.0, 10.0]], 3.0, [12.0, 4.0])];
    let registry = repair_registry();
    let late = recipe(vec![
        Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.2})),
        repair_layer(&ops),
    ]);
    let before = late.clone();
    for error in [
        render(&registry, &source, SnapshotId::new(), &late).unwrap_err(),
        registry.validate_recipe(&late).unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Incompatible, "{}", error.detail);
        assert!(
            error.detail.contains("must follow only the source development"),
            "{}",
            error.detail
        );
    }
    assert_eq!(late, before, "nothing rewritten");
    // After a source layer, and first, it is admitted.
    registry
        .validate_recipe(&recipe(vec![repair_layer(&ops)]))
        .unwrap();
}

#[test]
fn a_table_past_the_limit_is_refused_by_name() {
    let source = textured(40, 30, 2);
    let stage = stage_of(&source);
    let ops: Vec<Op> = (1..=101)
        .map(|id| Op::new(id, RepairKind::Clone, stage, &[[10.0, 10.0]], 2.0, [8.0, 0.0]))
        .collect();
    let registry = repair_registry();
    let error = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![repair_layer(&ops)]),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit, "{}", error.detail);
    assert!(error.detail.contains("100"), "{}", error.detail);
    // A hundred render.
    render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![repair_layer(&ops[..100])]),
    )
    .unwrap();
}

#[test]
fn a_cancelled_repair_stops_with_cancelled() {
    let source = textured(120, 90, 4);
    let ops = varied_ops(stage_of(&source));
    let registry = repair_registry();
    let cancel = Cancel::new();
    cancel.cancel();
    let error = frame_in(
        &RenderContext::new(),
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![repair_layer(&ops)]),
        RenderOptions::exact(&cancel),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn a_window_reads_only_what_its_operations_need() {
    let stage = Stage {
        width: 400,
        height: 300,
    };
    let near = Op::new(1, RepairKind::Clone, stage, &[[50.0, 50.0]], 6.0, [300.0, 200.0]);
    let heal = Op::new(2, RepairKind::Heal, stage, &[[350.0, 250.0]], 6.0, [-100.0, 0.0]);
    let table = table_of(&repair_layer(&[near.clone(), heal.clone()]).payload).unwrap();
    let entry = RepairEntry::new(table, stage);
    let [first, second] = [&near, &heal].map(|op| {
        let table = table_of(&repair_layer(&[(*op).clone()]).payload).unwrap();
        regions_of(&table.operations()[0], stage)
    });
    // A window no destination meets reads itself.
    let quiet = Region {
        x0: 150,
        y0: 100,
        width: 20,
        height: 20,
    };
    let (set, relevant) = entry.reads(quiet);
    assert_eq!(set.rects(), &[quiet]);
    assert!(relevant.is_empty());
    // A window over the Heal reads its whole destination and its source, with the ring, and
    // the Clone whose destination its source meets is not needed: the Clone's destination lies
    // far away. The Clone's source lies under the Heal, which reads it first.
    let over = Region {
        x0: 348,
        y0: 248,
        width: 4,
        height: 4,
    };
    let (set, relevant) = entry.reads(over);
    assert_eq!(relevant, vec![1]);
    for read in second.reads {
        assert!(set.rects().iter().any(|r| r.x0 <= read.x0
            && r.y0 <= read.y0
            && r.x1() >= read.x1()
            && r.y1() >= read.y1()));
    }
    // A window over the Clone reads its source, which the Heal does not write.
    let (set, relevant) = entry.reads(first.writes);
    assert_eq!(relevant, vec![0]);
    assert!(set.meets(first.reads[1]));
    assert!(set.rects().len() <= super::repair_entry::MAX_READ_RECTS);
    // The GPU window is one rectangle: the set's bounding box.
    assert_eq!(entry.window_reads(first.writes), set.bounds());
}

#[test]
fn every_window_of_a_repair_frame_is_the_whole_frames() {
    // Each window evaluated on its own, over exactly its declared reads, gives the whole frame's
    // pixels: what a GPU window or a tile relies on.
    let source = textured(120, 90, 21);
    let stage = stage_of(&source);
    let ops = varied_ops(stage);
    let table = table_of(&repair_layer(&ops).payload).unwrap();
    let entry = RepairEntry::new(table, stage);
    let decoded: Vec<[f32; 3]> = source
        .rgba
        .chunks_exact(4)
        .map(|p| [0, 1, 2].map(|c| srgb::decode(p[c]) as f32))
        .collect();
    let whole = {
        let mut values = vec![0.0f32; decoded.len() * 3];
        for (index, p) in decoded.iter().enumerate() {
            for c in 0..3 {
                values[c * decoded.len() + index] = p[c];
            }
        }
        let mut frame =
            crate::repair::RepairFrame::new(stage, Region::whole(stage), &mut values).unwrap();
        crate::repair::apply(&mut frame, entry.bound(), &Cancel::never()).unwrap();
        values
    };
    for window in [
        Region { x0: 30, y0: 30, width: 20, height: 20 },
        Region { x0: 90, y0: 10, width: 30, height: 30 },
        Region { x0: 0, y0: 0, width: 8, height: 8 },
        Region { x0: 60, y0: 50, width: 40, height: 30 },
    ] {
        let (set, relevant) = entry.reads(window);
        let mut store: Vec<(Region, Vec<f32>)> = set
            .rects()
            .iter()
            .map(|&rect| {
                let mut values = vec![0.0f32; rect.pixels() as usize * 3];
                let len = rect.pixels() as usize;
                for y in rect.y0..rect.y1() {
                    for x in rect.x0..rect.x1() {
                        let at = ((y - rect.y0) * rect.width + x - rect.x0) as usize;
                        let p = decoded[(y * stage.width + x) as usize];
                        for c in 0..3 {
                            values[c * len + at] = p[c];
                        }
                    }
                }
                (rect, values)
            })
            .collect();
        let ops: Vec<_> = relevant.iter().map(|&i| entry.bound()[i].clone()).collect();
        {
            let parts = store
                .iter_mut()
                .map(|(rect, values)| (*rect, values.as_mut_slice()))
                .collect();
            let mut frame = crate::repair::RepairFrame::with_parts(stage, parts).unwrap();
            crate::repair::apply(&mut frame, &ops, &Cancel::never()).unwrap();
        }
        let plane = decoded.len();
        for y in window.y0..window.y1() {
            for x in window.x0..window.x1() {
                let (rect, values) = store.iter().find(|(r, _)| r.contains(x, y)).unwrap();
                let len = rect.pixels() as usize;
                let at = ((y - rect.y0) * rect.width + x - rect.x0) as usize;
                for c in 0..3 {
                    assert_eq!(
                        values[c * len + at].to_bits(),
                        whole[c * plane + (y * stage.width + x) as usize].to_bits(),
                        "{window:?} ({x}, {y})"
                    );
                }
            }
        }
    }
}

#[test]
fn a_gpu_plan_draws_a_repair_and_names_a_solve_it_cannot_hold() {
    let source = textured(64, 48, 6);
    let stage = stage_of(&source);
    let ops = varied_ops(stage);
    let registry = repair_registry();
    let stack = recipe(vec![
        repair_layer(&ops),
        Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.3})),
    ]);
    let answer = crate::gpu_plan(
        &registry,
        &stack,
        crate::GpuPlanRequest::exact(1, stage).from_source(),
    )
    .unwrap();
    let plan = answer.plan().expect("a repair stack has a GPU plan");
    let step = &plan.spatial[0];
    let repair = step.repair.as_ref().expect("the repair step");
    assert_eq!(step.layer, 0);
    assert_eq!(repair.operations.len(), ops.len());
    assert!(step.block.as_ref().is_some_and(|block| block[0] == ops.len() as u32));
    // The Basic layer runs on the repair's output.
    assert_eq!(plan.operations().count(), 1);
    // A Heal of a quarter of a large frame's height needs more level planes than a step may hold:
    // the reference draws it, by name.
    let large = Stage {
        width: 9000,
        height: 6000,
    };
    let heal = Op::new(4, RepairKind::Heal, large, &[[4500.0, 3000.0]], 1500.0, [2000.0, 0.0]);
    let answer = crate::gpu_plan(
        &registry,
        &recipe(vec![repair_layer(&[heal])]),
        crate::GpuPlanRequest::exact(0, large).from_source(),
    )
    .unwrap();
    let fallback = answer.fallback().expect("past the level budget");
    assert_eq!(fallback.code(), "repair");
    assert!(fallback.to_string().contains("Heal 4"), "{fallback}");
}

/// The committed stack `layers` over `source`, as the desktop's evaluation of an entry holds it.
fn evaluation_of(source: &SourceImage, layers: Vec<Layer>) -> crate::Evaluation {
    use crate::{AssetId, EntryId, HistoryEntry, Snapshot};
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
            recipe: recipe(layers.clone()),
        },
        undo_parent: None,
        restore_target: None,
    };
    crate::Evaluation::new(
        std::sync::Arc::new(repair_registry()),
        RenderContext::new(),
        crate::PreviewSource::Jpeg(source.clone()),
        entry,
        recipe(layers),
        None,
    )
}

#[test]
fn the_warm_list_covers_repair_gestures_at_open_and_first_use() {
    let source = textured(120, 90, 8);
    let stage = stage_of(&source);
    let bounds = crate::ProxyBounds {
        width: 120,
        height: 90,
    };
    let repairs = |plan: &crate::GpuPlan| -> Option<usize> {
        plan.spatial
            .first()
            .and_then(|step| step.repair.as_ref())
            .map(|repair| repair.operations.len())
    };
    // A stack holding two corrections: its own links, an update drag, and a new top Heal and
    // Clone, each a repair step of the table and one more operation.
    let ops = vec![
        Op::new(1, RepairKind::Heal, stage, &[[30.0, 30.0]], 5.0, [20.0, 0.0]),
        Op::new(2, RepairKind::Clone, stage, &[[80.0, 60.0]], 4.0, [-20.0, 5.0]),
    ];
    let held = evaluation_of(
        &source,
        vec![
            repair_layer(&ops),
            Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.2})),
        ],
    );
    let warm = super::gpu::plan_warm_list(&held, crate::GpuView::Fit(bounds)).unwrap();
    let counts: Vec<usize> = warm.plans.iter().filter_map(repairs).collect();
    assert!(counts.contains(&2), "the stack's own repair links: {counts:?}");
    assert!(counts.contains(&3), "a drafted top operation: {counts:?}");
    let kernels = |count: usize| -> Vec<&'static str> {
        warm.plans
            .iter()
            .filter(|plan| repairs(plan) == Some(count))
            .flat_map(|plan| plan.spatial[0].passes.iter().map(|pass| pass.kernel))
            .collect()
    };
    assert!(kernels(3).contains(&"lf_repair_clone"));
    assert!(kernels(3).contains(&"lf_repair_heal_sweep"));
    // A stack with none: the first Heal and Clone where the repair layer would go.
    let bare = evaluation_of(
        &source,
        vec![Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.2}))],
    );
    let warm = super::gpu::plan_warm_list(&bare, crate::GpuView::Fit(bounds)).unwrap();
    let first: Vec<&crate::GpuPlan> = warm
        .plans
        .iter()
        .filter(|plan| repairs(plan) == Some(1))
        .collect();
    assert!(!first.is_empty(), "the first correction's plans are warmed");
    for plan in first {
        // Basic runs after the repair, on its output.
        assert_eq!(plan.content.len(), 0);
        assert_eq!(plan.operations().count(), 1);
    }
}
