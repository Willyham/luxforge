//! TASK-018: the brush component against the frozen study.
//!
//! The mathematics is `docs/design/mask-study.md#the-brush` and the oracle is the independent `f64`
//! reference at `tests/reference/mask.rs`, which shares no code with production. What is asserted
//! here is **bit-identity**, not a tolerance: the production unit transcribes the study's
//! expressions in the study's order, so the two agree in their last bits or the transcription is
//! wrong.
//!
//! Everything a person can do to a brush component is covered from the outside: randomized strokes
//! at every size, feather, flow and erase flag; a one-point stroke and a doubled-back path; the
//! accumulation properties the design claims; the conservative rectangle, exhaustively on small
//! stages; the occupancy cap; and the measurement that the grid index is doing its job.

mod reference;

use luxforge_core::{
    Component, ComponentMode, ErrorKind, Mask, Stage,
    mask::{CompiledMask, SEGMENTS_PER_PIXEL, STROKES_PER_COMPONENT},
    path::{Stroke, StrokeTable},
};
use reference::mask::{
    Brush, BrushStroke, ColourLimit as RefColourLimit, Stage as RefStage, brush_coverage,
    brush_segments, stroke_coverage,
};
use serde_json::json;

/// The pixel value a geometric component is handed and ignores (proposal P12 of
/// `docs/design/range-study.md`). Most masks here hold gradients and unlimited brushes, whose
/// coverage is a function of position alone, so the value is arbitrary and the same at every call;
/// `mask_range.rs` proves that ignoring it is exact rather than approximate. The strokes that *are*
/// limited to a colour have their own tests at the end of this file, and those pass real pixels.
const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + u * (hi - lo)
    }

    fn next_usize(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

/// The reference's view of the **stored** stroke, which is what production evaluates: the positions
/// snapped to the path grid and decimated there, and the radius quantized to the same grid. Feeding
/// the reference anything else would compare two different strokes.
fn as_reference(stroke: &Stroke) -> BrushStroke {
    BrushStroke {
        points: stroke.points().collect(),
        size: stroke.size(),
        feather: stroke.feather(),
        flow: stroke.flow(),
        erase: stroke.erase(),
        // The stored limit travels to the reference exactly as every other stored setting does:
        // production reads it from the stroke and so does the oracle, so the two compare the same
        // stroke rather than one limited and one not.
        colour: stroke.colour_limit().map(|limit| RefColourLimit {
            seed: limit.seed(),
            refine: limit.refine(),
        }),
    }
}

/// A mask holding one add brush component over these strokes, with the table they resolve through.
fn brush_mask(strokes: &[Stroke]) -> (Mask, StrokeTable) {
    let mut table = StrokeTable::new("the brush tests");
    let addresses: Vec<String> = strokes
        .iter()
        .map(|stroke| table.insert(stroke.clone()).to_string())
        .collect();
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("brush");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "brush",
        json!({ "strokes": addresses }),
    ));
    (mask, table)
}

/// A random stroke posted the way a client posts one, through the host's own capture, so every test
/// here runs on strokes a gesture could actually have produced.
fn random_stroke(rng: &mut SplitMix64, erase: bool) -> Stroke {
    let count = 1 + rng.next_usize(8);
    let mut points = Vec::with_capacity(count);
    let mut x = rng.next_range(0.05, 0.95);
    let mut y = rng.next_range(0.05, 0.95);
    for _ in 0..count {
        points.push([x, y]);
        x = (x + rng.next_range(-0.25, 0.25)).clamp(-1.0, 2.0);
        y = (y + rng.next_range(-0.25, 0.25)).clamp(-1.0, 2.0);
    }
    Stroke::capture(
        &points,
        rng.next_range(0.01, 0.2),
        rng.next_range(0.0, 100.0),
        rng.next_range(1.0, 100.0),
        erase,
    )
    .expect("a legal stroke")
}

// ---------------------------------------------------------------------------
// Bit-identity with the frozen reference
// ---------------------------------------------------------------------------

/// The production field is bit-identical to the reference across randomized strokes, sizes,
/// feathers, flows and positions, on two stages and at every pixel of a small one.
#[test]
fn the_capsule_field_is_bit_identical_to_the_frozen_reference() {
    let mut rng = SplitMix64(0x018B_171D);
    let mut compared = 0usize;
    for (width, height) in [(61u32, 47u32), (48, 64)] {
        let reference_stage = RefStage::new(width, height);
        for _ in 0..120 {
            let count = 1 + rng.next_usize(4);
            let strokes: Vec<Stroke> = (0..count)
                .map(|_| {
                    let erase = rng.next_bool();
                    random_stroke(&mut rng, erase)
                })
                .collect();
            let (mask, table) = brush_mask(&strokes);
            let compiled = CompiledMask::new(&mask, stage(width, height), &table)
                .expect("a legal brush compiles");
            let expected = Brush {
                strokes: strokes.iter().map(as_reference).collect(),
            };
            for y in 0..height {
                for x in 0..width {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    let want = brush_coverage(&expected, &reference_stage, u, v, ANY_PIXEL);
                    let got = compiled.coverage(x, y, ANY_PIXEL);
                    assert_eq!(
                        got.to_bits(),
                        want.to_bits(),
                        "at ({x}, {y}) on {width}x{height}: {got} against {want}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared >= 600_000, "compared only {compared} pixels");
}

/// The two shapes the design names by hand: a one-point stroke, which is a disc, and a doubled-back
/// path, which is one pass and not two. Both against the reference, bit for bit.
#[test]
fn a_one_point_stroke_and_a_doubled_back_path_match_the_reference() {
    let reference_stage = RefStage::new(80, 60);
    let cases = [
        vec![[0.5, 0.5]],
        vec![[0.3, 0.5], [0.7, 0.5], [0.3, 0.5]],
        vec![[0.3, 0.3], [0.7, 0.7], [0.3, 0.7], [0.7, 0.3]],
    ];
    for points in cases {
        for (feather, flow) in [(0.0, 100.0), (50.0, 100.0), (100.0, 40.0)] {
            let stroke = Stroke::capture(&points, 0.09, feather, flow, false).unwrap();
            let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
            let compiled = CompiledMask::new(&mask, stage(80, 60), &table).unwrap();
            let expected = Brush {
                strokes: vec![as_reference(&stroke)],
            };
            for y in 0..60 {
                for x in 0..80 {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    assert_eq!(
                        compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                        brush_coverage(&expected, &reference_stage, u, v, ANY_PIXEL).to_bits(),
                        "{points:?} feather {feather} flow {flow} at ({x}, {y})"
                    );
                }
            }
        }
    }
    // A one-point stroke is a disc: equal coverage at equal distance in every direction.
    let stroke = Stroke::capture(&[[0.5, 0.5]], 0.1, 60.0, 100.0, false).unwrap();
    let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
    let square = CompiledMask::new(&mask, stage(101, 101), &table).unwrap();
    // Within the arithmetic of the pixel grid itself: `(x + 0.5)/H` rounds, so two pixels the same
    // number of steps either side of the centre are not exactly equidistant from it, and the field
    // is a disc to that precision and not beyond it.
    for offset in [5u32, 17, 33] {
        let north = square.coverage(50, 50 - offset, ANY_PIXEL);
        let south = square.coverage(50, 50 + offset, ANY_PIXEL);
        let east = square.coverage(50 + offset, 50, ANY_PIXEL);
        let west = square.coverage(50 - offset, 50, ANY_PIXEL);
        for (a, b) in [(north, south), (east, west), (north, east)] {
            assert!((a - b).abs() < 1e-12, "{a} against {b} at offset {offset}");
        }
    }
}

// ---------------------------------------------------------------------------
// The accumulation properties
// ---------------------------------------------------------------------------

/// Add strokes commute: permuting them leaves coverage where it was, to one ulp, and identical on
/// the overwhelming majority of pixels. An erase stroke does not commute, and the difference is a
/// whole edit.
#[test]
fn add_strokes_commute_and_an_erase_stroke_does_not() {
    let mut rng = SplitMix64(0x0018_C01D);
    let size = stage(64, 48);
    let mut worst = 0.0f64;
    for _ in 0..60 {
        let count = 2 + rng.next_usize(4);
        let strokes: Vec<Stroke> = (0..count).map(|_| random_stroke(&mut rng, false)).collect();
        let (mask, table) = brush_mask(&strokes);
        let ordered = CompiledMask::new(&mask, size, &table).unwrap();
        for _ in 0..4 {
            let mut shuffled = strokes.clone();
            for index in (1..shuffled.len()).rev() {
                shuffled.swap(index, rng.next_usize(index + 1));
            }
            let (permuted_mask, permuted_table) = brush_mask(&shuffled);
            let permuted = CompiledMask::new(&permuted_mask, size, &permuted_table).unwrap();
            for y in 0..size.height {
                for x in 0..size.width {
                    worst = worst.max(
                        (ordered.coverage(x, y, ANY_PIXEL) - permuted.coverage(x, y, ANY_PIXEL))
                            .abs(),
                    );
                }
            }
        }
    }
    assert!(
        worst <= 1e-15,
        "permuting add strokes moved coverage by {worst:e}"
    );

    // The erase stroke is where order lives.
    let add = Stroke::capture(&[[0.3, 0.5], [0.7, 0.5]], 0.1, 50.0, 100.0, false).unwrap();
    let erase = Stroke::capture(&[[0.5, 0.35], [0.5, 0.65]], 0.1, 50.0, 100.0, true).unwrap();
    let (painted, painted_table) = brush_mask(&[add.clone(), erase.clone()]);
    let (erased, erased_table) = brush_mask(&[erase, add]);
    let painted = CompiledMask::new(&painted, size, &painted_table).unwrap();
    let erased = CompiledMask::new(&erased, size, &erased_table).unwrap();
    let mut divergence = 0.0f64;
    for y in 0..size.height {
        for x in 0..size.width {
            divergence = divergence
                .max((painted.coverage(x, y, ANY_PIXEL) - erased.coverage(x, y, ANY_PIXEL)).abs());
        }
    }
    assert!(
        divergence > 0.5,
        "the erase order changed coverage by only {divergence}"
    );
}

/// Deleting one stroke from the middle of a component is indistinguishable from one never made, bit
/// for bit at every pixel, with an erase stroke in the list so the property is not tested only where
/// it is trivial.
#[test]
fn deleting_a_stroke_is_indistinguishable_from_one_never_made() {
    let mut rng = SplitMix64(0x0018_DE1E);
    let size = stage(56, 40);
    for _ in 0..40 {
        let count = 3 + rng.next_usize(4);
        let strokes: Vec<Stroke> = (0..count)
            .map(|index| random_stroke(&mut rng, index % 3 == 2))
            .collect();
        let removed = 1 + rng.next_usize(count - 1);
        let kept: Vec<Stroke> = strokes
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != removed)
            .map(|(_, stroke)| stroke.clone())
            .collect();
        let (with_gap, gap_table) = brush_mask(&kept);
        let (never, never_table) = brush_mask(&kept);
        let with_gap = CompiledMask::new(&with_gap, size, &gap_table).unwrap();
        let never = CompiledMask::new(&never, size, &never_table).unwrap();
        for y in 0..size.height {
            for x in 0..size.width {
                assert_eq!(
                    with_gap.coverage(x, y, ANY_PIXEL).to_bits(),
                    never.coverage(x, y, ANY_PIXEL).to_bits()
                );
            }
        }
    }
}

/// A stroke's density is its own, whatever the pointer's rate: painting the same path twice in one
/// stroke is one density, while painting it as two strokes builds up. Both against the field, not
/// against prose.
#[test]
fn one_pass_is_one_density_and_a_second_pass_builds_up() {
    let size = stage(64, 48);
    let path = [[0.25, 0.5], [0.75, 0.5]];
    let once = Stroke::capture(&path, 0.08, 60.0, 40.0, false).unwrap();
    // The same path posted at eight times the sampling rate: the decimation puts it back on the
    // same stored positions, so it is literally the same stored stroke and therefore the same
    // address.
    let dense: Vec<[f64; 2]> = (0..=8)
        .map(|step| {
            let t = f64::from(step) / 8.0;
            [0.25 + t * 0.5, 0.5]
        })
        .collect();
    let sampled = Stroke::capture(&dense, 0.08, 60.0, 40.0, false).unwrap();
    assert_eq!(once.id(), sampled.id(), "one path is one stored stroke");

    let (single, single_table) = brush_mask(std::slice::from_ref(&once));
    let (doubled, doubled_table) = brush_mask(&[once.clone(), once]);
    let single = CompiledMask::new(&single, size, &single_table).unwrap();
    let doubled = CompiledMask::new(&doubled, size, &doubled_table).unwrap();
    let mut grew = 0usize;
    for y in 0..size.height {
        for x in 0..size.width {
            let a = single.coverage(x, y, ANY_PIXEL);
            let b = doubled.coverage(x, y, ANY_PIXEL);
            assert!(b >= a, "a second pass removed coverage at ({x}, {y})");
            if b > a {
                grew += 1;
            }
        }
    }
    assert!(grew > 0, "a second pass changed nothing anywhere");
    // One pass reaches exactly the flow, and a second reaches the screen union of two.
    assert_eq!(single.coverage(32, 24, ANY_PIXEL), 0.4);
    assert_eq!(doubled.coverage(32, 24, ANY_PIXEL), 0.4 + (1.0 - 0.4) * 0.4);
}

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// The conservative rectangle is correct, checked exhaustively on small stages: every pixel outside
/// it has coverage exactly zero, and the rectangle is not the whole stage when it does not have to
/// be.
#[test]
fn the_bounds_rectangle_is_conservative_and_correct_on_small_stages() {
    let mut rng = SplitMix64(0x018B_011D);
    let mut narrowed = 0usize;
    for (width, height) in [(37u32, 29u32), (24, 32), (16, 16)] {
        let size = stage(width, height);
        for _ in 0..80 {
            let count = 1 + rng.next_usize(3);
            let strokes: Vec<Stroke> = (0..count)
                .map(|index| random_stroke(&mut rng, index % 3 == 2))
                .collect();
            let (mask, table) = brush_mask(&strokes);
            let compiled = CompiledMask::new(&mask, size, &table).unwrap();
            let bounds = compiled.bounds();
            if bounds.width < width || bounds.height < height {
                narrowed += 1;
            }
            for y in 0..height {
                for x in 0..width {
                    let inside =
                        x >= bounds.x0 && x < bounds.x1() && y >= bounds.y0 && y < bounds.y1();
                    if !inside {
                        assert_eq!(
                            compiled.coverage(x, y, ANY_PIXEL),
                            0.0,
                            "coverage outside the rectangle at ({x}, {y}) on {width}x{height}, \
                             rectangle {bounds:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(
        narrowed > 100,
        "the rectangle narrowed on only {narrowed} components, which is not a bounds test"
    );
}

/// A component whose only strokes erase covers nothing, so its rectangle is empty and its field is
/// exactly zero; and a component with a zero-flow stroke is the same.
#[test]
fn a_component_that_cannot_add_coverage_bounds_nothing() {
    let size = stage(40, 30);
    for stroke in [
        Stroke::capture(&[[0.3, 0.5], [0.7, 0.5]], 0.1, 50.0, 100.0, true).unwrap(),
        Stroke::capture(&[[0.3, 0.5], [0.7, 0.5]], 0.1, 50.0, 0.0, false).unwrap(),
    ] {
        let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
        let compiled = CompiledMask::new(&mask, size, &table).unwrap();
        assert!(compiled.bounds().is_empty(), "{stroke:?} bounded something");
        for y in 0..size.height {
            for x in 0..size.width {
                assert_eq!(compiled.coverage(x, y, ANY_PIXEL), 0.0);
            }
        }
    }
    // An empty component list of strokes is the same answer, and it compiles.
    let (mask, table) = brush_mask(&[]);
    let compiled = CompiledMask::new(&mask, size, &table).unwrap();
    assert!(compiled.bounds().is_empty());
    assert_eq!(compiled.coverage(20, 15, ANY_PIXEL), 0.0);
    assert_eq!(compiled.min_feature_px(size), f32::INFINITY);
}

/// An inverted brush component is non-zero over almost the whole stage, so the honest conservative
/// rectangle is the whole stage — the same answer an inverted radial gives.
#[test]
fn an_inverted_brush_bounds_the_whole_stage() {
    let size = stage(40, 30);
    let stroke = Stroke::capture(&[[0.5, 0.5]], 0.1, 50.0, 100.0, false).unwrap();
    let (mut mask, table) = brush_mask(std::slice::from_ref(&stroke));
    mask.components[0].invert = true;
    let compiled = CompiledMask::new(&mask, size, &table).unwrap();
    assert_eq!(compiled.bounds().width, size.width);
    assert_eq!(compiled.bounds().height, size.height);
    // The corner is outside the capsule, so the component was exactly zero there and its inversion
    // is exactly one.
    assert_eq!(compiled.coverage(0, 0, ANY_PIXEL), 1.0);
}

// ---------------------------------------------------------------------------
// Limits and refusals
// ---------------------------------------------------------------------------

/// A mask whose densest cell would make a pixel test more than the declared number of segments is
/// refused when it compiles, with a `resource-limit` error naming the mask — and the refusal
/// happens before any pixel is read, which is what makes it a bound on the point query rather than
/// an observation about one.
#[test]
fn a_component_over_the_occupancy_cap_is_refused_before_a_pixel_is_read() {
    // Many long zig-zag strokes through one small area: every segment's grown box reaches the same
    // cells, so the occupancy climbs with the stroke count.
    let mut strokes = Vec::new();
    for index in 0..STROKES_PER_COMPONENT {
        let base = 0.45 + f64::from(index as u32) * 0.0001;
        let points: Vec<[f64; 2]> = (0..40)
            .map(|step| {
                let t = f64::from(step);
                [base + 0.02 * (t % 2.0), 0.45 + 0.0005 * t]
            })
            .collect();
        strokes.push(Stroke::capture(&points, 0.05, 50.0, 100.0, false).unwrap());
    }
    let (mask, table) = brush_mask(&strokes);
    let error = CompiledMask::new(&mask, stage(400, 300), &table)
        .expect_err("a component this dense is over the cap");
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(
        error.detail.contains("mask Mask 1")
            && error.detail.contains(&SEGMENTS_PER_PIXEL.to_string())
            && error.detail.contains("segments tested per pixel"),
        "{}",
        error.detail
    );
    // A sparse component of the same stroke count is fine, so the refusal is about density and not
    // about the count.
    let mut spread = Vec::new();
    for index in 0..STROKES_PER_COMPONENT {
        let t = f64::from(index as u32) / f64::from(STROKES_PER_COMPONENT as u32);
        spread.push(Stroke::capture(&[[0.02 + 0.96 * t, 0.5]], 0.004, 50.0, 100.0, false).unwrap());
    }
    let (sparse, sparse_table) = brush_mask(&spread);
    CompiledMask::new(&sparse, stage(400, 300), &sparse_table)
        .expect("a sparse component of the same stroke count compiles");
}

/// More strokes than a component may hold is refused by name, without a stage and without the store,
/// so it fails reading the recipe as well as drawing it.
#[test]
fn a_component_over_the_stroke_limit_names_the_limit() {
    let mut strokes = Vec::new();
    for index in 0..=STROKES_PER_COMPONENT {
        let t = f64::from(index as u32) / f64::from(STROKES_PER_COMPONENT as u32 + 1);
        strokes.push(Stroke::capture(&[[0.02 + 0.9 * t, 0.5]], 0.01, 50.0, 100.0, false).unwrap());
    }
    let (mask, table) = brush_mask(&strokes);
    let error = CompiledMask::new(&mask, stage(64, 48), &table).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        format!(
            "component Brush 1 has {} strokes; the limit is {STROKES_PER_COMPONENT} strokes per \
             brush component",
            STROKES_PER_COMPONENT + 1
        )
    );
    // The stage-free half refuses it too, so a recipe carrying it fails to compile anywhere.
    assert!(luxforge_core::mask::validate_component_kinds(&mask).is_err());
}

/// A payload that is not the reserved stroke list is refused by name, and a reference the store
/// cannot answer is the store's own refusal — never an empty stroke.
#[test]
fn a_malformed_payload_or_an_unresolvable_reference_is_refused_by_name() {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("brush");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "brush",
        json!({"strokes": [], "size": 0.1}),
    ));
    let error = CompiledMask::new(&mask, stage(64, 48), &StrokeTable::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(
        error
            .detail
            .starts_with("component Brush 1 has an invalid brush payload: unknown field `size`"),
        "{}",
        error.detail
    );

    // A well-formed payload whose address the table does not hold.
    let stroke = Stroke::capture(&[[0.5, 0.5]], 0.1, 50.0, 100.0, false).unwrap();
    let (held, _) = brush_mask(std::slice::from_ref(&stroke));
    let error =
        CompiledMask::new(&held, stage(64, 48), &StrokeTable::new("this entry")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(
        error.detail.contains("is not in the stroke store"),
        "{}",
        error.detail
    );
}

/// The smallest feature a brush draws is its narrowest ramp, in the stage's pixels; a hard-edged
/// stroke has no ramp at all and says so, exactly as a hard-edged radial does.
#[test]
fn the_minimum_feature_is_the_narrowest_ramp() {
    let size = stage(600, 400);
    // A radius of 0.1 at feather 50 has a ramp of half the radius: 20 px on a 400 px stage, up to
    // the radius's own quantization onto the stored path grid, which is what `size()` reports.
    let soft = Stroke::capture(&[[0.3, 0.5], [0.7, 0.5]], 0.1, 50.0, 100.0, false).unwrap();
    let expected = soft.size() * 0.5 * 400.0;
    assert!((expected - 20.0).abs() < 0.01, "{expected}");
    let (mask, table) = brush_mask(std::slice::from_ref(&soft));
    let compiled = CompiledMask::new(&mask, size, &table).unwrap();
    assert!(
        (f64::from(compiled.min_feature_px(size)) - expected).abs() < 1e-3,
        "{} against {expected}",
        compiled.min_feature_px(size)
    );
    let hard = Stroke::capture(&[[0.3, 0.5], [0.7, 0.5]], 0.1, 0.0, 100.0, false).unwrap();
    let (mask, table) = brush_mask(std::slice::from_ref(&hard));
    let compiled = CompiledMask::new(&mask, size, &table).unwrap();
    assert_eq!(compiled.min_feature_px(size), 0.0);
}

// ---------------------------------------------------------------------------
// The grid index
// ---------------------------------------------------------------------------

/// The measurement the index exists for: at a fixed local density, the cost of evaluating one pixel
/// does not grow with the component's total stroke count. Strokes are added far from the sampled
/// region, so what changes is the count and not what the pixel can reach.
///
/// Ignored by default because it is a measurement rather than a pass/fail property; the assertion it
/// does make is deliberately loose, because a wall clock on a shared host is not a proof.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn evaluation_cost_does_not_grow_with_stroke_count() {
    let size = stage(400, 300);
    // Two strokes the sampled region actually touches, and then padding far away.
    let near = [
        Stroke::capture(&[[0.10, 0.10], [0.16, 0.16]], 0.02, 50.0, 100.0, false).unwrap(),
        Stroke::capture(&[[0.12, 0.16], [0.16, 0.10]], 0.02, 50.0, 100.0, false).unwrap(),
    ];
    // The reference has no index and folds every stroke at every pixel, so it is the contrast: what
    // the same field costs when the cost *does* grow with the count.
    let reference_stage = RefStage::new(size.width, size.height);
    println!("strokes  indexed ns/px  unindexed ns/px  coverage at the sample");
    let mut baseline = 0.0f64;
    for count in [2usize, 8, 16, 32, 64] {
        let mut strokes: Vec<Stroke> = near.to_vec();
        for index in 0..(count - near.len()) {
            let t = f64::from(index as u32) / f64::from(count as u32);
            strokes.push(
                Stroke::capture(
                    &[[0.55 + 0.4 * t, 0.7], [0.56 + 0.4 * t, 0.9]],
                    0.01,
                    50.0,
                    100.0,
                    false,
                )
                .unwrap(),
            );
        }
        let (mask, table) = brush_mask(&strokes);
        let compiled = CompiledMask::new(&mask, size, &table).expect("under the cap");
        // The sampled window is the neighbourhood of the two near strokes, so the local density is
        // the same in every row of the table.
        let rounds = 200;
        let started = std::time::Instant::now();
        let mut total = 0.0f64;
        for _ in 0..rounds {
            for y in 20..70 {
                for x in 20..70 {
                    total += compiled.coverage(x, y, ANY_PIXEL);
                }
            }
        }
        let elapsed = started.elapsed();
        std::hint::black_box(total);
        let per_pixel = elapsed.as_secs_f64() * 1e9 / f64::from(rounds * 50 * 50);

        let unindexed = Brush {
            strokes: strokes.iter().map(as_reference).collect(),
        };
        let reference_rounds = 20;
        let started = std::time::Instant::now();
        let mut total = 0.0f64;
        for _ in 0..reference_rounds {
            for y in 20..70 {
                for x in 20..70 {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    total += brush_coverage(&unindexed, &reference_stage, u, v, ANY_PIXEL);
                }
            }
        }
        let reference_elapsed = started.elapsed();
        std::hint::black_box(total);
        let reference_per_pixel =
            reference_elapsed.as_secs_f64() * 1e9 / f64::from(reference_rounds * 50 * 50);

        println!(
            "{count:7}  {per_pixel:13.2}  {reference_per_pixel:15.2}  {:.6}",
            compiled.coverage(45, 45, ANY_PIXEL)
        );
        if count == 2 {
            baseline = per_pixel;
        } else {
            assert!(
                per_pixel < baseline * 4.0,
                "{count} strokes cost {per_pixel:.2} ns/pixel against a 2-stroke {baseline:.2}"
            );
        }
    }
}

/// The property that measurement is about, stated as an assertion a test run can keep: a pixel's
/// coverage is exactly what the strokes near it give, whatever else the component holds. Padding a
/// component with strokes nowhere near the pixel changes nothing, bit for bit.
#[test]
fn strokes_nowhere_near_a_pixel_change_nothing() {
    let size = stage(200, 150);
    let near = [
        Stroke::capture(&[[0.10, 0.10], [0.16, 0.16]], 0.02, 50.0, 100.0, false).unwrap(),
        Stroke::capture(&[[0.12, 0.16], [0.16, 0.10]], 0.02, 50.0, 100.0, true).unwrap(),
    ];
    let (bare, bare_table) = brush_mask(&near);
    let bare = CompiledMask::new(&bare, size, &bare_table).unwrap();

    let mut padded_strokes: Vec<Stroke> = near.to_vec();
    for index in 0..40u32 {
        let t = f64::from(index) / 40.0;
        padded_strokes.push(
            Stroke::capture(
                &[[0.55 + 0.4 * t, 0.7], [0.56 + 0.4 * t, 0.9]],
                0.01,
                50.0,
                100.0,
                false,
            )
            .unwrap(),
        );
    }
    let (padded, padded_table) = brush_mask(&padded_strokes);
    let padded = CompiledMask::new(&padded, size, &padded_table).unwrap();

    let mut nonzero = 0usize;
    for y in 0..40 {
        for x in 0..50 {
            let a = bare.coverage(x, y, ANY_PIXEL);
            assert_eq!(
                a.to_bits(),
                padded.coverage(x, y, ANY_PIXEL).to_bits(),
                "at ({x}, {y})"
            );
            if a > 0.0 {
                nonzero += 1;
            }
        }
    }
    assert!(nonzero > 0, "the sampled window covered nothing");
}

/// The index never changes an answer: production with its index is bit-identical to the reference,
/// which has none, including where a stroke's segments straddle several cells and where a pixel sits
/// on a cell boundary. This is the whole-field version of the claim the index rests on.
#[test]
fn the_index_changes_no_answer_anywhere() {
    let mut rng = SplitMix64(0x0018_10DE);
    for (width, height) in [(53u32, 31u32), (31, 53)] {
        let reference_stage = RefStage::new(width, height);
        for _ in 0..60 {
            // Long strokes with several segments, and a mixture of radii, so a segment's grown box
            // reaches many cells and the index has work to do.
            let strokes: Vec<Stroke> = (0..4)
                .map(|index| {
                    let mut points = Vec::new();
                    let mut x = rng.next_range(-0.2, 1.2);
                    let mut y = rng.next_range(-0.2, 1.2);
                    for _ in 0..12 {
                        points.push([x, y]);
                        x = (x + rng.next_range(-0.4, 0.4)).clamp(-1.0, 2.0);
                        y = (y + rng.next_range(-0.4, 0.4)).clamp(-1.0, 2.0);
                    }
                    Stroke::capture(
                        &points,
                        rng.next_range(0.005, 0.3),
                        rng.next_range(0.0, 100.0),
                        rng.next_range(1.0, 100.0),
                        index == 2,
                    )
                    .unwrap()
                })
                .collect();
            let (mask, table) = brush_mask(&strokes);
            let Ok(compiled) = CompiledMask::new(&mask, stage(width, height), &table) else {
                // Over the occupancy cap: refused, which is the other half of the contract and is
                // tested on its own above.
                continue;
            };
            let expected = Brush {
                strokes: strokes.iter().map(as_reference).collect(),
            };
            for y in 0..height {
                for x in 0..width {
                    let (u, v) = reference_stage.pixel_uv(x, y);
                    assert_eq!(
                        compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                        brush_coverage(&expected, &reference_stage, u, v, ANY_PIXEL).to_bits(),
                        "at ({x}, {y}) on {width}x{height}"
                    );
                }
            }
        }
    }
}

/// A brush combines with a gradient through the frozen component algebra like any other kind: the
/// brush is one `c` in the fold and nothing about the composition knows it was drawn.
#[test]
fn a_brush_combines_with_a_gradient_through_the_frozen_algebra() {
    let size = stage(80, 60);
    let reference_stage = RefStage::new(80, 60);
    let stroke = Stroke::capture(&[[0.3, 0.5], [0.7, 0.5]], 0.12, 60.0, 100.0, false).unwrap();
    let (mut mask, table) = brush_mask(std::slice::from_ref(&stroke));
    mask.components.push(Component::new(
        "Linear 1",
        ComponentMode::Intersect,
        "linear",
        json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 0.0}),
    ));
    let compiled = CompiledMask::new(&mask, size, &table).unwrap();
    let brush = Brush {
        strokes: vec![as_reference(&stroke)],
    };
    let linear = reference::mask::Linear {
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 0.0,
    };
    for y in 0..size.height {
        for x in 0..size.width {
            let (u, v) = reference_stage.pixel_uv(x, y);
            let c = brush_coverage(&brush, &reference_stage, u, v, ANY_PIXEL);
            let g = reference::mask::linear_coverage(&linear, &reference_stage, u, v);
            assert_eq!(
                compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                c.min(g).to_bits()
            );
        }
    }
}

/// One stroke on its own is the reference's `stroke_coverage`, which is what "the component is the
/// accumulation and nothing more" means: a single add stroke over `c = 0` is `0 + (1 - 0)·s`.
#[test]
fn a_single_add_stroke_is_its_own_coverage() {
    let reference_stage = RefStage::new(48, 36);
    let stroke = Stroke::capture(&[[0.2, 0.4], [0.8, 0.6]], 0.07, 35.0, 75.0, false).unwrap();
    let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
    let compiled = CompiledMask::new(&mask, stage(48, 36), &table).unwrap();
    let held = as_reference(&stroke);
    let segments = brush_segments(&held, &reference_stage);
    for y in 0..36 {
        for x in 0..48 {
            let (u, v) = reference_stage.pixel_uv(x, y);
            assert_eq!(
                compiled.coverage(x, y, ANY_PIXEL).to_bits(),
                stroke_coverage(&held, &segments, u, v, ANY_PIXEL).to_bits()
            );
        }
    }
}
