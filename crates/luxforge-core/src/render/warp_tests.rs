//! Host integration proofs use test-only modules: profile lookup and metadata are independent.
use super::*;
use super::{
    map::{Mapping, RadialModel, WarpStep},
    testing,
    tests::{fitted_crop, gradient, image, turn},
};
use crate::modules::{
    ActionInput, ActionPlan, EffectDescriptor, EffectStage, ModuleDescriptor, Processing, Region,
    Stage, StageContext, ToolModule,
};
use crate::{
    BASIC_EFFECT, CROP_EFFECT, Cancel, EFFECT_FORMAT, Error, Layer, LayerId, Recipe, SnapshotId,
    Transform,
};
use serde_json::{Map, Value, json};
use std::{borrow::Cow, sync::Arc};
const LENS: &str = "luxforge.lens.distortion";
const PERSPECTIVE: &str = "luxforge.perspective";
struct WarpModule(ModuleDescriptor);
impl WarpModule {
    fn new() -> Self {
        Self(ModuleDescriptor {
            id: "test.warp".into(),
            title: "Warp test".into(),
            effects: vec![EffectDescriptor {
                order: 2,
                ..EffectDescriptor::new(LENS, EffectStage::Geometry)
            }],
            ..Default::default()
        })
    }
}
impl ToolModule for WarpModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::internal("no actions"))
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new("Warp"))
    }
    fn compile(
        &self,
        id: &str,
        _: u32,
        p: &Value,
        at: crate::CompileStage,
    ) -> Result<Processing, Error> {
        let stage = at.stage;
        if let Some(step) = p.get("declared_step") {
            return Ok(Processing::Warp(
                serde_json::from_value(step.clone()).unwrap(),
            ));
        }
        let step = if id == LENS {
            let model = match p["model"].as_str().unwrap_or("poly3") {
                "poly3" => RadialModel::Poly3,
                "poly5" => RadialModel::Poly5,
                _ => RadialModel::PtLens,
            };
            WarpStep::radial(
                model,
                serde_json::from_value(p["terms"].clone()).unwrap(),
                p["unit"].as_f64().unwrap_or(1.0),
                stage,
            )?
        } else {
            WarpStep::projective(
                p["horizontal"].as_i64().unwrap(),
                p["vertical"].as_i64().unwrap(),
                stage,
            )?
        };
        Ok(if step.is_identity() {
            Processing::ExactGeometry(crate::ExactGeometry::identity(stage.width, stage.height))
        } else {
            Processing::Warp(step)
        })
    }
}
pub(super) fn registry() -> crate::ModuleRegistry {
    let mut r = crate::ModuleRegistry::new();
    r.register(Arc::new(WarpModule::new())).unwrap();
    r.register(Arc::new(crate::modules::CropModule::new()))
        .unwrap();
    r.register(Arc::new(crate::modules::PerspectiveModule::new()))
        .unwrap();
    r.register(Arc::new(crate::modules::BasicModule::new()))
        .unwrap();
    r
}
fn layer(id: &str, payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: id.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
        artifacts: vec![],
    }
}
fn lens(terms: [f64; 3]) -> Layer {
    layer(LENS, json!({"terms":terms}))
}
fn perspective() -> Layer {
    layer(PERSPECTIVE, json!({"horizontal":35,"vertical":-25}))
}
fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        layers,
        ..Recipe::default()
    }
}
fn fused(w: u32, h: u32) -> Recipe {
    recipe(vec![
        lens([-0.079, 0.0, 0.0]),
        perspective(),
        Layer::crop(fitted_crop(w, h, 7.0, [0.12, 0.12, 0.7, 0.65])),
    ])
}

fn detail_warp_recipe(width: u32, height: u32) -> Recipe {
    let mut mask = crate::Mask::new("Restoration gradient");
    mask.components.push(crate::Component::new(
        "Linear",
        crate::ComponentMode::Add,
        "linear",
        json!({"x0":0.1,"y0":0.2,"x1":0.9,"y1":0.8}),
    ));
    let mut masked_detail = layer(
        crate::DETAIL_EFFECT,
        json!({"sharpening":30.0,"luminance":12.0,"colour":15.0}),
    );
    masked_detail.mask = Some(mask.id.clone());
    Recipe {
        layers: vec![
            layer(
                crate::DETAIL_EFFECT,
                json!({"sharpening":40.0,"luminance":25.0,"colour":20.0}),
            ),
            masked_detail,
            layer(BASIC_EFFECT, json!({"exposure":0.25})),
            layer(
                crate::CURVE_EFFECT,
                json!({"luminance":[[0.0,0.03],[0.5,0.6],[1.0,1.0]]}),
            ),
            layer(crate::PRESENCE_EFFECT, json!({"clarity":5.0})),
            testing::frozen_lens(width, height, 35.0),
            perspective(),
            Layer::crop(fitted_crop(width, height, 3.0, [0.1, 0.12, 0.75, 0.7])),
            layer(crate::VIGNETTE_EFFECT, json!({"amount":-20.0})),
        ],
        masks: vec![mask],
        ..Recipe::default()
    }
}

fn detail_warp_sources() -> [crate::PreviewSource; 2] {
    let (width, height) = (96, 64);
    let raw = image(
        width,
        height,
        &(0..width * height)
            .map(|i| {
                [
                    (i % 29) as f32 / 21.0 - 0.25,
                    (i % 37) as f32 / 31.0,
                    (i % 43) as f32 / 23.0,
                ]
            })
            .collect::<Vec<_>>(),
    );
    assert!(raw.planes().iter().any(|value| *value < 0.0));
    assert!(raw.planes().iter().any(|value| *value > 1.0));
    [
        crate::PreviewSource::Jpeg(gradient(width, height)),
        crate::PreviewSource::Raw {
            image: raw,
            settings: LinearSettings::default(),
        },
    ]
}

#[test]
fn detail_and_curve_keep_warp_full_point_and_window_pixels_identical() {
    let registry = crate::ModuleRegistry::builtin();
    let recipe = detail_warp_recipe(96, 64);
    for source in detail_warp_sources() {
        let (original_bytes, original_planes) = match &source {
            crate::PreviewSource::Jpeg(image) => (image.rgba.clone(), Vec::new()),
            crate::PreviewSource::Raw { image, .. } => {
                (Arc::new(Vec::new()), image.planes().to_vec())
            }
        };
        let context = RenderContext::new();
        let rendered = render(
            &registry,
            source.input(),
            &recipe,
            RenderOptions::default(),
            &context,
        )
        .unwrap();
        let widths = super::byte::byte_frame_widths(&rendered.compiled);
        let mut warps = 0;
        for (index, segment) in rendered.compiled.segments.iter().enumerate() {
            if segment.entry.as_ref().is_some_and(Entry::has_warp) {
                warps += 1;
                assert!(!widths[index].input && !widths[index].output);
                let Entry::Resample(entry) = segment.entry.as_ref().unwrap() else {
                    unreachable!()
                };
                let Mapping::Warp(chain) = &entry.resample.map else {
                    unreachable!()
                };
                assert_eq!(chain.steps.len(), 3);
            }
        }
        assert_eq!(warps, 1, "Lens, Perspective and crop interpolate once");
        let boundary = rendered.compiled.restoration_boundary().unwrap();
        assert!(
            widths[boundary].input,
            "Basic and Curve receive the wide restoration boundary"
        );
        let colour_units = rendered.compiled.segments[boundary]
            .operations
            .iter()
            .filter_map(|operation| match operation {
                Processing::Color(operation) => Some(operation.len()),
                _ => None,
            })
            .sum::<usize>();
        assert!(colour_units >= 2, "Basic and Curve compile into real units");
        let frame = rendered.frame(SnapshotId::new()).unwrap();
        for (x, y) in [
            (0, 0),
            (frame.width / 2, frame.height / 2),
            (frame.width - 1, frame.height - 1),
            (frame.width / 3, frame.height / 4),
        ] {
            assert_eq!(rendered.sample(x, y).unwrap().rgba, frame.pixel(x, y));
        }
        match &source {
            crate::PreviewSource::Jpeg(image) => {
                assert!(Arc::ptr_eq(&image.rgba, &original_bytes));
                assert_eq!(image.rgba, original_bytes);
            }
            crate::PreviewSource::Raw { image, .. } => assert_eq!(image.planes(), original_planes),
        }
        assert_eq!(context.scratch().in_use(), 0);
        assert_eq!(context.spatial().in_use(), 0);
    }
}

#[test]
fn lens_perspective_straighten_compile_to_one_resample_entry() {
    let r = registry();
    let c = r.compile(6000, 4000, &fused(6000, 4000)).unwrap();
    assert_eq!(c.segments.len(), 2);
    let Entry::Resample(entry) = c.segments[1].entry.as_ref().unwrap() else {
        panic!()
    };
    let Mapping::Warp(chain) = &entry.resample.map else {
        panic!()
    };
    assert_eq!(chain.steps.len(), 3);
}
#[test]
fn integer_crop_after_warp_stays_exact_geometry() {
    let c = registry()
        .compile(
            6000,
            4000,
            &recipe(vec![
                lens([-0.079, 0.0, 0.0]),
                perspective(),
                Layer::crop(fitted_crop(6000, 4000, 0.0, [0.1, 0.1, 0.7, 0.7])),
            ]),
        )
        .unwrap();
    assert_eq!(c.segments.len(), 2);
    assert!(!c.segments[1].geometry.is_identity(6000, 4000));
    let Entry::Resample(entry) = c.segments[1].entry.as_ref().unwrap() else {
        panic!()
    };
    let Mapping::Warp(chain) = &entry.resample.map else {
        panic!()
    };
    assert_eq!(chain.steps.len(), 2);
}
#[test]
fn crop_only_stack_compiles_as_before() {
    let r = registry();
    let plain = r
        .compile(
            6000,
            4000,
            &recipe(vec![Layer::crop(fitted_crop(
                6000,
                4000,
                7.0,
                [0.1, 0.1, 0.7, 0.7],
            ))]),
        )
        .unwrap();
    assert_eq!(plain.segments.len(), 2);
    let Entry::Resample(entry) = plain.segments[1].entry.as_ref().unwrap() else {
        panic!()
    };
    assert!(matches!(entry.resample.map, Mapping::Affine(_)));
}
#[test]
fn noncanonical_warp_interleavings_are_refused_without_rewrite() {
    let crop = Layer::crop(fitted_crop(6000, 4000, 0.0, [0.1, 0.1, 0.7, 0.7]));
    for layers in [
        vec![
            lens([-0.079, 0.0, 0.0]),
            layer(BASIC_EFFECT, json!({"exposure":1.0})),
            crop.clone(),
        ],
        vec![crop.clone(), lens([-0.079, 0.0, 0.0])],
        vec![lens([-0.079, 0.0, 0.0]), turn(Transform::RotateRight)],
        vec![perspective(), lens([-0.079, 0.0, 0.0])],
    ] {
        let p = recipe(layers);
        let before = p.clone();
        let e = registry().compile(6000, 4000, &p).err().unwrap();
        assert_eq!(e.kind, crate::ErrorKind::Validation);
        assert!(e.detail.contains("lens and perspective layers must follow"));
        assert_eq!(p, before);
    }
}

#[test]
fn module_warp_declarations_and_wire_descriptors_are_validated() {
    let r = registry();
    let stage = Stage {
        width: 180,
        height: 120,
    };
    let radial = serde_json::to_value(
        WarpStep::radial(RadialModel::Poly3, [-0.079, 0.0, 0.0], 1.0, stage).unwrap(),
    )
    .unwrap();
    let projective = serde_json::to_value(WarpStep::projective(35, -25, stage).unwrap()).unwrap();
    let mut bad_cover = radial.clone();
    bad_cover["parameters"]["cover"] = json!(0.5);
    let mut bad_local = radial.clone();
    bad_local["parameters"]["local_scale_max"] = json!(0.25);
    let mut bad_domain = radial.clone();
    bad_domain["parameters"]["monotone_radius"] = json!(0.25);
    let mut bad_unit = projective.clone();
    bad_unit["parameters"]["unit"] = json!(0.0);
    for step in [
        serde_json::to_value(WarpStep::Affine([0.0; 6])).unwrap(),
        serde_json::to_value(WarpStep::Affine([2.6, 0.0, 0.0, 0.0, 2.6, 0.0])).unwrap(),
        bad_cover,
        bad_local,
        bad_domain,
        bad_unit,
        serde_json::to_value(
            WarpStep::projective(
                35,
                -25,
                Stage {
                    width: 6000,
                    height: 4000,
                },
            )
            .unwrap(),
        )
        .unwrap(),
    ] {
        let p = recipe(vec![layer(LENS, json!({"declared_step":step}))]);
        let before = p.clone();
        assert!(r.compile(stage.width, stage.height, &p).is_err());
        assert_eq!(p, before, "rejection cannot rewrite the stored declaration");
    }
    let geometry = stage_transform(&r, 180, 120, &fused(180, 120)).unwrap();
    let descriptor = super::map::MappingDescriptor {
        entry_id: crate::EntryId::new(),
        snapshot_id: SnapshotId::new(),
        source_fingerprint: "sha256:test".into(),
        draft: None,
        geometry,
    };
    let wire = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(
        serde_json::from_value::<super::map::MappingDescriptor>(wire.clone()).unwrap(),
        descriptor
    );
    for (path, value) in [
        (vec!["mapping_sha256"], json!("forged")),
        (vec!["mapping", "domain", "width"], json!(1)),
        (vec!["mapping", "cover", "combined"], json!(0.5)),
    ] {
        let mut bad = wire.clone();
        let mut target = &mut bad;
        for key in path {
            target = &mut target[key];
        }
        *target = value;
        assert!(serde_json::from_value::<super::map::MappingDescriptor>(bad).is_err());
    }
}
#[test]
fn identity_calibration_keeps_exact_byte_path_and_shared_buffer() {
    let s = gradient(160, 100);
    let r = registry();
    let p = recipe(vec![
        lens([0.0; 3]),
        layer(PERSPECTIVE, json!({"horizontal":0,"vertical":0})),
    ]);
    let c = r.compile(s.width, s.height, &p).unwrap();
    assert_eq!(c.segments.len(), 1);
    let frame = testing::render(&r, &s, SnapshotId::new(), &p).unwrap();
    assert!(Arc::ptr_eq(&frame.rgba, &s.rgba));
}
#[test]
fn warp_sample_equals_render_at_ten_thousand_points() {
    let s = gradient(160, 100);
    let linear = image(
        160,
        100,
        &(0..16000)
            .map(|i| {
                [
                    (i % 23) as f32 / 17.0 - 0.2,
                    (i % 31) as f32 / 23.0,
                    (i % 47) as f32 / 29.0,
                ]
            })
            .collect::<Vec<_>>(),
    );
    let r = registry();
    let p = fused(160, 100);
    let context = RenderContext::new();
    let byte = render(&r, &s, &p, RenderOptions::default(), &context).unwrap();
    let raw = render(
        &r,
        testing::linear(&linear, LinearSettings::default()),
        &p,
        RenderOptions::default(),
        &context,
    )
    .unwrap();
    let snapshot = SnapshotId::new();
    let b = byte.frame(snapshot.clone()).unwrap();
    let l = raw.frame(snapshot).unwrap();
    for i in 0..10_000u32 {
        let x = (i * 53) % b.width;
        let y = (i * 17) % b.height;
        assert_eq!(byte.sample(x, y).unwrap().rgba, b.pixel(x, y));
        assert_eq!(raw.sample(x, y).unwrap().rgba, l.pixel(x, y));
    }
}
#[test]
fn warp_serial_equals_pool() {
    let r = registry();
    let s = gradient(240, 160);
    let p = fused(240, 160);
    let linear = image(240, 160, &vec![[0.2, 0.8, 1.3]; 240 * 160]);
    for raw in [false, true] {
        let old = parallel::force(Some(false));
        let serial = if raw {
            testing::render_linear(
                &r,
                &linear,
                SnapshotId::new(),
                &p,
                LinearSettings::default(),
            )
            .unwrap()
        } else {
            testing::render(&r, &s, SnapshotId::new(), &p).unwrap()
        };
        parallel::force(Some(true));
        let pool = if raw {
            testing::render_linear(
                &r,
                &linear,
                SnapshotId::new(),
                &p,
                LinearSettings::default(),
            )
            .unwrap()
        } else {
            testing::render(&r, &s, SnapshotId::new(), &p).unwrap()
        };
        parallel::force(old);
        assert_eq!(serial.rgba, pool.rgba);
    }
}
#[test]
fn raw_highlights_survive_warp_until_terminal_quantization() {
    let r = registry();
    let s = image(80, 60, &vec![[-0.5, 0.7, 3.0]; 80 * 60]);
    let p = fused(80, 60);
    let c = r.compile(80, 60, &p).unwrap();
    let context = RenderContext::new();
    let e = Evaluation::new(
        linear::Linear::new(&s, LinearSettings::default()).unwrap(),
        Cow::Owned(c),
        spatial::Tiling::Halo,
        SpatialMode::Frames,
        &Cancel::never(),
        &context,
    )
    .unwrap();
    let rgb = e
        .pixel_in(e.compiled.segments.len() - 1, 10, 10)
        .unwrap()
        .unwrap();
    assert!((rgb[0] + 0.5).abs() < 1e-12);
    assert!((rgb[2] - 3.0).abs() < 1e-12);
}

#[test]
fn byte_tail_quantizes_once_under_lens_perspective_straighten() {
    let r = registry();
    let source = gradient(180, 120);
    let p = fused(180, 120);
    let map = stage_transform(&r, 180, 120, &p).unwrap();
    let frame = testing::render(&r, &source, SnapshotId::new(), &p).unwrap();
    let decoded: Vec<[f64; 3]> = source
        .rgba
        .chunks_exact(4)
        .map(|p| std::array::from_fn(|i| luxforge_reference::srgb::decode(p[i])))
        .collect();
    for y in 0..frame.height {
        for x in 0..frame.width {
            let (u, v) = map.to_content(x as f64 + 0.5, y as f64 + 0.5).unwrap();
            let rgb = luxforge_reference::geometry::bilinear_linear(&decoded, 180, 120, [u, v]);
            let got = frame.pixel(x, y).unwrap();
            for i in 0..3 {
                assert!(got[i].abs_diff(luxforge_reference::srgb::code(rgb[i])) <= 1);
            }
        }
    }
}

#[test]
fn linear_tail_with_lens_perspective_and_straighten_renders() {
    let r = registry();
    let pixels: Vec<[f32; 3]> = (0..180 * 120)
        .map(|i| {
            [
                (i % 13) as f32 / 7.0 - 0.2,
                (i % 17) as f32 / 19.0,
                (i % 31) as f32 / 9.0,
            ]
        })
        .collect();
    let source = image(180, 120, &pixels);
    let p = fused(180, 120);
    let map = stage_transform(&r, 180, 120, &p).unwrap();
    let context = RenderContext::new();
    let cancel = Cancel::never();
    let e = Evaluation::new(
        linear::Linear::new(&source, LinearSettings::default()).unwrap(),
        Cow::Owned(r.compile(180, 120, &p).unwrap()),
        spatial::Tiling::Halo,
        SpatialMode::Point,
        &cancel,
        &context,
    )
    .unwrap();
    let pixels: Vec<_> = pixels.iter().map(|p| p.map(f64::from)).collect();
    for y in 0..map.output.height {
        for x in 0..map.output.width {
            let (u, v) = map.to_content(x as f64 + 0.5, y as f64 + 0.5).unwrap();
            let want = luxforge_reference::geometry::bilinear_linear(&pixels, 180, 120, [u, v]);
            let got = e.pixel(x, y).unwrap().unwrap();
            for i in 0..3 {
                assert!((got[i] - want[i]).abs() <= 1e-6 + 1e-6 * want[i].abs());
            }
        }
    }
}

#[test]
fn warp_render_cancels_between_tap_blocks() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CancelOnRead {
        cancel: Cancel,
        calls: Arc<AtomicUsize>,
    }
    impl crate::modules::PointwiseColor for CancelOnRead {
        fn identity(&self) -> crate::OperationIdentity {
            crate::OperationIdentity::new("test.render/warp_tests.rs.CancelOnRead", [])
        }

        fn apply_row(&self, _: u32, _: u32, _: &mut [[f32; 3]]) {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.cancel.cancel();
        }
        fn is_finite(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "cancel-on-read test hook".into()
        }
    }
    let r = registry();
    let source = image(240, 160, &vec![[0.2, 0.4, 0.6]; 240 * 160]);
    let mut c = r.compile(240, 160, &fused(240, 160)).unwrap();
    let last = &c.segments[1];
    let Entry::Resample(entry) = last.entry.as_ref().unwrap() else {
        panic!()
    };
    let local = last.geometry.unmap_region(Region {
        x0: 0,
        y0: 0,
        width: 64,
        height: 16,
    });
    let first = entry.reads(local, c.segments[0].stage()).unwrap();
    let cancel = Cancel::new();
    let calls = Arc::new(AtomicUsize::new(0));
    c.segments[0].has_color = true;
    c.segments[0]
        .operations
        .push(Processing::Color(crate::modules::ColorOperation::new(
            vec![Arc::new(CancelOnRead {
                cancel: cancel.clone(),
                calls: calls.clone(),
            })],
        )));
    let context = RenderContext::new();
    let e = Evaluation::new(
        linear::Linear::new(&source, LinearSettings::default()).unwrap(),
        Cow::Owned(c),
        spatial::Tiling::Halo,
        SpatialMode::Frames,
        &cancel,
        &context,
    )
    .unwrap();
    let old = parallel::force(Some(false));
    let outcome = linear::rasterize(&e, SnapshotId::new(), &cancel, &context);
    parallel::force(old);
    assert_eq!(outcome.unwrap_err().kind, crate::ErrorKind::Cancelled);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        first.height as usize,
        "cancellation must stop before reading the next tap block"
    );
}

#[test]
fn warp_tap_scratch_stays_within_tap_block() {
    let r = registry();
    let source = image(800, 600, &vec![[0.2, 0.4, 0.6]; 800 * 600]);
    let p = recipe(vec![lens([-0.079, 0.0, 0.0]), perspective()]);
    let mut c = r.compile(800, 600, &p).unwrap();
    let Entry::Resample(entry) = c.segments[1].entry.as_mut().unwrap() else {
        panic!()
    };
    let Mapping::Warp(chain) = &mut entry.resample.map else {
        panic!()
    };
    // Admission refuses 2.6× minification. Stress the bounded primitive independently of
    // admission so even a more demanding future kernel cannot grow a whole-frame tap buffer.
    Arc::make_mut(chain)
        .steps
        .insert(0, WarpStep::Affine([2.6, 0.0, -640.0, 0.0, 2.6, -480.0]));
    let context = RenderContext::new();
    let cancel = Cancel::never();
    let e = Evaluation::new(
        linear::Linear::new(&source, LinearSettings::default()).unwrap(),
        Cow::Owned(c),
        spatial::Tiling::Halo,
        SpatialMode::Frames,
        &cancel,
        &context,
    )
    .unwrap();
    linear::rasterize(&e, SnapshotId::new(), &cancel, &context).unwrap();
    assert!(context.resample_peak_bytes() > 0);
    assert!(
        context.resample_peak_bytes()
            <= linear::TAP_BLOCK_PIXELS * std::mem::size_of::<[f64; 3]>() as u64
    );
}
#[test]
fn locate_through_warp_uses_floor_of_mapped_centre() {
    let r = registry();
    let p = fused(6000, 4000);
    let c = r.compile(6000, 4000, &p).unwrap();
    let map = transform_of(&c, 6000, 4000).unwrap();
    for (x, y) in [(0, 0), (99, 87), (1000, 750)] {
        let (u, v) = map.to_content(x as f64 + 0.5, y as f64 + 0.5).unwrap();
        let point = locate(&c, 6000, 4000, x, y).unwrap();
        assert_eq!(
            (point.content_x, point.content_y),
            (u.floor() as u32, v.floor() as u32)
        );
    }
}

#[test]
fn all_eight_orientation_actions_carry_lens_perspective_and_crop_together() {
    use crate::modules::{LayerEdit, StageQuestions};
    struct Questions<'a> {
        r: &'a crate::ModuleRegistry,
        p: &'a Recipe,
        w: u32,
        h: u32,
    }
    impl StageQuestions for Questions<'_> {
        fn stage_before(&self, index: usize) -> Result<Stage, Error> {
            let mut prefix = self.p.clone();
            prefix.layers.truncate(index);
            Ok(self.r.compile(self.w, self.h, &prefix)?.stage())
        }
        fn sample_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[u8; 4]>, Error> {
            panic!("carry reads no pixel")
        }
    }
    fn apply(r: &crate::ModuleRegistry, p: &mut Recipe, transform: Transform) {
        let module = crate::modules::CropModule::new();
        let input = module
            .parse(
                "transform",
                json!({"transform":transform.action_id()})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        let q = Questions {
            r,
            p,
            w: 180,
            h: 120,
        };
        let context = StageContext {
            layers: &p.layers,
            registry: r,
            target: None,
            kind: crate::SourceTag::Jpeg,
            masks: &[],
            questions: &q,
        };
        let plan = module.plan(&input, &context).unwrap();
        let edits = match plan {
            ActionPlan::Commit(n) => vec![LayerEdit::Commit(n)],
            ActionPlan::Update(u) => vec![LayerEdit::Update(u)],
            ActionPlan::Edits(e) => e,
            _ => panic!("orientation requires edits"),
        };
        for edit in edits {
            match edit {
                LayerEdit::Update(u) => {
                    p.layers.iter_mut().find(|l| l.id == u.id).unwrap().payload = u.payload
                }
                LayerEdit::Commit(n) => {
                    let effect = r.effect(&n.effect_id).unwrap().1;
                    let index = r.insertion_index(&p.layers, effect.stage, effect.order);
                    p.layers.insert(index, layer(&n.effect_id, n.payload));
                }
            }
        }
    }
    let r = registry();
    let source = gradient(180, 120);
    let linear_source: Vec<[f64; 3]> = source
        .rgba
        .chunks_exact(4)
        .map(|p| std::array::from_fn(|i| luxforge_reference::srgb::decode(p[i])))
        .collect();
    for angle in [0.0, 2.5] {
        let original = recipe(vec![
            lens([-0.079, 0.0, 0.0]),
            perspective(),
            Layer::crop(fitted_crop(180, 120, angle, [0.12, 0.12, 0.7, 0.65])),
        ]);
        let base = testing::render(&r, &source, SnapshotId::new(), &original).unwrap();
        let base_map = stage_transform(&r, 180, 120, &original).unwrap();
        let crop: crate::CropPayload =
            serde_json::from_value(original.layers[2].payload.clone()).unwrap();
        let crop_stage = crate::CropStage {
            width: 180,
            height: 120,
            angle,
        };
        let rect = crop.output_rect(&crop_stage).unwrap();
        let lens_id = original.layers[0].id.clone();
        let crop_id = original.layers[2].id.clone();
        for mirror in [false, true] {
            for turns in 0..4 {
                let o = crate::Orientation { mirror, turns };
                let mut p = original.clone();
                if mirror {
                    apply(&r, &mut p, Transform::MirrorHorizontal);
                }
                for _ in 0..turns {
                    apply(&r, &mut p, Transform::RotateRight);
                }
                assert_eq!(
                    p.layers.iter().find(|l| l.effect_id == LENS).unwrap().id,
                    lens_id
                );
                assert_eq!(
                    p.layers
                        .iter()
                        .find(|l| l.effect_id == CROP_EFFECT)
                        .unwrap()
                        .id,
                    crop_id
                );
                let actual = testing::render(&r, &source, SnapshotId::new(), &p).unwrap();
                let transformed_source = crate::SourceImage {
                    width: base.width,
                    height: base.height,
                    rgba: base.rgba.clone(),
                    fingerprint: "test".into(),
                    orientation: 1,
                    capture: Default::default(),
                };
                let expected = testing::render(
                    &r,
                    &transformed_source,
                    SnapshotId::new(),
                    &recipe(vec![Layer::orientation(o)]),
                )
                .unwrap();
                assert_eq!(
                    (actual.width, actual.height),
                    (expected.width, expected.height)
                );
                if angle == 0.0 {
                    for (a, b) in actual.rgba.iter().zip(expected.rgba.iter()) {
                        assert!(a.abs_diff(*b) <= 1, "{o:?}: {a} differs from {b}");
                    }
                }
                // Straightening preserves an integer crop rectangle. When the rotated box has
                // fractional extents, carry places its origin at the nearest covered whole pixel.
                // Compare against that declared placement, then measure the sampling phase shift.
                let mut exact = (
                    rect.x as f64,
                    rect.y as f64,
                    rect.width as f64,
                    rect.height as f64,
                );
                let (mut bw, mut bh) = crop_stage.bounding_box();
                if mirror {
                    exact.0 = bw - exact.0 - exact.2;
                }
                for _ in 0..turns {
                    exact = (bh - exact.1 - exact.3, exact.0, exact.3, exact.2);
                    (bw, bh) = (bh, bw);
                }
                let carried: crate::CropPayload = serde_json::from_value(
                    p.layers
                        .iter()
                        .find(|l| l.id == crop_id)
                        .unwrap()
                        .payload
                        .clone(),
                )
                .unwrap();
                let (cw, ch) = if turns % 2 == 0 {
                    (180, 120)
                } else {
                    (120, 180)
                };
                let placed = carried
                    .output_rect(&crate::CropStage {
                        width: cw,
                        height: ch,
                        angle: carried.angle,
                    })
                    .unwrap();
                let shift = (placed.x as f64 - exact.0, placed.y as f64 - exact.1);
                assert!(
                    shift.0.abs() <= 0.500000001 && shift.1.abs() <= 0.500000001,
                    "{o:?}: {shift:?}"
                );
                let output_orientation = stage_transform(
                    &r,
                    base.width,
                    base.height,
                    &recipe(vec![Layer::orientation(o)]),
                )
                .unwrap();
                let actual_map = stage_transform(&r, 180, 120, &p).unwrap();
                let scale = match &base_map.mapping {
                    super::map::MappingShape::Warp {
                        local_scale_max, ..
                    } => *local_scale_max,
                    _ => panic!(),
                };
                for y in 0..actual.height {
                    for x in 0..actual.width {
                        let point = (x as f64 + 0.5, y as f64 + 0.5);
                        let old = output_orientation
                            .to_content(point.0 + shift.0, point.1 + shift.1)
                            .unwrap();
                        let want = base_map.to_content(old.0, old.1).unwrap();
                        let got = actual_map.to_content(point.0, point.1).unwrap();
                        assert!(
                            (want.0 - got.0).hypot(want.1 - got.1) < 1e-9,
                            "{o:?} angle {angle}: {want:?} != {got:?}"
                        );
                        let original_point =
                            output_orientation.to_content(point.0, point.1).unwrap();
                        let original_sample = base_map
                            .to_content(original_point.0, original_point.1)
                            .unwrap();
                        assert!(
                            (got.0 - original_sample.0).hypot(got.1 - original_sample.1)
                                <= shift.0.hypot(shift.1) * scale + 1e-9
                        );
                        let rgb = luxforge_reference::geometry::bilinear_linear(
                            &linear_source,
                            180,
                            120,
                            [want.0, want.1],
                        );
                        let offset = ((y * actual.width + x) * 4) as usize;
                        for (i, channel) in rgb.iter().enumerate() {
                            let code = luxforge_reference::srgb::code(*channel);
                            assert!(
                                actual.rgba[offset + i].abs_diff(code) <= 1,
                                "{o:?} angle {angle}: single sample differs at {x},{y}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn lens_proxy_map_equals_scaled_full_map_within_rounding() {
    let full = WarpStep::radial(
        RadialModel::PtLens,
        [0.019, -0.056, 0.063],
        0.9988157513549764,
        Stage {
            width: 6048,
            height: 4024,
        },
    )
    .unwrap();
    let proxy = WarpStep::radial(
        RadialModel::PtLens,
        [0.019, -0.056, 0.063],
        0.9988157513549764,
        Stage {
            width: 1008,
            height: 671,
        },
    )
    .unwrap();
    let a = Mapping::Warp(Arc::new(super::map::WarpChain::new(full)));
    let b = Mapping::Warp(Arc::new(super::map::WarpChain::new(proxy)));
    for y in 0..671 {
        for x in (0..1008).step_by(13) {
            let (ox, oy) = (x as f64 + 0.5, y as f64 + 0.5);
            let (u, v) = a.input_at(ox * 6.0, oy * 4024.0 / 671.0);
            let (q, r) = b.input_at(ox, oy);
            assert!((u / 6.0 - q).abs() < 0.5);
            assert!((v * 671.0 / 4024.0 - r).abs() < 0.5);
        }
    }
}

#[test]
fn vignette_uses_final_output_coordinates_after_warps() {
    use luxforge_reference::{
        srgb,
        vignette::{VignetteParams, apply, mask},
    };
    let r = crate::ModuleRegistry::builtin();
    let parameters = VignetteParams {
        amount: -65.0,
        midpoint: 37.0,
        roundness: -35.0,
        feather: 70.0,
    };
    let p = recipe(vec![
        testing::frozen_lens(180, 120, 24.0),
        perspective(),
        Layer::crop(fitted_crop(180, 120, 2.5, [0.2, 0.15, 0.6, 0.65])),
        layer(
            crate::VIGNETTE_EFFECT,
            json!({"amount":parameters.amount,"midpoint":parameters.midpoint,
            "roundness":parameters.roundness,"feather":parameters.feather}),
        ),
    ]);
    let byte = crate::SourceImage {
        width: 180,
        height: 120,
        rgba: (0..180 * 120)
            .flat_map(|_| [128, 96, 64, 255])
            .collect::<Vec<_>>()
            .into(),
        fingerprint: "vignette-warp".into(),
        orientation: 1,
        capture: Default::default(),
    };
    let linear = image(180, 120, &vec![[0.2, 0.3, 0.4]; 180 * 120]);
    for (frame, input) in [
        (
            testing::render(&r, &byte, SnapshotId::new(), &p).unwrap(),
            [srgb::decode(128), srgb::decode(96), srgb::decode(64)],
        ),
        (
            testing::render_linear(
                &r,
                &linear,
                SnapshotId::new(),
                &p,
                LinearSettings::default(),
            )
            .unwrap(),
            [0.2, 0.3, 0.4],
        ),
    ] {
        assert!(frame.width < 180 && frame.height < 120);
        let mut separates_wrong_stage = 0;
        for y in 0..frame.height {
            for x in 0..frame.width {
                let expected = apply(
                    input,
                    mask(x, y, frame.width, frame.height, &parameters),
                    parameters.amount,
                );
                let wrong = apply(input, mask(x, y, 180, 120, &parameters), parameters.amount);
                let actual = frame.pixel(x, y).unwrap();
                for (channel, actual_channel) in actual.iter().take(3).enumerate() {
                    assert!(
                        actual_channel.abs_diff(srgb::code(expected[channel])) <= 1,
                        "{x},{y} channel {channel}: {} != {}",
                        actual_channel,
                        srgb::code(expected[channel])
                    );
                    if srgb::code(expected[channel]).abs_diff(srgb::code(wrong[channel])) > 2 {
                        separates_wrong_stage += 1;
                    }
                }
            }
        }
        assert!(
            separates_wrong_stage > 1000,
            "fixture must expose use of the pre-warp canvas"
        );
    }
}
