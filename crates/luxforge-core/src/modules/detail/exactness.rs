//! The Detail passes against the code they replaced, bit for bit.
//!
//! The functions at the top are the passes as they were before they read whole rows, frozen as the
//! reference: every smoothing tap clamped and checked through [`Geometry::sample`], every level
//! smoothing all three channels, subtracting in a pass of its own and copying the next level's
//! input back, the shrinkage once per channel with chroma's factor computed for a and again for b,
//! and sharpening smoothing its blur and its guide separately. The tests hold the production passes
//! to them with `to_bits` over random planes, tiny and odd stages, stage-edge and interior tiles,
//! stages narrower than a kernel, every B3 spacing, sampled kernels, each noise-reduction strength
//! alone and both, sharpening at Radius 1 and other radii, and both parallelisms.
use super::{
    denoise::{self, Denoise},
    filters::{self, Geometry, Kernel},
    sharpen::Sharpen,
};
use crate::{
    Cancel, Error,
    modules::{Parallelism, Planes, PlanesMut, Region, SamplingScale, SpatialUnit, Stage},
};
use luxforge_reference::SplitMix64;

#[allow(clippy::too_many_arguments)]
fn smooth(
    src: &[f32],
    dst: &mut [f32],
    temporary: &mut [f32],
    geometry: Geometry,
    out: Region,
    kernels: &[Kernel; 2],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    let y0 = out.y0.saturating_sub(kernels[1].radius);
    let y1 = out
        .y1()
        .saturating_add(kernels[1].radius)
        .min(geometry.stage.height);
    let horizontal = Region {
        x0: out.x0,
        y0,
        width: out.width,
        height: y1 - y0,
    };
    filters::rows(
        temporary,
        geometry,
        horizontal,
        parallelism,
        cancel,
        |y, row| {
            for (column, value) in row.iter_mut().enumerate() {
                let x = i64::from(out.x0) + column as i64;
                let y = i64::from(y);
                let center = geometry.sample(src, x, y);
                let mut result = center;
                for &(offset, weight) in &kernels[0].taps {
                    result += weight * (geometry.sample(src, x + offset, y) - center);
                }
                *value = result;
            }
        },
    )?;
    filters::rows(dst, geometry, out, parallelism, cancel, |y, row| {
        for (column, value) in row.iter_mut().enumerate() {
            let x = i64::from(out.x0) + column as i64;
            let y = i64::from(y);
            let center = geometry.sample(temporary, x, y);
            let mut result = center;
            for &(offset, weight) in &kernels[1].taps {
                result += weight * (geometry.sample(temporary, x, y + offset) - center);
            }
            *value = result;
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn shrink(
    band: &[f32],
    delta: &mut [f32],
    geometry: Geometry,
    out: Region,
    thresholds: [f32; 2],
    details: [f32; 2],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    let len = geometry.held.pixels() as usize;
    for c in 0..3 {
        let kind = usize::from(c != 0);
        let threshold = thresholds[kind];
        if threshold == 0.0 {
            continue;
        }
        let detail = details[kind];
        filters::rows(
            &mut delta[c * len..(c + 1) * len],
            geometry,
            out,
            parallelism,
            cancel,
            |y, row| {
                for (column, value) in row.iter_mut().enumerate() {
                    let x = i64::from(out.x0) + column as i64;
                    let y = i64::from(y);
                    let index = geometry.index(x, y);
                    let mut energy = 0.0;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let i = geometry.index(x + dx, y + dy);
                            if kind == 0 {
                                energy += band[i] * band[i];
                            } else {
                                energy += band[len + i] * band[len + i]
                                    + band[2 * len + i] * band[2 * len + i];
                            }
                        }
                    }
                    energy /= 9.0;
                    let protection = energy / (energy + (3.0 * threshold) * (3.0 * threshold));
                    let effective = threshold * (1.0 - detail * protection);
                    let squared = if kind == 0 {
                        band[index] * band[index]
                    } else {
                        band[len + index] * band[len + index]
                            + band[2 * len + index] * band[2 * len + index]
                    };
                    if squared != 0.0 {
                        let factor = (1.0 - effective * effective / squared).max(0.0);
                        let d = band[c * len + index];
                        *value += d * factor - d;
                    }
                }
            },
        )?;
    }
    Ok(())
}

fn denoise(
    unit: &Denoise,
    input: &Planes<'_>,
    output: &mut PlanesMut<'_>,
    scratch: &mut [f32],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    let ((levels, thresholds_by_level), details) = (unit.levels(), unit.details());
    cancel.check()?;
    let stage = input.stage();
    let out = output.region();
    if out.is_empty() {
        return Ok(());
    }
    let held = out.grown(unit.halo(stage), stage);
    let geometry = Geometry { stage, held };
    let len = held.pixels() as usize;
    let mut scratch = scratch;
    let lab = filters::take(&mut scratch, 3 * len)?;
    let coarse = filters::take(&mut scratch, 3 * len)?;
    let next = filters::take(&mut scratch, 3 * len)?;
    let temporary = filters::take(&mut scratch, len)?;
    let delta = filters::take(&mut scratch, 3 * len)?;
    filters::lab(input, lab, geometry, parallelism, cancel)?;
    coarse.copy_from_slice(lab);
    delta.fill(0.0);
    let mut valid = held;
    for (kernels, thresholds) in levels.iter().zip(thresholds_by_level) {
        cancel.check()?;
        let next_valid = valid.shrunk(kernels[0].radius.max(kernels[1].radius), stage);
        for c in 0..3 {
            smooth(
                &coarse[c * len..(c + 1) * len],
                &mut next[c * len..(c + 1) * len],
                temporary,
                geometry,
                next_valid,
                kernels,
                parallelism,
                cancel,
            )?;
        }
        for c in 0..3 {
            filters::rows(
                &mut coarse[c * len..(c + 1) * len],
                geometry,
                next_valid,
                parallelism,
                cancel,
                |y, row| {
                    for (column, value) in row.iter_mut().enumerate() {
                        let x = next_valid.x0 + column as u32;
                        *value -= next[c * len + geometry.index(i64::from(x), i64::from(y))];
                    }
                },
            )?;
        }
        shrink(
            coarse,
            delta,
            geometry,
            out,
            *thresholds,
            details,
            parallelism,
            cancel,
        )?;
        for c in 0..3 {
            filters::rows(
                &mut coarse[c * len..(c + 1) * len],
                geometry,
                next_valid,
                parallelism,
                cancel,
                |y, row| {
                    for (column, value) in row.iter_mut().enumerate() {
                        *value = next[c * len
                            + geometry
                                .index(i64::from(next_valid.x0) + column as i64, i64::from(y))];
                    }
                },
            )?;
        }
        valid = next_valid;
    }
    output.for_rows(parallelism, |y, red, green, blue| {
        if cancel.is_cancelled() {
            return;
        }
        for column in 0..out.width as usize {
            let x = i64::from(out.x0) + column as i64;
            let y = i64::from(y);
            let i = geometry.index(x, y);
            let value = filters::reconstruct(
                input.sample(x, y),
                [lab[i], lab[len + i], lab[2 * len + i]],
                [delta[i], delta[len + i], delta[2 * len + i]],
            );
            [red[column], green[column], blue[column]] = value;
        }
    });
    cancel.check()
}

fn sharpen(
    unit: &Sharpen,
    input: &Planes<'_>,
    output: &mut PlanesMut<'_>,
    scratch: &mut [f32],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    let (kernels, guide_kernels) = unit.kernels();
    cancel.check()?;
    let stage = input.stage();
    let out = output.region();
    if out.is_empty() {
        return Ok(());
    }
    let held = out.grown(unit.halo(stage), stage);
    let geometry = Geometry { stage, held };
    let len = held.pixels() as usize;
    let mut scratch = scratch;
    let lab = filters::take(&mut scratch, 3 * len)?;
    let temporary = filters::take(&mut scratch, len)?;
    let blurred = filters::take(&mut scratch, len)?;
    let guide = filters::take(&mut scratch, len)?;
    filters::lab(input, lab, geometry, parallelism, cancel)?;
    let l = &lab[..len];
    smooth(
        l,
        blurred,
        temporary,
        geometry,
        out,
        kernels,
        parallelism,
        cancel,
    )?;
    smooth(
        l,
        guide,
        temporary,
        geometry,
        out.grown(1, stage),
        guide_kernels,
        parallelism,
        cancel,
    )?;
    output.for_rows(parallelism, |y, red, green, blue| {
        if cancel.is_cancelled() {
            return;
        }
        for column in 0..out.width as usize {
            let x = i64::from(out.x0) + column as i64;
            let y = i64::from(y);
            let i = geometry.index(x, y);
            let limited = unit.limited(l, blurred, guide, geometry, x, y);
            let value = filters::reconstruct(
                input.sample(x, y),
                [l[i], lab[len + i], lab[2 * len + i]],
                [limited - l[i], 0.0, 0.0],
            );
            [red[column], green[column], blue[column]] = value;
        }
    });
    cancel.check()
}

const PARALLELISMS: [Parallelism; 2] = [Parallelism::Serial, Parallelism::Pool];

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|v| v.to_bits()).collect()
}

/// A value of a detail band or a change: mostly uniform in `±spread`, with exact and signed zeros,
/// values whose squares underflow to zero and large values among them.
fn value(rng: &mut SplitMix64, spread: f64) -> f32 {
    match rng.next_u32(16) {
        0 => [0.0, -0.0, 1e-23, -3e-23, 2e-20, -4.5, 7.0][rng.next_usize(7)],
        _ => rng.next_range(-spread, spread) as f32,
    }
}

fn plane(rng: &mut SplitMix64, len: usize, spread: f64) -> Vec<f32> {
    (0..len).map(|_| value(rng, spread)).collect()
}

/// A linear RGB frame of 4 x 4 blocks, each flat, noisy, a neutral ramp, saturated, extended or
/// holding a single hot pixel, so the units meet zero bands, the neutral snap and the garrote.
fn frame(rng: &mut SplitMix64, stage: Stage) -> Vec<f32> {
    let blocks = (stage.width.div_ceil(4) * stage.height.div_ceil(4)) as usize;
    let kinds: Vec<(u32, [f32; 3])> = (0..blocks)
        .map(|_| {
            let base = [0.0; 3].map(|_: f32| rng.next_range(0.01, 0.6) as f32);
            (rng.next_u32(6), base)
        })
        .collect();
    let mut pixels = Vec::with_capacity(Region::whole(stage).pixels() as usize);
    for y in 0..stage.height {
        for x in 0..stage.width {
            let (kind, base) = kinds[((y / 4) * stage.width.div_ceil(4) + x / 4) as usize];
            pixels.push(match kind {
                0 => base,
                1 => base.map(|v| v + rng.next_range(-0.03, 0.03) as f32),
                2 => [0.02 + 0.01 * x as f32 + rng.next_range(-0.002, 0.002) as f32; 3],
                3 => [base[0] + 0.3, 0.02, base[2] * 0.1],
                4 => [-0.15 + 0.07 * (x % 5) as f32, 1.3, base[2]],
                _ if (x + y) % 5 == 0 => [0.9, 0.85, 0.95],
                _ => base,
            });
        }
    }
    (0..3)
        .flat_map(|c| pixels.iter().map(move |p| p[c]))
        .collect()
}

/// `values`, planar over the whole stage, cut to `region`.
fn cut(values: &[f32], stage: Stage, region: Region, planes: usize) -> Vec<f32> {
    let len = Region::whole(stage).pixels() as usize;
    (0..planes)
        .flat_map(|c| {
            (region.y0..region.y1()).flat_map(move |y| {
                (region.x0..region.x1()).map(move |x| c * len + (y * stage.width + x) as usize)
            })
        })
        .map(|i| values[i])
        .collect()
}

/// Output rectangles of `stage`: the whole stage, each corner, a stage-edge strip, a single pixel
/// and random rectangles.
fn regions(rng: &mut SplitMix64, stage: Stage) -> Vec<Region> {
    let (w, h) = (stage.width, stage.height);
    let mut regions = vec![
        Region::whole(stage),
        Region {
            x0: 0,
            y0: 0,
            width: w.min(7),
            height: h.min(5),
        },
        Region {
            x0: w - w.min(6),
            y0: h - h.min(4),
            width: w.min(6),
            height: h.min(4),
        },
        Region {
            x0: w / 3,
            y0: 0,
            width: (w / 3).max(1),
            height: h,
        },
        Region {
            x0: w / 2,
            y0: h / 2,
            width: 1,
            height: 1,
        },
    ];
    for _ in 0..3 {
        let x0 = rng.next_u32(w);
        let y0 = rng.next_u32(h);
        regions.push(Region {
            x0,
            y0,
            width: 1 + rng.next_u32(w - x0),
            height: 1 + rng.next_u32(h - y0),
        });
    }
    regions
}

/// Every kernel the units smooth with: B3 at spacings 1 to 8, and sampled Gaussians from below a
/// pixel to the widest a proxy takes, including sigma 0.
fn kernels() -> Vec<Kernel> {
    (1..=8)
        .map(Kernel::b3)
        .chain(
            [
                0.0, 0.2, 0.37, 0.51, 0.999, 1.0, 1.53, 2.04, 3.0, 4.08, 7.992,
            ]
            .map(Kernel::gaussian),
        )
        .collect()
}

#[test]
fn detail_smoothing_matches_the_tap_by_tap_reference_bit_for_bit() {
    let mut rng = SplitMix64(0xDE7A_115E);
    let kernels = kernels();
    for (width, height) in [
        (1, 1),
        (1, 9),
        (9, 1),
        (2, 3),
        (5, 4),
        (17, 9),
        (40, 33),
        (97, 61),
    ] {
        let stage = Stage { width, height };
        for out in regions(&mut rng, stage) {
            for _ in 0..4 {
                let pair = [0; 2].map(|_| kernels[rng.next_usize(kernels.len())].clone());
                let reach = pair[0].radius.max(pair[1].radius) + rng.next_u32(3);
                let held = out.grown(reach, stage);
                let geometry = Geometry { stage, held };
                let len = held.pixels() as usize;
                let src = plane(&mut rng, len, 0.5);
                let before = plane(&mut rng, len, 1.0);
                let temporary = plane(&mut rng, len, 1.0);
                for parallelism in PARALLELISMS {
                    let mut expected = before.clone();
                    smooth(
                        &src,
                        &mut expected,
                        &mut temporary.clone(),
                        geometry,
                        out,
                        &pair,
                        parallelism,
                        &Cancel::never(),
                    )
                    .unwrap();
                    let mut actual = before.clone();
                    filters::smooth(
                        &src,
                        &mut actual,
                        &mut temporary.clone(),
                        geometry,
                        out,
                        &pair,
                        parallelism,
                        &Cancel::never(),
                    )
                    .unwrap();
                    assert_eq!(
                        bits(&actual),
                        bits(&expected),
                        "{stage:?} {out:?} held {held:?} radii {} {} {parallelism:?}",
                        pair[0].radius,
                        pair[1].radius
                    );
                }
            }
        }
    }
}

#[test]
fn detail_shrinkage_matches_the_per_channel_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x5A41_4E4B);
    for (width, height) in [(1, 1), (2, 1), (1, 3), (5, 4), (17, 9), (40, 33)] {
        let stage = Stage { width, height };
        for out in regions(&mut rng, stage) {
            let held = out.grown(1 + rng.next_u32(3), stage);
            let geometry = Geometry { stage, held };
            let len = held.pixels() as usize;
            let spread = [0.002, 0.05, 0.4][rng.next_usize(3)];
            let band = plane(&mut rng, 3 * len, spread);
            let before = plane(&mut rng, 3 * len, 0.01);
            let threshold = [0.0891, 0.0199, 1.3e-3][rng.next_usize(3)];
            for thresholds in [
                [threshold, 0.0],
                [0.0, threshold],
                [threshold, 0.7 * threshold],
                [0.0, 0.0],
            ] {
                for details in [[0.0, 0.0], [0.5, 0.2], [1.0, 1.0]] {
                    for parallelism in PARALLELISMS {
                        let mut expected = before.clone();
                        shrink(
                            &band,
                            &mut expected,
                            geometry,
                            out,
                            thresholds,
                            details,
                            parallelism,
                            &Cancel::never(),
                        )
                        .unwrap();
                        let mut actual = before.clone();
                        denoise::shrink(
                            &band,
                            &mut actual,
                            geometry,
                            out,
                            thresholds,
                            details,
                            parallelism,
                            &Cancel::never(),
                        )
                        .unwrap();
                        assert_eq!(
                            bits(&actual),
                            bits(&expected),
                            "{stage:?} {out:?} held {held:?} {thresholds:?} {details:?} {parallelism:?}"
                        );
                    }
                }
            }
        }
    }
}

enum Unit {
    Denoise(Denoise),
    Sharpen(Sharpen),
}

impl Unit {
    fn production(&self) -> &dyn SpatialUnit {
        match self {
            Self::Denoise(unit) => unit,
            Self::Sharpen(unit) => unit,
        }
    }

    /// The frozen reference's output, serially: its serial and pooled outputs are equal, as
    /// `detail_serial_and_pool_and_region_are_exact` held it to while it was the production code.
    fn reference(&self, input: &Planes<'_>, output: &mut PlanesMut<'_>, scratch: &mut [f32]) {
        let (serial, never) = (Parallelism::Serial, &Cancel::never());
        match self {
            Self::Denoise(unit) => denoise(unit, input, output, scratch, serial, never),
            Self::Sharpen(unit) => sharpen(unit, input, output, scratch, serial, never),
        }
        .unwrap();
    }
}

/// Each unit the module compiles at `scale`: noise reduction with Luminance alone, Colour alone,
/// both, both at their extremes and with a strength so small its thresholds underflow, in the
/// CPU's shape and the GPU's every-level shape, and sharpening at Radius 1, where its blur is its
/// guide, and at other radii, with and without Masking.
fn units(scale: SamplingScale) -> Vec<(String, Unit)> {
    let denoise = [
        ("luminance", [40.0, 50.0, 0.0, 50.0]),
        ("colour", [0.0, 50.0, 40.0, 50.0]),
        ("both", [40.0, 50.0, 40.0, 50.0]),
        ("extremes", [100.0, 0.0, 100.0, 100.0]),
        ("luminance-underflowing-colour", [25.0, 75.0, 1e-30, 30.0]),
        ("underflowing-luminance", [1e-30, 50.0, 0.0, 50.0]),
    ];
    let sharpen = [
        ("radius-1", [40.0, 1.0, 25.0, 0.0]),
        ("radius-1-masked", [150.0, 1.0, 0.0, 60.0]),
        ("radius-0.5", [60.0, 0.5, 25.0, 0.0]),
        ("radius-2.3-masked", [80.0, 2.3, 50.0, 40.0]),
        ("radius-3", [150.0, 3.0, 100.0, 100.0]),
    ];
    let mut units = Vec::new();
    for (name, [l, ld, c, cd]) in denoise {
        units.push((
            format!("denoise {name}"),
            Unit::Denoise(Denoise::new(l, ld, c, cd, scale)),
        ));
        units.push((
            format!("denoise {name}, every level"),
            Unit::Denoise(Denoise::every_level(l, ld, c, cd, scale)),
        ));
    }
    for (name, [amount, radius, detail, masking]) in sharpen {
        units.push((
            format!("sharpen {name}"),
            Unit::Sharpen(Sharpen::new(amount, radius, detail, masking, scale)),
        ));
    }
    units
}

#[test]
fn detail_units_match_the_reference_bit_for_bit() {
    let mut rng = SplitMix64(0x0DE7_A11C);
    let scales = [
        [1.0, 1.0],
        [0.51, 0.37],
        [0.37, 0.51],
        [0.25, 0.25],
        [0.999, 0.999],
    ];
    // Tiny and odd stages, stages narrower than every kernel, and, at full resolution and one
    // sampled scale, one wide enough for tiles whose halo reaches no stage edge.
    let stages = [(1, 1), (3, 2), (5, 31), (31, 5), (23, 17), (150, 110)];
    for [sx, sy] in scales {
        for (name, unit) in units(SamplingScale { x: sx, y: sy }) {
            let production = unit.production();
            for (width, height) in stages {
                if width > 100 && ![[1.0, 1.0], [0.51, 0.37]].contains(&[sx, sy]) {
                    continue;
                }
                let stage = Stage { width, height };
                let input = frame(&mut rng, stage);
                // The reference over the whole stage, once: its cuts equal its whole frame, as
                // `detail_serial_and_pool_and_region_are_exact` held it to while it was the
                // production code, so each cut of the unit below is held to the reference and to
                // the whole frame at once.
                let whole = Region::whole(stage);
                let mut reference = vec![f32::NAN; input.len()];
                unit.reference(
                    &Planes::new(stage, whole, &input).unwrap(),
                    &mut PlanesMut::new(stage, whole, &mut reference).unwrap(),
                    &mut vec![f32::NAN; (production.scratch_bytes(stage) / 4) as usize],
                );
                let halo = production.halo(stage);
                let outs = if width > 100 {
                    // Tiles of a large stage only, one of them clear of every edge.
                    let middle = Region {
                        x0: halo + 1,
                        y0: halo + 1,
                        width: width - 2 * halo - 2,
                        height: (height - 2 * halo - 2).min(9),
                    };
                    vec![
                        middle,
                        Region {
                            x0: 0,
                            y0: height - 7,
                            width: 13,
                            height: 7,
                        },
                    ]
                } else {
                    regions(&mut rng, stage)
                };
                for out in outs {
                    let held = out.grown(halo, stage);
                    let tile = cut(&input, stage, held, 3);
                    let size = Stage {
                        width: held.width,
                        height: held.height,
                    };
                    let values = (production.scratch_bytes(size) / 4) as usize;
                    let input = Planes::new(stage, held, &tile).unwrap();
                    let expected = cut(&reference, stage, out, 3);
                    // Scratch holds whatever the last tile left, so the unit must read none of it
                    // before writing it: NaN and arbitrary values give the same output. The pool
                    // takes the whole stage and the large stage's tiles, where it has rows to share.
                    let mut runs = vec![
                        (Parallelism::Serial, vec![f32::NAN; values]),
                        (Parallelism::Serial, plane(&mut rng, values, 9.0)),
                    ];
                    if out == whole || width > 100 {
                        runs.push((Parallelism::Pool, vec![f32::NAN; values]));
                    }
                    for (parallelism, mut scratch) in runs {
                        let mut actual = vec![f32::NAN; 3 * out.pixels() as usize];
                        production
                            .apply(
                                &input,
                                &mut PlanesMut::new(stage, out, &mut actual).unwrap(),
                                None,
                                &mut scratch,
                                parallelism,
                            )
                            .unwrap();
                        assert_eq!(
                            bits(&actual),
                            bits(&expected),
                            "{name} at scale {sx} x {sy}, {stage:?} {out:?} {parallelism:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn detail_sharpening_smooths_once_at_radius_one() {
    for [x, y] in [[1.0, 1.0], [0.51, 0.37], [0.25, 0.25], [0.999, 0.999]] {
        let scale = SamplingScale { x, y };
        for (radius, shared) in [(1.0, true), (0.5, false), (1.1, false), (2.3, false)] {
            let unit = Sharpen::new(40.0, radius, 25.0, 0.0, scale);
            let (blur, guide) = unit.kernels();
            assert_eq!(
                blur[0].same(&guide[0]) && blur[1].same(&guide[1]),
                shared,
                "radius {radius} at scale {x} x {y}"
            );
        }
    }
}
