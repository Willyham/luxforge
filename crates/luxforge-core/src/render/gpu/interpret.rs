//! A reference executor of a GPU plan, for the plan's tests: the passes the photo surface runs,
//! on the CPU. A test program runs as a Rust twin evaluated in `f32` as a GPU evaluates it; a
//! shipped module's program runs as the CPU unit it was described from, handed the one pixel at
//! the `pos` the plan gives it, since the desktop's readback tests qualify the program's own
//! arithmetic against that unit on a device. A plan whose order, coordinate maps, mask data or
//! geometry tail is wrong draws a different picture from the CPU frame it previews, so the tests
//! compare the two.
//!
//! It is not a second renderer: it reads nothing a plan does not carry but the CPU units of its
//! operations' layers.
use super::{GpuComponent, GpuDescription, GpuMask, GpuOperation, GpuPlan, GpuSpatial};
use crate::{
    ComponentMode, Error,
    colour::srgb,
    modules::{ColorOperation, Parallelism, Planes, PlanesMut, Region, SpatialOperation, Stage},
};
use std::collections::BTreeMap;

/// The colour operations of the compilation a plan was made from, by layer: the CPU units a
/// shipped program's twin runs.
pub(super) type Units = BTreeMap<usize, ColorOperation>;

/// The spatial operations of the same compilation, by layer: a spatial step's twin is its CPU
/// units, run over the whole boundary as one tile.
pub(super) type Spatials = BTreeMap<usize, SpatialOperation>;

/// Unit `index` of `layer`'s operation over `rgb` at `pos`: a test program's Rust twin, or a
/// shipped program's CPU unit.
fn colour(
    units: &Units,
    layer: usize,
    index: usize,
    unit: &GpuDescription,
    rgb: [f32; 3],
    pos: [f32; 2],
) -> [f32; 3] {
    let word = |index: usize| f32::from_bits(unit.words[index]);
    match unit.program.entry {
        "lf_test_exposure" | "lf_test_disabled" => rgb.map(|channel| channel * word(0)),
        "lf_test_positional" => [rgb[0] + pos[0] / word(0), rgb[1] + pos[1] / word(1), rgb[2]],
        other if other.starts_with("lf_test_") => {
            panic!("the reference executor has no twin of {other}")
        }
        other => {
            let cpu = &units
                .get(&layer)
                .unwrap_or_else(|| panic!("layer {layer} compiled no colour operation"))
                .units()[index];
            assert_eq!(
                cpu.gpu().map(|description| description.program.entry),
                Some(other),
                "unit {index} of layer {layer} is not the one the plan describes"
            );
            let mut pixel = [rgb];
            cpu.apply_row(pos[1] as u32, pos[0] as u32, &mut pixel);
            pixel[0]
        }
    }
}

/// Component `index` of `layer`'s mask at `pos`, before its inversion: a test program's Rust twin,
/// or a shipped coverage program's CPU field, narrowed from its `f64` falloff.
fn coverage(
    units: &Units,
    layer: usize,
    index: usize,
    component: &GpuComponent,
    pos: [f32; 2],
    rgb: [f32; 3],
) -> f32 {
    let program = &component.program;
    let word = |index: usize| f32::from_bits(program.words[index]);
    let block = |index: usize| f32::from_bits(program.block.as_ref().expect("a block")[index]);
    match program.program.entry {
        "lf_test_ramp" => ((pos[0] + block(0) + 0.5) / word(0)).clamp(0.0, 1.0),
        other if other.starts_with("lf_test_") => {
            panic!("the reference executor has no twin of {other}")
        }
        other => {
            let field = units
                .get(&layer)
                .and_then(ColorOperation::mask)
                .unwrap_or_else(|| panic!("layer {layer} compiled no masked colour operation"));
            assert_eq!(
                field
                    .gpu()
                    .ok()
                    .map(|mask| mask.components[index].program.program.entry),
                Some(other),
                "component {index} of layer {layer} is not the one the plan describes"
            );
            field.gpu_component_falloff(index, pos[0] as u32, pos[1] as u32, rgb.map(f64::from))
                as f32
        }
    }
}

/// The mask's composed coverage at the pass pixel `(x, y)`, for the input `rgb` of `layer`'s
/// operation.
fn mask_coverage(
    units: &Units,
    layer: usize,
    mask: &GpuMask,
    x: i64,
    y: i64,
    rgb: [f32; 3],
) -> f32 {
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
        for (index, component) in mask.components.iter().enumerate() {
            let c = coverage(units, layer, index, component, [px as f32, py as f32], rgb);
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
fn operation(units: &Units, operation: &GpuOperation, rgb: [f32; 3], x: i64, y: i64) -> [f32; 3] {
    let (px, py) = operation.position.at(x, y);
    let pos = [px as f32, py as f32];
    let output = operation
        .units
        .iter()
        .enumerate()
        .fold(rgb, |value, (index, unit)| {
            colour(units, operation.layer, index, unit, value, pos)
        });
    let Some(mask) = &operation.mask else {
        return output;
    };
    let m = mask_coverage(units, operation.layer, mask, x, y, rgb);
    if m == 0.0 {
        return rgb;
    }
    std::array::from_fn(|channel| (1.0 - m) * rgb[channel] + m * output[channel])
}

/// A spatial step over the content operations' frame of a `stage`, in place: its input clamped
/// where the step says the CPU quantizes, its CPU units over the whole frame as one tile (each
/// global estimate prepared from the reduction of its own input, which is what the CPU stores and
/// the GPU takes), its mask's blend against the input, and its output clamped likewise. The
/// desktop's readback tests qualify the program's passes against the same units on a device.
fn spatial(
    spatials: &Spatials,
    step: &GpuSpatial,
    stage: Stage,
    texels: &mut [[f32; 3]],
) -> Result<(), Error> {
    let operation = spatials
        .get(&step.layer)
        .unwrap_or_else(|| panic!("layer {} compiled no spatial operation", step.layer));
    assert_eq!(
        operation.len(),
        step.applies.len(),
        "the step applies one function per unit"
    );
    if step.clamps {
        for texel in texels.iter_mut() {
            *texel = texel.map(|channel| channel.clamp(0.0, 1.0));
        }
    }
    let input = texels.to_vec();
    let len = texels.len();
    let mut planes = vec![0.0_f32; 3 * len];
    for (index, texel) in texels.iter().enumerate() {
        for channel in 0..3 {
            planes[channel * len + index] = texel[channel];
        }
    }
    let region = Region::whole(stage);
    for unit in operation.units() {
        let global = match unit.estimate_key() {
            Some(_) => {
                let reduction = crate::render::spatial::build_reduction(stage, |x, y| {
                    let index = (y * stage.width + x) as usize;
                    Ok([0, 1, 2].map(|channel| planes[channel * len + index]))
                })?;
                unit.prepare(&reduction)
            }
            None => None,
        };
        let mut scratch = vec![0.0_f32; (unit.scratch_bytes(stage) / 4) as usize];
        let mut next = vec![0.0_f32; 3 * len];
        {
            let source = Planes::new(stage, region, &planes)?;
            let mut output = PlanesMut::new(stage, region, &mut next)?;
            unit.apply(
                &source,
                &mut output,
                global.as_ref(),
                &mut scratch,
                Parallelism::Serial,
            )?;
        }
        planes = next;
    }
    for (index, texel) in texels.iter_mut().enumerate() {
        let applied: [f32; 3] = std::array::from_fn(|channel| planes[channel * len + index]);
        let rgb = input[index];
        // A mask's twin is the operation's own field, evaluated where the CPU's blend evaluates
        // it: at the stage pixel, on the operation's input.
        let value = match (&step.mask, operation.mask()) {
            (None, _) => applied,
            (Some(_), None) => panic!("layer {} compiled no mask", step.layer),
            (Some(_), Some(field)) => {
                let (x, y) = (index as u32 % stage.width, index as u32 / stage.width);
                let m = field.evaluate(x, y, rgb);
                if m == 0.0 {
                    rgb
                } else {
                    std::array::from_fn(|channel| (1.0 - m) * rgb[channel] + m * applied[channel])
                }
            }
        };
        *texel = if step.clamps {
            value.map(|channel| channel.clamp(0.0, 1.0))
        } else {
            value
        };
    }
    Ok(())
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
/// `units` are the colour operations of the compilation the plan was made from.
pub(super) fn execute(
    plan: &GpuPlan,
    boundary: &[[f32; 3]],
    units: &Units,
) -> Result<Vec<u8>, Error> {
    execute_with(plan, boundary, units, &Spatials::new())
}

/// [`execute`] of a plan that may hold a spatial step, whose layer's CPU units `spatials` holds.
pub(super) fn execute_with(
    plan: &GpuPlan,
    boundary: &[[f32; 3]],
    units: &Units,
    spatials: &Spatials,
) -> Result<Vec<u8>, Error> {
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
            .fold(*texel, |value, step| operation(units, step, value, x, y));
    }
    if let Some(step) = &plan.spatial {
        spatial(spatials, step, stage, &mut texels)?;
    }
    if plan.geometry.clamps {
        for texel in texels.iter_mut() {
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
                operation(units, step, value, i64::from(x), i64::from(y))
            });
            let [r, g, b] = quantizer.pixel(rgb);
            rgba.extend([r, g, b, 255]);
        }
    }
    Ok(rgba)
}
