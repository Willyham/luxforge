//! The geometry tail and the output encoding on a headless device, against a reference of the same
//! arithmetic on the CPU: an affine tail's taps, clamps and blend, a windowed boundary, a grid
//! that holds an affine map, a JPEG's quantizing segments, steps at the output pixel, and the
//! output codes the CPU's quantizer gives every value.
use super::qualification::{Qualifier, boundary, held};
use super::*;

fn encoding() -> &'static OutputEncoding {
    output_encoding().expect("the test reference's tables")
}

/// A value's output code by the CPU's rule: clamped, NaN to zero, the thresholds at or below it.
fn code(value: f32) -> u8 {
    let value = if value >= 0.0 { value.min(1.0) } else { 0.0 };
    encoding()
        .thresholds
        .partition_point(|threshold| *threshold <= value) as u8
}

/// A value quantized to its code and decoded again, as a JPEG's segment boundary does.
fn requantized(value: f32) -> f32 {
    encoding().decoded[usize::from(code(value))]
}

/// A deterministic boundary of `width` × `height` linear values, some past white and below black.
fn texels(width: u32, height: u32) -> Vec<[f32; 3]> {
    (0..width * height)
        .map(|index| {
            let (x, y) = ((index % width) as f32, (index / width) as f32);
            [
                held(((x * 0.37 + y * 0.11).sin() * 0.6 + 0.45).max(-0.1)),
                held((x * 0.07 + y * 0.31).cos() * 0.55 + 0.5),
                held(((x + 2.0 * y) % 17.0) / 15.0),
            ]
        })
        .collect()
}

/// The reference tail: each output pixel's centre through `map`, four taps clamped to `reads`
/// and offset by the window's `origin`, blended in `f32` in the order the shader blends them.
fn reference(
    values: &[[f32; 3]],
    (width, height): (u32, u32),
    origin: (u32, u32),
    reads: [u32; 4],
    output: (u32, u32),
    map: impl Fn(f32, f32) -> (f32, f32),
    quantize: bool,
) -> Vec<[u8; 3]> {
    let tap = |x: i32, y: i32| -> [f32; 3] {
        let x = x.clamp(reads[0] as i32, reads[2] as i32 - 1) - origin.0 as i32;
        let y = y.clamp(reads[1] as i32, reads[3] as i32 - 1) - origin.1 as i32;
        let (x, y) = (
            x.clamp(0, width as i32 - 1) as u32,
            y.clamp(0, height as i32 - 1) as u32,
        );
        let value = values[(y * width + x) as usize];
        if quantize {
            value.map(requantized)
        } else {
            value
        }
    };
    let mut codes = Vec::new();
    for y in 0..output.1 {
        for x in 0..output.0 {
            let (u, v) = map(x as f32 + 0.5, y as f32 + 0.5);
            let (px, py) = (u - 0.5, v - 0.5);
            let (cx, cy) = (px.floor(), py.floor());
            let (wx, wy) = (px - cx, py - cy);
            let (x0, y0) = (cx as i32, cy as i32);
            let corners = [
                (tap(x0, y0), (1.0 - wx) * (1.0 - wy)),
                (tap(x0 + 1, y0), wx * (1.0 - wy)),
                (tap(x0, y0 + 1), (1.0 - wx) * wy),
                (tap(x0 + 1, y0 + 1), wx * wy),
            ];
            let rgb: [f32; 3] = std::array::from_fn(|channel| {
                corners
                    .iter()
                    .map(|(texel, weight)| texel[channel] * weight)
                    .sum()
            });
            let rgb = if quantize { rgb.map(requantized) } else { rgb };
            codes.push(rgb.map(code));
        }
    }
    codes
}

fn plan(values: &[[f32; 3]], size: (u32, u32), origin: (u32, u32), steps: Vec<GpuStep>) -> GpuPlan {
    GpuPlan {
        boundary: boundary(size.0, size.1, 1, values).expect("a boundary"),
        texels: TexelMap {
            origin: [origin.0 as f32, origin.1 as f32],
            step: [1.0, 1.0],
        },
        steps,
        region: None,
    }
}

/// The largest difference between the drawn codes and the reference's, and how many differ.
fn compare(drawn: &[[u8; 4]], expected: &[[u8; 3]]) -> (u8, usize) {
    assert_eq!(drawn.len(), expected.len());
    let mut worst = 0;
    let mut differing = 0;
    for (drawn, expected) in drawn.iter().zip(expected) {
        let difference = (0..3)
            .map(|channel| drawn[channel].abs_diff(expected[channel]))
            .max()
            .unwrap();
        worst = worst.max(difference);
        differing += usize::from(difference > 0);
    }
    (worst, differing)
}

/// The output codes are the CPU quantizer's for every value: the thresholds' own neighbourhoods,
/// past both ends and NaN, through the identity program over a one-row boundary.
#[test]
fn the_output_codes_are_the_cpu_quantizers() {
    let Some(qualifier) = Qualifier::headless("the_output_codes_are_the_cpu_quantizers") else {
        return;
    };
    // Each threshold, and the half floats just below and above it, which the boundary can hold.
    let mut values: Vec<f32> = vec![-1.0, -0.0, 0.0, 1.0, 2.0, f32::NAN];
    for threshold in encoding().thresholds.iter() {
        let held = half::f16::from_f32(*threshold);
        values.extend([
            held.to_f32(),
            half::f16::from_bits(held.to_bits().wrapping_sub(1)).to_f32(),
            half::f16::from_bits(held.to_bits() + 1).to_f32(),
        ]);
    }
    let width = values.len() as u32;
    let pixels: Vec<[f32; 3]> = values.iter().map(|value| [*value; 3]).collect();
    let identity = GpuProgram::new(
        "identity",
        "fn identity(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return rgb;\n}\n",
    );
    let drawn = qualifier
        .evaluate_codes(&plan(
            &pixels,
            (width, 1),
            (0, 0),
            vec![GpuStep::colour(identity)],
        ))
        .unwrap();
    for (value, drawn) in pixels.iter().zip(&drawn) {
        let expected = code(half::f16::from_f32(value[0]).to_f32());
        assert_eq!(drawn[0], expected, "{}", value[0]);
    }
}

/// What one affine case names: its label, the held window's origin, the reads, the output size,
/// the matrix, and the tolerance in codes.
type AffineCase = (&'static str, (u32, u32), [u32; 4], (u32, u32), [f32; 6], u8);

/// An affine tail draws its output stage from the boundary as the reference does: a translation
/// copies texels exactly; a turn, a scale and a straightening blend within one code; and a
/// boundary that holds a window of its stage is read at the window's origin.
#[test]
fn an_affine_tail_draws_the_reference_blend() {
    let Some(qualifier) = Qualifier::headless("an_affine_tail_draws_the_reference_blend") else {
        return;
    };
    let (width, height) = (64, 48);
    let values = texels(width, height);
    let whole = [0, 0, width, height];
    let angle = 4f32.to_radians();
    let (sin, cos) = angle.sin_cos();
    let cases: [AffineCase; 4] = [
        (
            "a crop's translation",
            (0, 0),
            [10, 12, 42, 40],
            (32, 28),
            [1.0, 0.0, 10.0, 0.0, 1.0, 12.0],
            0,
        ),
        (
            "a downscale",
            (0, 0),
            whole,
            (40, 30),
            [1.6, 0.0, 0.0, 0.0, 1.6, 0.0],
            1,
        ),
        (
            "a straightening",
            (0, 0),
            whole,
            (56, 40),
            [cos, -sin, 6.0, sin, cos, 2.0],
            1,
        ),
        (
            "a straightening over a window of the stage",
            (3, 2),
            whole,
            (50, 36),
            [cos, -sin, 8.0, sin, cos, 5.0],
            1,
        ),
    ];
    for (what, origin, reads, output, matrix, tolerance) in cases {
        let held_size = (width - origin.0, height - origin.1);
        let window: Vec<[f32; 3]> = (0..held_size.1)
            .flat_map(|y| {
                let values = &values;
                (0..held_size.0)
                    .map(move |x| values[((y + origin.1) * width + x + origin.0) as usize])
            })
            .collect();
        let tail = GpuTail::affine(output, reads, false, matrix);
        let drawn = qualifier
            .evaluate_codes(&plan(
                &window,
                held_size,
                origin,
                vec![GpuStep::Geometry(tail)],
            ))
            .unwrap();
        let expected = reference(
            &window,
            held_size,
            origin,
            reads,
            output,
            |x, y| {
                (
                    matrix[0] * x + matrix[1] * y + matrix[2],
                    matrix[3] * x + matrix[4] * y + matrix[5],
                )
            },
            false,
        );
        let (worst, differing) = compare(&drawn, &expected);
        eprintln!(
            "{what}: worst {worst}, {differing} of {} differ",
            expected.len()
        );
        assert!(
            worst <= tolerance,
            "{what}: {worst} codes from the reference"
        );
    }
}

/// A coordinate grid that holds an affine map draws what the affine tail draws, within a code.
#[test]
fn a_grid_tail_interpolates_its_nodes() {
    let Some(qualifier) = Qualifier::headless("a_grid_tail_interpolates_its_nodes") else {
        return;
    };
    let (width, height) = (64, 48);
    let values = texels(width, height);
    let matrix = [0.9f32, 0.05, 3.0, -0.04, 0.95, 4.0];
    let output: (u32, u32) = (60, 40);
    let spacing: u32 = 8;
    let (columns, rows) = (
        output.0.div_ceil(spacing) + 1,
        output.1.div_ceil(spacing) + 1,
    );
    let mut nodes = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            let (x, y) = ((column * spacing) as f32, (row * spacing) as f32);
            nodes.push((matrix[0] * x + matrix[1] * y + matrix[2]).to_bits());
            nodes.push((matrix[3] * x + matrix[4] * y + matrix[5]).to_bits());
        }
    }
    let tail = GpuTail::grid(
        output,
        [0, 0, width, height],
        false,
        (0, 0),
        spacing,
        (columns, rows),
        nodes.into(),
    );
    let drawn = qualifier
        .evaluate_codes(&plan(
            &values,
            (width, height),
            (0, 0),
            vec![GpuStep::Geometry(tail)],
        ))
        .unwrap();
    let expected = reference(
        &values,
        (width, height),
        (0, 0),
        [0, 0, width, height],
        output,
        |x, y| {
            (
                matrix[0] * x + matrix[1] * y + matrix[2],
                matrix[3] * x + matrix[4] * y + matrix[5],
            )
        },
        false,
    );
    let (worst, _) = compare(&drawn, &expected);
    assert!(worst <= 1, "{worst} codes from the affine reference");
}

/// A projective tail evaluates its homography at every pixel and draws the reference's blend through
/// it, within a code, on its own and over a window of the stage; with no perspective in it, it draws
/// what the affine tail draws.
#[test]
fn a_projective_tail_draws_the_reference_blend() {
    let Some(qualifier) = Qualifier::headless("a_projective_tail_draws_the_reference_blend") else {
        return;
    };
    let (width, height) = (64, 48);
    let values = texels(width, height);
    let output: (u32, u32) = (60, 40);
    let tilted = [0.95f32, 0.04, 2.5, -0.03, 1.02, 3.0, 0.0021, -0.0014, 1.0];
    let affine = [0.9f32, 0.05, 3.0, -0.04, 0.95, 4.0];
    let flat = [
        affine[0], affine[1], affine[2], affine[3], affine[4], affine[5], 0.0, 0.0, 1.0,
    ];
    let homography = |m: [f32; 9]| {
        move |x: f32, y: f32| {
            let w = m[6] * x + m[7] * y + m[8];
            (
                (m[0] * x + m[1] * y + m[2]) / w,
                (m[3] * x + m[4] * y + m[5]) / w,
            )
        }
    };
    for (what, origin, matrix) in [
        ("a perspective", (0, 0), tilted),
        ("a perspective over a window of the stage", (2, 3), tilted),
        ("a homography with no perspective", (0, 0), flat),
    ] {
        let held_size = (width - origin.0, height - origin.1);
        let window: Vec<[f32; 3]> = (0..held_size.1)
            .flat_map(|y| {
                let values = &values;
                (0..held_size.0)
                    .map(move |x| values[((y + origin.1) * width + x + origin.0) as usize])
            })
            .collect();
        let reads = [0, 0, width, height];
        let tail = GpuTail::projective(output, reads, false, matrix);
        let drawn = qualifier
            .evaluate_codes(&plan(
                &window,
                held_size,
                origin,
                vec![GpuStep::Geometry(tail)],
            ))
            .unwrap();
        let expected = reference(
            &window,
            held_size,
            origin,
            reads,
            output,
            homography(matrix),
            false,
        );
        let (worst, differing) = compare(&drawn, &expected);
        eprintln!(
            "{what}: worst {worst}, {differing} of {} differ",
            expected.len()
        );
        assert!(worst <= 1, "{what}: {worst} codes from the reference");
    }
    // The flat homography is the affine tail's matrix: both draw the same codes.
    let draw = |step: GpuTail| {
        qualifier
            .evaluate_codes(&plan(
                &values,
                (width, height),
                (0, 0),
                vec![GpuStep::Geometry(step)],
            ))
            .unwrap()
    };
    let reads = [0, 0, width, height];
    let projective = draw(GpuTail::projective(output, reads, false, flat));
    let affine = draw(GpuTail::affine(output, reads, false, affine));
    let worst = projective
        .iter()
        .zip(&affine)
        .flat_map(|(a, b)| (0..3).map(move |channel| a[channel].abs_diff(b[channel])))
        .max()
        .unwrap();
    assert!(worst <= 1, "{worst} codes between the two tails");
}

/// A quantizing tail writes the content pass's result as codes and decodes them, and quantizes its
/// own blend, as a JPEG's segments do; a step after the tail runs at the output pixel.
#[test]
fn a_quantizing_tail_and_an_output_step() {
    let Some(qualifier) = Qualifier::headless("a_quantizing_tail_and_an_output_step") else {
        return;
    };
    let (width, height) = (64, 48);
    let values = texels(width, height);
    let matrix = [1.25f32, 0.0, 0.5, 0.0, 1.25, 0.25];
    let output = (48, 36);
    let tail = GpuTail::affine(output, [0, 0, width, height], true, matrix);
    let drawn = qualifier
        .evaluate_codes(&plan(
            &values,
            (width, height),
            (0, 0),
            vec![GpuStep::Geometry(tail.clone())],
        ))
        .unwrap();
    let expected = reference(
        &values,
        (width, height),
        (0, 0),
        [0, 0, width, height],
        output,
        |x, y| (1.25 * x + 0.5, 1.25 * y + 0.25),
        true,
    );
    let (worst, _) = compare(&drawn, &expected);
    assert!(worst <= 1, "quantized: {worst} codes from the reference");
    // A program after the tail paints the output pixel's own column into red.
    let column = GpuProgram::new(
        "column",
        "fn column(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return vec3<f32>(pos.x / 255.0, rgb.g, rgb.b);\n}\n",
    );
    let drawn = qualifier
        .evaluate(&plan(
            &values,
            (width, height),
            (0, 0),
            vec![GpuStep::Geometry(tail), GpuStep::colour(column)],
        ))
        .unwrap();
    assert_eq!(drawn.len(), (output.0 * output.1) as usize);
    for (index, texel) in drawn.iter().enumerate() {
        let x = index as u32 % output.0;
        assert_eq!((texel[0] * 255.0).round(), x as f32, "pixel {index}");
    }
}

/// A masked step after the tail covers the output pixel: its mask's position map, its bounds and
/// a position-based component all read the output stage, not the boundary's, so a crop's offset
/// moves nothing it covers.
#[test]
fn a_masked_step_after_the_tail_covers_the_output_pixel() {
    let Some(qualifier) =
        Qualifier::headless("a_masked_step_after_the_tail_covers_the_output_pixel")
    else {
        return;
    };
    let (width, height) = (64, 48);
    let black = vec![[0.0f32; 3]; (width * height) as usize];
    let output = (32u32, 28u32);
    let tail = GpuTail::affine(
        output,
        [10, 12, 42, 40],
        false,
        [1.0, 0.0, 10.0, 0.0, 1.0, 12.0],
    );
    // Coverage `(x + ½) / 32` of the output column; over black, a unit that paints white then
    // leaves exactly the coverage.
    let ramp = GpuProgram::new(
        "ramp",
        "fn ramp(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n    \
         return clamp((pos.x + 0.5) / 32.0, 0.0, 1.0);\n}\n",
    );
    let white = GpuProgram::new(
        "white",
        "fn white(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return vec3<f32>(1.0);\n}\n",
    );
    let bounds = [4, 2, 28, 26];
    let masked = GpuStep::Masked(MaskedColour {
        units: vec![white],
        position: PositionMap::IDENTITY,
        mask: Coverage {
            position: PositionMap::IDENTITY,
            bounds,
            supersample: false,
            components: vec![CoverageComponent {
                mode: CoverageMode::Add,
                invert: false,
                program: ramp,
            }],
            invert: false,
            scale: 1.0,
        },
    });
    let drawn = qualifier
        .evaluate(&plan(
            &black,
            (width, height),
            (0, 0),
            vec![GpuStep::Geometry(tail), masked],
        ))
        .unwrap();
    assert_eq!(drawn.len(), (output.0 * output.1) as usize);
    for (index, texel) in drawn.iter().enumerate() {
        let (x, y) = (index as u32 % output.0, index as u32 / output.0);
        let inside = (bounds[0]..bounds[2]).contains(&x) && (bounds[1]..bounds[3]).contains(&y);
        let expected = if inside { (x as f32 + 0.5) / 32.0 } else { 0.0 };
        for value in &texel[..3] {
            assert!(
                (value - expected).abs() <= 1e-6,
                "({x}, {y}): {value} for {expected}"
            );
        }
    }
}

/// A region plan draws only its rectangle of the output stage, each pixel at its own stage
/// coordinate: without a tail the content pass reads the boundary at the rectangle's offset into
/// the window it holds; with one the tail draws the rectangle's output pixels. A program after
/// either sees the output pixel's whole-stage coordinate.
#[test]
fn a_region_draws_its_rectangle_of_the_stage() {
    let Some(qualifier) = Qualifier::headless("a_region_draws_its_rectangle_of_the_stage") else {
        return;
    };
    let (width, height) = (40, 30);
    let values = texels(width, height);
    // The boundary holds the window at (10, 8) of a 64 × 48 stage.
    let origin = (10, 8);
    let stage = (64, 48);
    let rect = [14, 11, 34, 27];
    let region = GpuRegion { rect, stage };
    let (columns, rows) = region.size();
    // Red and green from the pixel's stage coordinate, blue the value it was handed.
    let position = GpuProgram::new(
        "position",
        "fn position(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return vec3<f32>(pos.x / 255.0, pos.y / 255.0, rgb.b);\n}\n",
    );
    // No tail: the window's texel at the rectangle's pixel.
    let mut flat = plan(
        &values,
        (width, height),
        origin,
        vec![GpuStep::colour(position.clone())],
    );
    flat.region = Some(region);
    let drawn = qualifier.evaluate(&flat).unwrap();
    assert_eq!(drawn.len(), (columns * rows) as usize);
    for (index, texel) in drawn.iter().enumerate() {
        let (x, y) = (index as u32 % columns, index as u32 / columns);
        let (sx, sy) = (rect[0] + x, rect[1] + y);
        assert_eq!((texel[0] * 255.0).round(), sx as f32, "({x}, {y})");
        assert_eq!((texel[1] * 255.0).round(), sy as f32, "({x}, {y})");
        let held = values[((sy - origin.1) * width + sx - origin.0) as usize];
        assert_eq!(texel[2], held[2], "({x}, {y})");
    }
    // A tail of a translation by (2, 1) into the 64 × 48 stage, drawn over the same rectangle:
    // its pixel (x, y) reads the boundary stage at (x + 2, y + 1).
    let tail = GpuTail::affine(
        (columns, rows),
        [origin.0, origin.1, origin.0 + width, origin.1 + height],
        false,
        [1.0, 0.0, 2.0, 0.0, 1.0, 1.0],
    );
    let mut tailed = plan(
        &values,
        (width, height),
        origin,
        vec![GpuStep::Geometry(tail), GpuStep::colour(position)],
    );
    tailed.region = Some(region);
    let drawn = qualifier.evaluate(&tailed).unwrap();
    assert_eq!(drawn.len(), (columns * rows) as usize);
    for (index, texel) in drawn.iter().enumerate() {
        let (x, y) = (index as u32 % columns, index as u32 / columns);
        let (sx, sy) = (rect[0] + x, rect[1] + y);
        assert_eq!((texel[0] * 255.0).round(), sx as f32, "tail ({x}, {y})");
        assert_eq!((texel[1] * 255.0).round(), sy as f32, "tail ({x}, {y})");
        let held = values[((sy + 1 - origin.1) * width + sx + 2 - origin.0) as usize];
        assert_eq!(texel[2], held[2], "tail ({x}, {y})");
    }
    // A rectangle the window does not hold is no frame of the plan.
    let mut outside = flat.clone();
    outside.region = Some(GpuRegion {
        rect: [4, 11, 24, 27],
        stage,
    });
    assert!(qualifier.evaluate(&outside).is_err());
}

/// The nearest half float toward zero of `value`, as a conversion that truncates gives it.
fn toward_zero(value: f32) -> f32 {
    let nearest = half::f16::from_f32(value);
    if nearest.to_f32().abs() > value.abs() {
        half::f16::from_bits(nearest.to_bits() - 1).to_f32()
    } else {
        nearest.to_f32()
    }
}

/// A RAW's tail reads its content pass's result from an `rgba32float` intermediate without
/// narrowing the linear values. An exact identity tail preserves values in every binade, of both
/// signs, bit for bit.
#[test]
fn a_raw_linear_tail_preserves_f32_intermediate_values() {
    let Some(qualifier) =
        Qualifier::headless("a_raw_linear_tail_preserves_f32_intermediate_values")
    else {
        return;
    };
    let (width, height) = (256, 64);
    let values: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            let t = index as f32 / (width * height) as f32;
            // From about 1e-3 to about 8, and a sign that flips.
            let value = 1.0e-3 * (9.0 * t).exp2() * (1.0 + 0.371 * (index as f32 * 0.77).sin());
            [value, -value * 0.73, value * 1.13 + 1.0e-4]
        })
        .collect();
    let gpu_boundary = GpuBoundary::from_linear(
        BoundaryFormat::Float,
        width,
        height,
        1,
        values.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
    )
    .expect("a boundary");
    let tail = GpuTail::affine(
        (width, height),
        [0, 0, width, height],
        false,
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    )
    .preserve_f32();
    let plan = GpuPlan {
        boundary: gpu_boundary,
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Geometry(tail)],
        region: None,
    };
    let drawn = qualifier.evaluate(&plan).expect("a readback");
    let mut changed = 0;
    for (texel, value) in drawn.iter().zip(&values) {
        for c in 0..3 {
            if texel[c].to_bits() != value[c].to_bits() {
                changed += 1;
            }
        }
    }
    assert_eq!(
        changed, 0,
        "identity geometry must preserve every f32 value"
    );
}

/// A spatial step's half-precision plane (`PlaneFormat::Colour`) holds each value a pass stores as
/// its nearest half float, ties to even: the M4's own conversion of a storage write rounds toward
/// zero, which the surface's rounding before the write replaces. A copy of an `f32` boundary into
/// the plane, shown by the frame's apply.
#[test]
fn a_half_planes_texels_are_the_nearest_half() {
    let Some(qualifier) = Qualifier::headless("a_half_planes_texels_are_the_nearest_half") else {
        return;
    };
    let (width, height) = (256, 64);
    let values: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            let t = index as f32 / (width * height) as f32;
            let value = 1.0e-3 * (9.0 * t).exp2() * (1.0 + 0.371 * (index as f32 * 0.77).sin());
            [value, -value * 0.73, value * 1.13 + 1.0e-4]
        })
        .collect();
    let program = "\
fn lf_test_copy(at: vec2<i32>, words: u32, block: u32) {
    lf_store(at, vec4<f32>(lf_source(at), 1.0));
}

fn lf_test_show(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    return lf_plane(planes, at).xyz;
}
";
    let spatial = GpuSpatial {
        program: GpuProgram::new("lf_test", program),
        planes: vec![GpuPlane {
            format: PlaneFormat::Colour,
            size: PlaneSize::Reduced(1),
        }],
        passes: vec![GpuPass {
            kernel: std::borrow::Cow::Borrowed("lf_test_copy"),
            inputs: vec![],
            output: 0,
            words: 0,
            source: 0,
            shape: PassShape::Texels { span: [1, 1] },
        }],
        applies: vec![GpuApply {
            function: std::borrow::Cow::Borrowed("lf_test_show"),
            planes: vec![0],
            words: 0,
        }],
        clamps: false,
        mask: None,
    };
    let plan = GpuPlan {
        boundary: GpuBoundary::from_linear(
            BoundaryFormat::Float,
            width,
            height,
            1,
            values.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
        )
        .expect("a boundary"),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Spatial(Box::new(spatial))],
        region: None,
    };
    let drawn = qualifier.evaluate(&plan).expect("a readback");
    let (mut nearest, mut truncated, mut neither) = (0, 0, 0);
    for (texel, value) in drawn.iter().zip(&values) {
        for c in 0..3 {
            let (got, want) = (texel[c], value[c]);
            if got == held(want) {
                nearest += 1;
            } else if got == toward_zero(want) {
                truncated += 1;
            } else {
                neither += 1;
            }
        }
    }
    eprintln!(
        "a_half_planes_texels_are_the_nearest_half: {nearest} nearest, {truncated} toward zero, \
         {neither} neither, of {}",
        3 * values.len()
    );
    assert_eq!((truncated, neither), (0, 0));
}
