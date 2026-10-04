//! The reduced-grid cache (`render::reduced`) through Presence's units and the render: the planes a
//! unit hands back and reads are the planes it computes, a warm render writes the cold render's
//! bytes, and the store keys, bounds and publishes only what it should.
//!
//! The stages are small and the tiles narrow, sides that are not multiples of the grid's factor
//! included, so every render runs many tiles on the calling thread, edge and corner tiles
//! included, and the linear source spans values from 3e-9 to 6 so a sum taken in another order
//! would round differently.

use super::{PRESENCE_EFFECT, clarity::Clarity, dehaze::Dehaze, filters};
use crate::{
    BASIC_EFFECT, Cancel, Component, ComponentMode, ErrorKind, Layer, LinearImage, LinearSettings,
    Mask, ModuleRegistry, Raster, Recipe, RenderContext, RenderOptions, RenderSource, SnapshotId,
    SourceImage,
    modules::{
        Cells, Global, GridPlanes, Parallelism, Planes, PlanesMut, Reduced, Region, SpatialUnit,
        Stage,
    },
    render::{
        reduced::ReducedEntry,
        testing::{frame_in, linear},
    },
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const WIDTH: u32 = 150;
const HEIGHT: u32 = 110;

/// A photo-like pattern with detail at every scale.
fn value(x: u32, y: u32, channel: u32) -> f64 {
    let (x, y) = (f64::from(x), f64::from(y));
    let wave = (x * 0.043 + y * 0.029 + f64::from(channel)).sin() * 0.3
        + (x * 0.21 - y * 0.17 * f64::from(channel + 1)).cos() * 0.15;
    (0.45 + wave + ((x * 7.0 + y * 3.0) % 11.0) / 60.0).clamp(0.0, 1.0)
}

fn jpeg(width: u32, height: u32, fingerprint: &str) -> SourceImage {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            for channel in 0..3 {
                rgba.push((value(x, y, channel) * 255.0).round() as u8);
            }
            rgba.push(255);
        }
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: fingerprint.into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// The linear value at a pixel: the pattern, with every thirteenth column alternating between
/// 6 and 3e-9, so the block sums span thirty binary orders.
fn raw_value(x: u32, y: u32, channel: u32) -> f32 {
    if x % 13 == 5 {
        if y.is_multiple_of(2) { 6.0 } else { 3.0e-9 }
    } else {
        (value(x, y, channel) * 1.4 - 0.05) as f32
    }
}

fn raw(width: u32, height: u32) -> LinearImage {
    let mut planes = Vec::with_capacity((width * height * 3) as usize);
    for channel in 0..3 {
        for y in 0..height {
            for x in 0..width {
                planes.push(raw_value(x, y, channel));
            }
        }
    }
    LinearImage::with_fingerprint(width, height, planes, "sha256:reduced-raw").unwrap()
}

/// Both domains' sources of one stage.
struct Sources {
    jpeg: SourceImage,
    raw: LinearImage,
}

impl Sources {
    fn new() -> Self {
        Self {
            jpeg: jpeg(WIDTH, HEIGHT, "sha256:reduced-jpeg"),
            raw: raw(WIDTH, HEIGHT),
        }
    }

    fn input(&self, linear_path: bool) -> RenderSource<'_> {
        if linear_path {
            linear(&self.raw, LinearSettings::default())
        } else {
            RenderSource::Byte(&self.jpeg)
        }
    }
}

/// A radial mask in the middle of the stage, so tiles near the edges are copied.
fn centre_mask() -> Mask {
    let mut mask = Mask::new("Centre");
    mask.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        json!({"x": 0.5, "y": 0.5, "radius_x": 0.25, "radius_y": 0.25, "angle": 0.0,
               "feather": 0.4}),
    ));
    mask
}

fn recipe(payload: Value, mask: Option<&Mask>) -> Recipe {
    let mut layer = Layer::new(PRESENCE_EFFECT, payload);
    layer.mask = mask.map(|mask| mask.id.clone());
    Recipe {
        layers: vec![layer],
        masks: mask.into_iter().cloned().collect(),
        ..Recipe::default()
    }
}

fn options(tile: u32) -> RenderOptions {
    RenderOptions::default().with_tile(tile)
}

fn frame(context: &RenderContext, input: RenderSource<'_>, recipe: &Recipe, tile: u32) -> Raster {
    frame_in(
        context,
        &ModuleRegistry::builtin(),
        input,
        SnapshotId::new(),
        recipe,
        options(tile),
    )
    .unwrap()
}

/// The three Presence stacks the cache treats differently: Dehaze alone and Clarity alone, each
/// the operation's first unit, and all three, where Dehaze runs first and Clarity behind Texture
/// runs as it always has. Each with a payload changed only in its amounts, and the reduction key
/// of the one unit whose planes are held.
fn stacks() -> [(&'static str, Value, Value, &'static str); 3] {
    [
        (
            "dehaze",
            json!({"dehaze": 40.0}),
            json!({"dehaze": 41.0}),
            "presence dehaze",
        ),
        (
            "clarity",
            json!({"clarity": -35.0}),
            json!({"clarity": 60.0}),
            "presence clarity",
        ),
        (
            "all three",
            json!({"dehaze": 30.0, "texture": 45.0, "clarity": 50.0}),
            json!({"dehaze": -20.0, "texture": 44.0, "clarity": 51.0}),
            "presence dehaze",
        ),
    ]
}

/// A cold render, the same render warm, and a warm render after an amount-only change each write
/// the bytes a render in a fresh context writes, in both domains, masked and unmasked, at tile
/// sides that divide neither side of the stage. The warm renders read the planes, every unmasked
/// tile of them, and only the first unit's planes are ever held.
#[test]
fn a_warm_render_reads_the_held_planes_and_writes_the_cold_bytes() {
    let sources = Sources::new();
    let mask = centre_mask();
    for linear_path in [false, true] {
        let input = || sources.input(linear_path);
        for (name, payload, changed, held_key) in stacks() {
            for masked in [false, true] {
                let mask = masked.then_some(&mask);
                let (seed, changed) =
                    (recipe(payload.clone(), mask), recipe(changed.clone(), mask));
                for tile in [7, 16, 64] {
                    let case =
                        format!("{name}, linear {linear_path}, masked {masked}, tile {tile}");
                    let cold = frame(&RenderContext::new(), input(), &seed, tile);
                    let cold_changed = frame(&RenderContext::new(), input(), &changed, tile);
                    let context = RenderContext::new();
                    let store = context.reduced();
                    assert_eq!(
                        frame(&context, input(), &seed, tile).rgba,
                        cold.rgba,
                        "{case}"
                    );
                    let filled = store.counts();
                    assert_eq!(
                        (filled.render_misses, filled.publishes, filled.tile_hits),
                        (1, 1, 0),
                        "{case}: the cold render fills"
                    );
                    assert!(
                        filled.tile_misses > 0 && filled.cells_handed_back > 0,
                        "{case}"
                    );
                    assert_eq!(
                        frame(&context, input(), &seed, tile).rgba,
                        cold.rgba,
                        "{case}: warm"
                    );
                    assert_eq!(
                        frame(&context, input(), &changed, tile).rgba,
                        cold_changed.rgba,
                        "{case}: warm after an amount-only change"
                    );
                    let warm = store.counts();
                    assert_eq!(warm.render_hits, 2, "{case}: the amount is not in the key");
                    // With all three fields Dehaze fills the rectangle Texture's and Clarity's
                    // halos need, whose reach crosses most of the tiles a small mask runs, so a
                    // masked tile is read only where every tile its reach touches ran.
                    if !masked || name != "all three" {
                        assert!(
                            warm.tile_hits > 0,
                            "{case}: the warm renders read the planes"
                        );
                    }
                    if !masked {
                        assert_eq!(
                            warm.tile_misses, filled.tile_misses,
                            "{case}: every unmasked warm tile is served"
                        );
                        assert_eq!(warm.tile_hits, 2 * filled.tile_misses, "{case}");
                    }
                    let keys = store.keys();
                    assert_eq!(keys.len(), 1, "{case}: one entry");
                    assert!(
                        keys[0].reduction.starts_with(held_key),
                        "{case}: only the first unit's planes, {:?}",
                        keys[0].reduction
                    );
                }
            }
        }
    }
}

/// Clarity behind Texture is not the operation's first unit, so nothing reads or fills the store.
#[test]
fn clarity_behind_texture_runs_as_it_always_has() {
    let sources = Sources::new();
    for linear_path in [false, true] {
        let stack = recipe(json!({"texture": 30.0, "clarity": 40.0}), None);
        let cold = frame(
            &RenderContext::new(),
            sources.input(linear_path),
            &stack,
            16,
        );
        let context = RenderContext::new();
        for _ in 0..2 {
            assert_eq!(
                frame(&context, sources.input(linear_path), &stack, 16).rgba,
                cold.rgba
            );
        }
        let counts = context.reduced().counts();
        assert_eq!(
            (
                counts.render_hits + counts.render_misses,
                counts.tile_hits + counts.tile_misses,
                counts.entries
            ),
            (0, 0, 0)
        );
    }
}

/// A masked render publishes the cells of the tiles that ran. An unmasked render of the same
/// operation then reads them where they cover a tile's reach, computes the rest, and writes the
/// bytes of a render that held nothing; what it computed joins the entry, so the next render reads
/// every tile.
#[test]
fn a_partial_plane_serves_only_the_tiles_it_covers() {
    let sources = Sources::new();
    let mask = centre_mask();
    for linear_path in [false, true] {
        let input = || sources.input(linear_path);
        // Dehaze and Clarity alone: with all three fields, Dehaze fills the rectangle the later
        // halos need, whose reach crosses most of the tiles this mask runs.
        for (name, payload, _, _) in stacks().into_iter().take(2) {
            let case = format!("{name}, linear {linear_path}");
            let (masked, unmasked) = (recipe(payload.clone(), Some(&mask)), recipe(payload, None));
            let cold = frame(&RenderContext::new(), input(), &unmasked, 7);
            let context = RenderContext::new();
            let store = context.reduced();
            frame(&context, input(), &masked, 7);
            let partial = store.counts();
            assert_eq!(partial.publishes, 1, "{case}");
            assert_eq!(
                frame(&context, input(), &unmasked, 7).rgba,
                cold.rgba,
                "{case}"
            );
            let served = store.counts();
            assert!(served.tile_hits > 0, "{case}: the covered tiles are read");
            assert!(
                served.tile_misses > partial.tile_misses,
                "{case}: the uncovered tiles compute"
            );
            assert_eq!(
                frame(&context, input(), &unmasked, 7).rgba,
                cold.rgba,
                "{case}"
            );
            let whole = store.counts();
            assert_eq!(whole.tile_misses, served.tile_misses, "{case}: now covered");
            assert_eq!(store.keys().len(), 1, "{case}");
        }
    }
}

/// A render cancelled between its tiles publishes nothing, though its first tiles handed cells
/// back; the next render fills the store.
#[test]
fn a_cancelled_render_publishes_nothing() {
    let sources = Sources::new();
    for linear_path in [false, true] {
        for (name, payload, _, _) in stacks() {
            let stack = recipe(payload, None);
            let context = RenderContext::new();
            let started = Arc::new(AtomicUsize::new(0));
            let counter = started.clone();
            // The stage is below the spatial threshold, so every tile runs on this thread.
            crate::render::spatial::observe_tile_checkpoint(Arc::new(move |cancel: &Cancel| {
                if counter.fetch_add(1, Ordering::SeqCst) == 5 {
                    cancel.cancel();
                }
            }));
            let error = frame_in(
                &context,
                &ModuleRegistry::builtin(),
                sources.input(linear_path),
                SnapshotId::new(),
                &stack,
                RenderOptions::exact(&Cancel::new()).with_tile(16),
            )
            .unwrap_err();
            crate::render::spatial::observe_tile_checkpoint(Arc::new(|_: &Cancel| {}));
            assert_eq!(error.kind, ErrorKind::Cancelled, "{name}");
            let counts = context.reduced().counts();
            assert!(counts.cells_handed_back > 0, "{name}: tiles ran before it");
            assert_eq!((counts.publishes, counts.entries), (0, 0), "{name}");
            frame(&context, sources.input(linear_path), &stack, 16);
            assert_eq!(context.reduced().counts().publishes, 1, "{name}");
        }
    }
}

/// A change of the amount alone hits; a change of the source, of the layers before the operation,
/// of the stage or of the unit misses. Clarity prepares no estimate, so each render differs from the
/// first in the part it names alone, where a Dehaze render behind another layer would also miss on
/// its atmospheric light; the store's own tests change each part of the key, the light included,
/// strictly alone. A JPEG's input identity names its dimensions, so the stage cannot change here
/// without it.
#[test]
fn an_amount_only_change_hits_and_a_change_of_each_key_part_misses() {
    let base = jpeg(WIDTH, HEIGHT, "sha256:reduced-key");
    let other_source = jpeg(WIDTH, HEIGHT, "sha256:reduced-key-other");
    let other_stage = jpeg(WIDTH + 2, HEIGHT, "sha256:reduced-key");
    let clarity = recipe(json!({"clarity": 40.0}), None);
    let context = RenderContext::new();
    let store = context.reduced();
    let render = |source: &SourceImage, stack: &Recipe| {
        frame(&context, RenderSource::Byte(source), stack, 16);
        let counts = store.counts();
        (counts.render_hits, counts.render_misses)
    };
    assert_eq!(render(&base, &clarity), (0, 1));
    assert_eq!(
        render(&base, &recipe(json!({"clarity": -70.0}), None)),
        (1, 1),
        "amount"
    );
    assert_eq!(render(&other_source, &clarity), (1, 2), "source");
    let mut behind_basic = clarity.clone();
    behind_basic
        .layers
        .insert(0, Layer::new(BASIC_EFFECT, json!({"exposure": 0.3})));
    assert_eq!(render(&base, &behind_basic), (1, 3), "layers before");
    assert_eq!(render(&other_stage, &clarity), (1, 4), "stage");
    assert_eq!(
        render(&base, &recipe(json!({"dehaze": 40.0}), None)),
        (1, 5),
        "unit"
    );
    // Each of them is now held, and reads.
    assert_eq!(render(&other_source, &clarity), (2, 5));
    assert_eq!(render(&base, &behind_basic), (3, 5));
}

/// The least recently used entry is evicted for one that would pass the limit beside it, and a
/// grid whose planes alone pass it is refused; neither changes a byte.
#[test]
fn the_store_evicts_the_least_recent_entry_and_refuses_an_oversized_one() {
    let first = jpeg(WIDTH, HEIGHT, "sha256:reduced-first");
    let second = jpeg(WIDTH, HEIGHT, "sha256:reduced-second");
    let stack = recipe(json!({"dehaze": 40.0}), None);
    let cold = frame(
        &RenderContext::new(),
        RenderSource::Byte(&first),
        &stack,
        64,
    );
    // Dehaze's two planes over 38 × 28 cells, and six tiles of 64 px.
    let entry = 38 * 28 * 2 * 4 + 6;
    let context = RenderContext::with_reduced_limit(entry + entry / 2);
    frame(&context, RenderSource::Byte(&first), &stack, 64);
    frame(&context, RenderSource::Byte(&second), &stack, 64);
    let store = context.reduced();
    let counts = store.counts();
    assert_eq!(
        (counts.evictions, counts.entries, counts.retained_bytes),
        (1, 1, entry)
    );
    assert_eq!(store.keys()[0].fingerprint, "sha256:reduced-second");
    assert_eq!(
        frame(&context, RenderSource::Byte(&first), &stack, 64).rgba,
        cold.rgba,
        "the evicted entry is computed again"
    );

    let small = RenderContext::with_reduced_limit(38 * 28 * 2 * 4 - 1);
    assert_eq!(
        frame(&small, RenderSource::Byte(&first), &stack, 64).rgba,
        cold.rgba
    );
    let counts = small.reduced().counts();
    assert_eq!(
        (counts.refusals, counts.entries, counts.cells_handed_back),
        (1, 0, 0)
    );
}

/// The cells of the grid, recomputed from the operation's input as each unit computes them: for
/// Clarity the 4x block means of the encoded luminance, and for Dehaze the encoded luminance of the
/// `f32` block means and the dark channel's input from the `f64` ones. Each block is summed in
/// `f64`, row by row and left to right.
fn recomputed(pixel: &dyn Fn(u32, u32) -> [f32; 3], dehaze: Option<&[f64]>) -> Vec<f32> {
    let (width, height) = (WIDTH.div_ceil(4), HEIGHT.div_ceil(4));
    let cells = (width * height) as usize;
    let mut planes = vec![0.0; cells * if dehaze.is_some() { 2 } else { 1 }];
    for j in 0..height {
        for i in 0..width {
            let (mut sums, mut encoded, mut count) = ([0.0_f64; 3], 0.0_f64, 0.0_f64);
            for y in j * 4..((j + 1) * 4).min(HEIGHT) {
                for x in i * 4..((i + 1) * 4).min(WIDTH) {
                    let rgb = pixel(x, y);
                    for (sum, value) in sums.iter_mut().zip(rgb) {
                        *sum += f64::from(value);
                    }
                    encoded += f64::from(filters::encoded_luminance(rgb));
                    count += 1.0;
                }
            }
            let index = (j * width + i) as usize;
            match dehaze {
                None => planes[index] = (encoded / count) as f32,
                Some(light) => {
                    let means = sums.map(|sum| sum / count);
                    planes[index] = filters::encoded_luminance(means.map(|mean| mean as f32));
                    planes[cells + index] = (0..3).fold(f32::INFINITY, |smallest, channel| {
                        smallest.min((means[channel] / light[channel]).clamp(0.0, 1.0) as f32)
                    });
                }
            }
        }
    }
    planes
}

fn held_values(entry: &ReducedEntry) -> Vec<u32> {
    let planes = entry.planes();
    (0..if entry.key().reduction.contains("dehaze") {
        2
    } else {
        1
    })
        .flat_map(|plane| planes.plane(plane).iter().map(|value| value.to_bits()))
        .collect()
}

/// A cold unmasked render hands back every cell of the grid exactly once, at tile sides that are
/// and are not multiples of the factor and narrower than it, and the planes it publishes are, bit
/// for bit, the cells recomputed from its input: a hit returns what a recomputation gives.
#[test]
fn every_cell_is_handed_back_once_and_holds_its_recomputed_value() {
    let sources = Sources::new();
    let cells = u64::from(WIDTH.div_ceil(4) * HEIGHT.div_ceil(4));
    let decoded = |x: u32, y: u32| {
        let index = ((y * WIDTH + x) * 4) as usize;
        let rgba = &sources.jpeg.rgba[index..index + 4];
        crate::colour::srgb::decode_pixel([rgba[0], rgba[1], rgba[2]])
    };
    let linear_pixel =
        |x: u32, y: u32| std::array::from_fn(|channel| raw_value(x, y, channel as u32));
    for linear_path in [false, true] {
        let pixel: &dyn Fn(u32, u32) -> [f32; 3] =
            if linear_path { &linear_pixel } else { &decoded };
        for (name, payload) in [
            ("dehaze", json!({"dehaze": 40.0})),
            ("clarity", json!({"clarity": 40.0})),
        ] {
            let stack = recipe(payload, None);
            for tile in [3, 4, 5, 7, 13, 150] {
                let case = format!("{name}, linear {linear_path}, tile {tile}");
                let context = RenderContext::new();
                frame(&context, sources.input(linear_path), &stack, tile);
                let counts = context.reduced().counts();
                assert_eq!(counts.cells_handed_back, cells, "{case}: each cell once");
                let entries = context.reduced().entries();
                assert_eq!(entries.len(), 1, "{case}");
                let entry = &entries[0];
                assert!(
                    entry.covers(Region::whole(Stage {
                        width: WIDTH.div_ceil(4),
                        height: HEIGHT.div_ceil(4),
                    })),
                    "{case}"
                );
                let light = entry.global();
                let expected: Vec<u32> = recomputed(pixel, light.as_deref())
                    .iter()
                    .map(|value| value.to_bits())
                    .collect();
                assert_eq!(held_values(entry), expected, "{case}");
            }
        }
    }
}

/// Each unit, run directly over the rectangles a tile's chain gives it on an odd stage, writes the
/// same bits computing its planes and handing its cells back, and reading them back from what its
/// tiles handed back with an input of its output rectangle alone, in the scratch it declares for
/// the input it reads without them; and at every tile side the cells handed back are the same.
#[test]
fn a_unit_writes_the_same_bits_handing_back_and_reading_its_planes() {
    let stage = Stage {
        width: 61,
        height: 47,
    };
    let values: Vec<f32> = (0..3)
        .flat_map(|channel| {
            (0..stage.height)
                .flat_map(move |y| (0..stage.width).map(move |x| raw_value(x, y, channel)))
        })
        .collect();
    let cut = |region: Region| -> Vec<f32> {
        let mut out = Vec::with_capacity(region.pixels() as usize * 3);
        for channel in 0..3 {
            for y in region.y0..region.y1() {
                let start = ((channel * stage.height + y) * stage.width + region.x0) as usize;
                out.extend_from_slice(&values[start..start + region.width as usize]);
            }
        }
        out
    };
    let light = Global::new(vec![0.8, 0.85, 0.9]).unwrap();
    let units: [(&str, Arc<dyn SpatialUnit>, Option<&Global>); 4] = [
        ("clarity", Arc::new(Clarity::new(-45.0, 480)), None),
        ("clarity wide", Arc::new(Clarity::new(70.0, 1600)), None),
        ("dehaze", Arc::new(Dehaze::new(55.0, 480)), Some(&light)),
        (
            "dehaze negative",
            Arc::new(Dehaze::new(-30.0, 900)),
            Some(&light),
        ),
    ];
    for (name, unit, global) in units {
        let grid = unit.reduced_grid().expect("a grid");
        let cells = grid.cells(stage);
        let halo = unit.halo(stage);
        let run = |input: Region, out: Region, reduced: Option<Reduced<'_>>, declared: Region| {
            let values = cut(input);
            let input = Planes::new(stage, input, &values).unwrap();
            let mut written = vec![f32::NAN; out.pixels() as usize * 3];
            let mut output = PlanesMut::new(stage, out, &mut written).unwrap();
            let declared = unit.scratch_bytes(Stage {
                width: declared.width,
                height: declared.height,
            });
            let mut scratch = vec![f32::NAN; (declared / 4) as usize];
            match reduced {
                None => unit
                    .apply(
                        &input,
                        &mut output,
                        global,
                        &mut scratch,
                        Parallelism::Serial,
                    )
                    .unwrap(),
                Some(reduced) => unit
                    .apply_reduced(
                        &input,
                        &mut output,
                        global,
                        &mut scratch,
                        Parallelism::Serial,
                        &Cancel::never(),
                        reduced,
                    )
                    .unwrap(),
            }
            written
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        };
        let mut handed: Option<Vec<f32>> = None;
        for side in [5_u32, 9, 16, 61] {
            let tiles: Vec<Region> = (0..stage.height)
                .step_by(side as usize)
                .flat_map(|y0| {
                    (0..stage.width)
                        .step_by(side as usize)
                        .map(move |x0| Region {
                            x0,
                            y0,
                            width: side.min(stage.width - x0),
                            height: side.min(stage.height - y0),
                        })
                })
                .collect();
            let mut plane =
                vec![f32::NAN; cells.width as usize * cells.height as usize * grid.planes];
            let mut chains = Vec::new();
            for tile in &tiles {
                let input = tile.grown(halo, stage);
                let out = input.shrunk(halo, stage);
                let computed = run(input, out, None, input);
                let mut back = Cells::for_tile(&grid, *tile);
                assert_eq!(
                    run(input, out, Some(Reduced::Hand(&mut back)), input),
                    computed,
                    "{name}, side {side}, {tile:?}: handing back"
                );
                let rect = back.rect();
                let planes = back.planes(cells);
                for index in 0..grid.planes {
                    let from = planes.plane(index);
                    for y in rect.y0..rect.y1() {
                        for x in rect.x0..rect.x1() {
                            plane[index * cells.width as usize * cells.height as usize
                                + (y * cells.width + x) as usize] =
                                from[((y - rect.y0) * rect.width + (x - rect.x0)) as usize];
                        }
                    }
                }
                chains.push((input, out, computed));
            }
            assert!(
                plane.iter().all(|value| !value.is_nan()),
                "{name}, side {side}"
            );
            let bits: Vec<f32> = plane.clone();
            match &handed {
                None => handed = Some(bits),
                Some(first) => assert_eq!(
                    first.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    bits.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    "{name}, side {side}: the same cells at every side"
                ),
            }
            let held = GridPlanes::new(cells, Region::whole(cells), grid.planes, &plane);
            for (input, out, computed) in chains {
                assert_eq!(
                    run(out, out, Some(Reduced::Held(held)), input),
                    computed,
                    "{name}, side {side}, {out:?}: reading"
                );
            }
        }
    }
}
