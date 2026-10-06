//! The picture at rest in tiles (`docs/design/gpu-preview.md`, "The picture at rest"): a stage drawn
//! on the GPU tile by tile — each tile the whole stack's plan over its rectangle of the output
//! stage, from its own window of the source anchored to the plan, through its part of a lens warp's
//! grid of the whole stage — is the same stage drawn as one region, bit for bit, every scratch
//! plane of every link starting from NaN, for every family whose texels depend on more than their
//! own pixel: Presence's running sums over its reductions and Dehaze's light from the whole stage,
//! Detail's neighbourhoods, a lens warp's grid and a perspective warp's homography. So tiles carry
//! no seam, and a pixel read back from one tile is the pixel any other window of the stage draws.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing.
use super::{
    gpu_plan::{WarpGrid, surface_plan_over},
    gpu_qualification::{headless, lit},
    gpu_window_tests::{HEIGHT, WIDTH, cut, source, whole},
};
use luxforge_core::{
    AssetId, BASIC_EFFECT, BoundaryFormat, Cancel, Component, ComponentMode, DETAIL_EFFECT,
    EntryId, Evaluation, GpuAnswer, GpuPlanRequest, GpuView, HistoryEntry, Layer, LinearImage,
    LinearSettings, Mask, ModuleRegistry, PERSPECTIVE_EFFECT, PRESENCE_EFFECT, PreviewSource,
    Recipe, Region, RenderContext, RenderOptions, Snapshot, SnapshotId, SourceImage, Stage,
    anchored, gpu_plan, render,
};
use luxforge_ui::photo_surface::gpu_preview::qualification::boundary_as;
use serde_json::json;
use std::sync::Arc;

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
                luxforge_core::qualification::lens_layer(-0.004, (WIDTH, HEIGHT)),
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
            let request = GpuPlanRequest::exact(0, stage).qualifying();
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
                // Every tile reads the one light of the whole stage.
                lit(&qualifier, &source, &converted).expect("the plan's lights");
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

/// A committed evaluation of `recipe` over `source`, as a displayed stack's job is.
fn committed(source: PreviewSource, recipe: Recipe) -> Evaluation {
    let asset = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "set-presence".into(),
        label: "Presence".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset,
            recipe: recipe.clone(),
        },
        undo_parent: None,
        restore_target: None,
    };
    Evaluation::new(
        Arc::new(ModuleRegistry::builtin()),
        RenderContext::new(),
        source,
        entry,
        recipe,
        None,
    )
}

/// A JPEG of `width` × `height` whose codes are never read: planning reads no pixel.
fn jpeg_of(width: u32, height: u32) -> PreviewSource {
    PreviewSource::Jpeg(SourceImage {
        width,
        height,
        rgba: Arc::new(vec![0; (width * height * 4) as usize]),
        fingerprint: format!("sha256:gpu-rest-{width}x{height}"),
        orientation: 1,
        capture: Default::default(),
    })
}

/// A RAW development of `width` × `height`, every value one grey.
fn raw_of(width: u32, height: u32) -> PreviewSource {
    let planes = vec![0.18f32; 3 * (width * height) as usize];
    PreviewSource::Raw {
        image: LinearImage::new(width, height, planes).expect("finite planes"),
        settings: LinearSettings::default(),
    }
}

/// The generated 60 MP JPEG's size, and the DJI Air 2S's.
const SIXTY: (u32, u32) = (10_000, 6_000);
const AIR_2S: (u32, u32) = (5_472, 3_648);

/// A radial mask a little off centre, numbered `index`.
fn radial_mask(index: usize) -> Mask {
    let mut mask = Mask::new(format!("Mask {}", index + 1));
    let t = index as f64 / 3.0;
    mask.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        json!({"x": 0.25 + 0.5 * t, "y": 0.5, "radius_x": 0.2, "radius_y": 0.25,
               "angle": 10.0, "feather": 45.0}),
    ));
    mask
}

fn detail() -> Layer {
    Layer::new(
        DETAIL_EFFECT,
        json!({"sharpening": 60.0, "luminance": 40.0, "colour": 40.0}),
    )
}

/// The stacks the rest is measured on (`docs/specs/performance.md`, "GPU-first against the
/// 2026-10-04 baseline"), each over a source of its photograph's size: the 60 MP drag stack, the
/// Air 2S's masked stack and three-segment stack, and Detail alone on a 24 MP JPEG.
fn measured_stacks() -> Vec<(&'static str, PreviewSource, Recipe)> {
    let presence = |fields: serde_json::Value| Layer::new(PRESENCE_EFFECT, fields);
    let full_presence = || presence(json!({"texture": 100.0, "clarity": 100.0, "dehaze": 100.0}));
    let masked_presence = || presence(json!({"clarity": 50.0, "texture": 40.0}));
    // Detail, the full Basic layer and Presence's three fields at +100, in the order
    // `editor-latency --detail --basic --presence` commits them.
    let drag = Recipe {
        layers: vec![
            detail(),
            Layer::new(
                BASIC_EFFECT,
                json!({"exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0,
                       "whites": -15.0, "blacks": 15.0, "temperature": 20.0, "tint": -10.0,
                       "vibrance": 30.0, "saturation": 15.0}),
            ),
            full_presence(),
        ],
        ..Recipe::default()
    };
    // Detail, a global Presence of Texture 25 and Clarity 20, and three masks each holding a
    // masked exposure and a masked Presence of Clarity 50 and Texture 40.
    let mut masked = Recipe {
        layers: vec![
            detail(),
            presence(json!({"texture": 25.0, "clarity": 20.0})),
        ],
        ..Recipe::default()
    };
    for index in 0..3 {
        let mask = radial_mask(index);
        masked.layers.push(Layer {
            mask: Some(mask.id.clone()),
            ..Layer::new(BASIC_EFFECT, json!({"exposure": 0.6}))
        });
        masked.layers.push(Layer {
            mask: Some(mask.id.clone()),
            ..masked_presence()
        });
        masked.masks.push(mask);
    }
    // Detail, Presence's three fields at +100, a radial's masked Presence and the lens profile.
    let mask = radial_mask(0);
    let three = Recipe {
        layers: vec![
            detail(),
            full_presence(),
            Layer {
                mask: Some(mask.id.clone()),
                ..masked_presence()
            },
            luxforge_core::qualification::lens_layer(-0.06, AIR_2S),
        ],
        masks: vec![mask],
        ..Recipe::default()
    };
    let detail_alone = Recipe {
        layers: vec![detail()],
        ..Recipe::default()
    };
    vec![
        ("the 60 MP drag stack", jpeg_of(SIXTY.0, SIXTY.1), drag),
        (
            "the Air 2S masked stack",
            raw_of(AIR_2S.0, AIR_2S.1),
            masked,
        ),
        (
            "the Air 2S three-segment stack",
            raw_of(AIR_2S.0, AIR_2S.1),
            three,
        ),
        ("Detail alone at 24 MP", jpeg_of(6_000, 4_000), detail_alone),
    ]
}

/// The tiles the editor plans for `evaluation`'s picture at rest at Fit in the evidence window.
fn planned_tiles(evaluation: &Evaluation) -> Box<luxforge_core::RestTiles> {
    let rest = luxforge_core::qualification::rest_plan(
        evaluation,
        GpuView::Fit(super::gpu_qualification::fit_bounds()),
    )
    .expect("a rest plan");
    match rest.tiles {
        Some(Ok(tiles)) => tiles,
        other => panic!("no tiles: {other:?}"),
    }
}

/// The measured stacks, and a colour layer before a spatial one, whose colour steps take a link of
/// their own ahead of the first spatial link: every shape of chain the charge counts.
fn charged_stacks() -> Vec<(&'static str, PreviewSource, Recipe)> {
    let mut stacks = measured_stacks();
    stacks.push((
        "Basic before Presence at 24 MP",
        jpeg_of(6_000, 4_000),
        Recipe {
            layers: vec![
                Layer::new(BASIC_EFFECT, json!({"exposure": 0.4, "contrast": 20.0})),
                Layer::new(
                    PRESENCE_EFFECT,
                    json!({"texture": 30.0, "clarity": 40.0, "dehaze": 25.0}),
                ),
            ],
            ..Recipe::default()
        },
    ));
    stacks
}

/// What the photo surface's slot for `tile` charges by the desktop's figures with no device
/// (`region_charge`'s parts): the boundary over its window, a tail's intermediate, the output in
/// its size bucket with its uniform, and the chain's charge.
fn surface_charge(tiles: &luxforge_core::RestTiles, tile: &luxforge_core::RestTile) -> u64 {
    let (plan, window, rect) = (&tiles.plan, tile.window, tile.rect);
    luxforge_ui::photo_surface::gpu_preview::texture_charge(
        (window.width, window.height),
        super::gpu_plan::boundary_format(tiles.format),
        (rect.width, rect.height),
        super::gpu_plan::has_tail(plan).then_some((plan.geometry.clamps, plan.linear)),
        true,
        super::compare_after::DEVICE_TEXTURE_LIMIT,
    ) + super::gpu_preview::chain_charge(plan, window, tiles.format)
}

/// The core charges a picture at rest's tile what the photo surface's slot allocates for it
/// (`docs/design/gpu-preview.md`, "The picture at rest"): the boundary, an intermediate for each
/// link before the last, a tail's, the output in its bucket, each link's kept planes and
/// parameters and the scratch pool once — the widget crate's `texture_charge` and `chain_charge`,
/// byte for byte — on every measured stack, over the tiles the editor plans and over tiles of every
/// side; and its light links' textures, the surface's light charge but for the links' buffers.
#[test]
fn gpu_rest_a_tiles_charge_is_what_its_slot_allocates() {
    assert_eq!(
        luxforge_core::GPU_PREVIEW_BYTES,
        luxforge_ui::photo_surface::GPU_PREVIEW_BUDGET,
        "the core plans within the surface's budget"
    );
    for (name, source, recipe) in charged_stacks() {
        let evaluation = committed(source, recipe);
        let mut planned = vec![planned_tiles(&evaluation)];
        for side in luxforge_core::REST_TILE_SIDES {
            planned.push(
                luxforge_core::qualification::rest_tiles(
                    &evaluation,
                    super::gpu_qualification::fit_bounds(),
                    side,
                )
                .expect("tiles"),
            );
        }
        for tiles in &planned {
            let count = tiles.tiles.len();
            for tile in [0, count / 3, count / 2, count - 1].map(|index| &tiles.tiles[index]) {
                let core = luxforge_core::rest_slot_bytes(
                    &tiles.plan,
                    tile.window,
                    tiles.format,
                    (tile.rect.width, tile.rect.height),
                    true,
                );
                assert_eq!(
                    core,
                    surface_charge(tiles, tile),
                    "{name}: the tile at {:?} over {:?}",
                    tile.rect,
                    tile.window
                );
            }
            let lights = luxforge_core::rest_light_bytes(&tiles.plan, tiles.format);
            let surface = super::gpu_preview::light_charge(&tiles.plan, tiles.format);
            assert_eq!(
                lights == 0,
                tiles.plan.lights.is_empty(),
                "{name}: light links"
            );
            assert!(
                (lights..lights + (1 << 20)).contains(&surface),
                "{name}: the light links' textures {lights} B, the surface's charge {surface} B"
            );
        }
    }
}

/// On a device, the slot the photo surface holds for a tile of the measured stacks charges the
/// core's figure, its light links' textures and the buffers the device sizes: every link's words
/// and blocks, and each light link's own, under a megabyte.
#[test]
fn gpu_rest_a_tiles_charge_is_the_slots_own_on_a_device() {
    let Some(qualifier) = headless("gpu_rest_a_tiles_charge_is_the_slots_own_on_a_device") else {
        return;
    };
    for (name, source, recipe) in charged_stacks() {
        let evaluation = committed(source, recipe);
        let tiles = planned_tiles(&evaluation);
        if tiles.warp().is_some() {
            // A lens warp's tail reads its part of the stage's grid, which only the desktop's grid
            // worker computes; its charge is held above without a device.
            continue;
        }
        let tile = tiles.tiles[tiles.tiles.len() / 2];
        let window = tile.window;
        let format = super::gpu_plan::boundary_format(tiles.format);
        let texels = vec![0u8; window.pixels() as usize * format.texel_bytes()];
        let boundary = luxforge_ui::photo_surface::GpuBoundary::new(
            Arc::new(texels),
            window.width,
            window.height,
            1,
            format,
        )
        .expect("a boundary");
        let plan = surface_plan_over(
            &tiles.plan,
            boundary,
            (window.x0, window.y0),
            None,
            Some(tile.rect),
        )
        .expect("a runnable plan");
        let charged = qualifier.charged_bytes(&plan).expect("a charge");
        let buffers: u64 = qualifier.buffer_bytes(&plan).expect("buffers").iter().sum();
        let core = luxforge_core::rest_slot_bytes(
            &tiles.plan,
            window,
            tiles.format,
            (tile.rect.width, tile.rect.height),
            true,
        ) + luxforge_core::rest_light_bytes(&tiles.plan, tiles.format);
        let rest = charged - buffers - core;
        assert!(
            rest < 1 << 20 && (rest == 0) == tiles.plan.lights.is_empty(),
            "{name}: the slot charges {charged} B, {buffers} B of it buffers, the core {core} B"
        );
    }
}

/// Whether `tile`'s rectangle holds the centre of `output`.
fn holds_centre(tile: &luxforge_core::RestTile, output: Stage) -> bool {
    let (x, y) = (output.width / 2, output.height / 2);
    (tile.rect.x0..tile.rect.x1()).contains(&x) && (tile.rect.y0..tile.rect.y1()).contains(&y)
}

/// The editor's tiles for the measured stacks, by the plan's own figures: within the rest's share
/// beside the view plan, the source and the accumulator, at most 1 GiB, and each tile's window
/// carrying at most about 24 MP·links, or the smallest side. The 60 MP drag stack's picture at rest
/// is 60 tiles of 1024 px. Prints each stack's side, count, slot shapes, share and a middle tile's
/// charge and work, and those of the tile holding the stage's centre at every side.
#[test]
fn gpu_rest_the_measured_stacks_are_tiled_within_the_rests_share() {
    for (name, source, recipe) in measured_stacks() {
        let evaluation = committed(source, recipe);
        let tiles = planned_tiles(&evaluation);
        let side = tiles
            .tiles
            .iter()
            .map(|tile| tile.rect.width.max(tile.rect.height))
            .max()
            .expect("a tile");
        let share = tiles.share.expect("a share");
        assert!(share <= luxforge_core::REST_SHARE_MAX, "{name}");
        let output = tiles.output;
        let middle = tiles
            .tiles
            .iter()
            .find(|tile| holds_centre(tile, output))
            .expect("a tile in the middle");
        let slot = luxforge_core::rest_slot_bytes(
            &tiles.plan,
            middle.window,
            tiles.format,
            (middle.rect.width, middle.rect.height),
            true,
        ) + luxforge_core::rest_light_bytes(&tiles.plan, tiles.format);
        let links = tiles.plan.spatial.len().max(1) as u64;
        let work = middle.window.pixels() * links;
        let shapes = tiles
            .tiles
            .windows(2)
            .filter(|pair| {
                (
                    pair[0].window.width,
                    pair[0].window.height,
                    pair[0].rect.width,
                    pair[0].rect.height,
                ) != (
                    pair[1].window.width,
                    pair[1].window.height,
                    pair[1].rect.width,
                    pair[1].rect.height,
                )
            })
            .count()
            + 1;
        eprintln!(
            "{name}: {} tiles of {side} px over {}x{} in {shapes} slot shapes; share {:.1} MB; \
             the centre tile's window {}x{}, its slot {:.1} MB, {:.1} MP·links over {links} \
             spatial links",
            tiles.tiles.len(),
            output.width,
            output.height,
            share as f64 / 1e6,
            middle.window.width,
            middle.window.height,
            slot as f64 / 1e6,
            work as f64 / 1e6,
        );
        for each in luxforge_core::REST_TILE_SIDES {
            let sided = luxforge_core::qualification::rest_tiles(
                &evaluation,
                super::gpu_qualification::fit_bounds(),
                each,
            )
            .expect("tiles");
            let tile = sided
                .tiles
                .iter()
                .find(|tile| holds_centre(tile, output))
                .expect("a tile in the middle");
            let new = luxforge_core::rest_slot_bytes(
                &sided.plan,
                tile.window,
                sided.format,
                (tile.rect.width, tile.rect.height),
                true,
            ) + luxforge_core::rest_light_bytes(&sided.plan, sided.format);
            eprintln!(
                "  at {each} px: {} tiles, the centre tile's window {}x{}, its slot {:.1} MB, {:.1} \
                 MP·links",
                sided.tiles.len(),
                tile.window.width,
                tile.window.height,
                new as f64 / 1e6,
                (tile.window.pixels() * links) as f64 / 1e6
            );
        }
        let smallest = luxforge_core::REST_TILE_SIDES[luxforge_core::REST_TILE_SIDES.len() - 1];
        assert!(
            side == smallest || (slot <= share && work <= luxforge_core::REST_TILE_WORK),
            "{name}: a side of {side} px"
        );
        let area: u64 = tiles.tiles.iter().map(|tile| tile.rect.pixels()).sum();
        assert_eq!(
            area,
            u64::from(output.width) * u64::from(output.height),
            "{name}: every pixel once"
        );
    }
    // The 60 MP drag stack's 2048 px tile in the middle of the stage, a 3363 px window, takes
    // about 1.13 GB, past the share's 1 GiB: it is drawn in 1024 px tiles, 60 of them.
    let (_, source, recipe) = measured_stacks().swap_remove(0);
    let tiles = planned_tiles(&committed(source, recipe));
    assert_eq!(
        (tiles.tiles.len(), tiles.tiles[0].rect.width),
        (60, 1024),
        "the 60 MP drag stack"
    );
}

/// The picture at rest the photo surface draws, reduced to the view, and its histogram and clipping
/// counts are the same, byte for byte and count for count, whatever side its tiles take and
/// whether they are drawn by slot shape or row by row, and so is the stage at full resolution:
/// every tile's codes are the whole stage's (the anchor contract, and a lens warp's tail
/// interpolating the stage's grid from its lattice alone, whatever node a tile's part of it starts
/// at). The reduction adds a view pixel's share of each tile it reaches in the order the tiles are
/// drawn, so a seam through a view pixel regroups its `f32` sum; here every such regrouping
/// quantizes to the same codes. On both paths, over every family whose texels depend on more than
/// their own pixel, a lens warp on a grid whose spacing is not a power of two among them.
#[test]
fn gpu_rest_the_picture_at_rest_is_the_same_at_every_side_and_order() {
    let test = "gpu_rest_the_picture_at_rest_is_the_same_at_every_side_and_order";
    let Some((backend, name)) = super::gpu_tiles_tests::host_adapter(test) else {
        return;
    };
    assert!(
        super::gpu_plan::install_output_encoding(),
        "the surface holds the core's output encoding"
    );
    let window = luxforge_ui::adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    let mut surface = luxforge_ui::photo_surface::gpu_preview::headless::HeadlessSurface::new(
        &window.device,
        &window.queue,
    );
    let bounds = luxforge_core::ProxyBounds {
        width: 160,
        height: 120,
    };
    // Each family's layers, made fresh for each evaluation.
    type Layers = fn() -> Vec<Layer>;
    let families: [(&str, Layers); 5] = [
        ("Presence", || {
            vec![Layer::new(
                PRESENCE_EFFECT,
                json!({"texture": 35.0, "clarity": 30.0, "dehaze": 20.0}),
            )]
        }),
        ("Detail", || vec![detail()]),
        ("Presence after Detail", || {
            vec![
                detail(),
                Layer::new(
                    PRESENCE_EFFECT,
                    json!({"texture": 20.0, "clarity": 25.0, "dehaze": 15.0}),
                ),
            ]
        }),
        // A lens warp, whose tail interpolates the stage's coordinate grid, each tile through its
        // part of it: a grid of a spacing whose reciprocal is inexact, so a tile's part starting at
        // another node rounds a division by the spacing otherwise.
        ("lens", || {
            vec![luxforge_core::qualification::lens_layer(
                -0.004,
                (WIDTH, HEIGHT),
            )]
        }),
        ("Detail before a lens", || {
            vec![
                detail(),
                luxforge_core::qualification::lens_layer(-0.004, (WIDTH, HEIGHT)),
            ]
        }),
    ];
    let mut version = 0;
    for (index, format) in [BoundaryFormat::Half, BoundaryFormat::Float]
        .into_iter()
        .enumerate()
    {
        let photograph = source(format);
        let gpu = super::gpu_tiles_tests::gpu_source(index as u64 + 1, &photograph);
        for (family, layers) in families {
            let what = format!("{format:?} {family}");
            let evaluation = committed(
                photograph.clone(),
                Recipe {
                    layers: layers(),
                    ..Recipe::default()
                },
            );
            let mut drawn = Vec::new();
            if let Some(grid) = evaluation_grid(&evaluation) {
                assert!(
                    !grid.spacing.is_power_of_two(),
                    "{what}: a grid of {} px divides exactly",
                    grid.spacing
                );
            }
            for side in [48, 96, 2048] {
                let tiles =
                    luxforge_core::qualification::rest_tiles(&evaluation, bounds, side).unwrap();
                let mut rows = tiles.clone();
                rows.tiles.sort_by_key(|tile| (tile.rect.y0, tile.rect.x0));
                for (order, tiles) in [("by shape", tiles), ("row by row", rows)] {
                    version += 1;
                    let handed = super::gpu_preview::rest_now(&gpu, &tiles, version).unwrap();
                    let rest = surface
                        .rest(&gpu, &handed)
                        .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
                    assert_eq!(
                        rest.codes.len(),
                        (tiles.reduction.as_ref().unwrap().view.0
                            * tiles.reduction.as_ref().unwrap().view.1)
                            as usize,
                        "{what}"
                    );
                    // The stage at full resolution, tile by tile, besides its reduction.
                    version += 1;
                    let stage = stage_codes(&mut surface, &gpu, &tiles, version);
                    drawn.push((
                        side,
                        order,
                        tiles.tiles.len(),
                        (rest.codes, stage),
                        rest.counts,
                    ));
                }
            }
            let (_, _, _, codes, counts) = &drawn[0];
            for (side, order, count, other, counted) in &drawn {
                assert!(
                    other == codes,
                    "{what}: {count} tiles of {side} px {order} draw another picture at rest"
                );
                assert_eq!(counted, counts, "{what}: {side} px {order}");
            }
            eprintln!(
                "{test}: {what}: {} pictures at rest, 1 to {} tiles, byte for byte",
                drawn.len(),
                drawn.iter().map(|(_, _, count, ..)| *count).max().unwrap()
            );
        }
    }
}

/// The coordinate grid of the whole output stage a lens warp in `evaluation`'s stack is drawn
/// through, `None` for a stack without one.
fn evaluation_grid(evaluation: &Evaluation) -> Option<luxforge_core::CoordinateGrid> {
    let tiles = planned_tiles(evaluation);
    tiles
        .warp()
        .map(|warp| warp.stage_grid(1.0).expect("a stage grid").expect("a grid"))
}

/// The output stage of `tiles` drawn tile by tile by `surface`, each tile as a picture at rest
/// draws it, laid at its place: RGBA codes row by row.
fn stage_codes(
    surface: &mut luxforge_ui::photo_surface::gpu_preview::headless::HeadlessSurface,
    gpu: &luxforge_ui::photo_surface::GpuSource,
    tiles: &luxforge_core::RestTiles,
    version: u64,
) -> Vec<[u8; 4]> {
    let handed = super::gpu_preview::rest_now(gpu, tiles, version).unwrap();
    let width = tiles.output.width as usize;
    let mut frame = vec![[0u8; 4]; width * tiles.output.height as usize];
    for plan in handed.tiles.iter() {
        let [x0, y0, x1, _] = plan.region.expect("a region").rect;
        let codes = surface
            .tile(gpu, plan)
            .unwrap_or_else(|fallback| panic!("the tile at ({x0}, {y0}): {fallback:?}"));
        let across = (x1 - x0) as usize;
        for (row, line) in codes.chunks_exact(across).enumerate() {
            let at = (y0 as usize + row) * width + x0 as usize;
            frame[at..at + across].copy_from_slice(line);
        }
    }
    frame
}

/// The corpus's RAWs at full resolution in tiles of 512 and 1024 px: the same codes, pixel for
/// pixel, through each recipe `LUXFORGE_REST_RECIPES` names (comma-separated corpus recipe ids,
/// the stack as opened by default) on each RAW `LUXFORGE_GPU_CORPUS_SOURCES` names (every RAW by
/// default). Prints where the differences fall.
/// `LUXFORGE_RAW_MANIFEST=MANIFEST cargo test --release -p luxforge-app
/// gpu_rest_raw_tiles_are_the_same_at_every_side -- --ignored --nocapture`.
#[test]
#[ignore = "the corpus RAWs: set LUXFORGE_RAW_MANIFEST to the private RAW manifest"]
fn gpu_rest_raw_tiles_are_the_same_at_every_side() {
    use super::gpu_qualification::{Opened, corpus_sources};
    let test = "gpu_rest_raw_tiles_are_the_same_at_every_side";
    let Some((backend, name)) = super::gpu_tiles_tests::host_adapter(test) else {
        return;
    };
    assert!(super::gpu_plan::install_output_encoding());
    let window = luxforge_ui::adapters::open(&backend, &name).unwrap();
    let mut surface = luxforge_ui::photo_surface::gpu_preview::headless::HeadlessSurface::new(
        &window.device,
        &window.queue,
    );
    let read = |path: std::path::PathBuf| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let manifest = read(
        std::env::var("LUXFORGE_RAW_MANIFEST")
            .expect("a manifest")
            .into(),
    );
    let corpus = read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/preview/corpus.json"),
    );
    let wanted = |key: &str| -> Option<Vec<String>> {
        std::env::var(key)
            .ok()
            .map(|list| list.split(',').map(str::to_owned).collect())
    };
    let sources = wanted("LUXFORGE_GPU_CORPUS_SOURCES");
    let recipes = wanted("LUXFORGE_REST_RECIPES").unwrap_or_else(|| vec![String::new()]);
    let dir = luxforge_testbase::paths::temp_dir("gpu-rest-raw");
    let mut version = 0;
    let mut failures = Vec::new();
    let found = corpus_sources(
        &corpus,
        std::path::Path::new("/nonexistent"),
        Some(&manifest),
    );
    for (index, source) in found
        .iter()
        .filter(|source| source.raw)
        .filter(|source| sources.as_ref().is_none_or(|ids| ids.contains(&source.id)))
        .enumerate()
    {
        for recipe in &recipes {
            let steps: Vec<serde_json::Value> = corpus["recipes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == recipe.as_str())
                .map_or_else(Vec::new, |entry| entry["steps"].as_array().unwrap().clone());
            let what = format!("{} {recipe}", source.id);
            let catalog = dir.join(format!("{}-{recipe}.sqlite", source.id));
            let opened = Opened::new(source, &steps, &catalog).expect("the RAW opened");
            let job = super::tasks::ready_preview_job(
                &opened.owner,
                luxforge_core::PreviewRequest::new(opened.client, opened.asset.clone()),
            )
            .expect("a job");
            let mut evaluation = job.evaluation;
            // Layers whose effect holds `LUXFORGE_REST_DROP` left out, to bisect a stack.
            if let Ok(dropped) = std::env::var("LUXFORGE_REST_DROP") {
                let mut recipe = evaluation.recipe().clone();
                recipe
                    .layers
                    .retain(|layer| !layer.effect_id.contains(dropped.as_str()));
                evaluation = committed(evaluation.source().clone(), recipe);
            }
            let gpu = super::gpu_preview::gpu_source_of(index as u64 + 1, evaluation.source())
                .expect("a GPU source");
            let bounds = super::gpu_qualification::fit_bounds();
            let mut frames = Vec::new();
            let sides: Vec<u32> = std::env::var("LUXFORGE_REST_SIDES").map_or_else(
                |_| vec![512, 1024],
                |list| list.split(',').map(|side| side.parse().unwrap()).collect(),
            );
            for side in sides {
                let tiles =
                    luxforge_core::qualification::rest_tiles(&evaluation, bounds, side).unwrap();
                version += 1;
                let codes = stage_codes(&mut surface, &gpu, &tiles, version);
                frames.push((tiles.output, codes));
            }
            let (output, first) = &frames[0];
            let (_, second) = &frames[1];
            let differing: Vec<(u32, u32)> = first
                .iter()
                .zip(second)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(at, _)| (at as u32 % output.width, at as u32 / output.width))
                .collect();
            let layers: Vec<&str> = evaluation
                .recipe()
                .layers
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect();
            eprintln!(
                "{test}: {what} {layers:?}, stage {}x{}: {} of {} pixels differ between 512 and \
                 1024 px tiles; first {:?}",
                output.width,
                output.height,
                differing.len(),
                first.len(),
                differing.iter().take(12).collect::<Vec<_>>()
            );
            if !differing.is_empty() {
                let columns: std::collections::BTreeSet<u32> =
                    differing.iter().map(|(x, _)| *x).collect();
                let rows: std::collections::BTreeSet<u32> =
                    differing.iter().map(|(_, y)| *y).collect();
                eprintln!(
                    "  columns {:?}..{:?} ({} distinct), rows {:?}..{:?} ({} distinct)",
                    columns.first(),
                    columns.last(),
                    columns.len(),
                    rows.first(),
                    rows.last(),
                    rows.len()
                );
                let largest = first
                    .iter()
                    .zip(second)
                    .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
                    .max();
                // How far each differing pixel is from the nearest seam of the 512 px tiles.
                let mut near = [0usize; 5];
                for (x, y) in &differing {
                    let seam = |at: u32| (at % 512).min(512 - at % 512);
                    let distance = seam(*x).min(seam(*y));
                    near[match distance {
                        0..=8 => 0,
                        9..=64 => 1,
                        65..=128 => 2,
                        129..=200 => 3,
                        _ => 4,
                    }] += 1;
                }
                eprintln!(
                    "  largest code difference {largest:?}; by distance to a 512 px seam \
                     (<=8, <=64, <=128, <=200, more): {near:?}"
                );
                failures.push(what);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "differ at 512 and 1024 px: {failures:?}"
    );
}
