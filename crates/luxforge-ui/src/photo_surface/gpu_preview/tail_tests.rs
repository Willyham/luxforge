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
