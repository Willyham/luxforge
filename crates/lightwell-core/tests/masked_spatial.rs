//! TASK-014: the masked **spatial** primitive through the public pipeline — a Presence layer bound
//! to a mask, on the JPEG byte path and on the RAW linear path.
//!
//! What this file proves, and what it deliberately does not. The Presence filter itself is frozen
//! elsewhere: `presence_reference.rs`, `presence_module.rs` and `tests/reference/presence.rs` are
//! the evidence that the units are right, and they pass unmodified because a mask changes nothing
//! about a unit, a halo, a tile or a global estimate. The blend algebra is frozen by
//! `masked_colour.rs` against the independent `f64` oracle. What is new here is the *write*, so
//! this file asserts the write:
//!
//! - **The two endpoints carry no tolerance at all.** `M = 0` renders the operation's input frame
//!   byte for byte and `M = 1` renders the unmasked Presence frame byte for byte, on both paths.
//! - **A tile the mask cannot reach is a copy**, counted rather than asserted, through the host's
//!   own `masked_tile_counts`.
//! - **A sample equals the rendered byte** inside the mask, outside it and on the bounds edge, on
//!   both paths, which is the delivered one-tile exception and not a second one.
//! - A partially covered frame lies strictly between the two endpoint frames, which is what a
//!   blend is; where it lands between them is the oracle's business and `masked_colour.rs` asserts
//!   it there.

mod reference;

use lightwell_core::{
    Component, ComponentMode, EFFECT_FORMAT, Layer, LayerId, LinearImage, LinearSettings, Mask,
    ModuleRegistry, PRESENCE_EFFECT, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage,
    masked_tile_counts, render, render_linear, reset_masked_tile_counts, sample, sample_linear,
};
use reference::mask::{
    Algebra, Component as RefComponent, Kind, Linear, Mask as RefMask, Mode, Stage as RefStage,
    coverage,
};
use reference::srgb_to_linear;
use serde_json::json;

/// The spatial budget, the estimate store and the masked-tile counters are all process-wide, so the
/// tests in this binary run one at a time rather than racing each other for them.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// A small stage: one production tile, so the endpoint and sample tests pay for one tile's chain.
const SMALL: (u32, u32) = (160, 120);

/// A stage of two tile columns and two tile rows (512 + 88 by 512 + 38), which is the smallest
/// frame that can show a tile being copied while another is evaluated.
const TILED: (u32, u32) = (600, 550);

/// A source whose bytes vary on both axes and in all three channels, so a wrong row, a dropped
/// channel or a mask read at the wrong coordinate is visible in the comparison. It also varies
/// fast enough that a neighbourhood filter actually changes it.
fn byte_source((width, height): (u32, u32)) -> SourceImage {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                (x * 9 + 3 + (y % 5) * 17) as u8,
                (y * 13 + 40 + (x % 7) * 11) as u8,
                (x * 5 + y * 7 + 90) as u8,
                255,
            ]);
        }
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: format!("sha256:masked-spatial-{width}x{height}"),
        orientation: 1,
    }
}

/// The same picture as a planar scene-linear source, decoded from the byte fixture's own codes.
///
/// The planes are `f32`, which is load-bearing for the `M = 0` comparison on this path: the RAW
/// path pulls `f64` pixels and a spatial operation materializes an `f32` frame, so a value that
/// began as an `f32` survives that round trip exactly and the operation's input frame is the frame
/// the identity stack produces, bit for bit.
fn linear_source(size: (u32, u32)) -> LinearImage {
    let source = byte_source(size);
    let mut planes = Vec::with_capacity((size.0 * size.1 * 3) as usize);
    for channel in 0..3 {
        for pixel in source.rgba.chunks_exact(4) {
            planes.push(srgb_to_linear(pixel[channel]) as f32);
        }
    }
    LinearImage::with_fingerprint(
        size.0,
        size.1,
        planes,
        format!("sha256:masked-spatial-linear-{}x{}", size.0, size.1),
    )
    .unwrap()
}

/// One Presence layer with a clarity amount large enough that every pixel of the fixture moves.
fn presence_layer() -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: PRESENCE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"clarity": 80.0, "texture": 60.0}),
        mask: None,
    }
}

fn masked(layer: Layer, mask: &Mask) -> Layer {
    Layer {
        mask: Some(mask.id.clone()),
        ..layer
    }
}

fn recipe(layers: Vec<Layer>, masks: Vec<Mask>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks,
        ..Recipe::default()
    }
}

/// One mask of one `add` linear gradient, and the reference's own description of the same thing,
/// built side by side from the same four numbers.
fn gradient_mask(x0: f64, y0: f64, x1: f64, y1: f64, amount: f64) -> (Mask, RefMask) {
    let mut mask = Mask::new("Mask 1");
    mask.amount = amount;
    let name = mask.next_component_name("linear");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "linear",
        json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1}),
    ));
    let oracle = RefMask {
        amount,
        invert: false,
        components: vec![RefComponent {
            mode: Mode::Add,
            invert: false,
            kind: Kind::Linear(Linear { x0, y0, x1, y1 }),
        }],
    };
    (mask, oracle)
}

/// The reference's coverage at one content-stage pixel centre, through the frozen algebra.
fn reference_coverage(oracle: &RefMask, size: (u32, u32), x: u32, y: u32) -> f64 {
    let stage = RefStage::new(size.0, size.1);
    let (u, v) = stage.pixel_uv(x, y);
    coverage(oracle, Algebra::Zadeh, &stage, u, v)
}

// ---------------------------------------------------------------------------------------------
// The two endpoints, byte for byte, on both paths
// ---------------------------------------------------------------------------------------------

/// `M = 0` everywhere renders the operation's **input** frame and `M = 1` everywhere renders the
/// unmasked Presence frame — byte for byte, on the JPEG byte path and on the RAW linear path.
///
/// Three spellings of the endpoints are checked because they take different routes through the
/// primitive, exactly as they do in the colour case: an `amount` of zero empties the bounds
/// rectangle so every tile is copied and no unit is evaluated at all, a gradient whose frame lies
/// behind `p0` is the same, and one whose frame lies entirely beyond `p1` runs the whole chain on
/// every tile and blends it with `M = 1`.
#[test]
fn the_mask_endpoints_are_byte_identical_to_the_unmasked_frames_on_both_paths() {
    let _guard = serial();
    let registry = ModuleRegistry::builtin();
    let source = byte_source(SMALL);
    let linear = linear_source(SMALL);
    let identity = recipe(Vec::new(), Vec::new());
    let unmasked = recipe(vec![presence_layer()], Vec::new());

    let (silent, _) = gradient_mask(0.5, 0.0, 0.5, 1.0, 0.0);
    let (behind, oracle_behind) = gradient_mask(0.5, 1.5, 0.5, 2.0, 100.0);
    let (ahead, oracle_ahead) = gradient_mask(0.5, -1.0, 0.5, -0.5, 100.0);
    // The oracle agrees that these are the endpoints, so the byte comparison below is a claim about
    // the blend and not about the geometry.
    for (x, y) in [
        (0, 0),
        (SMALL.0 / 2, SMALL.1 / 2),
        (SMALL.0 - 1, SMALL.1 - 1),
    ] {
        assert_eq!(reference_coverage(&oracle_behind, SMALL, x, y), 0.0);
        assert_eq!(reference_coverage(&oracle_ahead, SMALL, x, y), 1.0);
    }

    for (case, mask, expected) in [
        ("amount zero", silent, &identity),
        ("behind p0", behind, &identity),
        ("beyond p1", ahead, &unmasked),
    ] {
        let stack = recipe(vec![masked(presence_layer(), &mask)], vec![mask.clone()]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        let reference_frame = render(&registry, &source, SnapshotId::new(), expected).unwrap();
        assert_eq!(
            rendered.rgba.as_ref(),
            reference_frame.rgba.as_ref(),
            "{case}: the JPEG path is not byte-identical"
        );
        let rendered_linear = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap();
        let reference_linear = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            expected,
            LinearSettings::default(),
        )
        .unwrap();
        assert_eq!(
            rendered_linear.rgba.as_ref(),
            reference_linear.rgba.as_ref(),
            "{case}: the linear path is not byte-identical"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// A tile the mask cannot reach costs no unit evaluation
// ---------------------------------------------------------------------------------------------

/// The claim that makes a small masked Presence layer affordable on a 60 MP frame is that a tile
/// entirely outside the mask's bounds rectangle is **copied**, not evaluated and blended away. It
/// is a counter and not an assertion of intent: the host counts the tiles it copied and the tiles
/// it ran the chain over, and this reads them.
#[test]
fn a_tile_the_mask_cannot_reach_evaluates_no_unit() {
    let _guard = serial();
    let registry = ModuleRegistry::builtin();
    let source = byte_source(TILED);
    // A gradient confined to the right edge: its support cannot reach the two tiles whose columns
    // start at zero, so those two are copies and the two starting at column 512 run the chain.
    let (mask, _) = gradient_mask(0.90, 0.5, 0.97, 0.5, 100.0);
    let stack = recipe(vec![masked(presence_layer(), &mask)], vec![mask.clone()]);

    reset_masked_tile_counts();
    let masked_frame = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    let (copied, evaluated) = masked_tile_counts();
    assert_eq!(
        copied + evaluated,
        4,
        "a {}x{} stage is four production tiles",
        TILED.0,
        TILED.1
    );
    assert_eq!(copied, 2, "the two left-hand tiles cost no unit evaluation");
    assert_eq!(evaluated, 2, "the two right-hand tiles ran the chain");

    // An unmasked operation touches neither counter, so the counters measure masking and nothing
    // else.
    reset_masked_tile_counts();
    let unmasked_frame = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![presence_layer()], Vec::new()),
    )
    .unwrap();
    assert_eq!(masked_tile_counts(), (0, 0));

    // The copied tiles hold the operation's input, and the evaluated ones do not: the counter is
    // reporting what the picture shows.
    let source_frame = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(Vec::new(), Vec::new()),
    )
    .unwrap();
    let at = |frame: &lightwell_core::Raster, x: u32, y: u32| {
        let offset = ((y * TILED.0 + x) * 4) as usize;
        [
            frame.rgba[offset],
            frame.rgba[offset + 1],
            frame.rgba[offset + 2],
        ]
    };
    for (x, y) in [(10, 10), (400, 400), (10, 530)] {
        assert_eq!(
            at(&masked_frame, x, y),
            at(&source_frame, x, y),
            "({x}, {y}) is in a copied tile"
        );
    }
    let x = TILED.0 - 1;
    assert_ne!(
        at(&masked_frame, x, 10),
        at(&source_frame, x, 10),
        "the right edge is inside the mask"
    );
    assert_eq!(
        at(&masked_frame, x, 10),
        at(&unmasked_frame, x, 10),
        "coverage is one at the right edge, so the unmasked effect is what is drawn"
    );
}

// ---------------------------------------------------------------------------------------------
// A sample equals the rendered byte
// ---------------------------------------------------------------------------------------------

/// A point sample through a masked spatial layer equals the rendered byte, inside the mask, outside
/// it and on the bounds edge, on both paths. It holds by construction rather than by agreement: the
/// blend lives in the one tile function the render, the RAW float frame and the sample all call, so
/// there is no second implementation to disagree. The delivered declared exception — a sample
/// evaluates the one stage-aligned tile that contains its pixel — is unchanged, and a tile the mask
/// cannot reach costs the sample less than that, never more.
#[test]
fn a_sample_equals_the_rendered_byte_through_a_masked_spatial_layer() {
    let _guard = serial();
    let registry = ModuleRegistry::builtin();
    let source = byte_source(SMALL);
    let linear = linear_source(SMALL);
    // A gradient across the middle of the frame, so the columns to the left are outside the bounds
    // rectangle, the columns to the right are fully covered and the band between them is partial.
    let (mask, oracle) = gradient_mask(0.40, 0.5, 0.60, 0.5, 100.0);
    let stack = recipe(vec![masked(presence_layer(), &mask)], vec![mask.clone()]);

    let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    let rendered_linear = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &stack,
        LinearSettings::default(),
    )
    .unwrap();

    // Every column of one row, which walks the zero region, the bounds edge and the covered region
    // in one pass, plus three rows of interest at the boundary columns.
    let mut zero = 0;
    let mut partial = 0;
    let mut full = 0;
    let mut points: Vec<(u32, u32)> = (0..SMALL.0).map(|x| (x, SMALL.1 / 2)).collect();
    for x in [0, SMALL.0 * 2 / 5, SMALL.0 / 2, SMALL.0 - 1] {
        points.push((x, 0));
        points.push((x, SMALL.1 - 1));
    }
    for (x, y) in points {
        let m = reference_coverage(&oracle, SMALL, x, y);
        if m == 0.0 {
            zero += 1;
        } else if m < 1.0 {
            partial += 1;
        } else {
            full += 1;
        }
        let offset = ((y * SMALL.0 + x) * 4) as usize;
        let expected: [u8; 4] = rendered.rgba[offset..offset + 4].try_into().unwrap();
        let sampled = sample(&registry, &source, &stack, x, y).unwrap();
        assert_eq!(
            sampled.rgba,
            Some(expected),
            "the byte path disagrees at ({x}, {y}) with coverage {m}"
        );
        let expected_linear: [u8; 4] = rendered_linear.rgba[offset..offset + 4].try_into().unwrap();
        let sampled_linear =
            sample_linear(&registry, &linear, &stack, LinearSettings::default(), x, y).unwrap();
        assert_eq!(
            sampled_linear.rgba,
            Some(expected_linear),
            "the linear path disagrees at ({x}, {y}) with coverage {m}"
        );
    }
    assert!(
        zero > 0,
        "the walk crossed the region the mask cannot reach"
    );
    assert!(partial > 0, "the walk crossed the feathered band");
    assert!(full > 0, "the walk crossed the fully covered region");
}

// ---------------------------------------------------------------------------------------------
// A partially covered frame is a blend
// ---------------------------------------------------------------------------------------------

/// Where coverage is partial the frame lies between the operation's input and the unmasked effect,
/// per channel, and somewhere it lies strictly between them. Which value it takes between them is
/// the blend algebra's, and `masked_colour.rs` freezes that against the independent `f64` oracle;
/// what this asserts is that the spatial write reaches the same interpolation rather than either
/// endpoint or something outside the pair.
#[test]
fn a_partially_covered_frame_lies_between_the_two_endpoint_frames() {
    let _guard = serial();
    let registry = ModuleRegistry::builtin();
    let source = byte_source(SMALL);
    let (mask, oracle) = gradient_mask(0.20, 0.10, 0.80, 0.90, 100.0);
    let stack = recipe(vec![masked(presence_layer(), &mask)], vec![mask.clone()]);

    let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    let input = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(Vec::new(), Vec::new()),
    )
    .unwrap();
    let effect = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![presence_layer()], Vec::new()),
    )
    .unwrap();

    let mut strictly_between = 0;
    let mut partial_pixels = 0;
    for y in 0..SMALL.1 {
        for x in 0..SMALL.0 {
            let m = reference_coverage(&oracle, SMALL, x, y);
            let offset = ((y * SMALL.0 + x) * 4) as usize;
            for channel in 0..3 {
                let got = i32::from(rendered.rgba[offset + channel]);
                let low = i32::from(input.rgba[offset + channel]);
                let high = i32::from(effect.rgba[offset + channel]);
                let (lower, upper) = (low.min(high), low.max(high));
                // One code of slack, because production blends in `f32` and quantizes once: a
                // value that sits on a code boundary may land on either side of it.
                assert!(
                    got >= lower - 1 && got <= upper + 1,
                    "({x}, {y}) channel {channel} is {got}, outside [{lower}, {upper}] at coverage {m}"
                );
                if m > 0.0 && m < 1.0 && lower + 1 < upper {
                    partial_pixels += 1;
                    if got > lower && got < upper {
                        strictly_between += 1;
                    }
                }
            }
        }
    }
    assert!(
        partial_pixels > 0,
        "the gradient feathers across this frame"
    );
    assert!(
        strictly_between * 4 > partial_pixels,
        "only {strictly_between} of {partial_pixels} partially covered channels landed strictly \
         between the endpoints, which is not a blend"
    );
}
