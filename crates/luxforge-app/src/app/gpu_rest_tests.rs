//! The picture at rest in tiles (`docs/design/gpu-preview.md`, "The picture at rest"): a stage drawn
//! on the GPU tile by tile — each tile the whole stack's plan over its rectangle of the output
//! stage, from its own window of the source anchored to the plan, through its part of a lens warp's
//! grid of the whole stage — is the same stage drawn as one region, bit for bit, every scratch
//! plane of every link starting from NaN, for every family whose texels depend on more than their
//! own pixel: Presence's running sums over its reductions, Detail's neighbourhoods, a lens warp's
//! grid and a perspective warp's homography. So tiles carry no seam, and a pixel read back from one
//! tile is the pixel any other window of the stage draws.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing.
use super::{
    gpu_plan::{WarpGrid, surface_plan_over},
    gpu_qualification::headless,
    gpu_window_tests::{HEIGHT, WIDTH, cut, source, whole},
};
use luxforge_core::{
    BASIC_EFFECT, BoundaryFormat, Cancel, DETAIL_EFFECT, EstimateSource, GpuAnswer, GpuEstimates,
    GpuPlanRequest, Layer, ModuleRegistry, PERSPECTIVE_EFFECT, PRESENCE_EFFECT, Recipe, Region,
    RenderContext, RenderOptions, SnapshotId, Stage, anchored, gpu_plan_with, render,
};
use luxforge_ui::photo_surface::gpu_preview::qualification::boundary_as;

/// The tiles' side: twelve of them over the 360 × 240 stage, the last column and row narrower.
const SIDE: u32 = 96;

#[test]
fn gpu_rest_a_stage_in_tiles_is_the_stage_in_one_region_bit_for_bit() {
    let Some(qualifier) =
        headless("gpu_rest_a_stage_in_tiles_is_the_stage_in_one_region_bit_for_bit")
    else {
        return;
    };
    qualifier.set_poison(true);
    let registry = ModuleRegistry::builtin();
    let basic = Layer::new(
        BASIC_EFFECT,
        serde_json::json!({"exposure": 0.4, "contrast": 20.0}),
    );
    let families: [(&str, Vec<Layer>); 6] = [
        (
            "Texture",
            vec![Layer::new(
                PRESENCE_EFFECT,
                serde_json::json!({"texture": 35.0}),
            )],
        ),
        (
            "Presence",
            vec![Layer::new(
                PRESENCE_EFFECT,
                serde_json::json!({"texture": 35.0, "clarity": 30.0, "dehaze": 20.0}),
            )],
        ),
        (
            "Detail",
            vec![Layer::new(
                DETAIL_EFFECT,
                serde_json::json!({"sharpening": 60.0, "luminance": 40.0, "colour": 40.0}),
            )],
        ),
        (
            "lens",
            vec![
                basic.clone(),
                luxforge_core::qualification::lens_layer(-0.06, (WIDTH, HEIGHT)),
            ],
        ),
        (
            "perspective",
            vec![
                basic.clone(),
                Layer::new(
                    PERSPECTIVE_EFFECT,
                    serde_json::json!({"horizontal": 25, "vertical": -15}),
                ),
            ],
        ),
        (
            "Presence after Detail",
            vec![
                Layer::new(DETAIL_EFFECT, serde_json::json!({"sharpening": 50.0})),
                Layer::new(
                    PRESENCE_EFFECT,
                    serde_json::json!({"texture": 20.0, "clarity": 25.0}),
                ),
            ],
        ),
    ];
    let stage = Stage {
        width: WIDTH,
        height: HEIGHT,
    };
    for format in [BoundaryFormat::Half, BoundaryFormat::Float] {
        let source = source(format);
        let pixels = whole(&source);
        for (family, layers) in &families {
            let name = format!("{format:?} {family}");
            let recipe = Recipe {
                layers: layers.clone(),
                ..Recipe::default()
            };
            // The exact render, whose frame stores the global estimates every tile's plan reads.
            let context = RenderContext::new();
            let exact = render(
                &registry,
                &source,
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .expect("the exact render");
            exact.frame(SnapshotId::new()).expect("the exact frame");
            let request = GpuPlanRequest::exact(0, stage).qualifying();
            let request = match format {
                BoundaryFormat::Float => request.linear(),
                BoundaryFormat::Half => request,
            };
            let estimates = GpuEstimates {
                context: &context,
                source: EstimateSource::Render((&source).into()),
            };
            let plan = match gpu_plan_with(&registry, &recipe, request, Some(estimates))
                .expect("a stack")
            {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => panic!("{name}: {reason}"),
            };
            assert!(!plan.approximate(), "{name}: the stored estimates");
            let anchor = plan.anchor();
            let output = plan.geometry.output();
            let stage_grid = plan.geometry.stage_grid(1.0).expect("a stage grid");
            let draw = |rect: Region, window: Region| {
                let held = boundary_as(
                    super::gpu_plan::boundary_format(format),
                    window.width,
                    window.height,
                    1,
                    &cut(&pixels, window),
                )
                .expect("a boundary");
                let grid = stage_grid
                    .as_ref()
                    .map(|grid| WarpGrid::new(&grid.part(rect).expect("the tile's part")));
                let converted = surface_plan_over(
                    &plan,
                    held,
                    (window.x0, window.y0),
                    grid.as_ref(),
                    Some(rect),
                )
                .expect("a runnable plan");
                qualifier
                    .evaluate_codes(&converted)
                    .unwrap_or_else(|error| panic!("{name}: {rect:?}: {error}"))
            };
            // The whole output stage as one region, over the whole source.
            let whole_rect = Region {
                x0: 0,
                y0: 0,
                width: output.width,
                height: output.height,
            };
            let reference = draw(
                whole_rect,
                Region {
                    x0: 0,
                    y0: 0,
                    width: WIDTH,
                    height: HEIGHT,
                },
            );
            assert_eq!(reference.len(), whole_rect.pixels() as usize, "{name}");
            let mut tiles = 0;
            for y0 in (0..output.height).step_by(SIDE as usize) {
                for x0 in (0..output.width).step_by(SIDE as usize) {
                    let rect = Region {
                        x0,
                        y0,
                        width: SIDE.min(output.width - x0),
                        height: SIDE.min(output.height - y0),
                    };
                    // The window the region reads, as the planner plans a tile's, anchored.
                    let frame = luxforge_core::qualification::region_boundary(
                        &exact,
                        0,
                        [rect.x0, rect.y0, rect.width, rect.height],
                        format,
                    )
                    .expect("the tile's window");
                    let window = anchored(
                        Region {
                            x0: frame.origin.0,
                            y0: frame.origin.1,
                            width: frame.width,
                            height: frame.height,
                        },
                        anchor,
                    );
                    let codes = draw(rect, window);
                    for y in 0..rect.height {
                        for x in 0..rect.width {
                            let tile = codes[(y * rect.width + x) as usize];
                            let whole = reference[((y0 + y) * output.width + x0 + x) as usize];
                            assert!(
                                tile == whole,
                                "{name}: the tile at ({x0}, {y0}), window {window:?}, draws \
                                 {tile:?} at ({x}, {y}) where the whole stage draws {whole:?}"
                            );
                        }
                    }
                    tiles += 1;
                }
            }
            assert!(tiles > 6, "{name}: {tiles} tiles");
        }
    }
}
