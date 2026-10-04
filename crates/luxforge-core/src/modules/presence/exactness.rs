//! The Presence passes against the code they replaced, bit for bit.
//!
//! The functions at the top are the passes as they were before they read row slices, frozen as the
//! reference: every tap of the box passes, every read of the guided filters' pointwise loops and
//! every tap of the upsample through the clamped, index-checked [`Plane::get`], one row's running
//! sum at a time, the vertical strip written one value at a time, the upsample's column index and
//! weight computed at every pixel, and the three units reading their input through
//! [`Planes::sample`]. The tests hold the production passes to them with `to_bits` over planes of
//! distinct values whose exponents span eighty binary orders, so a sum taken in another order
//! rounds differently, and of values in the unit interval, on odd and degenerate frames, radii from
//! 1 to past the frame, output rectangles on every frame edge and on none, held rectangles larger
//! than a pass needs, and both parallelisms; the units also on tiles at every stage corner, with
//! input a block-sum order shows in.
use super::{
    clarity::Clarity,
    dehaze::Dehaze,
    filters::{
        self, Geometry, Plane, PlaneMut, Rect, Scratch, box_min, downsample, for_rows_of,
        full_rect, reduced_frame, reduced_rect,
    },
    texture::Texture,
};
use crate::{
    Error,
    colour::{luma, srgb},
    modules::{Global, Parallelism, Planes, PlanesMut, Region, SpatialUnit, Stage},
};
use luxforge_reference::SplitMix64;

// ---------------------------------------------------------------------------------------------
// The frozen passes.
// ---------------------------------------------------------------------------------------------

const STRIP: usize = 64;

fn horizontal_mean(src: &Plane<'_>, r: i64, dst: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    let n = (2 * r + 1) as f64;
    dst.for_rows(parallelism, |y, row| {
        let mut sum = 0.0_f64;
        for dx in -r..=r {
            sum += f64::from(src.get(out.x0 + dx, y));
        }
        row[0] = (sum / n) as f32;
        for x in (out.x0 + 1)..out.x1 {
            sum += f64::from(src.get(x + r, y)) - f64::from(src.get(x - 1 - r, y));
            row[(x - out.x0) as usize] = (sum / n) as f32;
        }
    });
}

fn vertical_mean(src: &Plane<'_>, r: i64, dst: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let out = dst.rect();
    if out.is_empty() {
        return;
    }
    match parallelism {
        Parallelism::Serial => {
            let mut x0 = out.x0;
            while x0 < out.x1 {
                let columns = ((out.x1 - x0) as usize).min(STRIP);
                vertical_strip(src, r, out, x0, columns, |row, column, value| {
                    dst.set(x0 + column as i64, out.y0 + row as i64, value);
                });
                x0 += columns as i64;
            }
        }
        Parallelism::Pool => {
            use rayon::prelude::*;
            let width = out.width() as usize;
            let mut strips: Vec<Vec<&mut [f32]>> = (0..width.div_ceil(STRIP))
                .map(|_| Vec::with_capacity(out.height() as usize))
                .collect();
            for row in dst.values_mut()[..out.pixels()].chunks_mut(width) {
                for (strip, segment) in row.chunks_mut(STRIP).enumerate() {
                    strips[strip].push(segment);
                }
            }
            strips
                .par_iter_mut()
                .enumerate()
                .for_each(|(strip, segments)| {
                    let columns = segments[0].len();
                    let x0 = out.x0 + (strip * STRIP) as i64;
                    vertical_strip(src, r, out, x0, columns, |row, column, value| {
                        segments[row][column] = value;
                    });
                });
        }
    }
}

fn vertical_strip(
    src: &Plane<'_>,
    r: i64,
    out: Rect,
    x0: i64,
    columns: usize,
    mut write: impl FnMut(usize, usize, f32),
) {
    let n = (2 * r + 1) as f64;
    let mut accumulator = [0.0_f64; STRIP];
    for dy in -r..=r {
        for (column, slot) in accumulator.iter_mut().take(columns).enumerate() {
            *slot += f64::from(src.get(x0 + column as i64, out.y0 + dy));
        }
    }
    for (column, slot) in accumulator.iter().take(columns).enumerate() {
        write(0, column, (*slot / n) as f32);
    }
    for (row, y) in ((out.y0 + 1)..out.y1).enumerate() {
        for (column, slot) in accumulator.iter_mut().take(columns).enumerate() {
            let x = x0 + column as i64;
            *slot += f64::from(src.get(x, y + r)) - f64::from(src.get(x, y - 1 - r));
        }
        for (column, slot) in accumulator.iter().take(columns).enumerate() {
            write(row + 1, column, (*slot / n) as f32);
        }
    }
}

fn box_mean(
    src: &Plane<'_>,
    r: i64,
    dst: &mut PlaneMut<'_>,
    temp: &mut [f32],
    parallelism: Parallelism,
) -> Result<(), Error> {
    let geometry = dst.geometry();
    let mid = dst.rect().expand_y(r).clip(geometry.frame());
    let mut horizontal = PlaneMut::over(temp, geometry, mid)?;
    horizontal_mean(src, r, &mut horizontal, parallelism);
    vertical_mean(&horizontal.as_plane(), r, dst, parallelism);
    Ok(())
}

fn guided_self(
    src: &Plane<'_>,
    r: i64,
    eps: f32,
    dst: &mut PlaneMut<'_>,
    scratch: &mut Scratch<'_>,
    parallelism: Parallelism,
) -> Result<(), Error> {
    let geometry = dst.geometry();
    let frame = geometry.frame();
    let out = dst.rect();
    if out.is_empty() {
        return Ok(());
    }
    let inner = out.expand(r).clip(frame);
    let source = inner.expand(r).clip(frame);

    let mut scratch = scratch.branch();
    let squared_buffer = scratch.take(source.pixels())?;
    let mean_buffer = scratch.take(inner.pixels())?;
    let mean_squared_buffer = scratch.take(inner.pixels())?;
    let coefficient_buffer = scratch.take(out.pixels())?;
    let temp_buffer = scratch.take(inner.expand_y(r).clip(frame).pixels())?;

    let mut squared = PlaneMut::over(squared_buffer, geometry, source)?;
    squared.for_rows(parallelism, |y, row| {
        for x in source.x0..source.x1 {
            let value = src.get(x, y);
            row[(x - source.x0) as usize] = value * value;
        }
    });

    let mut mean = PlaneMut::over(mean_buffer, geometry, inner)?;
    box_mean(src, r, &mut mean, temp_buffer, parallelism)?;
    let mut mean_squared = PlaneMut::over(mean_squared_buffer, geometry, inner)?;
    box_mean(
        &squared.as_plane(),
        r,
        &mut mean_squared,
        temp_buffer,
        parallelism,
    )?;

    for_rows_of(parallelism, [&mut mean, &mut mean_squared], |_, rows| {
        let [mean, mean_squared] = rows;
        for column in 0..mean.len() {
            let m = mean[column];
            let variance = (mean_squared[column] - m * m).max(0.0);
            let a = variance / (variance + eps);
            mean_squared[column] = a;
            mean[column] = (1.0 - a) * m;
        }
    });

    box_mean(&mean.as_plane(), r, dst, temp_buffer, parallelism)?;
    let mut mean_a = PlaneMut::over(coefficient_buffer, geometry, out)?;
    box_mean(
        &mean_squared.as_plane(),
        r,
        &mut mean_a,
        temp_buffer,
        parallelism,
    )?;
    let mean_a = mean_a.as_plane();
    dst.for_rows(parallelism, |y, row| {
        for x in out.x0..out.x1 {
            let column = (x - out.x0) as usize;
            row[column] += mean_a.get(x, y) * src.get(x, y);
        }
    });
    Ok(())
}

fn guided_filter(
    guide: &Plane<'_>,
    input: &Plane<'_>,
    r: i64,
    eps: f32,
    dst: &mut PlaneMut<'_>,
    scratch: &mut Scratch<'_>,
    parallelism: Parallelism,
) -> Result<(), Error> {
    let geometry = dst.geometry();
    let frame = geometry.frame();
    let out = dst.rect();
    if out.is_empty() {
        return Ok(());
    }
    let inner = out.expand(r).clip(frame);
    let source = inner.expand(r).clip(frame);

    let mut scratch = scratch.branch();
    let guide_squared_buffer = scratch.take(source.pixels())?;
    let guide_input_buffer = scratch.take(source.pixels())?;
    let mean_guide_buffer = scratch.take(inner.pixels())?;
    let mean_input_buffer = scratch.take(inner.pixels())?;
    let mean_guide_squared_buffer = scratch.take(inner.pixels())?;
    let mean_guide_input_buffer = scratch.take(inner.pixels())?;
    let coefficient_buffer = scratch.take(out.pixels())?;
    let temp_buffer = scratch.take(inner.expand_y(r).clip(frame).pixels())?;

    let mut guide_squared = PlaneMut::over(guide_squared_buffer, geometry, source)?;
    let mut guide_input = PlaneMut::over(guide_input_buffer, geometry, source)?;
    for_rows_of(
        parallelism,
        [&mut guide_squared, &mut guide_input],
        |y, rows| {
            let [guide_squared, guide_input] = rows;
            for x in source.x0..source.x1 {
                let column = (x - source.x0) as usize;
                let g = guide.get(x, y);
                guide_squared[column] = g * g;
                guide_input[column] = g * input.get(x, y);
            }
        },
    );

    let mut mean_guide = PlaneMut::over(mean_guide_buffer, geometry, inner)?;
    box_mean(guide, r, &mut mean_guide, temp_buffer, parallelism)?;
    let mut mean_input = PlaneMut::over(mean_input_buffer, geometry, inner)?;
    box_mean(input, r, &mut mean_input, temp_buffer, parallelism)?;
    let mut mean_guide_squared = PlaneMut::over(mean_guide_squared_buffer, geometry, inner)?;
    box_mean(
        &guide_squared.as_plane(),
        r,
        &mut mean_guide_squared,
        temp_buffer,
        parallelism,
    )?;
    let mut mean_guide_input = PlaneMut::over(mean_guide_input_buffer, geometry, inner)?;
    box_mean(
        &guide_input.as_plane(),
        r,
        &mut mean_guide_input,
        temp_buffer,
        parallelism,
    )?;

    let (mean_guide, mean_guide_squared) = (mean_guide.as_plane(), mean_guide_squared.as_plane());
    for_rows_of(
        parallelism,
        [&mut mean_guide_input, &mut mean_input],
        |y, rows| {
            let [mean_guide_input, mean_input] = rows;
            for x in inner.x0..inner.x1 {
                let column = (x - inner.x0) as usize;
                let mg = mean_guide.get(x, y);
                let mi = mean_input[column];
                let variance = (mean_guide_squared.get(x, y) - mg * mg).max(0.0);
                let covariance = mean_guide_input[column] - mg * mi;
                let a = covariance / (variance + eps);
                mean_guide_input[column] = a;
                mean_input[column] = mi - a * mg;
            }
        },
    );

    box_mean(&mean_input.as_plane(), r, dst, temp_buffer, parallelism)?;
    let mut mean_a = PlaneMut::over(coefficient_buffer, geometry, out)?;
    box_mean(
        &mean_guide_input.as_plane(),
        r,
        &mut mean_a,
        temp_buffer,
        parallelism,
    )?;
    let mean_a = mean_a.as_plane();
    dst.for_rows(parallelism, |y, row| {
        for x in out.x0..out.x1 {
            let column = (x - out.x0) as usize;
            row[column] += mean_a.get(x, y) * guide.get(x, y);
        }
    });
    Ok(())
}

fn upsample(reduced: &Plane<'_>, reduction: i64, dst: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let out = dst.rect();
    let s = reduction as f32;
    dst.for_rows(parallelism, |y, row| {
        let v = ((y as f32) + 0.5) / s - 0.5;
        let j0 = v.floor();
        let fy = v - j0;
        let j0 = j0 as i64;
        for x in out.x0..out.x1 {
            let u = ((x as f32) + 0.5) / s - 0.5;
            let i0 = u.floor();
            let fx = u - i0;
            let i0 = i0 as i64;
            let top = reduced.get(i0, j0) * (1.0 - fx) + reduced.get(i0 + 1, j0) * fx;
            let bottom = reduced.get(i0, j0 + 1) * (1.0 - fx) + reduced.get(i0 + 1, j0 + 1) * fx;
            row[(x - out.x0) as usize] = top * (1.0 - fy) + bottom * fy;
        }
    });
}

/// Clarity's `apply` as it was, over the frozen filters, reading its input through
/// [`Planes::sample`] and its planes through [`Plane::get`].
fn clarity(
    unit: &Clarity,
    input: &Planes<'_>,
    output: &mut PlanesMut<'_>,
    scratch: &mut [f32],
    parallelism: Parallelism,
) -> Result<(), Error> {
    use super::clarity::{EPS_CLARITY, LIMIT_CLARITY, REDUCTION};
    let stage = input.stage();
    let frame = Rect::frame(i64::from(stage.width), i64::from(stage.height));
    let out = Rect::of(output.region());
    if out.is_empty() {
        return Ok(());
    }
    let geometry = Geometry::new(frame.x1, frame.y1, out);
    let (reduced_width, reduced_height) = reduced_frame(frame.x1, frame.y1, REDUCTION);
    let reduced_geometry = Geometry::new(
        reduced_width,
        reduced_height,
        Rect::frame(reduced_width, reduced_height),
    );
    let reduced_frame_rect = Rect::frame(reduced_width, reduced_height);

    let base_rect = reduced_rect(out, REDUCTION)
        .expand(1)
        .clip(reduced_frame_rect);
    let reduced_source = base_rect
        .expand(2 * unit.reduced_radius())
        .clip(reduced_frame_rect);
    let encoded_rect = full_rect(reduced_source, REDUCTION).clip(frame);

    let mut scratch = Scratch::new(scratch);
    let encoded_buffer = scratch.take(encoded_rect.pixels())?;
    let base_buffer = scratch.take(out.pixels())?;
    let reduced_buffer = scratch.take(reduced_source.pixels())?;
    let base_reduced_buffer = scratch.take(base_rect.pixels())?;

    let mut encoded = PlaneMut::over(encoded_buffer, geometry, encoded_rect)?;
    encoded.for_rows(parallelism, |y, row| {
        for x in encoded_rect.x0..encoded_rect.x1 {
            row[(x - encoded_rect.x0) as usize] = filters::encoded_luminance(input.sample(x, y));
        }
    });
    let encoded: Plane<'_> = encoded.as_plane();

    let mut encoded_reduced = PlaneMut::over(reduced_buffer, reduced_geometry, reduced_source)?;
    downsample(&encoded, REDUCTION, &mut encoded_reduced, parallelism);
    let mut base_reduced = PlaneMut::over(base_reduced_buffer, reduced_geometry, base_rect)?;
    guided_self(
        &encoded_reduced.as_plane(),
        unit.reduced_radius(),
        EPS_CLARITY,
        &mut base_reduced,
        &mut scratch,
        parallelism,
    )?;
    let mut base = PlaneMut::over(base_buffer, geometry, out)?;
    upsample(&base_reduced.as_plane(), REDUCTION, &mut base, parallelism);

    let base = base.as_plane();
    output.for_rows(parallelism, |y, red, green, blue| {
        let y = i64::from(y);
        for x in out.x0..out.x1 {
            let rgb = input.sample(x, y);
            let e = encoded.get(x, y);
            let residual = e - base.get(x, y);
            let delta = filters::soft_clip(unit.gain() * residual, e, LIMIT_CLARITY);
            let value = if delta == 0.0 {
                rgb
            } else {
                luma::reconstruct(rgb, luma::rec709(rgb), srgb::decode_f32(e + delta))
            };
            let column = (x - out.x0) as usize;
            [red[column], green[column], blue[column]] = value;
        }
    });
    Ok(())
}

/// Texture's `apply` as it was.
fn texture(
    unit: &Texture,
    input: &Planes<'_>,
    output: &mut PlanesMut<'_>,
    scratch: &mut [f32],
    parallelism: Parallelism,
) -> Result<(), Error> {
    use super::texture::{EPS_TEXTURE, LIMIT_TEXTURE};
    let stage = input.stage();
    let frame = Rect::frame(i64::from(stage.width), i64::from(stage.height));
    let out = Rect::of(output.region());
    if out.is_empty() {
        return Ok(());
    }
    let geometry = Geometry::new(frame.x1, frame.y1, out);
    let encoded_rect = out.expand(2 * unit.coarse()).clip(frame);

    let mut scratch = Scratch::new(scratch);
    let encoded_buffer = scratch.take(encoded_rect.pixels())?;
    let fine_buffer = scratch.take(out.pixels())?;
    let coarse_buffer = scratch.take(out.pixels())?;

    let mut encoded = PlaneMut::over(encoded_buffer, geometry, encoded_rect)?;
    encoded.for_rows(parallelism, |y, row| {
        for x in encoded_rect.x0..encoded_rect.x1 {
            row[(x - encoded_rect.x0) as usize] = filters::encoded_luminance(input.sample(x, y));
        }
    });
    let encoded: Plane<'_> = encoded.as_plane();

    let mut fine = PlaneMut::over(fine_buffer, geometry, out)?;
    guided_self(
        &encoded,
        unit.fine(),
        EPS_TEXTURE,
        &mut fine,
        &mut scratch,
        parallelism,
    )?;
    let mut coarse = PlaneMut::over(coarse_buffer, geometry, out)?;
    guided_self(
        &encoded,
        unit.coarse(),
        EPS_TEXTURE,
        &mut coarse,
        &mut scratch,
        parallelism,
    )?;

    let (fine, coarse) = (fine.as_plane(), coarse.as_plane());
    output.for_rows(parallelism, |y, red, green, blue| {
        let y = i64::from(y);
        for x in out.x0..out.x1 {
            let rgb = input.sample(x, y);
            let e = encoded.get(x, y);
            let band = fine.get(x, y) - coarse.get(x, y);
            let delta = filters::soft_clip(unit.gain() * band, e, LIMIT_TEXTURE);
            let value = if delta == 0.0 {
                rgb
            } else {
                luma::reconstruct(rgb, luma::rec709(rgb), srgb::decode_f32(e + delta))
            };
            let column = (x - out.x0) as usize;
            [red[column], green[column], blue[column]] = value;
        }
    });
    Ok(())
}

/// Dehaze's `apply` as it was, given its atmospheric light.
fn dehaze(
    unit: &Dehaze,
    input: &Planes<'_>,
    output: &mut PlanesMut<'_>,
    atmosphere: [f64; 3],
    scratch: &mut [f32],
    parallelism: Parallelism,
) -> Result<(), Error> {
    use super::dehaze::{EPS_DEHAZE, REDUCTION, T_FLOOR};
    let stage = input.stage();
    let frame = Rect::frame(i64::from(stage.width), i64::from(stage.height));
    let out = Rect::of(output.region());
    if out.is_empty() {
        return Ok(());
    }
    let geometry = Geometry::new(frame.x1, frame.y1, out);
    let (reduced_width, reduced_height) = reduced_frame(frame.x1, frame.y1, REDUCTION);
    let reduced_geometry = Geometry::new(
        reduced_width,
        reduced_height,
        Rect::frame(reduced_width, reduced_height),
    );
    let reduced_frame_rect = Rect::frame(reduced_width, reduced_height);

    let refined_rect = reduced_rect(out, REDUCTION)
        .expand(1)
        .clip(reduced_frame_rect);
    let raw_rect = refined_rect
        .expand(2 * unit.guide_radius())
        .clip(reduced_frame_rect);
    let dark_source_rect = raw_rect.expand(unit.dark_radius()).clip(reduced_frame_rect);

    let mut scratch = Scratch::new(scratch);
    let transmission_buffer = scratch.take(out.pixels())?;
    let red_buffer = scratch.take(dark_source_rect.pixels())?;
    let green_buffer = scratch.take(dark_source_rect.pixels())?;
    let blue_buffer = scratch.take(dark_source_rect.pixels())?;
    let normalized_buffer = scratch.take(dark_source_rect.pixels())?;
    let dark_buffer = scratch.take(raw_rect.pixels())?;
    let raw_buffer = scratch.take(raw_rect.pixels())?;
    let guide_buffer = scratch.take(raw_rect.pixels())?;
    let refined_buffer = scratch.take(refined_rect.pixels())?;

    let mut reduced = [
        PlaneMut::over(red_buffer, reduced_geometry, dark_source_rect)?,
        PlaneMut::over(green_buffer, reduced_geometry, dark_source_rect)?,
        PlaneMut::over(blue_buffer, reduced_geometry, dark_source_rect)?,
    ];
    let mut normalized = PlaneMut::over(normalized_buffer, reduced_geometry, dark_source_rect)?;
    let [red, green, blue] = &mut reduced;
    for_rows_of(
        parallelism,
        [red, green, blue, &mut normalized],
        |j, rows| {
            let y0 = j * REDUCTION;
            let y1 = ((j + 1) * REDUCTION).min(frame.y1);
            for i in dark_source_rect.x0..dark_source_rect.x1 {
                let x0 = i * REDUCTION;
                let x1 = ((i + 1) * REDUCTION).min(frame.x1);
                let mut sums = [0.0_f64; 3];
                let mut count = 0.0_f64;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let pixel = input.sample(x, y);
                        for (channel, sum) in sums.iter_mut().enumerate() {
                            *sum += f64::from(pixel[channel]);
                        }
                        count += 1.0;
                    }
                }
                let column = (i - dark_source_rect.x0) as usize;
                let mut smallest = f32::INFINITY;
                for (channel, sum) in sums.iter().enumerate() {
                    let mean = sum / count;
                    rows[channel][column] = mean as f32;
                    smallest = smallest.min((mean / atmosphere[channel]).clamp(0.0, 1.0) as f32);
                }
                rows[3][column] = smallest;
            }
        },
    );

    let mut dark = PlaneMut::over(dark_buffer, reduced_geometry, raw_rect)?;
    {
        let mut temp = scratch.branch();
        let temp_buffer = temp.take(
            raw_rect
                .expand_y(unit.dark_radius())
                .clip(reduced_frame_rect)
                .pixels(),
        )?;
        box_min(
            &normalized.as_plane(),
            unit.dark_radius(),
            &mut dark,
            temp_buffer,
            parallelism,
        )?;
    }

    let mut raw = PlaneMut::over(raw_buffer, reduced_geometry, raw_rect)?;
    let mut guide = PlaneMut::over(guide_buffer, reduced_geometry, raw_rect)?;
    let dark = dark.as_plane();
    let reduced = reduced.each_ref().map(|plane| plane.as_plane());
    for_rows_of(parallelism, [&mut raw, &mut guide], |j, rows| {
        for i in raw_rect.x0..raw_rect.x1 {
            let column = (i - raw_rect.x0) as usize;
            rows[0][column] = 1.0 - unit.omega() * dark.get(i, j);
            let pixel = [
                reduced[0].get(i, j),
                reduced[1].get(i, j),
                reduced[2].get(i, j),
            ];
            rows[1][column] = filters::encoded_luminance(pixel);
        }
    });

    let mut refined = PlaneMut::over(refined_buffer, reduced_geometry, refined_rect)?;
    guided_filter(
        &guide.as_plane(),
        &raw.as_plane(),
        unit.guide_radius(),
        EPS_DEHAZE,
        &mut refined,
        &mut scratch,
        parallelism,
    )?;
    let mut transmission = PlaneMut::over(transmission_buffer, geometry, out)?;
    upsample(
        &refined.as_plane(),
        REDUCTION,
        &mut transmission,
        parallelism,
    );

    let atmosphere: [f32; 3] = std::array::from_fn(|channel| atmosphere[channel] as f32);
    let positive = unit.positive();
    let transmission = transmission.as_plane();
    output.for_rows(parallelism, |y, red, green, blue| {
        let y = i64::from(y);
        for x in out.x0..out.x1 {
            let pixel = input.sample(x, y);
            let t = transmission.get(x, y).clamp(T_FLOOR, 1.0);
            let value: [f32; 3] = std::array::from_fn(|channel| {
                if positive {
                    (pixel[channel] - atmosphere[channel]) / t + atmosphere[channel]
                } else {
                    let veil = t * unit.veil();
                    veil * pixel[channel] + (1.0 - veil) * atmosphere[channel]
                }
            });
            let column = (x - out.x0) as usize;
            [red[column], green[column], blue[column]] = value;
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Planes and rectangles to compare them over.
// ---------------------------------------------------------------------------------------------

const PARALLELISMS: [Parallelism; 2] = [Parallelism::Serial, Parallelism::Pool];

/// Odd, thin and degenerate frames, and two larger ones whose rows outnumber a pass's row group
/// and whose columns outnumber a vertical strip.
const FRAMES: [(i64, i64); 10] = [
    (1, 1),
    (1, 9),
    (9, 1),
    (2, 3),
    (5, 4),
    (7, 6),
    (17, 9),
    (40, 33),
    (97, 61),
    (131, 70),
];

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

/// A value whose sums with its neighbours round in `f64`: a random 24-bit mantissa at a binary
/// exponent from -40 to 40 and either sign, so reordering or reassociating any sum changes bits,
/// with exact and negative zeros among them.
fn value(rng: &mut SplitMix64) -> f32 {
    match rng.next_u32(40) {
        0 => 0.0,
        1 => -0.0,
        _ => {
            let magnitude = rng.next_range(1.0, 2.0) * 2f64.powi(rng.next_u32(81) as i32 - 40);
            (if rng.next_bool() {
                magnitude
            } else {
                -magnitude
            }) as f32
        }
    }
}

/// `len` values of one of four kinds: distinct values from [`value`], distinct values in the unit
/// interval (the encoded domain the guided filters smooth), all negative zero (a sum seeded
/// anywhere but `0.0` keeps its sign), or one value in a field of negative zeros.
fn values(rng: &mut SplitMix64, len: usize) -> Vec<f32> {
    match rng.next_u32(8) {
        _ if len == 0 => Vec::new(),
        0 => vec![-0.0; len],
        1 => {
            let mut values = vec![-0.0; len];
            values[rng.next_usize(len)] = value(rng);
            values
        }
        2 | 3 => (0..len).map(|_| rng.next_range(0.0, 1.0) as f32).collect(),
        _ => (0..len).map(|_| value(rng)).collect(),
    }
}

/// Output rectangles of a frame: the whole frame, each corner, a strip down the middle touching
/// the top and bottom, one across touching the left and right, one pixel in the middle, and
/// random rectangles, some touching no edge where the frame allows it.
fn rects(rng: &mut SplitMix64, width: i64, height: i64) -> Vec<Rect> {
    let (w, h) = (width, height);
    let mut rects = vec![
        Rect::frame(w, h),
        Rect::new(0, 0, w.min(5), h.min(3)),
        Rect::new(w - w.min(6), 0, w, h.min(4)),
        Rect::new(0, h - h.min(2), w.min(3), h),
        Rect::new(w - w.min(4), h - h.min(5), w, h),
        Rect::new(w / 3, 0, w / 3 + (w / 3).max(1), h),
        Rect::new(0, h / 3, w, h / 3 + (h / 3).max(1)),
        Rect::new(w / 2, h / 2, w / 2 + 1, h / 2 + 1),
    ];
    if w > 2 && h > 2 {
        rects.push(Rect::new(1, 1, w - 1, h - 1));
    }
    for _ in 0..3 {
        let x0 = i64::from(rng.next_u32(w as u32));
        let y0 = i64::from(rng.next_u32(h as u32));
        let x1 = x0 + 1 + i64::from(rng.next_u32((w - x0) as u32));
        let y1 = y0 + 1 + i64::from(rng.next_u32((h - y0) as u32));
        rects.push(Rect::new(x0, y0, x1, y1));
    }
    rects
}

/// Box radii from 1 to past the frame: the small ones, about half the frame, and beyond it.
fn radii(width: i64, height: i64) -> Vec<i64> {
    let long = width.max(height);
    let mut radii = vec![1, 2, 3, long / 2, long / 2 + 1, long, 2 * long + 1];
    radii.retain(|r| *r >= 1);
    radii.sort_unstable();
    radii.dedup();
    radii
}

/// `before`, written over `rect` of a frame by `pass`, as bits.
fn written(
    geometry: Geometry,
    rect: Rect,
    before: &[f32],
    pass: impl FnOnce(&mut PlaneMut<'_>),
) -> Vec<u32> {
    let mut values = before.to_vec();
    pass(&mut PlaneMut::over(&mut values, geometry, rect).expect("the output plane"));
    bits(&values)
}

/// `rect` grown by `x` columns and `y` rows on each side and clipped to `frame`: the rectangle a
/// pass's source holds, at least what it reads.
fn grown(rect: Rect, x: i64, y: i64, frame: Rect) -> Rect {
    Rect::new(rect.x0 - x, rect.y0 - y, rect.x1 + x, rect.y1 + y).clip(frame)
}

// ---------------------------------------------------------------------------------------------
// The box passes.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_horizontal_mean_matches_the_tap_by_tap_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x4B0E_57A1);
    let mut compared = 0;
    for (width, height) in FRAMES {
        let frame = Rect::frame(width, height);
        let geometry = Geometry::new(width, height, frame);
        for out in rects(&mut rng, width, height) {
            for r in radii(width, height) {
                let extra = [0, 0, 1, 3][rng.next_usize(4)];
                let held = grown(out, r + extra, extra, frame);
                let mut source = values(&mut rng, held.pixels());
                let source = PlaneMut::over(&mut source, geometry, held).expect("the source");
                let source = source.as_plane();
                let before = values(&mut rng, out.pixels());
                for parallelism in PARALLELISMS {
                    let expected = written(geometry, out, &before, |dst| {
                        horizontal_mean(&source, r, dst, parallelism)
                    });
                    let actual = written(geometry, out, &before, |dst| {
                        filters::horizontal_mean(&source, r, dst, parallelism)
                    });
                    assert_eq!(
                        actual, expected,
                        "{width}x{height} {out:?} held {held:?} radius {r} {parallelism:?}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 1000, "{compared} comparisons");
}

#[test]
fn the_vertical_mean_matches_the_tap_by_tap_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x7E27_1CA1);
    let mut compared = 0;
    for (width, height) in FRAMES {
        let frame = Rect::frame(width, height);
        let geometry = Geometry::new(width, height, frame);
        for out in rects(&mut rng, width, height) {
            for r in radii(width, height) {
                let extra = [0, 0, 1, 3][rng.next_usize(4)];
                let held = grown(out, extra, r + extra, frame);
                let mut source = values(&mut rng, held.pixels());
                let source = PlaneMut::over(&mut source, geometry, held).expect("the source");
                let source = source.as_plane();
                let before = values(&mut rng, out.pixels());
                for parallelism in PARALLELISMS {
                    let expected = written(geometry, out, &before, |dst| {
                        vertical_mean(&source, r, dst, parallelism)
                    });
                    let actual = written(geometry, out, &before, |dst| {
                        filters::vertical_mean(&source, r, dst, parallelism)
                    });
                    assert_eq!(
                        actual, expected,
                        "{width}x{height} {out:?} held {held:?} radius {r} {parallelism:?}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 1000, "{compared} comparisons");
}

// ---------------------------------------------------------------------------------------------
// The guided filters.
// ---------------------------------------------------------------------------------------------

/// The three units' regularizations, and one far below and one far above them.
const EPSILONS: [f32; 5] = [1.0e-2, 2.5e-3, 1.0e-4, 1.0e-9, 4.0];

/// The frozen filters run serially: their serial and pooled outputs are equal, as the box tests
/// above and `slow_a_tile_evaluated_on_the_pool_is_bit_identical_to_a_serial_one` hold them, and
/// the production filters are held to them under both parallelisms.
#[test]
fn the_guided_filters_match_the_tap_by_tap_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x6E1D_ED00);
    let mut compared = 0;
    for (width, height) in FRAMES {
        let frame = Rect::frame(width, height);
        let geometry = Geometry::new(width, height, frame);
        for out in rects(&mut rng, width, height) {
            for r in radii(width, height) {
                let eps = EPSILONS[rng.next_usize(EPSILONS.len())];
                let mut held = || {
                    let extra = [0, 0, 1, 3][rng.next_usize(4)];
                    grown(out, 2 * r + extra, 2 * r + extra, frame)
                };
                let (guide_held, input_held) = (held(), held());
                let mut guide = values(&mut rng, guide_held.pixels());
                let guide = PlaneMut::over(&mut guide, geometry, guide_held).expect("the guide");
                let guide = guide.as_plane();
                let mut input = values(&mut rng, input_held.pixels());
                let input = PlaneMut::over(&mut input, geometry, input_held).expect("the input");
                let input = input.as_plane();
                let before = values(&mut rng, out.pixels());
                // Scratch the passes find holding values, as a reused tile slot's does.
                let scratch = values(&mut rng, frame.pixels() * filters::GUIDED_PLANES);
                let smoothed = written(geometry, out, &before, |dst| {
                    let mut scratch = scratch.clone();
                    let mut scratch = Scratch::new(&mut scratch);
                    guided_self(&guide, r, eps, dst, &mut scratch, Parallelism::Serial)
                        .expect("the reference smoother")
                });
                let filtered = written(geometry, out, &before, |dst| {
                    let mut scratch = scratch.clone();
                    let mut scratch = Scratch::new(&mut scratch);
                    guided_filter(
                        &guide,
                        &input,
                        r,
                        eps,
                        dst,
                        &mut scratch,
                        Parallelism::Serial,
                    )
                    .expect("the reference guided filter")
                });
                for parallelism in PARALLELISMS {
                    let context = format!(
                        "{width}x{height} {out:?} guide {guide_held:?} input {input_held:?} \
                         radius {r} eps {eps} {parallelism:?}"
                    );
                    let actual = written(geometry, out, &before, |dst| {
                        let mut scratch = scratch.clone();
                        let mut scratch = Scratch::new(&mut scratch);
                        filters::guided_self(&guide, r, eps, dst, &mut scratch, parallelism)
                            .expect("the smoother")
                    });
                    assert_eq!(actual, smoothed, "self-guided {context}");
                    let actual = written(geometry, out, &before, |dst| {
                        let mut scratch = scratch.clone();
                        let mut scratch = Scratch::new(&mut scratch);
                        filters::guided_filter(
                            &guide,
                            &input,
                            r,
                            eps,
                            dst,
                            &mut scratch,
                            parallelism,
                        )
                        .expect("the guided filter")
                    });
                    assert_eq!(actual, filtered, "guided {context}");
                    compared += 2;
                }
            }
        }
    }
    assert!(compared > 2000, "{compared} comparisons");
}

// ---------------------------------------------------------------------------------------------
// The upsample.
// ---------------------------------------------------------------------------------------------

/// Every reduction from 1 to 5, the units' own 4 among them, two that do not divide the frames,
/// and the host's estimate reduction of 16, which leaves most frames here a reduced grid one or two
/// pixels wide, all interior lost to the edges.
const REDUCTIONS: [i64; 7] = [1, 2, 3, 4, 5, 7, 16];

#[test]
fn the_upsample_matches_the_tap_by_tap_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x0B5A_3B1E);
    let mut compared = 0;
    for (width, height) in FRAMES {
        let frame = Rect::frame(width, height);
        let geometry = Geometry::new(width, height, frame);
        for reduction in REDUCTIONS {
            let (reduced_width, reduced_height) = filters::reduced_frame(width, height, reduction);
            let reduced_frame = Rect::frame(reduced_width, reduced_height);
            let reduced_geometry = Geometry::new(reduced_width, reduced_height, reduced_frame);
            for out in rects(&mut rng, width, height) {
                // What the units hold: the output's reduced rectangle and one more reduced index
                // on each side, here sometimes more.
                let extra = [0, 0, 1, 2][rng.next_usize(4)];
                let held = filters::reduced_rect(out, reduction)
                    .expand(1 + extra)
                    .clip(reduced_frame);
                let mut reduced = values(&mut rng, held.pixels());
                let reduced = PlaneMut::over(&mut reduced, reduced_geometry, held)
                    .expect("the reduced plane");
                let reduced = reduced.as_plane();
                let before = values(&mut rng, out.pixels());
                let expected = written(geometry, out, &before, |dst| {
                    upsample(&reduced, reduction, dst, Parallelism::Serial)
                });
                for parallelism in PARALLELISMS {
                    let actual = written(geometry, out, &before, |dst| {
                        filters::upsample(&reduced, reduction, dst, parallelism)
                    });
                    assert_eq!(
                        actual, expected,
                        "{width}x{height} by {reduction}: {out:?} from {held:?} {parallelism:?}"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 1000, "{compared} comparisons");
}

// ---------------------------------------------------------------------------------------------
// The units.
// ---------------------------------------------------------------------------------------------

enum Unit {
    Clarity(Clarity),
    Texture(Texture),
    Dehaze(Dehaze),
}

impl Unit {
    fn production(&self) -> &dyn SpatialUnit {
        match self {
            Self::Clarity(unit) => unit,
            Self::Texture(unit) => unit,
            Self::Dehaze(unit) => unit,
        }
    }

    /// The frozen unit's output, serially: its serial and pooled outputs are equal, as
    /// `slow_a_tile_evaluated_on_the_pool_is_bit_identical_to_a_serial_one` held it while it was
    /// the production code.
    fn reference(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        atmosphere: [f64; 3],
        scratch: &mut [f32],
    ) {
        let serial = Parallelism::Serial;
        match self {
            Self::Clarity(unit) => clarity(unit, input, output, scratch, serial),
            Self::Texture(unit) => texture(unit, input, output, scratch, serial),
            Self::Dehaze(unit) => dehaze(unit, input, output, atmosphere, scratch, serial),
        }
        .expect("the frozen unit");
    }
}

/// Each unit the module compiles at `long_side`, at both extremes and a value between: Dehaze's
/// inverse and forward branches, and both signs of Clarity's and Texture's gains.
fn units(long_side: u32) -> Vec<(String, Unit)> {
    let mut units = Vec::new();
    for amount in [100.0, 37.0, -100.0] {
        units.push((
            format!("clarity {amount:+}"),
            Unit::Clarity(Clarity::new(amount, long_side)),
        ));
        units.push((
            format!("texture {amount:+}"),
            Unit::Texture(Texture::new(amount, long_side)),
        ));
        units.push((
            format!("dehaze {amount:+}"),
            Unit::Dehaze(Dehaze::new(amount, long_side)),
        ));
    }
    units
}

/// A 4 x 4 block whose `f64` sum taken row by row, as Dehaze's reduction takes it, is `1 + 2^-24`
/// (each `2^-54` meets the `1` alone and is lost), and taken column by column `1 + 2^-24 + 2^-52`
/// (the three `2^-54` add up first): its mean rounds to `2^-4` in `f32` one way, a tie broken to
/// even, and to the next `f32` up the other.
const ROUNDING_BLOCK: [[f32; 4]; 4] = {
    let tiny = 1.0 / (1u64 << 54) as f32;
    let half_ulp = 1.0 / (1u64 << 24) as f32;
    [
        [0.0, 1.0, 0.0, 0.0],
        [tiny, half_ulp, 0.0, 0.0],
        [tiny, 0.0, 0.0, 0.0],
        [tiny, 0.0, 0.0, 0.0],
    ]
};

/// Planar linear RGB over `region` of a stage: distinct values mostly in the unit interval, some
/// past either end of it and some exact zeros; or one flat colour, whose residual and band are
/// zero, so Clarity and Texture pass it through; or [`value`]'s, which span eighty binary orders;
/// or [`ROUNDING_BLOCK`] on Dehaze's block grid, scaled by a power of two per block, whose
/// reduction taken column by column instead of row by row changes the reduced means.
fn rgb(rng: &mut SplitMix64, region: Region) -> Vec<f32> {
    let pixels = region.pixels() as usize;
    match rng.next_u32(6) {
        0 => {
            let flat = [0; 3].map(|_| rng.next_range(0.0, 1.0) as f32);
            return flat
                .iter()
                .flat_map(|value| std::iter::repeat_n(*value, pixels))
                .collect();
        }
        1 => return (0..3 * pixels).map(|_| value(rng)).collect(),
        2 => {
            let shift = [0; 3].map(|_| rng.next_u32(4));
            return (0..3)
                .flat_map(|channel| {
                    (region.y0..region.y1()).flat_map(move |y| {
                        (region.x0..region.x1()).map(move |x| {
                            let block = (x / 4 + 3 * (y / 4) + shift[channel]) % 4;
                            ROUNDING_BLOCK[(y % 4) as usize][(x % 4) as usize]
                                / (1u32 << block) as f32
                        })
                    })
                })
                .collect();
        }
        _ => {}
    }
    (0..3 * pixels)
        .map(|_| match rng.next_u32(16) {
            0 => 0.0,
            1 => rng.next_range(-0.05, 0.0) as f32,
            2 => rng.next_range(1.0, 1.3) as f32,
            _ => rng.next_range(0.0, 1.0) as f32,
        })
        .collect()
}

/// Output tiles of a stage: the whole stage, a tile at each corner, one in the middle and two at
/// random.
fn tiles(rng: &mut SplitMix64, stage: Stage) -> Vec<Region> {
    let (w, h) = (stage.width, stage.height);
    let (tw, th) = (w.min(16), h.min(16));
    let (mw, mh) = (w.min(8), h.min(8));
    let tile = |x0, y0, width, height| Region {
        x0,
        y0,
        width,
        height,
    };
    let mut tiles = vec![
        Region::whole(stage),
        tile(0, 0, tw, th),
        tile(w - tw, 0, tw, th),
        tile(0, h - th, tw, th),
        tile(w - tw, h - th, tw, th),
        tile((w - mw) / 2, (h - mh) / 2, mw, mh),
    ];
    for _ in 0..2 {
        let (x0, y0) = (rng.next_u32(w), rng.next_u32(h));
        let width = 1 + rng.next_u32((w - x0).min(24));
        let height = 1 + rng.next_u32((h - y0).min(24));
        tiles.push(tile(x0, y0, width, height));
    }
    tiles
}

/// Odd stages, from one pixel to one large enough that a middle tile's input reaches no edge at
/// the smaller long sides.
const STAGES: [(u32, u32); 7] = [
    (1, 1),
    (3, 2),
    (9, 7),
    (40, 33),
    (97, 61),
    (131, 70),
    (203, 157),
];

/// Each unit at its stage's own long side, at 480 and at 6000 (where Clarity's halo of 199 passes
/// every stage here), over every tile with the input the host would give it (the tile grown by
/// the unit's halo, sometimes more), scratch and output holding stale values, and both
/// parallelisms against the serial reference.
#[test]
fn the_units_match_their_frozen_passes_bit_for_bit() {
    let mut rng = SplitMix64(0x9E5E_4CE0);
    let mut compared = 0;
    for (width, height) in STAGES {
        let stage = Stage { width, height };
        for long_side in [width.max(height), 480, 6000] {
            for (name, unit) in units(long_side) {
                let production = unit.production();
                let halo = production.halo(stage);
                for tile in tiles(&mut rng, stage) {
                    let input_region = tile.grown(halo + rng.next_u32(3), stage);
                    let output_region = input_region.shrunk(halo, stage);
                    let values = rgb(&mut rng, input_region);
                    let input = Planes::new(stage, input_region, &values).expect("the input");
                    let atmosphere = [0; 3].map(|_| rng.next_range(0.3, 1.0));
                    let global = Global::new(atmosphere.to_vec()).expect("an atmospheric light");
                    let scratch_values = production.scratch_bytes(Stage {
                        width: input_region.width,
                        height: input_region.height,
                    }) / 4;
                    let scratch: Vec<f32> = (0..scratch_values)
                        .map(|_| rng.next_range(-2.0, 2.0) as f32)
                        .collect();
                    let before: Vec<f32> = (0..3 * output_region.pixels())
                        .map(|_| rng.next_range(-1.0, 1.0) as f32)
                        .collect();
                    let expected = {
                        let mut values = before.clone();
                        let mut output =
                            PlanesMut::new(stage, output_region, &mut values).expect("the output");
                        unit.reference(&input, &mut output, atmosphere, &mut scratch.clone());
                        bits(&values)
                    };
                    for parallelism in PARALLELISMS {
                        let mut values = before.clone();
                        let mut output =
                            PlanesMut::new(stage, output_region, &mut values).expect("the output");
                        production
                            .apply(
                                &input,
                                &mut output,
                                Some(&global),
                                &mut scratch.clone(),
                                parallelism,
                            )
                            .expect("the unit");
                        assert_eq!(
                            bits(&values),
                            expected,
                            "{name} at long side {long_side}: {stage:?} tile {tile:?} input \
                             {input_region:?} {parallelism:?}"
                        );
                        compared += 1;
                    }
                }
            }
        }
    }
    assert!(compared > 2000, "{compared} comparisons");
}
