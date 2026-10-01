//! A reference executor of a GPU plan, for the plan's tests: the passes the photo surface runs,
//! on the CPU, with a Rust twin of each program evaluated in `f32` as a GPU evaluates it. A plan
//! whose order, coordinate maps, mask data or geometry tail is wrong draws a different picture from
//! the CPU frame it previews, so the tests compare the two.
//!
//! It is not a second renderer: it knows only the programs the tests use, and it reads nothing a
//! plan does not carry.
use super::{GpuComponent, GpuDescription, GpuMask, GpuOperation, GpuPlan};
use crate::{ComponentMode, Error, colour::srgb, modules::Region};

/// A colour program's twin: `rgb` at `pos`.
fn colour(unit: &GpuDescription, rgb: [f32; 3], pos: [f32; 2]) -> [f32; 3] {
    let word = |index: usize| f32::from_bits(unit.words[index]);
    match unit.program.entry {
        "lf_basic_exposure" | "lf_test_exposure" => rgb.map(|channel| channel * word(0)),
        "lf_test_positional" => [rgb[0] + pos[0] / word(0), rgb[1] + pos[1] / word(1), rgb[2]],
        other => panic!("the reference executor has no twin of {other}"),
    }
}

/// A coverage program's twin: one component's falloff at `pos`, before its inversion.
fn coverage(component: &GpuComponent, pos: [f32; 2], _rgb: [f32; 3]) -> f32 {
    let program = &component.program;
    let word = |index: usize| f32::from_bits(program.words[index]);
    let block = |index: usize| f32::from_bits(program.block.as_ref().expect("a block")[index]);
    match program.program.entry {
        "lf_test_ramp" => ((pos[0] + block(0) + 0.5) / word(0)).clamp(0.0, 1.0),
        other => panic!("the reference executor has no twin of {other}"),
    }
}

/// The mask's composed coverage at the pass pixel `(x, y)`, for the operation's input `rgb`.
fn mask_coverage(mask: &GpuMask, x: i64, y: i64, rgb: [f32; 3]) -> f32 {
    let (mx, my) = mask.position.at(x, y);
    let bounds = mask.bounds;
    let inside = mx >= i64::from(bounds.x0)
        && my >= i64::from(bounds.y0)
        && mx < i64::from(bounds.x1())
        && my < i64::from(bounds.y1());
    if !inside {
        return 0.0;
    }
    let fold = |px: i64, py: i64| -> f32 {
        let mut m = 0.0_f32;
        for component in &mask.components {
            let c = coverage(component, [px as f32, py as f32], rgb);
            let c = if component.invert { 1.0 - c } else { c };
            m = match component.mode {
                ComponentMode::Add => m.max(c),
                ComponentMode::Subtract => m.min(1.0 - c),
                ComponentMode::Intersect => m.min(c),
            };
        }
        let m = if mask.invert { 1.0 - m } else { m };
        mask.scale * m
    };
    if mask.supersample {
        let mut sum = 0.0;
        for (i, j) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            sum += fold(2 * mx + i, 2 * my + j);
        }
        sum * 0.25
    } else {
        fold(mx, my)
    }
}

/// One operation over one pixel of its pass.
fn operation(operation: &GpuOperation, rgb: [f32; 3], x: i64, y: i64) -> [f32; 3] {
    let (px, py) = operation.position.at(x, y);
    let pos = [px as f32, py as f32];
    let output = operation
        .units
        .iter()
        .fold(rgb, |value, unit| colour(unit, value, pos));
    let Some(mask) = &operation.mask else {
        return output;
    };
    let m = mask_coverage(mask, x, y, rgb);
    if m == 0.0 {
        return rgb;
    }
    std::array::from_fn(|channel| (1.0 - m) * rgb[channel] + m * output[channel])
}

/// The bilinear blend the tail takes at the boundary coordinate `(u, v)`, its taps clamped to
/// `reads`.
fn bilinear(texels: &[[f32; 3]], width: u32, reads: Region, u: f32, v: f32) -> [f32; 3] {
    let x = u - 0.5;
    let y = v - 0.5;
    let (left, top) = (x.floor(), y.floor());
    let (wx, wy) = (x - left, y - top);
    let clamp =
        |value: f32, low: u32, high: u32| value.max(low as f32).min((high - 1) as f32) as u32;
    let x0 = clamp(left, reads.x0, reads.x1());
    let x1 = clamp(left + 1.0, reads.x0, reads.x1());
    let y0 = clamp(top, reads.y0, reads.y1());
    let y1 = clamp(top + 1.0, reads.y0, reads.y1());
    let at = |x: u32, y: u32| texels[(y * width + x) as usize];
    let corners = [
        (at(x0, y0), (1.0 - wx) * (1.0 - wy)),
        (at(x1, y0), wx * (1.0 - wy)),
        (at(x0, y1), (1.0 - wx) * wy),
        (at(x1, y1), wx * wy),
    ];
    std::array::from_fn(|channel| {
        corners
            .iter()
            .map(|(texel, weight)| texel[channel] * weight)
            .sum()
    })
}

/// The output frame, RGBA8, that `plan` draws from `boundary`'s texels: the content operations,
/// the clamp, the tail through its matrix or grid, the output operations and the output codes.
pub(super) fn execute(plan: &GpuPlan, boundary: &[[f32; 3]]) -> Result<Vec<u8>, Error> {
    let stage = plan.boundary.stage;
    assert_eq!(
        boundary.len(),
        (stage.width * stage.height) as usize,
        "the boundary is its stage's texels"
    );
    let mut texels = boundary.to_vec();
    for (index, texel) in texels.iter_mut().enumerate() {
        let (x, y) = (
            (index as u32 % stage.width) as i64,
            (index as u32 / stage.width) as i64,
        );
        *texel = plan
            .content
            .iter()
            .fold(*texel, |value, step| operation(step, value, x, y));
        if plan.geometry.clamps {
            *texel = texel.map(|channel| channel.clamp(0.0, 1.0));
        }
    }
    let output = plan.geometry.output();
    let whole = Region {
        x0: 0,
        y0: 0,
        width: output.width,
        height: output.height,
    };
    let grid = plan.geometry.grid(whole, 1.0)?;
    let matrix = plan.geometry.affine().map(|m| m.map(|value| value as f32));
    let quantizer = srgb::quantizer();
    let mut rgba = Vec::with_capacity((output.width * output.height * 4) as usize);
    for y in 0..output.height {
        for x in 0..output.width {
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let [u, v] = match (&matrix, &grid) {
                (Some(m), _) => [m[0] * cx + m[1] * cy + m[2], m[3] * cx + m[4] * cy + m[5]],
                (None, Some(grid)) => grid.sample(cx, cy),
                (None, None) => unreachable!("a warp has a grid"),
            };
            let rgb = bilinear(&texels, stage.width, plan.geometry.reads, u, v);
            let rgb = plan.output.iter().fold(rgb, |value, step| {
                operation(step, value, i64::from(x), i64::from(y))
            });
            let [r, g, b] = quantizer.pixel(rgb);
            rgba.extend([r, g, b, 255]);
        }
    }
    Ok(rgba)
}
