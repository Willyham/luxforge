//! The GPU plan against the CPU frame it previews: every stack shape of the render table, planned
//! from every boundary, either draws the CPU's picture through the reference executor or names its
//! reason; masks, supersampling, the output pass and warps are drawn the same way; and planning is
//! one compile that holds nothing that scales with the image.
use super::interpret;
use super::program::testing;
use super::{
    GpuAnswer, GpuDescription, GpuFallback, GpuPlan, GpuPlanRequest, GpuPosition, gpu_plan,
};
use crate::{
    ComponentMode, EffectStage, Layer, Mask, ModuleRegistry, Recipe, SnapshotId, SourceImage,
    Transform,
    colour::srgb::decode_table,
    mask::{CompiledMask, ComponentField},
    mask_field::MaskField,
    modules::{ColorOperation, Processing, Region, Stage},
    render::{
        Compiled, Render, RenderContext, RenderOptions, RenderSource,
        sample_tests::cases,
        testing::render,
        tests::{
            colour_layer, colour_recipe, colour_registry, exposure_layer, fitted_crop, gradient,
            positional_layer, scale_layer, turn,
        },
    },
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

/// A straightened crop of most of the `width` × `height` stage it receives.
fn crop(width: u32, height: u32, angle: f64) -> Layer {
    Layer::crop(fitted_crop(width, height, angle, [0.1, 0.12, 0.8, 0.75]))
}

fn answer(registry: &ModuleRegistry, recipe: &Recipe, request: GpuPlanRequest) -> GpuAnswer {
    gpu_plan(registry, recipe, request).unwrap()
}

fn planned(answer: GpuAnswer) -> GpuPlan {
    match answer {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("expected a plan, got {reason}"),
    }
}

/// The CPU's input of the plan's boundary layer, in linear light: the stack before it, rendered
/// and decoded as the next segment decodes it. A boundary inside a colour run holds the run's
/// unclamped value: when the run's segment has no exact step and no mask before the boundary, that
/// is the segment's input with the run's earlier units applied in `f32`, as the CPU applies them;
/// otherwise it is the quantized prefix, which the tolerance allows a code for.
fn boundary_texels(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    plan: &GpuPlan,
) -> Vec<[f32; 3]> {
    let compiled = registry
        .compile(source.width, source.height, recipe)
        .unwrap();
    let (segment, start) = compiled.layers[plan.boundary.layer];
    let operations = &compiled.segments[segment].operations;
    let run: Option<Vec<&ColorOperation>> = plan
        .boundary
        .continues_run
        .then(|| {
            let uncut = operations
                .iter()
                .all(|operation| !matches!(operation, Processing::ExactGeometry(_)));
            operations[..start]
                .iter()
                .map(|operation| match operation {
                    Processing::Color(colour) if uncut && colour.mask().is_none() => Some(colour),
                    _ => None,
                })
                .collect()
        })
        .flatten();
    // The run starts from its segment's input: the stack before the segment's first layer.
    let first = match &run {
        Some(_) => compiled
            .layers
            .partition_point(|&position| position < (segment, 0)),
        None => plan.boundary.layer,
    };
    let prefix = Recipe {
        layers: recipe.layers[..first].to_vec(),
        ..recipe.clone()
    };
    let input = render(registry, source, SnapshotId::new(), &prefix).unwrap();
    assert_eq!(
        stage(input.width, input.height),
        plan.boundary.stage,
        "the boundary is the stage its layer receives"
    );
    let table = decode_table();
    let mut texels: Vec<[f32; 3]> = input
        .rgba
        .chunks_exact(4)
        .map(|pixel| {
            [
                table[usize::from(pixel[0])],
                table[usize::from(pixel[1])],
                table[usize::from(pixel[2])],
            ]
        })
        .collect();
    for colour in run.into_iter().flatten() {
        for (y, row) in texels.chunks_mut(input.width as usize).enumerate() {
            for unit in colour.units() {
                unit.apply_row(y as u32, 0, row);
            }
        }
    }
    texels
}

/// The largest difference, in output codes, between two RGBA8 frames of one size.
fn largest_difference(left: &[u8], right: &[u8]) -> u8 {
    assert_eq!(left.len(), right.len(), "the frames differ in size");
    left.iter()
        .zip(right)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

/// How far the reference executor's frame may lie from the CPU's: nothing through a run of colour
/// units over an exact tail, whose arithmetic is the same `f32` in the same order; one code where
/// the CPU quantizes a frame the plan carries in float (a resample's input and output, a boundary
/// inside a colour run), holds a stage boundary's frame at 16 bits that these tests read back at 8
/// (`staged`), quantizes a spatial operation's output, or folds a mask in `f64`, and where a
/// perspective warp's homography is evaluated in `f32`, as the CPU's map is in `f64`; two through a
/// lens warp's grid.
fn tolerance(plan: &GpuPlan, staged: bool) -> u8 {
    if plan.geometry.needs_grid() {
        2
    } else if plan.geometry.clamps
        || plan.geometry.projective().is_some()
        || plan.boundary.continues_run
        || staged
        || !plan.spatial.is_empty()
        || plan.operations().any(|operation| operation.mask.is_some())
    {
        1
    } else {
        0
    }
}

/// The plan's frame, from the CPU's boundary, against the CPU frame of the whole recipe.
fn assert_draws_the_cpu_frame(
    registry: &ModuleRegistry,
    source: &SourceImage,
    recipe: &Recipe,
    plan: &GpuPlan,
    what: &str,
) {
    let cpu = render(registry, source, SnapshotId::new(), recipe).unwrap();
    let output = plan.geometry.output();
    assert_eq!(
        (cpu.width, cpu.height),
        (output.width, output.height),
        "{what}: the tail ends at the output stage"
    );
    let texels = boundary_texels(registry, source, recipe, plan);
    let compiled = registry
        .compile(source.width, source.height, recipe)
        .unwrap();
    let staged = compiled.layers[plan.boundary.layer].0 > 0;
    let gpu = interpret::execute_with(
        plan,
        &texels,
        &compiled.colour_operations(),
        &compiled.spatial_operations(),
    )
    .unwrap();
    let difference = largest_difference(&cpu.rgba, &gpu);
    assert!(
        difference <= tolerance(plan, staged),
        "{what}: the plan draws up to {difference} codes from the CPU frame"
    );
}

/// The acceptance table: every stack shape of the render table, from every boundary, with and
/// without disabled programs, answers either a plan whose operations are in recipe order from the
/// boundary and whose picture is the CPU frame, or a named reason.
#[test]
fn every_stack_shape_of_the_render_table_plans_in_recipe_order_or_names_a_reason() {
    let registry = colour_registry();
    let source = gradient(41, 29);
    let mut answers = BTreeMap::new();
    for case in cases() {
        let recipe = case.recipe();
        for boundary in 0..recipe.layers.len() {
            for qualifying in [false, true] {
                let mut request = GpuPlanRequest::exact(boundary, stage(41, 29));
                if qualifying {
                    request = request.qualifying();
                }
                let what = format!("{} from layer {boundary}", case.name);
                let code = match answer(&registry, &recipe, request) {
                    GpuAnswer::Plan(plan) => {
                        assert_eq!(plan.boundary.layer, boundary, "{what}");
                        let layers: Vec<usize> =
                            plan.operations().map(|operation| operation.layer).collect();
                        assert!(
                            layers.windows(2).all(|pair| pair[0] < pair[1])
                                && layers.iter().all(|layer| *layer >= boundary),
                            "{what}: operations {layers:?} are not in recipe order"
                        );
                        for operation in plan.operations() {
                            assert!(
                                matches!(
                                    registry
                                        .effect_stage(&recipe.layers[operation.layer].effect_id),
                                    Some(EffectStage::Color | EffectStage::Finish)
                                ),
                                "{what}: layer {} is not pointwise colour",
                                operation.layer
                            );
                            assert!(!operation.units.is_empty(), "{what}");
                        }
                        assert_draws_the_cpu_frame(&registry, &source, &recipe, &plan, &what);
                        "plan"
                    }
                    GpuAnswer::Fallback(reason) => {
                        assert!(
                            reason
                                .layer()
                                .is_some_and(|layer| layer < recipe.layers.len()),
                            "{what}: {reason}"
                        );
                        reason.code()
                    }
                };
                answers.insert((case.name, boundary, qualifying), code);
            }
        }
    }
    // The answers that decide the stack shapes, by name.
    let expected = [
        (
            "replacements on either side of a colour run",
            1,
            false,
            "pixel-stage",
        ),
        (
            "a replacement behind an exact orientation",
            1,
            false,
            "pixel-stage",
        ),
        // Every colour unit of Basic, the mixer and the vignette has an enabled program.
        ("Basic and the colour mixer", 0, false, "plan"),
        ("Basic and the colour mixer", 0, true, "plan"),
        ("Basic and the colour mixer", 1, true, "plan"),
        (
            "colour after a straightened crop's resample",
            1,
            false,
            "boundary-stage",
        ),
        (
            "colour after a straightened crop's resample",
            2,
            false,
            "plan",
        ),
        (
            "colour after a straightened crop's resample",
            0,
            true,
            "plan",
        ),
        // Presence's program is enabled: the spatial operation joins the plan between the
        // content and the tail.
        ("colour after a spatial operation's frame", 0, false, "plan"),
        ("colour after a spatial operation's frame", 0, true, "plan"),
        ("colour after a spatial operation's frame", 2, true, "plan"),
        (
            "a spatial operation then a straightened crop",
            0,
            true,
            "plan",
        ),
        // The units and the linear mask component have enabled programs.
        ("a masked colour layer", 0, false, "plan"),
        ("a masked colour layer", 0, true, "plan"),
        // A masked Presence layer behind geometry plans, its mask with it.
        (
            "a masked spatial operation behind geometry",
            1,
            true,
            "plan",
        ),
        ("a positional unit alone", 0, false, "plan"),
        (
            "a positional unit after an exact rotation in its segment",
            0,
            false,
            "boundary-stage",
        ),
        (
            "a positional unit after an exact rotation in its segment",
            1,
            false,
            "plan",
        ),
        (
            "a positional unit after a crop resample, in output coordinates",
            1,
            false,
            "plan",
        ),
        ("a positional unit over the whole tail", 0, false, "plan"),
        ("a positional unit over the whole tail", 3, false, "plan"),
        ("two resamples", 3, true, "pixel-stage"),
        ("an upscale", 0, false, "pixel-stage"),
    ];
    for (name, boundary, qualifying, code) in expected {
        assert_eq!(
            answers.get(&(name, boundary, qualifying)),
            Some(&code),
            "{name} from layer {boundary}, qualifying {qualifying}"
        );
    }
    assert!(
        answers.values().filter(|code| **code == "plan").count() >= 5,
        "the table exercises plans as well as reasons"
    );
}

/// A positional unit's `pos` is the coordinate the CPU unit is handed, through every arrangement of
/// the tail: in content space before exact steps, after a resample in the output pass, and over a
/// whole tail of exact steps.
#[test]
fn positions_follow_the_cpu_addressing_through_every_pass() {
    let registry = colour_registry();
    let source = gradient(41, 29);
    let stacks = [
        (
            "content colour, a straightened crop, output colour",
            vec![
                exposure_layer(&[0.3]),
                crop(41, 29, 4.0),
                positional_layer(),
            ],
        ),
        (
            "content colour behind a turn, then a resample",
            vec![
                positional_layer(),
                turn(Transform::RotateRight),
                crop(29, 41, -6.0),
            ],
        ),
        (
            "output colour behind an exact turn after a resample",
            vec![
                exposure_layer(&[-0.2]),
                crop(41, 29, 3.0),
                positional_layer(),
                turn(Transform::MirrorHorizontal),
            ],
        ),
        (
            "colour on both sides of an exact turn",
            vec![
                exposure_layer(&[0.25]),
                turn(Transform::RotateLeft),
                positional_layer(),
                turn(Transform::MirrorHorizontal),
            ],
        ),
        (
            "an upscale then output colour",
            vec![exposure_layer(&[0.1]), scale_layer(1.5), positional_layer()],
        ),
        (
            "an exact crop read by an upscale, which replicates the crop's edge",
            vec![
                exposure_layer(&[0.3]),
                crop(41, 29, 0.0),
                scale_layer(1.5),
                positional_layer(),
            ],
        ),
    ];
    for (what, layers) in stacks {
        let recipe = colour_recipe(layers);
        for boundary in 0..recipe.layers.len() {
            match answer(
                &registry,
                &recipe,
                GpuPlanRequest::exact(boundary, stage(41, 29)),
            ) {
                GpuAnswer::Plan(plan) => {
                    let what = format!("{what}, from layer {boundary}");
                    assert_draws_the_cpu_frame(&registry, &source, &recipe, &plan, &what);
                }
                GpuAnswer::Fallback(reason) => assert_eq!(
                    reason,
                    GpuFallback::BoundaryStage {
                        layer: boundary,
                        stage: EffectStage::Geometry
                    },
                    "{what}"
                ),
            }
        }
    }
    // The output pass exists where a resample separates it, and its pixels are the output's.
    let recipe = colour_recipe(vec![
        exposure_layer(&[0.3]),
        crop(41, 29, 4.0),
        positional_layer(),
    ]);
    let plan = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(41, 29)),
    ));
    assert_eq!(plan.content.len(), 1);
    assert_eq!(plan.output.len(), 1);
    assert_eq!(plan.output[0].layer, 2);
    assert_eq!(plan.output[0].position, GpuPosition::IDENTITY);
    assert!(
        plan.geometry.clamps,
        "the CPU quantizes before the resample"
    );
}

/// A colour layer between two resamples has no pass; a spatial layer after the boundary whose
/// units have no program, or whose program is disabled, is named; a spatial layer's own input is
/// the frame its segment wrote; a boundary inside a colour run continues it.
#[test]
fn stack_shapes_the_plan_cannot_hold_are_named() {
    let registry = colour_registry();
    let between = colour_recipe(vec![
        exposure_layer(&[0.1]),
        crop(41, 29, 4.0),
        positional_layer(),
        scale_layer(1.5),
    ]);
    assert_eq!(
        answer(&registry, &between, GpuPlanRequest::exact(0, stage(41, 29))),
        GpuAnswer::Fallback(GpuFallback::BetweenResamples { layer: 2 })
    );
    // From the layer between them, its stage is the boundary and the plan holds.
    let plan = planned(answer(
        &registry,
        &between,
        GpuPlanRequest::exact(2, stage(41, 29)),
    ));
    assert_eq!((plan.content.len(), plan.output.len()), (1, 0));
    assert!(plan.geometry.clamps);
    let spatial = colour_recipe(vec![
        exposure_layer(&[0.5]),
        Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30.0})),
        positional_layer(),
    ]);
    // The operation follows the content operation at the boundary's stage, its input and output
    // clamped as the byte path quantizes them; the colour after it is the output pass's, over an
    // exact tail with nothing more to clamp.
    let plan = planned(answer(
        &registry,
        &spatial,
        GpuPlanRequest::exact(0, stage(41, 29)),
    ));
    let step = plan.spatial.first().expect("the spatial operation");
    assert_eq!((step.layer, step.applies.len()), (1, 1));
    assert!(step.clamps && !step.estimated && !plan.approximate());
    assert_eq!((plan.content.len(), plan.output.len()), (1, 1));
    assert!(!plan.geometry.clamps);
    // On the linear path nothing is clamped.
    let plan = planned(answer(
        &registry,
        &spatial,
        GpuPlanRequest::exact(0, stage(41, 29))
            .qualifying()
            .linear(),
    ));
    assert!(!plan.spatial.first().unwrap().clamps);
    // From the spatial layer itself, the boundary is the frame its segment wrote, not a colour
    // run's unclamped value.
    let plan = planned(answer(
        &registry,
        &spatial,
        GpuPlanRequest::exact(1, stage(41, 29)).qualifying(),
    ));
    assert!(plan.content.is_empty() && !plan.spatial.is_empty());
    assert!(!plan.boundary.continues_run);
    // A restoration layer after the boundary is its spatial operation: Detail's, one apply per
    // unit.
    let detail = colour_recipe(vec![
        exposure_layer(&[0.5]),
        Layer::new(
            crate::DETAIL_EFFECT,
            json!({"sharpening": 40.0, "luminance": 20.0}),
        ),
    ]);
    let plan = planned(answer(
        &registry,
        &detail,
        GpuPlanRequest::exact(0, stage(41, 29)),
    ));
    let step = plan.spatial.first().expect("Detail's operation");
    assert_eq!((step.layer, step.applies.len()), (1, 2));
    assert_eq!(step.program.entry, "lf_detail");
    // A second spatial layer after the first chains: Detail's operation, then Presence's.
    let mut both = detail.clone();
    both.layers
        .push(Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30.0})));
    let plan = planned(answer(
        &registry,
        &both,
        GpuPlanRequest::exact(0, stage(41, 29)).qualifying(),
    ));
    let layers: Vec<usize> = plan.spatial.iter().map(|step| step.layer).collect();
    assert_eq!(layers, [1, 2]);
    // From after the spatial layer, its output is the boundary and the plan holds.
    let plan = planned(answer(
        &registry,
        &spatial,
        GpuPlanRequest::exact(2, stage(41, 29)),
    ));
    assert!(!plan.boundary.continues_run);
    let run = colour_recipe(vec![exposure_layer(&[0.5]), positional_layer()]);
    let plan = planned(answer(
        &registry,
        &run,
        GpuPlanRequest::exact(1, stage(41, 29)),
    ));
    assert!(
        plan.boundary.continues_run,
        "the CPU hands the second layer the first one's unclamped value"
    );
    assert_eq!(plan.content.len(), 1);
    let unprogrammed = colour_recipe(vec![colour_layer(json!({"overflow": 1}))]);
    assert_eq!(
        answer(
            &registry,
            &unprogrammed,
            GpuPlanRequest::exact(0, stage(41, 29))
        ),
        GpuAnswer::Fallback(GpuFallback::NoProgram {
            layer: 0,
            unit: "overflow".into()
        })
    );
    let single = colour_recipe(vec![exposure_layer(&[1.0])]);
    assert!(
        gpu_plan(&registry, &single, GpuPlanRequest::exact(1, stage(41, 29))).is_err(),
        "a boundary outside the stack is an error"
    );
}

/// A disabled program is named, and planned only when qualifying, when it then draws the CPU
/// frame.
#[test]
fn a_disabled_program_is_named_and_planned_only_when_qualifying() {
    let registry = colour_registry();
    let recipe = colour_recipe(vec![
        colour_layer(json!({"disabled": [0.7]})),
        crop(41, 29, 5.0),
    ]);
    let request = GpuPlanRequest::exact(0, stage(41, 29));
    assert_eq!(
        answer(&registry, &recipe, request),
        GpuAnswer::Fallback(GpuFallback::DisabledProgram {
            layer: 0,
            program: "lf_test_disabled"
        })
    );
    let plan = planned(answer(&registry, &recipe, request.qualifying()));
    assert_eq!(plan.content[0].units[0].program.entry, "lf_test_disabled");
    assert_draws_the_cpu_frame(
        &registry,
        &gradient(41, 29),
        &recipe,
        &plan,
        "a disabled program, qualifying",
    );
}

/// A test mask component: a ramp across the stage, `(x + ½ + offset) / width`, whose program is
/// [`testing::RAMP`]. The kind table holds no such kind, so a test binds it itself.
#[derive(Debug)]
struct Ramp {
    height: f64,
    width: f64,
    offset: f64,
}

impl ComponentField for Ramp {
    fn coverage(&self, u: f64, _v: f64, _rgb: [f64; 3]) -> f64 {
        ((u * self.height + self.offset) / self.width).clamp(0.0, 1.0)
    }
    fn reads_pixels(&self) -> bool {
        false
    }
    fn support(&self, stage: Stage, _inverted: bool) -> Region {
        Region::whole(stage)
    }
    fn feature_px(&self, _stage: Stage) -> f64 {
        f64::INFINITY
    }
    fn gpu(&self, _stage: Stage) -> Option<GpuDescription> {
        Some(
            GpuDescription::new(&testing::RAMP, vec![(self.width as f32).to_bits()])
                .with_block(Arc::from([(self.offset as f32).to_bits()])),
        )
    }
}

/// Two ramps composed and inverted at an amount, bound to `stage`, scaled by `scale` for a
/// supersample's doubled stage.
fn ramps(stage: Stage, scale: f64) -> CompiledMask {
    let ramp = |offset: f64, width: f64| -> Arc<dyn ComponentField> {
        Arc::new(Ramp {
            height: f64::from(stage.height),
            width: width * scale,
            offset: offset * scale,
        })
    };
    let width = f64::from(stage.width) / scale;
    CompiledMask::from_fields(
        stage,
        vec![
            ("ramp", ComponentMode::Add, false, ramp(-4.0, width * 0.8)),
            (
                "ramp",
                ComponentMode::Subtract,
                true,
                ramp(-30.0, width * 0.5),
            ),
        ],
        true,
        80.0,
    )
}

/// `recipe` compiled exactly, with every masked colour operation's mask replaced by [`ramps`]
/// bound to the stage its layer receives, supersampled when asked.
fn with_ramps(registry: &ModuleRegistry, recipe: &Recipe, supersample: bool) -> Compiled {
    let mut compiled = registry.compile(41, 29, recipe).unwrap();
    for segment in &mut compiled.segments {
        for operation in &mut segment.operations {
            let Processing::Color(colour) = operation else {
                continue;
            };
            let Some(mask) = colour.mask() else {
                continue;
            };
            let stage = mask.stage();
            let field = if supersample {
                let doubled = Stage {
                    width: stage.width * 2,
                    height: stage.height * 2,
                };
                MaskField::with_fine(ramps(stage, 1.0), ramps(doubled, 2.0))
            } else {
                MaskField::point(ramps(stage, 1.0))
            };
            *operation =
                Processing::Color(ColorOperation::new(colour.units().to_vec()).with_mask(field));
        }
    }
    compiled
}

/// Masks are plan data: the components' programs, their modes and inversions, the whole mask's
/// inversion and amount, and the map to the mask's own stage, before and after exact steps and in
/// the output pass. Blended against the operation's input, they draw the CPU frame; supersampled,
/// they read the doubled stage's four pixels.
#[test]
fn masked_operations_carry_their_blend_and_draw_the_cpu_frame() {
    let registry = colour_registry();
    let source = gradient(41, 29);
    let mut mask = Mask::new("Mask 1");
    mask.components.push(crate::Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
    ));
    let masked = |layer: Layer| Layer {
        mask: Some(mask.id.clone()),
        ..layer
    };
    let stacks = [
        vec![masked(positional_layer())],
        vec![
            exposure_layer(&[0.2]),
            turn(Transform::RotateRight),
            masked(exposure_layer(&[0.6])),
            turn(Transform::MirrorHorizontal),
        ],
        vec![
            exposure_layer(&[0.2]),
            crop(41, 29, 4.0),
            masked(positional_layer()),
            turn(Transform::RotateRight),
        ],
    ];
    for (index, layers) in stacks.into_iter().enumerate() {
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..colour_recipe(layers)
        };
        // The production answer: the linear component's program is enabled, so the stack is
        // planned and the plan draws the CPU frame, its coverage the CPU field's.
        let masked_layer = recipe
            .layers
            .iter()
            .position(|layer| layer.mask.is_some())
            .unwrap();
        let plan = planned(answer(
            &registry,
            &recipe,
            GpuPlanRequest::exact(0, stage(41, 29)),
        ));
        let blend = plan
            .operations()
            .find(|operation| operation.layer == masked_layer)
            .and_then(|operation| operation.mask.as_ref())
            .expect("the masked layer's blend");
        assert_eq!(blend.components[0].program.program.entry, "lf_mask_linear");
        assert_draws_the_cpu_frame(
            &registry,
            &source,
            &recipe,
            &plan,
            &format!("stack {index}, the linear component"),
        );
        for supersample in [false, true] {
            let what = format!("stack {index}, supersampled {supersample}");
            let compiled = with_ramps(&registry, &recipe, supersample);
            let plan = match compiled
                .gpu_plan(
                    0,
                    stage(41, 29),
                    Some(EffectStage::Color),
                    Default::default(),
                )
                .unwrap()
            {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => panic!("{what}: {reason}"),
            };
            let operation = plan
                .operations()
                .find(|operation| operation.layer == masked_layer)
                .unwrap();
            let blend = operation.mask.as_ref().expect("the masked layer");
            assert_eq!(blend.components.len(), 2, "{what}");
            assert_eq!(blend.components[1].mode, ComponentMode::Subtract);
            assert!(blend.components[1].invert && !blend.components[0].invert);
            assert!(blend.invert, "{what}");
            assert_eq!(blend.scale, 0.8_f32, "{what}");
            assert_eq!(blend.supersample, supersample, "{what}");
            let units = compiled.colour_operations();
            let context = RenderContext::new();
            let cpu = Render::compiled(
                RenderSource::Byte(&source),
                compiled,
                RenderOptions::default(),
                &context,
            )
            .unwrap()
            .frame(SnapshotId::new())
            .unwrap();
            let table = decode_table();
            let texels: Vec<[f32; 3]> = source
                .rgba
                .chunks_exact(4)
                .map(|pixel| {
                    [
                        table[usize::from(pixel[0])],
                        table[usize::from(pixel[1])],
                        table[usize::from(pixel[2])],
                    ]
                })
                .collect();
            let gpu = interpret::execute(&plan, &texels, &units).unwrap();
            let difference = largest_difference(&cpu.rgba, &gpu);
            assert!(
                difference <= 1,
                "{what}: the masked plan draws up to {difference} codes from the CPU frame"
            );
        }
    }
}

/// A smooth opaque gradient on every channel. The render table's [`gradient`] has a sawtooth in
/// blue whose wraps put a hard edge between codes 255 and 0, where a blend in linear light turns a
/// hundredth of a pixel into dozens of dark-tone codes: right for proving exact taps, wrong for a
/// grid that is allowed a tenth of a pixel.
fn smooth(width: u32, height: u32) -> SourceImage {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                (16 + x * 223 / width) as u8,
                (24 + y * 211 / height) as u8,
                (40 + (x + y) * 190 / (width + height)) as u8,
                255,
            ]);
        }
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:smooth".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// A perspective warp with a straightened crop after it joins the tail as one homography, which
/// the executor evaluates at every pixel with no grid and which draws the CPU frame.
#[test]
fn a_perspective_tail_is_drawn_through_its_homography() {
    let registry = colour_registry();
    let source = smooth(157, 101);
    let recipe = colour_recipe(vec![
        exposure_layer(&[0.4]),
        Layer::new(
            crate::PERSPECTIVE_EFFECT,
            json!({"horizontal": 60, "vertical": -45}),
        ),
        crop(157, 101, 3.0),
    ]);
    let plan = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(157, 101)),
    ));
    assert!(plan.geometry.affine().is_none(), "a warp has no matrix");
    assert!(plan.geometry.projective().is_some(), "{:?}", plan.geometry);
    assert!(!plan.geometry.needs_grid());
    let output = plan.geometry.output();
    let whole = Region {
        x0: 0,
        y0: 0,
        width: output.width,
        height: output.height,
    };
    assert_eq!(plan.geometry.grid(whole, 1.0).unwrap(), None);
    assert_draws_the_cpu_frame(&registry, &source, &recipe, &plan, "a perspective warp");
}

/// A lens warp joins the tail as a warp map no homography states, which the executor reads through
/// its coordinate grid and which draws the CPU frame.
#[test]
fn a_lens_tail_is_drawn_through_its_grid() {
    let registry = colour_registry();
    let source = smooth(180, 120);
    let recipe = colour_recipe(vec![
        exposure_layer(&[0.4]),
        crate::render::testing::frozen_lens(180, 120, 24.0),
        Layer::new(
            crate::PERSPECTIVE_EFFECT,
            json!({"horizontal": 60, "vertical": -45}),
        ),
        crop(180, 120, 3.0),
    ]);
    let plan = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(180, 120)),
    ));
    assert!(plan.geometry.affine().is_none(), "a warp has no matrix");
    assert!(
        plan.geometry.projective().is_none(),
        "a lens has no homography"
    );
    assert!(plan.geometry.needs_grid());
    let output = plan.geometry.output();
    let grid = plan
        .geometry
        .grid(
            Region {
                x0: 0,
                y0: 0,
                width: output.width,
                height: output.height,
            },
            1.0,
        )
        .unwrap()
        .expect("a warp has a grid");
    assert!(grid.nodes.len() <= super::GRID_MAX_NODES);
    assert_draws_the_cpu_frame(&registry, &source, &recipe, &plan, "a lens warp");
}

/// A perspective tail's homography is the map's own steps multiplied together: evaluated in `f32`
/// as the surface evaluates it, it lands within `f32` precision of the CPU's `GeometryMap` at every
/// pixel centre checked, on photo-sized stages with the corpus's and the strongest perspective,
/// alone, under a straightened crop and under a downscale. A lens step leaves the tail no
/// homography.
#[test]
fn a_perspective_homography_is_the_cpu_map_in_f32() {
    use crate::render::map::{GeometryMap, StageSize, WarpStep};
    use std::f64::consts::PI;
    let size = |width, height| StageSize { width, height };
    let perspective = |h, v, width, height| {
        WarpStep::projective(h, v, stage(width, height)).expect("a perspective within limits")
    };
    // A straightened crop's output-to-input matrix: a turn by `degrees` about the output's centre,
    // which lands on `centre` of the stage it reads.
    let straightened = |degrees: f64, output: (f64, f64), centre: (f64, f64)| {
        let (sin, cos) = (degrees * PI / 180.0).sin_cos();
        let (ox, oy) = (output.0 / 2.0, output.1 / 2.0);
        WarpStep::Affine([
            cos,
            -sin,
            centre.0 - cos * ox + sin * oy,
            sin,
            cos,
            centre.1 - sin * ox - cos * oy,
        ])
    };
    let cases: Vec<(&str, StageSize, StageSize, Vec<WarpStep>)> = vec![
        (
            "the corpus's perspective on 24 MP",
            size(6000, 4000),
            size(6000, 4000),
            vec![perspective(20, -10, 6000, 4000)],
        ),
        (
            "the corpus's perspective at Fit",
            size(1716, 1144),
            size(1716, 1144),
            vec![perspective(20, -10, 1716, 1144)],
        ),
        (
            "a strong perspective on 60 MP",
            size(10000, 6000),
            size(10000, 6000),
            vec![perspective(-100, 60, 10000, 6000)],
        ),
        (
            "a perspective under a straightened crop",
            size(6000, 4000),
            size(5000, 2812),
            vec![
                perspective(35, 25, 6000, 4000),
                straightened(7.0, (5000.0, 2812.0), (3000.0, 2000.0)),
            ],
        ),
        (
            "a perspective under a downscale",
            size(6000, 4000),
            size(1716, 1144),
            vec![
                perspective(-40, -70, 6000, 4000),
                WarpStep::Affine([6000.0 / 1716.0, 0.0, 0.0, 0.0, 4000.0 / 1144.0, 0.0]),
            ],
        ),
    ];
    for (what, content, output, steps) in cases {
        let map = GeometryMap::from_steps(content, output, steps).unwrap();
        let geometry = super::GpuGeometry {
            map: map.clone(),
            reads: Region::whole(stage(content.width, content.height)),
            clamps: false,
        };
        assert!(
            geometry.affine().is_none() && !geometry.needs_grid(),
            "{what}"
        );
        let m = geometry.projective().expect(what).map(|value| value as f32);
        let columns = (0..output.width).step_by(5).chain([output.width - 1]);
        let mut worst = 0.0f64;
        for y in (0..output.height).step_by(5).chain([output.height - 1]) {
            for x in columns.clone() {
                // The surface's arithmetic: the pixel's centre and the nine coefficients in f32.
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w = m[6] * px + m[7] * py + m[8];
                let u = (m[0] * px + m[1] * py + m[2]) / w;
                let v = (m[3] * px + m[4] * py + m[5]) / w;
                let (eu, ev) = map.content_at(f64::from(x) + 0.5, f64::from(y) + 0.5);
                worst = worst
                    .max((f64::from(u) - eu).abs())
                    .max((f64::from(v) - ev).abs());
            }
        }
        // A few ulps of the largest coordinate the boundary stage holds.
        let ulp = f64::from(f32::EPSILON) * f64::from(content.width.max(content.height));
        eprintln!("{what}: worst {worst:.6} px, {:.2} ulps", worst / ulp);
        assert!(worst <= 4.0 * ulp, "{what}: {worst} px from the CPU's map");
    }
    // A lens step makes the map no homography: its tail keeps the grid.
    let content = size(6048, 4024);
    let lens = WarpStep::radial(
        crate::render::map::RadialModel::Poly3,
        [-0.02, 0.0, 0.0],
        1.0,
        stage(6048, 4024),
    )
    .unwrap();
    let map = GeometryMap::from_steps(
        content,
        content,
        vec![lens, perspective(20, -10, 6048, 4024)],
    )
    .unwrap();
    let geometry = super::GpuGeometry {
        map,
        reads: Region::whole(stage(6048, 4024)),
        clamps: false,
    };
    assert!(geometry.projective().is_none() && geometry.needs_grid());
}

/// The proxy phase plans against the proxy stage, the stage its frame is drawn at.
#[test]
fn a_fit_plan_addresses_the_proxy_stage() {
    let registry = colour_registry();
    let recipe = colour_recipe(vec![positional_layer(), crop(400, 300, 4.0)]);
    let plan = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::fit(0, stage(400, 300), stage(4000, 3000)),
    ));
    assert_eq!(plan.boundary.stage, stage(400, 300));
    let exact = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(4000, 3000)),
    ));
    assert_eq!(exact.boundary.stage, stage(4000, 3000));
    assert_ne!(
        plan.content[0].units[0].words, exact.content[0].units[0].words,
        "a positional unit is compiled against the stage it addresses"
    );
}

/// Planning is one compile of the stack, reads no pixel — it is handed none — and holds nothing
/// that scales with the image: the same stack planned against a tiny stage and the largest one
/// carries the same operations, units, words and blocks.
#[test]
fn planning_is_one_compile_and_holds_nothing_that_scales_with_the_image() {
    let registry = colour_registry();
    let recipe = colour_recipe(vec![
        exposure_layer(&[0.3, -0.1]),
        crop(40, 30, 4.0),
        positional_layer(),
    ]);
    let shape = |plan: &GpuPlan| -> Vec<(usize, Vec<usize>, Vec<usize>)> {
        plan.operations()
            .map(|operation| {
                (
                    operation.layer,
                    operation
                        .units
                        .iter()
                        .map(|unit| unit.words.len())
                        .collect(),
                    operation
                        .units
                        .iter()
                        .map(|unit| unit.block.as_ref().map_or(0, |block| block.len()))
                        .collect(),
                )
            })
            .collect()
    };
    crate::modules::stack_compiles::take();
    let small = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(40, 30)),
    ));
    assert_eq!(
        crate::modules::stack_compiles::take(),
        1,
        "a plan is one compile"
    );
    let large = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(16384, 12288)),
    ));
    assert_eq!(shape(&small), shape(&large));
    let steps = |plan: &GpuPlan| match &plan.geometry.map.mapping {
        crate::MappingShape::Affine { .. } => 1,
        crate::MappingShape::Warp { steps, .. } => steps.len(),
    };
    assert_eq!(steps(&small), steps(&large));
}

/// The clipping marks agree with the CPU's quantizer for every `f32` around both thresholds.
#[test]
fn clipping_marks_agree_with_the_output_quantizer() {
    let registry = colour_registry();
    let recipe = colour_recipe(vec![exposure_layer(&[0.1])]);
    let plan = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(41, 29)),
    ));
    let clipping = plan.clipping;
    let quantizer = crate::colour::srgb::quantizer();
    for threshold in [clipping.shadow_below, clipping.highlight_from] {
        let mut value = threshold;
        for _ in 0..64 {
            value = value.next_down();
        }
        for _ in 0..128 {
            let code = quantizer.pixel([value; 3])[0];
            assert_eq!(code == 0, value < clipping.shadow_below, "{value}");
            assert_eq!(code == 255, value >= clipping.highlight_from, "{value}");
            value = value.next_up();
        }
    }
}

/// Dehaze's atmospheric light is the store's when it holds the one a CPU frame of the boundary's
/// content at the plan's stage prepared, which every Dehaze amount shares; otherwise the GPU takes
/// it from the stage it holds and the plan says the frame is approximate. The passes are the same
/// either way, so a store that fills mid-gesture changes words alone.
#[test]
fn a_dehaze_light_is_the_stores_when_it_holds_the_boundarys_content() {
    use super::{GpuEstimates, GpuPassShape, gpu_plan_with};
    let registry = colour_registry();
    let source = gradient(41, 29);
    // One stack, its layers' identities kept as its values change, as a drag keeps them.
    let base = colour_recipe(vec![
        exposure_layer(&[0.3]),
        Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40.0})),
    ]);
    let with = |layer: usize, payload: serde_json::Value| {
        let mut recipe = base.clone();
        recipe.layers[layer].payload = payload;
        recipe
    };
    let context = RenderContext::new();
    let estimates = GpuEstimates {
        context: &context,
        source: RenderSource::Byte(&source).into(),
    };
    let request = GpuPlanRequest::exact(1, stage(41, 29)).qualifying();
    let plan_of = |recipe: &Recipe| {
        planned(gpu_plan_with(&registry, recipe, request, Some(estimates)).unwrap())
    };

    // Before any CPU frame the store holds nothing: the light is taken on the GPU, by one
    // workgroup over the 16x reduction of the stage it holds.
    let plan = plan_of(&base);
    assert!(plan.approximate());
    let step = plan.spatial.first().unwrap();
    let atmosphere = step
        .passes
        .iter()
        .find(|pass| pass.kernel == "lf_presence_atmosphere")
        .expect("the estimate's pass");
    assert_eq!(atmosphere.shape, GpuPassShape::Workgroup);
    // What decides the operation's pipelines: every pass but its words, and every apply.
    let pipelines = |step: &super::GpuSpatial| {
        let passes: Vec<_> = step
            .passes
            .iter()
            .map(|pass| (pass.kernel, pass.inputs.clone(), pass.output, pass.shape))
            .collect();
        (step.planes.clone(), passes, step.applies.clone())
    };
    let estimated = pipelines(step);

    // The CPU frame stores it; the plan then writes the stored light as its words, through the
    // same passes, which reduce nothing.
    Render::compiled(
        RenderSource::Byte(&source),
        registry
            .compile(source.width, source.height, &base)
            .unwrap(),
        RenderOptions::exact(&crate::Cancel::never()),
        &context,
    )
    .unwrap()
    .frame(SnapshotId::new())
    .unwrap();
    let stored: Vec<f32> = {
        let keys = context.estimates().keys();
        assert_eq!(keys.len(), 1);
        let global = context.estimates().cached(&keys[0]).unwrap().unwrap();
        global.values().iter().map(|value| *value as f32).collect()
    };
    for dehaze in [40.0, -65.0] {
        let plan = plan_of(&with(1, json!({"dehaze": dehaze})));
        assert!(!plan.approximate(), "dehaze {dehaze}");
        let step = plan.spatial.first().unwrap();
        assert_eq!(
            pipelines(step),
            estimated,
            "the same passes whether the light is stored or not"
        );
        let [reduce, atmosphere] = [0, 1].map(|pass| step.passes[pass].words);
        assert_eq!(step.words[reduce], 3, "the reduction reduces nothing");
        assert_eq!(step.words[atmosphere + 3], 1, "the light is given");
        let light = &step.words[atmosphere + 4..atmosphere + 7];
        assert_eq!(
            light
                .iter()
                .map(|word| f32::from_bits(*word))
                .collect::<Vec<_>>(),
            stored,
            "every amount reads the one stored light"
        );
    }
    // A new upstream is new content: the store holds nothing for it.
    let mut moved = base.clone();
    moved.layers[0].payload = exposure_layer(&[-0.4]).payload;
    assert!(plan_of(&moved).approximate());
    // Without a store, the light is always taken on the GPU.
    assert!(
        planned(answer(&registry, &base, request)).approximate(),
        "no store, no stored light"
    );
    // A request on one path with a source of the other is refused.
    assert!(gpu_plan_with(&registry, &base, request.linear(), Some(estimates)).is_err());
}

/// A Presence plan's planes, passes and words are the same at the smallest and the largest stage
/// but for the radii its words hold: nothing in it scales with the image.
#[test]
fn a_presence_plan_holds_nothing_that_scales_with_the_image() {
    let registry = colour_registry();
    let recipe = colour_recipe(vec![Layer::new(
        crate::PRESENCE_EFFECT,
        json!({"texture": 30.0, "clarity": -20.0, "dehaze": 15.0}),
    )]);
    let shape = |size: Stage| {
        let plan = planned(answer(
            &registry,
            &recipe,
            GpuPlanRequest::exact(0, size).qualifying(),
        ));
        let step = plan
            .spatial
            .into_iter()
            .next()
            .expect("the spatial operation");
        (
            step.planes.clone(),
            step.passes
                .iter()
                .map(|pass| (pass.kernel, pass.inputs.clone(), pass.output, pass.source))
                .collect::<Vec<_>>(),
            step.words.len(),
            step.applies.len(),
        )
    };
    let small = shape(stage(40, 30));
    assert_eq!(small, shape(stage(16384, 12288)));
    // Dehaze, then Texture, then Clarity, each unit's passes before the next's: a pass that reads
    // its unit's input runs the applies before it, and one that reads only planes runs none, so it
    // is the same module whatever unit runs it.
    let (planes, passes, _, applies) = small;
    assert_eq!(applies, 3);
    let sources: Vec<usize> = passes
        .iter()
        .map(|pass| pass.3)
        .filter(|source| *source > 0)
        .collect();
    assert!(sources.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!((sources[0], *sources.last().unwrap()), (1, 2));
    assert!(passes.iter().filter(|pass| pass.3 == 0).count() > 8);
    // A later unit's scratch reuses an earlier unit's of the same format and size.
    assert!(planes.len() < 9 + 4 + 4, "{} planes", planes.len());
}

/// A proxy named for the estimate store before it is built is named as the built proxy's own pixel
/// domain names it: a JPEG's window, and a RAW's derived development under a cropped and turned
/// view and a window, equal whenever it is built and apart from its source's.
#[test]
fn a_proxy_is_named_as_its_built_pixels_are() {
    use super::EstimateSource;
    use crate::{PreviewSource, ProxyBounds, ProxyPlan, ProxyWindow};
    let jpeg = PreviewSource::Jpeg(gradient(240, 160));
    let raw = PreviewSource::Raw {
        image: crate::render::tests::varied(240, 160)
            .with_view([10, 6, 200, 140], 6)
            .unwrap(),
        settings: crate::LinearSettings::default(),
    };
    for source in [jpeg, raw] {
        let (width, height) = source.dimensions();
        let bounds = ProxyBounds {
            width: width / 2,
            height: height / 2,
        };
        let plans = [
            ProxyPlan {
                width: width / 2,
                height: height / 2,
                bounds,
                window: None,
            },
            ProxyPlan {
                width: width / 2,
                height: height / 2,
                bounds,
                window: Some(ProxyWindow {
                    x: 7,
                    y: 5,
                    width: width / 4,
                    height: height / 4,
                }),
            },
        ];
        for plan in plans {
            let named = EstimateSource::Proxy {
                source: &source,
                plan,
            }
            .identity("prefix")
            .unwrap();
            for _ in 0..2 {
                let built = source.proxy(plan).unwrap();
                let own = EstimateSource::Render((&built).into())
                    .identity("prefix")
                    .unwrap();
                assert_eq!(named, own, "{plan:?}");
            }
            let whole = EstimateSource::Render((&source).into())
                .identity("prefix")
                .unwrap();
            assert_ne!(named.1, whole.1, "a proxy is not its source");
        }
    }
}

/// Spatial and restoration layers after the boundary chain in recipe order, with the colour layers
/// between them: Detail, then a colour layer on its output, then Presence, plain or through a
/// mask, then the output's colour, each operation's input the boundary through everything before
/// it, and the plan draws the CPU frame. Past [`super::spatial::GPU_CHAIN_APPLY_PLANES`] the stack
/// names `spatial-chain`.
#[test]
fn chained_spatial_operations_draw_the_cpu_frame() {
    let registry = colour_registry();
    let source = gradient(41, 29);
    let detail = || {
        Layer::new(
            crate::DETAIL_EFFECT,
            json!({"sharpening": 40.0, "luminance": 20.0, "colour": 20.0}),
        )
    };
    let presence = |payload: serde_json::Value| Layer::new(crate::PRESENCE_EFFECT, payload);
    let mut mask = Mask::new("Mask 1");
    mask.components.push(crate::Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
    ));
    let masked = |layer: Layer| Layer {
        mask: Some(mask.id.clone()),
        ..layer
    };
    let all = json!({"texture": 30.0, "clarity": 30.0, "dehaze": 20.0});
    for (what, layers) in [
        (
            "unmasked",
            vec![
                detail(),
                exposure_layer(&[0.3]),
                presence(all.clone()),
                positional_layer(),
            ],
        ),
        (
            "masked",
            vec![
                detail(),
                exposure_layer(&[0.3]),
                masked(presence(all.clone())),
                positional_layer(),
            ],
        ),
    ] {
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..colour_recipe(layers)
        };
        let plan = planned(answer(
            &registry,
            &recipe,
            GpuPlanRequest::exact(0, stage(41, 29)).qualifying(),
        ));
        let layers: Vec<usize> = plan.spatial.iter().map(|step| step.layer).collect();
        assert_eq!(layers, [0, 2], "{what}");
        let between: Vec<usize> = plan.spatial[0].after.iter().map(|op| op.layer).collect();
        assert_eq!(between, [1], "{what}: the colour layer between them");
        assert!(plan.spatial[1].after.is_empty(), "{what}");
        assert_eq!(plan.output.len(), 1, "{what}");
        assert_eq!(plan.spatial[1].mask.is_some(), what == "masked");
        let order: Vec<usize> = plan.operations().map(|op| op.layer).collect();
        assert_eq!(order, [1, 3], "{what}");
        assert_draws_the_cpu_frame(&registry, &source, &recipe, &plan, what);
    }
    // Detail's three apply planes and two Presences' four each fit; a third Presence does not.
    let mut second = Mask::new("Mask 2");
    second.components = mask.components.clone();
    let through = |layer: Layer, mask: &Mask| Layer {
        mask: Some(mask.id.clone()),
        ..layer
    };
    let mut layers = vec![
        detail(),
        presence(all.clone()),
        through(presence(all.clone()), &mask),
    ];
    let recipe = Recipe {
        masks: vec![mask.clone(), second.clone()],
        ..colour_recipe(layers.clone())
    };
    let plan = planned(answer(
        &registry,
        &recipe,
        GpuPlanRequest::exact(0, stage(41, 29)).qualifying(),
    ));
    assert_eq!(plan.spatial.len(), 3);
    layers.push(through(presence(all), &second));
    let recipe = Recipe {
        masks: vec![mask, second],
        ..colour_recipe(layers)
    };
    assert_eq!(
        answer(
            &registry,
            &recipe,
            GpuPlanRequest::exact(0, stage(41, 29)).qualifying()
        ),
        GpuAnswer::Fallback(GpuFallback::SpatialChain { layer: 3 })
    );
}
