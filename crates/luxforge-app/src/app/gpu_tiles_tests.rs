//! The tile runner (`luxforge_gpu::tiles`, `docs/design/gpu-preview.md`,
//! "Qualifying a program") over the core's stacks: each a plan of the whole output stage at full
//! scale from the source, drawn tile by tile as the picture at rest draws it — each tile its
//! rectangle of the output stage over the window of the source its halos read, anchored to the
//! plan ([`anchored`]), through its part of a lens warp's grid of the whole stage — on the byte path
//! (a JPEG's codes, half-float boundaries) and the linear path (a RAW's planes, `f32` ones).
//!
//! - **Across devices.** A tile the runner draws on a device of its own is, bit for bit, the tile
//!   the photo surface's own drawing (`HeadlessSurface`) draws on another device opened on the
//!   same adapter with the same descriptor, as the window's renderer and the runner each hold one:
//!   what a sample equal to the byte on screen rests on (`docs/design/gpu-first.md`, stage 4). The
//!   runner draws each tile twice, every scratch plane of every link starting from NaN the second
//!   time, so no pass of its own read scratch it did not write.
//! - **Run to run.** Two runs of one tile read back the same codes and the same linear bits.
//! - **Lights.** The Presence families read Dehaze's light, which the surface's light link computes
//!   from the whole source it holds and the runner from the source a window of each of the link's
//!   tiles at a time, read back once and written into every tile's light plane: the same light, so
//!   the same codes.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing.
use super::{
    gpu_plan::{WarpGrid, install_output_encoding, surface_plan_over},
    gpu_window_tests::{HEIGHT, WIDTH, source},
};
use crate::adapters;
use luxforge_core::{
    BASIC_EFFECT, BoundaryFormat, CURVE_EFFECT, Cancel, DETAIL_EFFECT, GpuAnswer, GpuPlanRequest,
    Layer, LinearImage, MIXER_EFFECT, ModuleRegistry, PERSPECTIVE_EFFECT, PRESENCE_EFFECT,
    PreviewSource, Recipe, Region, RenderContext, RenderOptions, Stage, VIGNETTE_EFFECT, anchored,
    gpu_plan, render,
};
use luxforge_gpu::{
    Derivation, GpuBoundary, GpuPlan, GpuSource, headless::HeadlessSurface, tiles::GPU_TILE_BUDGET,
    tiles::TileEnd, tiles::TilePixels, tiles::TileRunner,
};
use serde_json::json;
use std::sync::Arc;

/// The tiles' side: twelve of them over the 360 × 240 stage, the last column and row narrower.
const SIDE: u32 = 96;

/// This host's adapter the tests open their devices on, by backend and name: the first wgpu
/// offers of the renderer's backends that is not a software rasterizer, or else the first. `None`,
/// having printed that `test` was skipped, without one.
pub(super) fn host_adapter(test: &str) -> Option<(String, String)> {
    let offered = adapters::enumerate(adapters::renderer_backends());
    let Some(adapter) = offered
        .iter()
        .find(|adapter| adapter.device_type != "Cpu")
        .or(offered.first())
    else {
        eprintln!("skipped: no GPU adapter; {test} ran nothing and is not GPU evidence");
        return None;
    };
    Some((adapter.backend.clone(), adapter.name.clone()))
}

/// A RAW development's planes as the GPU source uploads them, borrowed through a clone of the
/// image, as the desktop hands them.
struct Planes(LinearImage);

impl AsRef<[f32]> for Planes {
    fn as_ref(&self) -> &[f32] {
        self.0.shared_planes().0
    }
}

/// `source` as the photo surface holds it on the GPU: a JPEG's upright codes, or a RAW
/// development's planes through its view, never copied.
pub(super) fn gpu_source(version: u64, source: &PreviewSource) -> GpuSource {
    match source {
        PreviewSource::Jpeg(image) => {
            GpuSource::codes(version, Arc::clone(&image.rgba), image.width, image.height)
        }
        PreviewSource::Raw { image, .. } => {
            let (_, base, crop, orientation) = image.shared_planes();
            GpuSource::planes(
                version,
                Arc::new(Planes(image.clone())),
                base,
                crop,
                orientation,
            )
        }
    }
    .expect("a GPU source")
}

/// A radial component: an ellipse a little off centre, tilted, with a broad feather.
pub(super) fn radial() -> luxforge_core::Component {
    luxforge_core::Component::new(
        "Radial 1",
        luxforge_core::ComponentMode::Add,
        "radial",
        json!({"x": 0.45, "y": 0.55, "radius_x": 0.3, "radius_y": 0.22, "angle": 18.0,
               "feather": 45.0}),
    )
}

/// A rotated gradient across the frame.
fn gradient() -> luxforge_core::Component {
    luxforge_core::Component::new(
        "Linear 1",
        luxforge_core::ComponentMode::Add,
        "linear",
        json!({"x0": 0.13, "y0": 0.91, "x1": 0.71, "y1": 0.17}),
    )
}

/// A recipe of `layers`, each of the `masked` ones masked by a mask of its own holding its
/// component.
pub(super) fn recipe(layers: Vec<Layer>, masked: Vec<(usize, luxforge_core::Component)>) -> Recipe {
    let mut recipe = Recipe {
        layers,
        ..Recipe::default()
    };
    for (index, (layer, component)) in masked.into_iter().enumerate() {
        let mut mask = luxforge_core::Mask::new(format!("Mask {}", index + 1));
        mask.components.push(component);
        recipe.layers[layer].mask = Some(mask.id.clone());
        recipe.masks.push(mask);
    }
    recipe
}

/// The families a tile is drawn through: every kind of step a plan holds, a link before the last
/// among them.
pub(super) fn families() -> Vec<(&'static str, Recipe)> {
    let basic = Layer::new(
        BASIC_EFFECT,
        json!({"exposure": 0.4, "contrast": 20.0, "vibrance": 25.0}),
    );
    let colour = vec![
        Layer::new(
            BASIC_EFFECT,
            json!({
                "exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0,
                "whites": -15.0, "blacks": 15.0, "vibrance": 30.0, "saturation": 15.0,
                "temperature": 20.0, "tint": -10.0
            }),
        ),
        Layer::new(
            CURVE_EFFECT,
            json!({"luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}),
        ),
        Layer::new(
            MIXER_EFFECT,
            json!({
                "red-hue": 30, "orange-saturation": -40, "green-saturation": 40,
                "aqua-hue": -25, "blue-luminance": -30, "magenta-saturation": 25
            }),
        ),
        Layer::new(
            VIGNETTE_EFFECT,
            json!({"amount": -60, "midpoint": 40, "roundness": 20, "feather": 60}),
        ),
    ];
    let presence = |values| Layer::new(PRESENCE_EFFECT, values);
    vec![
        ("a colour stack", recipe(colour, Vec::new())),
        (
            "a masked stack",
            recipe(
                vec![
                    basic.clone(),
                    Layer::new(BASIC_EFFECT, json!({"exposure": -0.8, "saturation": -40.0})),
                    Layer::new(MIXER_EFFECT, json!({"red-hue": 30, "blue-luminance": -30})),
                ],
                vec![(1, radial()), (2, gradient())],
            ),
        ),
        (
            "Presence",
            recipe(
                vec![presence(
                    json!({"texture": 35.0, "clarity": 30.0, "dehaze": 20.0}),
                )],
                Vec::new(),
            ),
        ),
        (
            "Detail",
            recipe(
                vec![Layer::new(
                    DETAIL_EFFECT,
                    json!({"sharpening": 60.0, "luminance": 40.0, "colour": 40.0}),
                )],
                Vec::new(),
            ),
        ),
        (
            "a lens warp",
            recipe(
                vec![
                    basic.clone(),
                    luxforge_core::qualification::lens_layer(-0.06, (WIDTH, HEIGHT)),
                ],
                Vec::new(),
            ),
        ),
        (
            "a perspective warp",
            recipe(
                vec![
                    basic.clone(),
                    Layer::new(
                        PERSPECTIVE_EFFECT,
                        json!({"horizontal": 25, "vertical": -15}),
                    ),
                ],
                Vec::new(),
            ),
        ),
        (
            "Presence after Detail",
            recipe(
                vec![
                    Layer::new(DETAIL_EFFECT, json!({"sharpening": 50.0})),
                    presence(json!({"texture": 20.0, "clarity": 25.0})),
                ],
                Vec::new(),
            ),
        ),
        (
            "a masked Presence after Basic",
            recipe(
                vec![
                    basic,
                    presence(json!({"texture": 30.0, "clarity": 40.0, "dehaze": 15.0})),
                ],
                vec![(1, radial())],
            ),
        ),
    ]
}

/// One tile of a case: its rectangle of the output stage, the window of the source it reads and
/// its plan over the boundary cut from that window.
struct Tile {
    rect: Region,
    window: Region,
    plan: GpuPlan,
}

impl Tile {
    fn window(&self) -> [u32; 4] {
        let window = self.window;
        [window.x0, window.y0, window.width, window.height]
    }
}

/// A family on one path, its source as the GPU holds it, and its tiles.
struct Case {
    name: String,
    source: GpuSource,
    tiles: Vec<Tile>,
}

/// Every family on both paths, each planned once from the source over the whole output stage, as
/// the picture at rest is, and drawn in tiles of [`SIDE`]: the windows the region planner gives
/// each tile's rectangle, anchored to the plan, every global estimate read from the store the
/// exact render filled. `O(cases × tiles)` CPU work: each tile's window is the CPU's own.
fn cases() -> Vec<Case> {
    let registry = ModuleRegistry::builtin();
    let stage = Stage {
        width: WIDTH,
        height: HEIGHT,
    };
    let mut cases = Vec::new();
    let mut versions = 0;
    for (format, path, version) in [
        (BoundaryFormat::Half, "the byte path", 1),
        (BoundaryFormat::Float, "the linear path", 2),
    ] {
        let source = source(format);
        let gpu = gpu_source(version, &source);
        for (family, recipe) in families() {
            let name = format!("{family} on {path}");
            // The exact render, whose boundaries name every tile's window.
            let context = RenderContext::new();
            let exact = render(
                &registry,
                &source,
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .expect("the exact render");
            let request = GpuPlanRequest::exact(0, stage).from_source();
            let request = match format {
                BoundaryFormat::Float => request.linear(),
                BoundaryFormat::Half => request,
            };
            let plan = match gpu_plan(&registry, &recipe, request).expect("a stack") {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => panic!("{name}: {reason}"),
            };
            let anchor = plan.anchor();
            let output = plan.geometry.output();
            let stage_grid = plan.geometry.stage_grid(1.0).expect("a stage grid");
            let mut tiles = Vec::new();
            for y0 in (0..output.height).step_by(SIDE as usize) {
                for x0 in (0..output.width).step_by(SIDE as usize) {
                    let rect = Region {
                        x0,
                        y0,
                        width: SIDE.min(output.width - x0),
                        height: SIDE.min(output.height - y0),
                    };
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
                    versions += 1;
                    let boundary = GpuBoundary::derived(
                        &gpu,
                        Derivation::Cut {
                            origin: (window.x0, window.y0),
                        },
                        window.width,
                        window.height,
                        versions,
                    )
                    .expect("a derived boundary");
                    let grid = stage_grid
                        .as_ref()
                        .map(|grid| WarpGrid::new(&grid.part(rect).expect("the tile's part")));
                    let plan = surface_plan_over(
                        &plan,
                        boundary,
                        (window.x0, window.y0),
                        grid.as_ref(),
                        Some(rect),
                    )
                    .expect("a runnable plan");
                    tiles.push(Tile { rect, window, plan });
                }
            }
            cases.push(Case {
                name,
                source: gpu.clone(),
                tiles,
            });
        }
    }
    cases
}

/// The first pixel two tiles' codes differ at, as the failure reports it: its place in the output
/// stage and both pixels; and how many differ.
fn first_difference(rect: Region, ours: &[u8], theirs: &[u8]) -> Option<String> {
    let differing: Vec<usize> = (0..ours.len() / 4)
        .filter(|index| ours[index * 4..index * 4 + 4] != theirs[index * 4..index * 4 + 4])
        .collect();
    let index = *differing.first()?;
    let (x, y) = (
        rect.x0 + index as u32 % rect.width,
        rect.y0 + index as u32 / rect.width,
    );
    Some(format!(
        "{} of {} pixels differ, the first at ({x}, {y}): the runner's {:?}, the surface's {:?}",
        differing.len(),
        ours.len() / 4,
        &ours[index * 4..index * 4 + 4],
        &theirs[index * 4..index * 4 + 4],
    ))
}

fn codes(runner: &mut TileRunner, case: &Case, tile: &Tile) -> Vec<u8> {
    match runner.run(&tile.plan, &case.source, tile.window(), TileEnd::Codes) {
        Ok(TilePixels::Codes(codes)) => codes,
        other => panic!("{}: the tile at {:?}: {other:?}", case.name, tile.rect),
    }
}

fn linear(runner: &mut TileRunner, case: &Case, tile: &Tile) -> Vec<[u32; 3]> {
    match runner.run(&tile.plan, &case.source, tile.window(), TileEnd::Linear) {
        Ok(TilePixels::Linear(values)) => {
            values.iter().map(|value| value.map(f32::to_bits)).collect()
        }
        other => panic!("{}: the tile at {:?}: {other:?}", case.name, tile.rect),
    }
}

/// Every tile of every family on both paths, drawn by the photo surface's own drawing on one device
/// and by the runner on a second device of the same adapter, each opened with the window's
/// descriptor: the same codes, bit for bit, the runner's every scratch plane starting from NaN or
/// not. Each case's result is printed with the adapter both devices are on.
#[test]
fn a_tile_runner_reads_what_the_headless_surface_reads_bit_for_bit() {
    let test = "a_tile_runner_reads_what_the_headless_surface_reads_bit_for_bit";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(
        install_output_encoding(),
        "the surface holds the core's output encoding"
    );
    // The window's device, as Iced's renderer opens it, drawing through the photo surface.
    let window = adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let mut runner = crate::adapters::tile_runner(&backend, &name)
        .unwrap_or_else(|refusal| panic!("{test}: the runner: {refusal:?}"));
    eprintln!(
        "{test}: the surface's adapter {:?}; the runner's adapter {:?}",
        window.adapter,
        runner.adapter()
    );
    assert_eq!(
        &window.adapter,
        runner.adapter(),
        "one adapter, two devices"
    );
    for case in cases() {
        let mut pixels = 0;
        for tile in &case.tiles {
            let drawn: Vec<u8> = surface
                .tile(&case.source, &tile.plan)
                .unwrap_or_else(|fallback| {
                    panic!(
                        "{}: the surface's tile at {:?}: {fallback:?}",
                        case.name, tile.rect
                    )
                })
                .concat();
            assert_eq!(
                drawn.len(),
                tile.rect.pixels() as usize * 4,
                "{}",
                case.name
            );
            assert!(
                drawn.chunks_exact(4).any(|pixel| pixel != &drawn[..4]),
                "{}: a tile of one colour at {:?} would prove nothing",
                case.name,
                tile.rect
            );
            for poisoned in [true, false] {
                runner.set_poison(poisoned);
                let read = codes(&mut runner, &case, tile);
                if let Some(difference) = first_difference(tile.rect, &read, &drawn) {
                    panic!(
                        "{}: the tile at {:?} over the window {:?}, poisoned {poisoned}: \
                         {difference}",
                        case.name, tile.rect, tile.window
                    );
                }
            }
            pixels += tile.rect.pixels();
        }
        eprintln!(
            "{test}: {}: {} tiles, {pixels} pixels: bit for bit, the runner's pool poisoned and not",
            case.name,
            case.tiles.len()
        );
    }
    eprintln!("{test}: the runner's figures {:?}", runner.figures());
}

/// Two runs of one tile read back the same bytes: the codes, and the bits of every linear value,
/// for every family on both paths, each run a fresh evaluation with every scratch plane starting
/// from NaN. Beside it, how many pixels' linear values the core's output quantizer takes to the
/// codes the codes' own run read, which is reported, not judged: the two ends are two shader
/// modules.
#[test]
fn two_runs_are_byte_identical() {
    let test = "two_runs_are_byte_identical";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(
        install_output_encoding(),
        "the surface holds the core's output encoding"
    );
    let mut runner = crate::adapters::tile_runner(&backend, &name)
        .unwrap_or_else(|refusal| panic!("{test}: the runner: {refusal:?}"));
    eprintln!("{test}: the runner's adapter {:?}", runner.adapter());
    runner.set_poison(true);
    let thresholds = luxforge_core::colour::srgb::output_thresholds();
    let quantized = |bits: u32| {
        let value = f32::from_bits(bits);
        let value = if value.is_nan() {
            0.0
        } else {
            value.clamp(0.0, 1.0)
        };
        thresholds.partition_point(|threshold| *threshold <= value) as u8
    };
    for case in cases() {
        let (mut pixels, mut agreeing) = (0, 0);
        for tile in &case.tiles {
            let first = codes(&mut runner, &case, tile);
            assert_eq!(
                codes(&mut runner, &case, tile),
                first,
                "{}: the tile at {:?}",
                case.name,
                tile.rect
            );
            let values = linear(&mut runner, &case, tile);
            assert_eq!(
                linear(&mut runner, &case, tile),
                values,
                "{}: the tile at {:?}",
                case.name,
                tile.rect
            );
            for (index, value) in values.iter().enumerate() {
                pixels += 1;
                agreeing += usize::from(value.map(quantized) == first[index * 4..index * 4 + 3]);
            }
        }
        eprintln!(
            "{test}: {}: byte-identical twice; the linear values quantize to the codes at \
             {agreeing} of {pixels} pixels",
            case.name
        );
    }
}

/// What the runner would hold for one tile of a 60-megapixel RAW (9504 × 6336) through Detail and
/// all three Presence fields, by its own charge over the core's plan of that stage, the tile in the
/// stage's interior: its window the tile grown by every unit's halo and anchored to the plan, a
/// codes read. An estimate from the figures the budget is charged, never a measurement: a
/// 2048-pixel tile fits [`GPU_TILE_BUDGET`] less the reads' reserve with its whole band's window of
/// the source held, the stage's width of the tile window's rows, which the worker uploads once for
/// every tile of the band.
#[test]
fn a_60_megapixel_raw_through_detail_and_presence_fits_the_budget_in_2048_pixel_tiles() {
    let test = "a_60_megapixel_raw_through_detail_and_presence_fits_the_budget_in_2048_pixel_tiles";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    let runner = crate::adapters::tile_runner(&backend, &name)
        .unwrap_or_else(|refusal| panic!("{test}: the runner: {refusal:?}"));
    let registry = ModuleRegistry::builtin();
    let stage = Stage {
        width: 9504,
        height: 6336,
    };
    let recipe = recipe(
        vec![
            Layer::new(
                DETAIL_EFFECT,
                json!({"sharpening": 60.0, "luminance": 40.0, "colour": 40.0}),
            ),
            Layer::new(
                PRESENCE_EFFECT,
                json!({"texture": 35.0, "clarity": 30.0, "dehaze": 20.0}),
            ),
        ],
        Vec::new(),
    );
    let request = GpuPlanRequest::exact(0, stage).from_source().linear();
    let plan = match luxforge_core::gpu_plan(&registry, &recipe, request).expect("a stack") {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{test}: {reason}"),
    };
    let halo: u32 = plan
        .spatial
        .iter()
        .flat_map(|spatial| spatial.halos.iter())
        .sum();
    let anchor = plan.anchor();
    let charge = |side: u32| {
        let rect = Region {
            x0: 4096,
            y0: 2048,
            width: side,
            height: side,
        };
        let window = anchored(
            Region {
                x0: rect.x0 - halo,
                y0: rect.y0 - halo,
                width: side + 2 * halo,
                height: side + 2 * halo,
            },
            anchor,
        );
        // Planes reaching the window's far corner, zeroed and never read: a charge uploads nothing.
        let (width, height) = (window.x1(), window.y1());
        let planes = Arc::new(vec![0f32; 3 * width as usize * height as usize]);
        let source = GpuSource::planes(1, planes, (width, height), [0, 0, width, height], 1)
            .expect("planes");
        let boundary = GpuBoundary::derived(
            &source,
            Derivation::Cut {
                origin: (window.x0, window.y0),
            },
            window.width,
            window.height,
            1,
        )
        .expect("a derived boundary");
        let converted =
            surface_plan_over(&plan, boundary, (window.x0, window.y0), None, Some(rect))
                .expect("a runnable plan");
        let window = [window.x0, window.y0, window.width, window.height];
        let charge = runner
            .charge(&converted, &source, window, TileEnd::Codes)
            .expect("a charge");
        // The band's window: the stage's width of the same rows, 12 bytes a texel of planes.
        let band = charge + u64::from(stage.width - window[2]) * u64::from(window[3]) * 12;
        eprintln!(
            "{test}: a {side}-pixel tile reads a {} x {} window and is charged {:.1} MB, \
             {:.1} MB with its band's window",
            window[2],
            window[3],
            charge as f64 / 1e6,
            band as f64 / 1e6
        );
        band
    };
    eprintln!("{test}: a summed halo of {halo} px, the anchor {anchor:?}");
    assert!(charge(2048) <= GPU_TILE_BUDGET - super::gpu_tiles::STREAM_READ_RESERVE);
    charge(1024);
}
