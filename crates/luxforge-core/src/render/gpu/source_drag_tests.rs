//! The drags the GPU draws from the source that once named `boundary-stage`
//! (`docs/design/gpu-preview.md`, "A tick"; `docs/design/gpu-first.md`, stage 5): a RAW
//! white-balance drag, drawn over the source the photo surface holds — the planes its entry
//! developed — with the change as a leading pointwise step, the matrix the drafted preview's
//! evaluation approximates it by; and a geometry drag on a stack of geometry alone, whose
//! geometry is the plan's tail.
use super::{GpuAnswer, GpuPlan, GpuPreview, GpuView, interpret, plan_preview, plan_rest};
use crate::{
    AssetId, BASIC_EFFECT, Cancel, Draft, DraftStamp, EntryId, Evaluation, HistoryEntry, Layer,
    LayerId, LinearSettings, ModuleRegistry, PERSPECTIVE_EFFECT, PreviewSource, ProxyBounds,
    RECIPE_FORMAT, RawPayload, Recipe, RenderContext, RenderOptions, Snapshot, SnapshotId,
    WhiteBalanceApproximation, WhiteBalanceMode,
    modules::{Region, basic::WHITE_BALANCE_PROGRAM},
    render::tests::{fitted_crop, varied},
};
use serde_json::{Value, json};
use std::sync::Arc;

const WIDTH: u32 = 96;
const HEIGHT: u32 = 64;

/// A calibration with a usable inverse, for a payload's validation.
const CAM_XYZ: [[f32; 3]; 4] = [
    [0.9, 0.1, 0.0],
    [0.1, 0.8, 0.1],
    [0.0, 0.2, 0.9],
    [0.0, 0.0, 0.0],
];
const AS_SHOT: [f32; 3] = [2.0, 1.0, 1.5];

/// The RAW development's layer: As shot, or explicit custom gains.
fn raw(id: &LayerId, gains: Option<[f32; 3]>) -> Layer {
    let mut payload = RawPayload::for_as_shot(AS_SHOT, CAM_XYZ).expect("an As shot payload");
    if let Some(gains) = gains {
        payload.wb_mode = WhiteBalanceMode::Custom;
        payload.gains = gains;
    }
    payload.layer(id.clone())
}

fn basic(payload: Value) -> Layer {
    Layer::new(BASIC_EFFECT, payload)
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        ..Recipe::default()
    }
}

/// The approximation a drafted RAW preview's evaluation carries when its gains differ from the
/// planes'.
fn balance() -> WhiteBalanceApproximation {
    WhiteBalanceApproximation::from_matrix([
        [1.21, 0.06, -0.03],
        [0.02, 0.97, 0.01],
        [-0.05, 0.03, 0.78],
    ])
    .unwrap()
}

/// The planes a RAW's entry developed, as the photo surface holds them.
fn planes() -> crate::LinearImage {
    varied(WIDTH, HEIGHT)
}

/// The evaluation of `drafted` over an entry holding `entry` on `source`, as `action`'s draft's
/// job sees it, or the committed stack's own with no action.
fn evaluation(
    source: PreviewSource,
    entry: Vec<Layer>,
    drafted: Vec<Layer>,
    action: Option<&str>,
) -> (Evaluation, Option<Draft>) {
    let asset = AssetId::new();
    let draft = action.map(|action| {
        let mut draft = Draft::new(action, asset.clone(), 1);
        draft.draft_revision = 2;
        draft
    });
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "set-raw".into(),
        label: "White balance".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset,
            recipe: recipe(entry),
        },
        undo_parent: None,
        restore_target: None,
    };
    let evaluation = Evaluation::new(
        Arc::new(ModuleRegistry::builtin()),
        RenderContext::new(),
        source,
        entry,
        recipe(drafted),
        draft.as_ref().map(|draft| DraftStamp {
            draft_id: draft.draft_id.clone(),
            draft_revision: draft.draft_revision,
        }),
    );
    (evaluation, draft)
}

/// A RAW source over `image`, under `white_balance`.
fn raw_source(
    image: &crate::LinearImage,
    white_balance: Option<WhiteBalanceApproximation>,
) -> PreviewSource {
    PreviewSource::Raw {
        image: image.clone(),
        settings: LinearSettings { white_balance },
    }
}

fn planned(preview: &GpuPreview) -> &GpuPlan {
    match &preview.answer {
        GpuAnswer::Plan(plan) => plan,
        GpuAnswer::Fallback(reason) => panic!("expected a plan, got {reason}"),
    }
}

/// The views a drag is drawn at: Fit at a reduced stage, the exact stage at Fit, and a region at
/// 100%.
fn views() -> [(&'static str, GpuView); 3] {
    [
        (
            "Fit",
            GpuView::Fit(ProxyBounds {
                width: 48,
                height: 32,
            }),
        ),
        (
            "the exact stage at Fit",
            GpuView::Fit(ProxyBounds {
                width: 4096,
                height: 4096,
            }),
        ),
        (
            "100%",
            GpuView::Region {
                rect: Region {
                    x0: 10,
                    y0: 8,
                    width: 40,
                    height: 30,
                },
                magnification: 1.0,
            },
        ),
    ]
}

/// A RAW white-balance drag at Fit, at the exact stage and at 100% is a plan from the source: its
/// first step is the drafted approximation's matrix, narrowed to `f32`, over each source texel,
/// reported under the RAW layer, and its boundary is the one the picture at rest of the entry
/// holds, the source the surface already has. Without an approximation, a drag back to the planes'
/// own white balance, the plan has no such step.
#[test]
fn a_raw_white_balance_drag_is_a_leading_step_over_the_source() {
    let id = LayerId::new();
    let entry = vec![raw(&id, None), basic(json!({"exposure": 0.2}))];
    let drafted = vec![
        raw(&id, Some([1.6, 1.0, 1.9])),
        basic(json!({"exposure": 0.2})),
    ];
    let image = planes();
    let words: Vec<u32> = balance()
        .matrix()
        .iter()
        .flatten()
        .map(|value| (*value as f32).to_bits())
        .collect();
    for (what, view) in views() {
        let (dragged, draft) = evaluation(
            raw_source(&image, Some(balance())),
            entry.clone(),
            drafted.clone(),
            Some("set-raw"),
        );
        let preview = plan_preview(&dragged, draft.as_ref().unwrap(), view).unwrap();
        let plan = planned(&preview);
        assert!(plan.linear, "{what}: the linear path");
        let first = &plan.content[0];
        assert_eq!(first.layer, 0, "{what}: reported under the RAW layer");
        assert!(first.mask.is_none(), "{what}");
        assert_eq!(first.units.len(), 1, "{what}");
        assert!(
            std::ptr::eq(first.units[0].program, &WHITE_BALANCE_PROGRAM),
            "{what}: Basic's white-balance program"
        );
        assert_eq!(first.units[0].words, words, "{what}: the matrix, narrowed");
        assert_eq!(plan.content.len(), 2, "{what}: then the Basic layer");
        let (rest, _) = evaluation(raw_source(&image, None), entry.clone(), entry.clone(), None);
        let rest = plan_rest(&rest, view).unwrap().view;
        assert_eq!(
            preview.boundary.as_ref().map(|boundary| &boundary.key),
            rest.boundary.as_ref().map(|boundary| &boundary.key),
            "{what}: the source the picture at rest holds"
        );
        // Back at the planes' own white balance: no step.
        let (back, draft) = evaluation(
            raw_source(&image, None),
            entry.clone(),
            drafted.clone(),
            Some("set-raw"),
        );
        let preview = plan_preview(&back, draft.as_ref().unwrap(), view).unwrap();
        let plan = planned(&preview);
        assert_eq!(plan.content.len(), 1, "{what}: the Basic layer alone");
        assert!(
            !std::ptr::eq(plan.content[0].units[0].program, &WHITE_BALANCE_PROGRAM),
            "{what}"
        );
    }
}

/// Behind Dehaze, whose light the change reaches, every light link computes its light from the
/// source with the change applied first, as its stand-in does.
#[test]
fn a_raw_white_balance_drag_lights_dehaze_from_the_changed_source() {
    let id = LayerId::new();
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 30}));
    let entry = vec![raw(&id, None), presence.clone()];
    let drafted = vec![raw(&id, Some([1.6, 1.0, 1.9])), presence];
    let image = planes();
    for (what, view) in views() {
        let (dragged, draft) = evaluation(
            raw_source(&image, Some(balance())),
            entry.clone(),
            drafted.clone(),
            Some("set-raw"),
        );
        let preview = plan_preview(&dragged, draft.as_ref().unwrap(), view).unwrap();
        let plan = planned(&preview);
        assert!(plan.reads_lights(), "{what}: Dehaze reads a light");
        for light in &plan.lights {
            assert!(
                std::ptr::eq(light.content[0].units[0].program, &WHITE_BALANCE_PROGRAM),
                "{what}: the light's input starts with the change"
            );
            if let Some(stand_in) = &light.stand_in {
                assert!(
                    std::ptr::eq(stand_in.content[0].units[0].program, &WHITE_BALANCE_PROGRAM),
                    "{what}: so does its stand-in's"
                );
            }
        }
    }
}

/// The plan's frame at the exact stage, run by the reference executor over the developed planes, is
/// the CPU's drafted frame, which applies the approximation to each source pixel in `f64`: within a
/// code, through Basic and a straightened crop's resample.
#[test]
fn a_raw_white_balance_drag_draws_the_cpus_approximate_frame() {
    let id = LayerId::new();
    let crop = Layer::crop(fitted_crop(WIDTH, HEIGHT, 5.0, [0.1, 0.1, 0.7, 0.7]));
    let entry = vec![
        raw(&id, None),
        basic(json!({"exposure": 0.3})),
        crop.clone(),
    ];
    let drafted = vec![
        raw(&id, Some([1.6, 1.0, 1.9])),
        basic(json!({"exposure": 0.3})),
        crop,
    ];
    let image = planes();
    let (dragged, draft) = evaluation(
        raw_source(&image, Some(balance())),
        entry,
        drafted.clone(),
        Some("set-raw"),
    );
    let view = GpuView::Fit(ProxyBounds {
        width: 4096,
        height: 4096,
    });
    let preview = plan_preview(&dragged, draft.as_ref().unwrap(), view).unwrap();
    let plan = planned(&preview);
    let texels: Vec<[f32; 3]> = (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| image.pixel(x, y).expect("inside the planes"))
        .collect();
    let registry = ModuleRegistry::builtin();
    let compiled = registry
        .compile(WIDTH, HEIGHT, &recipe(drafted.clone()))
        .unwrap();
    let gpu = interpret::execute(plan, &texels, &compiled.colour_operations()).unwrap();
    let context = RenderContext::new();
    let cpu = crate::render(
        &registry,
        crate::RenderSource::Linear {
            image: &image,
            settings: LinearSettings {
                white_balance: Some(balance()),
            },
        },
        &recipe(drafted),
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .unwrap()
    .frame(SnapshotId::new())
    .unwrap();
    assert_eq!(cpu.rgba.len(), gpu.len(), "one output stage");
    let largest = cpu
        .rgba
        .iter()
        .zip(&gpu)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(
        largest <= 1,
        "the plan draws {largest} codes from the CPU's"
    );
}

/// A gradient JPEG source.
fn jpeg() -> PreviewSource {
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgba.extend([
                (x * 251 / WIDTH) as u8,
                (y * 241 / HEIGHT) as u8,
                ((x * 7 + y * 3) % 256) as u8,
                255,
            ]);
        }
    }
    PreviewSource::Jpeg(crate::SourceImage {
        width: WIDTH,
        height: HEIGHT,
        rgba: rgba.into(),
        fingerprint: "sha256:geometry-only".into(),
        orientation: 1,
        capture: Default::default(),
    })
}

/// A Perspective drag on a photograph whose stack holds nothing but geometry — none at all, or a
/// straightened crop — is a plan from the source at every view, its geometry the tail, over the
/// boundary the picture at rest holds: no `boundary-stage`.
#[test]
fn a_geometry_drag_on_a_stack_of_geometry_alone_is_planned_from_the_source() {
    let perspective = |horizontal: i64| {
        Layer::new(
            PERSPECTIVE_EFFECT,
            json!({"horizontal": horizontal, "vertical": -10}),
        )
    };
    let crop = Layer::crop(fitted_crop(WIDTH, HEIGHT, 4.0, [0.1, 0.1, 0.8, 0.8]));
    for (stack, entry, drafted) in [
        ("an empty stack", vec![], vec![perspective(20)]),
        (
            "a straightened crop",
            vec![perspective(10), crop.clone()],
            vec![perspective(25), crop],
        ),
    ] {
        for (what, view) in views() {
            let (dragged, draft) = evaluation(
                jpeg(),
                entry.clone(),
                drafted.clone(),
                Some("set-perspective"),
            );
            let preview = plan_preview(&dragged, draft.as_ref().unwrap(), view).unwrap();
            let plan = planned(&preview);
            assert_eq!(plan.boundary.layer, 0, "{stack} at {what}: from the source");
            assert!(
                plan.geometry.projective().is_some() || plan.geometry.needs_grid(),
                "{stack} at {what}: the perspective is the tail"
            );
            assert!(plan.content.is_empty(), "{stack} at {what}: no content");
            let (rest, _) = evaluation(jpeg(), drafted.clone(), drafted.clone(), None);
            let rest = plan_rest(&rest, view).unwrap().view;
            assert_eq!(
                preview.boundary.as_ref().map(|boundary| &boundary.key),
                rest.boundary.as_ref().map(|boundary| &boundary.key),
                "{stack} at {what}: the boundary its picture at rest holds"
            );
        }
    }
}

/// A RAW's warm list holds a white-balance drag: the stack's plan with the change's program first,
/// so a drag's first tick finds its sequence compiled.
#[test]
fn a_raw_warm_list_holds_a_white_balance_drag() {
    let id = LayerId::new();
    let entry = vec![raw(&id, None), basic(json!({"exposure": 0.2}))];
    let image = planes();
    let (committed, _) = evaluation(raw_source(&image, None), entry.clone(), entry, None);
    let warm = super::plan_warm_list(
        &committed,
        GpuView::Fit(ProxyBounds {
            width: 48,
            height: 32,
        }),
    )
    .unwrap();
    assert!(
        warm.plans[..warm.open]
            .iter()
            .any(|plan| plan
                .content
                .first()
                .is_some_and(|first| first.units.len() == 1
                    && std::ptr::eq(first.units[0].program, &WHITE_BALANCE_PROGRAM)
                    && first.layer == 0)),
        "a white-balance drag among the open stack's plans"
    );
    // A JPEG's holds none.
    let (jpeg, _) = evaluation(
        jpeg(),
        vec![basic(json!({"exposure": 0.2}))],
        vec![basic(json!({"exposure": 0.2}))],
        None,
    );
    let warm = super::plan_warm_list(
        &jpeg,
        GpuView::Fit(ProxyBounds {
            width: 48,
            height: 32,
        }),
    )
    .unwrap();
    assert!(!warm.plans.iter().any(|plan| {
        plan.content.first().is_some_and(|first| {
            first.units.len() == 1 && std::ptr::eq(first.units[0].program, &WHITE_BALANCE_PROGRAM)
        })
    }));
}
