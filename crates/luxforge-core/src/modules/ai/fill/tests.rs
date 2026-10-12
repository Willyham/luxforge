//! The fill pipeline's stages against `f64` references and synthetic fixtures.
//!
//! Each reference evaluates one output value directly from its definition over the whole input
//! held in memory, with no bands, windows or shared intermediate rows, and rounds to `f32` at the
//! points the pipeline stores its values; the pipeline's streamed, windowed evaluation must equal
//! it bit for bit.
use super::{
    Context, Coverage, FillPlan, Planes,
    blend::{self, BINOMIAL, down, push_pull, up, weight_pyramid},
    crop::{self, plan_crop, reflect},
    encode::{Encoding, MAX_SCALE},
    grain::{self, dilate, grain_field, grain_highpass_gain, grain_kernel, ring_grain},
    resample::{self, Taps},
};
use crate::{
    Error,
    modules::{Region, Stage, ai::selection::Selection},
};
use luxforge_reference::srgb;
use luxforge_test_macros::slow;

fn rect(x0: u32, y0: u32, width: u32, height: u32) -> Region {
    Region {
        x0,
        y0,
        width,
        height,
    }
}

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

/// SplitMix64.
fn mix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn uniform(state: &mut u64) -> f32 {
    (mix(state) >> 40) as f32 / (1_u64 << 24) as f32
}

/// A linear image of `stage` pixels: a smooth gradient with noise, some values past white.
fn linear_image(stage: Stage, seed: u64) -> Vec<[f32; 3]> {
    let mut state = seed;
    (0..stage.height)
        .flat_map(|y| (0..stage.width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let base =
                0.1 + 0.8 * x as f32 / stage.width as f32 + 0.3 * y as f32 / stage.height as f32;
            [0, 1, 2].map(|c| base * (0.7 + 0.15 * c as f32) + 0.05 * uniform(&mut state))
        })
        .collect()
}

/// A reader over an image in memory, as the stage reader answers: row-major over the region.
fn reader(
    image: &[[f32; 3]],
    stage: Stage,
    reads: &mut Vec<Region>,
) -> impl FnMut(Region) -> Result<Vec<[f32; 3]>, Error> {
    move |region: Region| {
        assert!(
            region.x1() <= stage.width && region.y1() <= stage.height,
            "{region:?}"
        );
        reads.push(region);
        let mut out = Vec::with_capacity(region.pixels() as usize);
        for y in region.y0..region.y1() {
            for x in region.x0..region.x1() {
                out.push(image[(y * stage.width + x) as usize]);
            }
        }
        Ok(out)
    }
}

/// A soft rectangle of coverage: 1 inside, falling to 0 over `soft` pixels outside.
fn soft_rect(stage: Stage, r: Region, soft: u32) -> Coverage {
    let outer = r.grown(soft, stage);
    let mut values = Vec::with_capacity(outer.pixels() as usize);
    for y in outer.y0..outer.y1() {
        for x in outer.x0..outer.x1() {
            let dx = (r.x0 as i64 - x as i64)
                .max(x as i64 - r.x1() as i64 + 1)
                .max(0);
            let dy = (r.y0 as i64 - y as i64)
                .max(y as i64 - r.y1() as i64 + 1)
                .max(0);
            let d = dx.max(dy) as f32;
            values.push(if soft == 0 {
                f32::from(d == 0.0)
            } else {
                (1.0 - d / (soft as f32 + 1.0)).max(0.0)
            });
        }
    }
    Coverage::new(outer, values).unwrap().tightened().unwrap()
}

fn ok() -> Result<(), Error> {
    Ok(())
}

#[test]
fn the_crop_is_a_centred_square_with_one_region_length_of_context() {
    // Context equal to the longer side.
    let c = plan_crop(rect(1000, 1000, 100, 50), stage(4000, 3000));
    assert_eq!((c.context, c.side), (100, 300));
    assert_eq!(c.origin, (900, 875));
    assert_eq!(c.inside, rect(900, 875, 300, 300));
    // A small region takes the 48-pixel minimum.
    let c = plan_crop(rect(1000, 1000, 20, 10), stage(4000, 3000));
    assert_eq!((c.context, c.side), (48, 116));
    assert_eq!(c.inside, rect(952, 947, 116, 116));
    // Near an edge the square slides inside the stage rather than shrinking.
    let c = plan_crop(rect(10, 500, 100, 100), stage(4000, 3000));
    assert_eq!(c.origin, (0, 400));
    assert_eq!(c.inside, rect(0, 400, 300, 300));
    // Larger than the stage: padded by reflection, centred on the region.
    let c = plan_crop(rect(100, 50, 400, 300), stage(600, 400));
    assert_eq!((c.context, c.side), (400, 1200));
    assert_eq!(c.origin, (-300, -400));
    assert_eq!(c.inside, rect(0, 0, 600, 400));
    // Larger on one axis only: square on the other, padded on this one.
    let c = plan_crop(rect(1000, 20, 300, 60), stage(4000, 200));
    assert_eq!(c.side, 900);
    assert_eq!(c.inside, rect(700, 0, 900, 200));
    // Every case contains the region and the square's inside lies in the stage.
    for (b, s) in [
        (rect(0, 0, 1, 1), stage(10, 10)),
        (rect(9, 9, 1, 1), stage(10, 10)),
        (rect(0, 0, 10, 10), stage(10, 10)),
        (rect(3, 0, 5, 700), stage(20, 700)),
    ] {
        let c = plan_crop(b, s);
        assert!(c.origin.0 <= i64::from(b.x0) && c.origin.1 <= i64::from(b.y0));
        assert!(c.origin.0 + i64::from(c.side) >= i64::from(b.x1()));
        assert!(c.origin.1 + i64::from(c.side) >= i64::from(b.y1()));
        assert!(c.inside.x1() <= s.width && c.inside.y1() <= s.height);
    }
}

#[test]
fn the_plan_keeps_every_window_inside_the_square() {
    for (b, s) in [
        (rect(1000, 1000, 100, 50), stage(4000, 3000)),
        (rect(10, 10, 600, 400), stage(1000, 800)),
        (rect(0, 0, 30, 30), stage(64, 48)),
        (rect(1500, 600, 2800, 2800), stage(6000, 4000)),
    ] {
        let plan = FillPlan::new(b, s, 512);
        let inside = plan.crop.inside;
        for r in [plan.window, plan.area] {
            assert!(r.x0 >= inside.x0 && r.y0 >= inside.y0, "{r:?} {inside:?}");
            assert!(
                r.x1() <= inside.x1() && r.y1() <= inside.y1(),
                "{r:?} {inside:?}"
            );
            assert!(r.x0 <= b.x0 && r.y0 <= b.y0 && r.x1() >= b.x1() && r.y1() >= b.y1());
        }
        assert!((3..=7).contains(&plan.levels));
    }
}

#[test]
fn reflection_repeats_past_the_edges() {
    let r: Vec<u32> = (-5..9).map(|i| reflect(i, 4)).collect();
    assert_eq!(r, [1, 2, 3, 2, 1, 0, 1, 2, 3, 2, 1, 0, 1, 2]);
    assert_eq!(reflect(-7, 1), 0);
    assert_eq!(reflect(12, 1), 0);
}

#[test]
fn dilation_matches_brute_force() {
    let (w, h) = (37_usize, 23_usize);
    let mut state = 5_u64;
    let mask: Vec<bool> = (0..w * h)
        .map(|_| mix(&mut state).is_multiple_of(17))
        .collect();
    for r in [0_usize, 1, 3, 10, 40] {
        let fast = dilate(&mask, w, h, r);
        for y in 0..h {
            for x in 0..w {
                let want = (y.saturating_sub(r)..(y + r + 1).min(h)).any(|yy| {
                    (x.saturating_sub(r)..(x + r + 1).min(w)).any(|xx| mask[yy * w + xx])
                });
                assert_eq!(fast[y * w + x], want, "r {r} at ({x}, {y})");
            }
        }
    }
}

#[test]
fn the_encoding_is_the_reference_transfer_function_and_its_inverse() {
    for scale in [1.0, 2.5, MAX_SCALE] {
        let encoding = Encoding { scale };
        for i in 0..=4000 {
            let linear = (i as f64 / 4000.0 * scale * 1.1 - 0.05 * scale) as f32;
            let code = encoding.code(linear);
            let want =
                (255.0 * srgb::encode_extended((f64::from(linear) / scale).clamp(0.0, 1.0))) as f32;
            assert_eq!(code.to_bits(), want.to_bits(), "k {scale} v {linear}");
            let back = encoding.linear(code);
            let want =
                (scale * srgb::decode_encoded((f64::from(code) / 255.0).clamp(0.0, 1.0))) as f32;
            assert_eq!(back.to_bits(), want.to_bits(), "k {scale} code {code}");
            // The inverse within the encoded range.
            if (0.0..=scale as f32).contains(&linear) {
                let tolerance = 2e-6 * scale as f32;
                assert!(
                    (back - linear).abs() <= tolerance,
                    "k {scale}: {linear} came back {back}"
                );
            }
        }
        // Codes outside 0..=255 decode to the range's ends.
        assert_eq!(encoding.linear(-3.0), 0.0);
        assert_eq!(encoding.linear(300.0), scale as f32);
    }
}

#[test]
fn the_encoding_range_is_the_brightest_channels_99_9th_percentile() {
    let flat = |count: usize, value: f32| std::iter::repeat_n(value, count);
    // Within white, or a few hot pixels past it: white.
    assert_eq!(Encoding::measure(flat(10_000, 0.8)).scale, 1.0);
    assert_eq!(
        Encoding::measure(flat(10_000, 0.8).chain(flat(10, 40.0))).scale,
        1.0
    );
    // One percent at 4: the range reaches 4, the edge of its bin.
    let k = Encoding::measure(flat(9_900, 0.8).chain(flat(100, 4.0))).scale;
    assert_eq!(k, 4.0);
    // Just past an edge: the next edge up, 2^(1/32) further.
    let k = Encoding::measure(flat(9_900, 0.8).chain(flat(100, 4.01))).scale;
    assert_eq!(k, 2_f64.powf(65.0 / 32.0));
    // Past the largest range: the largest.
    assert_eq!(
        Encoding::measure(flat(9_000, 0.8).chain(flat(1_000, 500.0))).scale,
        MAX_SCALE
    );
    // Nothing measured, and values that are not numbers, leave it at white.
    assert_eq!(Encoding::measure(std::iter::empty()).scale, 1.0);
    assert_eq!(Encoding::measure(flat(100, f32::NAN)).scale, 1.0);
}

#[test]
fn resampling_taps_sum_to_one_and_cover_each_footprint() {
    for (src, dst) in [(1200, 512), (513, 512), (97, 512), (4096, 512), (512, 512)] {
        let taps = Taps::new(src, dst, 0..dst);
        for i in 0..dst {
            let sum: f64 = taps.of(i).iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "{src}->{dst} at {i}: {sum}");
        }
        if src > dst {
            let f = src as f64 / dst as f64;
            for i in 0..dst {
                // The area taps start at the footprint's first source pixel.
                assert_eq!(taps.first[i], (i as f64 * f).floor() as i64);
            }
        }
    }
}

/// The reference reduction: each model pixel evaluated directly from the whole image, its
/// horizontal sums rounded to `f32` as the pipeline stores them.
fn reference_reduce(
    image: &[[f32; 3]],
    s: Stage,
    coverage: &Coverage,
    plan: &crop::CropPlan,
    n: usize,
) -> (Vec<[f32; 3]>, Vec<u8>) {
    let taps = Taps::new(plan.side as usize, n, 0..n);
    let mut pixels = Vec::with_capacity(n * n);
    let mut hole = Vec::with_capacity(n * n);
    for jy in 0..n {
        for i in 0..n {
            let mut sum = [0_f64; 3];
            let mut covered = 0_u8;
            for (j, &wy) in taps.of(jy).iter().enumerate() {
                if wy == 0.0 {
                    continue;
                }
                let sy = reflect(plan.origin.1 + taps.first[jy] + j as i64, s.height);
                let mut across = [0_f64; 3];
                for (k, &wx) in taps.of(i).iter().enumerate() {
                    if wx == 0.0 {
                        continue;
                    }
                    let sx = reflect(plan.origin.0 + taps.first[i] + k as i64, s.width);
                    let p = image[(sy * s.width + sx) as usize];
                    for c in 0..3 {
                        across[c] += wx * f64::from(p[c]);
                    }
                    if coverage.at(sx, sy) > 0.0 {
                        covered = 1;
                    }
                }
                for c in 0..3 {
                    sum[c] += wy * f64::from(across[c] as f32);
                }
            }
            pixels.push(sum.map(|v| v as f32));
            hole.push(covered);
        }
    }
    (pixels, hole)
}

#[test]
fn the_reduction_equals_its_reference_whatever_bands_it_reads() {
    let cases = [
        // Shrinking inside the stage, and padded past two edges.
        (stage(400, 300), rect(150, 120, 60, 40), 32),
        (stage(160, 90), rect(20, 30, 70, 50), 24),
        // Enlarging: the square is smaller than the model.
        (stage(200, 150), rect(90, 60, 8, 6), 160),
    ];
    for (s, r, n) in cases {
        let image = linear_image(s, 3);
        let coverage = soft_rect(s, r, 3);
        let plan = plan_crop(coverage.bounds(), s);
        let want = reference_reduce(&image, s, &coverage, &plan, n);
        for tile in [1_u64, 997, 1 << 22] {
            let mut reads = Vec::new();
            let mut kept = 0_u64;
            let got = resample::reduce(
                &plan,
                s,
                n,
                &coverage,
                tile,
                &mut reader(&image, s, &mut reads),
                &mut |band, pixels| {
                    assert_eq!(pixels.len() as u64, band.pixels());
                    kept += band.pixels();
                },
                &ok,
            )
            .unwrap();
            // Everything the square holds is read once, and at most the filter's reach more.
            let span = reads.iter().map(|read| read.height).sum::<u32>();
            assert_eq!(kept, u64::from(span) * u64::from(reads[0].width));
            assert!(kept >= plan.inside.pixels(), "{s:?} {r:?} tile {tile}");
            assert!(reads[0].width <= plan.inside.width + 4 && span <= plan.inside.height + 4);
            for (a, b) in got.pixels.iter().zip(&want.0) {
                assert_eq!(
                    a.map(f32::to_bits),
                    b.map(f32::to_bits),
                    "{s:?} {r:?} tile {tile}"
                );
            }
            assert_eq!(got.hole, want.1, "{s:?} {r:?} tile {tile}");
            // Each read is a band of whole rows, one after another, within the tile.
            for (read, next) in reads.iter().zip(reads.iter().skip(1)) {
                assert_eq!((read.x0, read.width), (next.x0, next.width));
                assert_eq!(read.y1(), next.y0);
            }
            for read in &reads {
                assert!(read.pixels() <= tile.max(u64::from(read.width)));
            }
        }
    }
}

#[test]
fn the_model_hole_holds_every_covered_pixel_and_no_more_than_its_reach() {
    let s = stage(1400, 900);
    let image = linear_image(s, 6);
    // A thin diagonal line: area resampling must not skip it.
    let line = rect(500, 340, 300, 160);
    let mut values = Vec::new();
    for y in line.y0..line.y1() {
        for x in line.x0..line.x1() {
            let d = (x as f32 * 0.5 - y as f32 + 100.0).abs();
            values.push(if d < 0.6 { 1.0 } else { 0.0 });
        }
    }
    let coverage = Coverage::new(line, values).unwrap().tightened().unwrap();
    let n = 512;
    let mut reads = Vec::new();
    let context = Context::prepare(
        coverage.clone(),
        s,
        n,
        1 << 22,
        &mut reader(&image, s, &mut reads),
        &ok,
    )
    .unwrap();
    let plan = context.plan.crop;
    let scale = f64::from(plan.side) / n as f64;
    let mut touched = vec![false; n * n];
    for y in 0..s.height {
        for x in 0..s.width {
            if coverage.at(x, y) == 0.0 {
                continue;
            }
            let mx = ((i64::from(x) - plan.origin.0) as f64 / scale) as usize;
            let my = ((i64::from(y) - plan.origin.1) as f64 / scale) as usize;
            touched[my * n + mx] = true;
            assert_eq!(context.model_mask[my * n + mx], 1.0, "({x}, {y})");
        }
    }
    let near = dilate(&touched, n, n, 1);
    for (i, &m) in context.model_mask.iter().enumerate() {
        assert!(
            m == 0.0 || near[i],
            "model pixel {i} is in the hole, far from the line"
        );
    }
    assert!(context.model_image.iter().all(|v| (0.0..=1.0).contains(v)));
    // The input keeps what the region covers: the generator is told the hole by the mask.
    assert!(
        context
            .model_mask
            .iter()
            .enumerate()
            .filter(|(_, m)| **m > 0.0)
            .any(|(i, _)| context.model_image[i * 3..i * 3 + 3] != [0.0; 3])
    );
}

/// The reference enlargement: each window pixel directly from the model plane.
fn reference_upscale(src: &[f32], n: usize, plan: &crop::CropPlan, window: Region) -> Vec<f32> {
    let s = plan.side as usize;
    let cx0 = (i64::from(window.x0) - plan.origin.0) as usize;
    let cy0 = (i64::from(window.y0) - plan.origin.1) as usize;
    let tx = Taps::new(n, s, cx0..cx0 + window.width as usize);
    let ty = Taps::new(n, s, cy0..cy0 + window.height as usize);
    let clamp = |j: i64| j.clamp(0, n as i64 - 1) as usize;
    let mut out = Vec::new();
    for y in 0..window.height as usize {
        for x in 0..window.width as usize {
            let mut sum = 0_f64;
            for (j, &wy) in ty.of(y).iter().enumerate() {
                if wy == 0.0 {
                    continue;
                }
                let m = clamp(ty.first[y] + j as i64);
                let mut across = 0_f64;
                for (k, &wx) in tx.of(x).iter().enumerate() {
                    across += wx * f64::from(src[m * n + clamp(tx.first[x] + k as i64)]);
                }
                sum += wy * across;
            }
            out.push(sum as f32);
        }
    }
    out
}

#[test]
fn the_enlargement_equals_its_reference() {
    let n = 64;
    let mut state = 11;
    let src: Vec<f32> = (0..n * n).map(|_| 255.0 * uniform(&mut state)).collect();
    for (b, s) in [
        (rect(300, 200, 120, 90), stage(1000, 800)),
        (rect(5, 5, 40, 30), stage(80, 60)),
    ] {
        let plan = FillPlan::new(b, s, n);
        let got = resample::upscale_plane(&src, n, &plan.crop, plan.window);
        let want = reference_upscale(&src, n, &plan.crop, plan.window);
        assert_eq!(got.len(), want.len());
        for (i, (a, b)) in got.iter().zip(&want).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "{i}");
        }
    }
}

/// The reference blur-and-decimate: each coarse value directly, its horizontal sums rounded to
/// `f32` as the pipeline stores them.
fn reference_down(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = Vec::new();
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = 0_f64;
            for (j, bj) in BINOMIAL.iter().enumerate() {
                let sy = reflect(2 * y as i64 + j as i64 - 2, h as u32) as usize;
                let mut across = 0_f64;
                for (i, bi) in BINOMIAL.iter().enumerate() {
                    let sx = reflect(2 * x as i64 + i as i64 - 2, w as u32) as usize;
                    across += bi * f64::from(src[sy * w + sx]);
                }
                sum += bj * f64::from(across as f32);
            }
            out.push(sum as f32);
        }
    }
    out
}

/// The reference expand, from Burt and Adelson's weights.
fn reference_up(src: &[f32], w: usize, h: usize, tw: usize, th: usize) -> Vec<f32> {
    let taps = |i: usize, len: usize| -> Vec<(usize, f64)> {
        let c = |j: i64| j.clamp(0, len as i64 - 1) as usize;
        let k = (i / 2) as i64;
        if i.is_multiple_of(2) {
            vec![(c(k - 1), 0.125), (c(k), 0.75), (c(k + 1), 0.125)]
        } else {
            vec![(c(k), 0.5), (c(k + 1), 0.5), (c(k), 0.0)]
        }
    };
    let mut out = Vec::new();
    for y in 0..th {
        for x in 0..tw {
            let mut sum = 0_f64;
            for (row, wy) in taps(y, h) {
                let mut across = 0_f64;
                for (column, wx) in taps(x, w) {
                    across += wx * f64::from(src[row * w + column]);
                }
                sum += wy * f64::from(across as f32);
            }
            out.push(sum as f32);
        }
    }
    out
}

#[test]
fn the_pyramid_levels_equal_their_references() {
    let mut state = 4;
    for (w, h) in [(97, 61), (2, 3), (1, 1), (64, 64)] {
        let src: Vec<f32> = (0..w * h)
            .map(|_| 200.0 * uniform(&mut state) - 50.0)
            .collect();
        let (nw, nh, got) = down(&src, w, h);
        assert_eq!((nw, nh), (w.div_ceil(2), h.div_ceil(2)));
        let want = reference_down(&src, w, h);
        assert_eq!(
            got.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            want.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "down {w}x{h}"
        );
        let got = up(&got, nw, nh, w, h);
        let want = reference_up(&reference_down(&src, w, h), nw, nh, w, h);
        assert_eq!(
            got.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            want.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "up {w}x{h}"
        );
    }
}

/// The reference push-pull: the definition's recursion over levels, each level's estimate
/// `S_k / W_k` blended with the expanded coarser result by `min(4 W_k, 1)`, the finest `skip`
/// levels taking the coarser result alone.
fn reference_push_pull(
    values: &[f32],
    weight: &[f32],
    w: usize,
    h: usize,
    skip: usize,
) -> Vec<f32> {
    let mut sizes = vec![(w, h)];
    let mut weights = vec![weight.to_vec()];
    let mut sums = vec![values.to_vec()];
    while sizes.len() < 24 && *sizes.last().unwrap() != (1, 1) {
        let (pw, ph) = *sizes.last().unwrap();
        weights.push(reference_down(weights.last().unwrap(), pw, ph));
        sums.push(reference_down(sums.last().unwrap(), pw, ph));
        sizes.push((pw.div_ceil(2), ph.div_ceil(2)));
    }
    let estimate = |s: f32, wt: f32| if wt > 1e-8 { s / wt } else { 0.0 };
    let last = sizes.len() - 1;
    let mut c: Vec<f32> = (0..sums[last].len())
        .map(|i| estimate(sums[last][i], weights[last][i]))
        .collect();
    for k in (0..last).rev() {
        let (kw, kh) = sizes[k];
        let (cw, ch) = sizes[k + 1];
        let coarse = reference_up(&c, cw, ch, kw, kh);
        c = (0..kw * kh)
            .map(|i| {
                if k < skip {
                    coarse[i]
                } else {
                    let a = (4.0 * weights[k][i]).min(1.0);
                    a * estimate(sums[k][i], weights[k][i]) + (1.0 - a) * coarse[i]
                }
            })
            .collect();
    }
    c
}

#[test]
fn push_pull_equals_its_reference_and_extends_a_band_smoothly() {
    let (w, h) = (75, 53);
    let mut state = 9;
    // Known on a band around a hole in the middle.
    let weight: Vec<f32> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let inside = (20..55).contains(&x) && (15..40).contains(&y);
            let band = (14..61).contains(&x) && (9..46).contains(&y);
            if band && !inside {
                0.5 + 0.5 * uniform(&mut state)
            } else {
                0.0
            }
        })
        .collect();
    let values: Vec<f32> = weight.iter().map(|&wt| wt * 12.0).collect();
    for skip in [0, 3] {
        let got = push_pull(values.clone(), &weight_pyramid(weight.clone(), w, h), skip);
        let want = reference_push_pull(&values, &weight, w, h, skip);
        assert_eq!(
            got.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            want.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "skip {skip}"
        );
        // A constant known on the band is that constant in the hole.
        for y in 20..35 {
            for x in 28..48 {
                assert!(
                    (got[y * w + x] - 12.0).abs() < 0.05,
                    "({x}, {y}): {}",
                    got[y * w + x]
                );
            }
        }
    }
    // Known everywhere with full weight and nothing skipped: the values themselves.
    let full = vec![1.0_f32; w * h];
    let values: Vec<f32> = (0..w * h).map(|_| uniform(&mut state)).collect();
    let got = push_pull(values.clone(), &weight_pyramid(full, w, h), 0);
    assert_eq!(got, values);
}

#[test]
fn the_pyramid_blend_is_the_whole_difference_outside_and_none_inside() {
    let (w, h) = (97_usize, 61_usize);
    let area = rect(0, 0, w as u32, h as u32);
    let mut state = 8;
    let mut input = Planes::new(w, h);
    let mut fill = Planes::new(w, h);
    for c in 0..3 {
        for i in 0..w * h {
            input.c[c][i] = 255.0 * uniform(&mut state);
            fill.c[c][i] = 50.0 + (i % 17) as f32 * 3.0 + c as f32;
        }
    }
    // Nothing covered: the correction is the whole difference, so the blend is the input.
    let none = vec![0.0; w * h];
    let d = blend::pyramid_correction(&fill, &input, area, area, &none, 4);
    for c in 0..3 {
        for i in 0..w * h {
            let want = input.c[c][i] - fill.c[c][i];
            assert!((d.c[c][i] - want).abs() < 1e-3, "{c} {i}");
        }
    }
    // Everything covered: no correction, so the input never enters.
    let all = vec![1.0; w * h];
    let d = blend::pyramid_correction(&fill, &input, area, area, &all, 4);
    assert!(d.c.iter().flatten().all(|v| v.abs() < 1e-6));
}

#[test]
fn the_robust_sigma_is_its_reference() {
    let mut state = 2;
    let mut samples: Vec<f32> = (0..1001)
        .map(|_| {
            10.0 * (uniform(&mut state) - 0.5)
                + if mix(&mut state).is_multiple_of(50) {
                    400.0
                } else {
                    0.0
                }
        })
        .collect();
    let mut sorted = samples.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    let mut dev: Vec<f32> = samples.iter().map(|v| (v - median).abs()).collect();
    let mut sorted_dev = dev.clone();
    sorted_dev.sort_by(f32::total_cmp);
    let mad = sorted_dev[sorted_dev.len() / 2];
    let cut = 3.0 * (1.4826 * mad).max(0.5);
    dev.retain(|&d| d <= cut);
    let sum: f64 = dev.iter().map(|&d| f64::from(d).powi(2)).sum();
    let want = ((sum / dev.len() as f64) / 0.9733).sqrt() as f32;
    assert_eq!(grain::robust_sigma(&mut samples), want);
}

#[test]
fn grain_depends_only_on_the_seed_and_the_pixel() {
    let kernel = grain_kernel(0.8);
    let whole = grain_field(7, 1, rect(10, 20, 64, 48), &kernel);
    let part = grain_field(7, 1, rect(30, 31, 20, 9), &kernel);
    for y in 0..9 {
        for x in 0..20 {
            assert_eq!(part[y * 20 + x], whole[(y + 11) * 64 + x + 20]);
        }
    }
    assert_ne!(grain_field(8, 1, rect(10, 20, 64, 48), &kernel), whole);
    // Unit σ whatever its correlation.
    for px in [0.0, 0.7, 1.2] {
        let f = grain_field(5, 0, rect(0, 0, 400, 400), &grain_kernel(px));
        let var = f.iter().map(|v| f64::from(v * v)).sum::<f64>() / f.len() as f64;
        assert!((var.sqrt() - 1.0).abs() < 0.02, "{px}: σ {}", var.sqrt());
    }
}

/// Codes of a grey image with grain of `sigma` made with a Gaussian of `px`, shared between
/// channels when `shared`.
fn grain_planes(size: u32, sigma: f32, px: f64, shared: bool) -> Planes {
    let kernel = grain_kernel(px);
    let r = rect(0, 0, size, size);
    let fields: Vec<Vec<f32>> = (0..3)
        .map(|c| grain_field(21, if shared { 0 } else { c }, r, &kernel))
        .collect();
    let mut planes = Planes::new(size as usize, size as usize);
    for (c, base) in [128.0_f32, 120.0, 110.0].into_iter().enumerate() {
        for (i, v) in planes.c[c].iter_mut().enumerate() {
            *v = (base + sigma * fields[c][i]).round().clamp(0.0, 255.0);
        }
    }
    planes
}

#[test]
fn the_grain_estimate_recovers_level_and_correlation() {
    let s = stage(600, 600);
    let area = rect(0, 0, 600, 600);
    let region = soft_rect(s, rect(200, 200, 200, 200), 0);
    let bounds = region.bounds();
    let width = crop::ring_width(bounds);
    let white = grain_planes(600, 4.0, 0.0, false);
    let g = ring_grain(&white, area, &region, bounds, width).unwrap();
    let gain = grain_highpass_gain(&grain_kernel(f64::from(g.correlation_px)), 1) as f32;
    assert!(g.correlation_px < 0.2, "{g:?}");
    assert!(g.channel_correlation < 0.1, "{g:?}");
    for c in 0..3 {
        assert!((g.highpass[c] / gain - 4.0).abs() < 0.2, "{g:?}");
    }
    let grainy = grain_planes(600, 5.0, 0.8, true);
    let g = ring_grain(&grainy, area, &region, bounds, width).unwrap();
    let gain = grain_highpass_gain(&grain_kernel(f64::from(g.correlation_px)), 1) as f32;
    assert!((g.correlation_px - 0.8).abs() < 0.06, "{g:?}");
    assert!(g.channel_correlation > 0.9, "{g:?}");
    for c in 0..3 {
        assert!((g.highpass[c] / gain - 5.0).abs() < 0.3, "{g:?}");
    }
}

/// A model answer of one colour everywhere, display-referred.
fn flat_answer(n: usize, colour: [f32; 3]) -> Vec<f32> {
    std::iter::repeat_n(colour, n * n).flatten().collect()
}

#[test]
fn a_candidate_keeps_the_input_where_coverage_is_zero_and_is_deterministic() {
    let s = stage(640, 480);
    let image = linear_image(s, 12);
    let coverage = soft_rect(s, rect(250, 180, 120, 90), 6);
    let mut reads = Vec::new();
    let context = Context::prepare(
        coverage.clone(),
        s,
        128,
        50_000,
        &mut reader(&image, s, &mut reads),
        &ok,
    )
    .unwrap();
    // Several bands, never past the tile, all inside the square.
    assert!(reads.len() > 1);
    let answer = flat_answer(128, [0.2, 0.6, 0.4]);
    let a = context.finish(&answer, 3).unwrap();
    let b = context.finish(&answer, 3).unwrap();
    assert_eq!(a, b);
    let bounds = coverage.bounds();
    assert_eq!(a.rect, [bounds.x0, bounds.y0, bounds.width, bounds.height]);
    let mut changed = 0;
    for y in 0..bounds.height {
        for x in 0..bounds.width {
            let i = (y * bounds.width + x) as usize;
            let input = image[((bounds.y0 + y) * s.width + bounds.x0 + x) as usize];
            let patch = &a.linear[i * 3..i * 3 + 3];
            assert_eq!(a.coverage[i], coverage.at(bounds.x0 + x, bounds.y0 + y));
            if a.coverage[i] == 0.0 {
                assert_eq!(patch, input, "({x}, {y})");
            } else if patch != input {
                changed += 1;
            }
        }
    }
    assert!(changed > 0);
    assert!(a.linear.iter().all(|v| v.is_finite()));
    assert_eq!(
        (a.preview_width, a.preview_height),
        (bounds.width, bounds.height)
    );
    assert_eq!(a.preview.len(), (bounds.width * bounds.height * 3) as usize);
    // Another seed draws other grain, only where it adds any.
    let c = context.finish(&answer, 4).unwrap();
    assert_eq!(c.grain, a.grain);
    if a.grain.iter().any(|&g| g > 0.0) {
        assert_ne!(c.linear, a.linear);
    }
}

#[test]
fn the_seam_correction_takes_out_a_colour_offset() {
    // A flat image whose answer is 30 codes too bright in the hole: the correction meets the input.
    let s = stage(800, 600);
    let grey = [0.18_f32, 0.22, 0.26];
    let image = vec![grey; (s.width * s.height) as usize];
    let coverage = soft_rect(s, rect(300, 200, 200, 160), 0);
    let mut reads = Vec::new();
    let context = Context::prepare(
        coverage.clone(),
        s,
        256,
        1 << 22,
        &mut reader(&image, s, &mut reads),
        &ok,
    )
    .unwrap();
    let codes = grey.map(|v| context.encoding.code(v));
    let bright = codes.map(|c| (c + 30.0) / 255.0);
    // The answer keeps the known pixels and fills the hole 30 codes too bright.
    let answer: Vec<f32> = context
        .model_mask
        .iter()
        .enumerate()
        .flat_map(|(i, &hole)| {
            let known = &context.model_image[i * 3..i * 3 + 3];
            [0, 1, 2].map(|c| if hole > 0.0 { bright[c] } else { known[c] })
        })
        .collect();
    let candidate = context.finish(&answer, 1).unwrap();
    assert!(candidate.seam_band > 0);
    let (mut sum, mut count) = (0_f64, 0);
    for (i, &c) in candidate.coverage.iter().enumerate() {
        if c > 0.0 {
            for (k, &expected) in codes.iter().enumerate() {
                let code = context.encoding.code(candidate.linear[i * 3 + k]);
                sum += f64::from((code - expected).abs());
                count += 1;
            }
        }
    }
    assert!(
        sum / f64::from(count) < 0.5,
        "mean error {} codes",
        sum / f64::from(count)
    );

    // Without the blend (a Replace) the answer's own colour stands where the coverage is full.
    let replaced = context.finish_with(&answer, 1, false).unwrap();
    assert_eq!(replaced.seam_band, 0);
    let (mut sum, mut count) = (0_f64, 0);
    for (i, &c) in replaced.coverage.iter().enumerate() {
        if c >= 1.0 {
            for (k, &expected) in codes.iter().enumerate() {
                let code = context.encoding.code(replaced.linear[i * 3 + k]);
                sum += f64::from(code - expected);
                count += 1;
            }
        }
    }
    let offset = sum / f64::from(count);
    assert!((offset - 30.0).abs() < 1.0, "mean offset {offset} codes");
    // And every pixel the coverage leaves is still the input's own.
    for (i, &c) in replaced.coverage.iter().enumerate() {
        if c == 0.0 {
            assert_eq!(
                replaced.linear[i * 3..i * 3 + 3],
                candidate.linear[i * 3..i * 3 + 3]
            );
        }
    }
}

#[test]
fn a_cancelled_read_stops_between_bands() {
    let s = stage(300, 200);
    let image = linear_image(s, 1);
    let coverage = soft_rect(s, rect(120, 80, 30, 20), 2);
    let mut reads = Vec::new();
    let stopped = Context::prepare(
        coverage,
        s,
        64,
        1_000,
        &mut reader(&image, s, &mut reads),
        &|| Err(Error::new(crate::ErrorKind::Cancelled, "cancelled")),
    )
    .unwrap_err();
    assert_eq!(stopped.kind, crate::ErrorKind::Cancelled);
    assert!(reads.is_empty());
}

#[test]
fn a_preview_is_at_most_1024_on_its_longer_side() {
    assert_eq!(super::preview_size(300, 200), (300, 200));
    assert_eq!(super::preview_size(4096, 1024), (1024, 256));
    assert_eq!(super::preview_size(3000, 1), (1024, 1));
}

#[test]
fn a_selection_is_full_over_what_was_painted_and_feathers_past_it() {
    // A 120 px radius along y = 600: full coverage out to the painted edge, then a feather of 18
    // px (15% of the radius) past it.
    let s = stage(1600, 1200);
    let parameters = serde_json::json!({
        "selection-points": [[0.4, 0.5], [0.6, 0.5]],
        "selection-radius": 0.1,
    });
    let selection = Selection::parse(parameters.as_object().unwrap()).unwrap();
    let coverage = selection.coverage(s).unwrap().unwrap();
    // Pixel row `y`'s centre is `|y + 0.5 − 600|` from the stroke.
    for y in [600, 560, 481, 719] {
        assert_eq!(coverage.at(800, y), 1.0, "row {y}");
    }
    for y in [470, 729] {
        let c = coverage.at(800, y);
        assert!(c > 0.0 && c < 1.0, "row {y}: {c}");
    }
    assert!(coverage.at(800, 470) > coverage.at(800, 465));
    for y in [461, 738] {
        assert_eq!(coverage.at(800, y), 0.0, "row {y}");
    }
    let bounds = coverage.bounds();
    assert_eq!((bounds.y0, bounds.y1()), (462, 738));
    // What the owner admits by holds every covered pixel.
    let support = selection.support(s);
    assert!(
        support.y0 <= bounds.y0 && support.y1() >= bounds.y1(),
        "{support:?}"
    );
}

#[test]
#[slow]
fn an_object_the_brush_paints_over_never_shows_through_the_candidate() {
    // A flat background with a red block, the object, whose top edge lies 12 px inside the top
    // edge of the brush stroke painted over it: a 120 px radius along y = 600 paints 480 to 720,
    // and the block starts at 492. Whatever the model answers, nothing of the block may reach the
    // composite: a flat answer of the background must composite to the background everywhere,
    // with the seam correction and detail blend (a removal) and without them (a Replace).
    let s = stage(1600, 1200);
    let background = [0.30_f32, 0.40, 0.50];
    let red = [0.75_f32, 0.03, 0.03];
    let block = rect(672, 492, 256, 188);
    let image: Vec<[f32; 3]> = (0..s.height)
        .flat_map(|y| (0..s.width).map(move |x| (x, y)))
        .map(|(x, y)| {
            if block.contains(x, y) {
                red
            } else {
                background
            }
        })
        .collect();
    let parameters = serde_json::json!({
        "selection-points": [[0.4, 0.5], [0.6, 0.5]],
        "selection-radius": 0.1,
    });
    let selection = Selection::parse(parameters.as_object().unwrap()).unwrap();
    let coverage = selection.coverage(s).unwrap().unwrap();
    // The painted footprint holds the whole block.
    for (x, y) in [(672, 492), (927, 492), (800, 492), (672, 679), (927, 679)] {
        assert!(coverage.at(x, y) > 0.0, "({x}, {y})");
    }
    let n = 256;
    let mut reads = Vec::new();
    let context = Context::prepare(
        coverage.clone(),
        s,
        n,
        1 << 22,
        &mut reader(&image, s, &mut reads),
        &ok,
    )
    .unwrap();
    let codes = background.map(|v| context.encoding.code(v));
    let answer = flat_answer(n, codes.map(|c| c / 255.0));
    for blend in [true, false] {
        let candidate = context.finish_with(&answer, 7, blend).unwrap();
        let [x0, y0, width, _] = candidate.rect;
        let (mut worst, mut at) = (0.0_f32, (0, 0));
        for (i, &c) in candidate.coverage.iter().enumerate() {
            let (x, y) = (x0 + i as u32 % width, y0 + i as u32 / width);
            let input = image[(y * s.width + x) as usize];
            for k in 0..3 {
                // The repair stage's composite, `in + c · (patch − in)`.
                let patch = f64::from(candidate.linear[i * 3 + k]);
                let shown = f64::from(input[k]) + f64::from(c) * (patch - f64::from(input[k]));
                let error = (context.encoding.code(shown as f32) - codes[k]).abs();
                if error > worst {
                    (worst, at) = (error, (x, y));
                }
            }
        }
        assert!(
            worst < 1.0,
            "blend {blend}: the block shows through by {worst} codes at {at:?}"
        );
    }
}
