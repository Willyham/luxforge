//! Meeting the input at the region's edge: the seam correction's push-pull and the Laplacian
//! pyramid's detail blend, on the fill's codes over its window. Sums run in `f64` and every
//! level is stored in `f32`.
use super::{Planes, crop::CropPlan, crop::reflect, resample::upscale_plane};
use crate::modules::Region;

/// The 5-tap binomial every pyramid level is blurred with.
pub(crate) const BINOMIAL: [f64; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
/// The most push-pull levels: enough to reach one pixel from any window the admission allows.
const MAX_PUSH_PULL_LEVELS: usize = 24;

/// One pyramid level: its size and values.
pub(crate) type Level = (usize, usize, Vec<f32>);

/// Blurs with the 5-tap binomial and keeps every second sample, reflecting at the edges.
pub(crate) fn down(src: &[f32], w: usize, h: usize) -> Level {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let rx = |i: i64| reflect(i, w as u32) as usize;
    let ry = |i: i64| reflect(i, h as u32) as usize;
    let mut rows = vec![0_f32; nw * h];
    for y in 0..h {
        for x in 0..nw {
            let cx = 2 * x as i64;
            let mut sum = 0_f64;
            for (j, b) in BINOMIAL.iter().enumerate() {
                sum += b * f64::from(src[y * w + rx(cx + j as i64 - 2)]);
            }
            rows[y * nw + x] = sum as f32;
        }
    }
    let mut out = vec![0_f32; nw * nh];
    for y in 0..nh {
        let cy = 2 * y as i64;
        for x in 0..nw {
            let mut sum = 0_f64;
            for (j, b) in BINOMIAL.iter().enumerate() {
                sum += b * f64::from(rows[ry(cy + j as i64 - 2) * nw + x]);
            }
            out[y * nw + x] = sum as f32;
        }
    }
    (nw, nh, out)
}

/// The coarse taps of fine index `i` of an expand from `len` samples: even samples take
/// `[1, 6, 1] / 8` of the coarse neighbours, odd samples their mean, clamped at the edges.
fn expand(i: usize, len: usize) -> [(usize, f64); 3] {
    let c = |j: i64| j.clamp(0, len as i64 - 1) as usize;
    let k = (i / 2) as i64;
    if i.is_multiple_of(2) {
        [(c(k - 1), 0.125), (c(k), 0.75), (c(k + 1), 0.125)]
    } else {
        [(c(k), 0.5), (c(k + 1), 0.5), (c(k), 0.0)]
    }
}

/// Burt and Adelson's expand from `(w, h)` to `(tw, th)`.
pub(crate) fn up(src: &[f32], w: usize, h: usize, tw: usize, th: usize) -> Vec<f32> {
    let mut rows = vec![0_f32; tw * h];
    for y in 0..h {
        for x in 0..tw {
            let mut sum = 0_f64;
            for (j, weight) in expand(x, w) {
                sum += weight * f64::from(src[y * w + j]);
            }
            rows[y * tw + x] = sum as f32;
        }
    }
    let mut out = vec![0_f32; tw * th];
    for y in 0..th {
        let taps = expand(y, h);
        for x in 0..tw {
            let mut sum = 0_f64;
            for (j, weight) in taps {
                sum += weight * f64::from(rows[j * tw + x]);
            }
            out[y * tw + x] = sum as f32;
        }
    }
    out
}

/// The low-frequency seam correction, returning the band's size in pixels. The hole enlarged with
/// the fill's own filter, `α`, is 1 inside the region and fades over a band of background just
/// outside it, where the fill is `α` parts regenerated content `G` and the rest the input's own
/// pixels, so `I − F ≈ α (I − G)`. Push-pull on a pyramid fits `I − G` there by least squares
/// (values `α (I − F)`, weights `α²`, over pixels with coverage 0 and `α > 0.2`) and extends it
/// smoothly into the region; adding `α` times it makes the fill meet the input without a step in
/// colour or tone. It holds nothing finer than 8 stage pixels or two model pixels, so texture
/// differences in the band are not carried in. The band has coverage 0, so what the region covers
/// never enters. `input` holds the codes over `area`, which contains `window`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn seam_correction(
    fill: &mut Planes,
    input: &Planes,
    area: Region,
    window: Region,
    cov: &[f32],
    plan: &CropPlan,
    hole: &[u8],
    n: usize,
    scale: f64,
) -> usize {
    let (w, h) = (fill.width, fill.height);
    let hole_f: Vec<f32> = hole.iter().map(|&v| f32::from(v)).collect();
    let alpha = upscale_plane(&hole_f, n, plan, window);
    let weight: Vec<f32> = alpha
        .iter()
        .zip(cov)
        .map(|(&a, &r)| {
            if r == 0.0 && a > 0.2 {
                a.min(1.0).powi(2)
            } else {
                0.0
            }
        })
        .collect();
    let band = weight.iter().filter(|&&v| v > 0.0).count();
    if band == 0 {
        return 0;
    }
    let weights = weight_pyramid(weight, w, h);
    // A colour cast lives at the model's scale and above; texture differences in the band must not
    // be carried in, so nothing finer than 8 stage pixels or two model pixels survives.
    let skip = ((2.0 * scale).log2().ceil().max(3.0) as usize).min(weights.len() - 1);
    let (ox, oy) = (
        (window.x0 - area.x0) as usize,
        (window.y0 - area.y0) as usize,
    );
    for c in 0..3 {
        let mut d = vec![0_f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if weights[0].2[i] > 0.0 {
                    let a = alpha[i].min(1.0);
                    d[i] = a * (input.c[c][(y + oy) * input.width + x + ox] - fill.c[c][i]);
                }
            }
        }
        let correction = push_pull(d, &weights, skip);
        for ((v, d), &a) in fill.c[c].iter_mut().zip(&correction).zip(&alpha) {
            *v += a * d;
        }
    }
    band
}

/// The weights' pyramid down to one pixel, at most [`MAX_PUSH_PULL_LEVELS`] levels.
pub(crate) fn weight_pyramid(weight: Vec<f32>, w: usize, h: usize) -> Vec<Level> {
    let mut weights = vec![(w, h, weight)];
    while weights.len() < MAX_PUSH_PULL_LEVELS {
        let (pw, ph, p) = weights.last().expect("level 0");
        if *pw == 1 && *ph == 1 {
            break;
        }
        let next = down(p, *pw, *ph);
        weights.push(next);
    }
    weights
}

/// Push-pull interpolation: values known where the level-0 weight is positive (zero elsewhere),
/// extended everywhere by normalised averages on a binomial pyramid, each level taking its own
/// estimate where it has support and the coarser level's where it has little. The finest `skip`
/// levels only upsample, so the result holds no detail finer than `2^skip` pixels.
pub(crate) fn push_pull(values: Vec<f32>, weights: &[Level], skip: usize) -> Vec<f32> {
    let mut sums = vec![values];
    for k in 1..weights.len() {
        let (pw, ph, _) = &weights[k - 1];
        let next = down(sums.last().expect("level 0"), *pw, *ph).2;
        sums.push(next);
    }
    let estimate = |s: f32, wt: f32| if wt > 1e-8 { s / wt } else { 0.0 };
    let last = weights.len() - 1;
    let (mut cw, mut ch, top) = (weights[last].0, weights[last].1, &weights[last].2);
    let mut c: Vec<f32> = sums[last]
        .iter()
        .zip(top)
        .map(|(&s, &wt)| estimate(s, wt))
        .collect();
    for k in (0..last).rev() {
        let (kw, kh, wt) = &weights[k];
        let coarse = up(&c, cw, ch, *kw, *kh);
        c = if k < skip {
            coarse
        } else {
            (0..kw * kh)
                .map(|i| {
                    let a = (4.0 * wt[i]).min(1.0);
                    a * estimate(sums[k][i], wt[i]) + (1.0 - a) * coarse[i]
                })
                .collect()
        };
        (cw, ch) = (*kw, *kh);
    }
    c
}

/// The Laplacian pyramid blend of the fill `F` against the input with its region already filled,
/// `I' = I (1 − r) + F r`, under the region's coverage `r`, returned as the correction to add to
/// `F`: `Σ_k (1 − G_k(r)) · L_k((1 − r)(I − F))`. It carries detail across the region's edge band
/// by band: fine bands over a few pixels, coarse ones further. The input under full coverage is
/// multiplied by 0 before any band is built, so what the region covers never enters the blend.
/// `input` holds the codes over `area`, which contains `window`. One channel at a time.
pub(crate) fn pyramid_correction(
    fill: &Planes,
    input: &Planes,
    area: Region,
    window: Region,
    cov: &[f32],
    levels: usize,
) -> Planes {
    let (w, h) = (fill.width, fill.height);
    let mut gw: Vec<Level> = vec![(w, h, cov.to_vec())];
    for _ in 0..levels {
        let (pw, ph, p) = gw.last().expect("level 0");
        let next = down(p, *pw, *ph);
        gw.push(next);
    }
    let (ox, oy) = (
        (window.x0 - area.x0) as usize,
        (window.y0 - area.y0) as usize,
    );
    let channel = |c: usize| -> Vec<f32> {
        let mut diff = vec![0_f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let r = gw[0].2[i];
                diff[i] = (1.0 - r) * (input.c[c][(y + oy) * input.width + x + ox] - fill.c[c][i]);
            }
        }
        let mut gauss = vec![(w, h, diff)];
        for _ in 0..levels {
            let (pw, ph, p) = gauss.last().expect("level 0");
            let next = down(p, *pw, *ph);
            gauss.push(next);
        }
        let (tw, th, top) = &gauss[levels];
        let mut acc: Vec<f32> = top
            .iter()
            .zip(&gw[levels].2)
            .map(|(v, g)| (1.0 - g) * v)
            .collect();
        let (mut aw, mut ah) = (*tw, *th);
        for k in (0..levels).rev() {
            let (lw, lh, g) = &gauss[k];
            let (cw, ch, coarse) = &gauss[k + 1];
            let up_g = up(coarse, *cw, *ch, *lw, *lh);
            let up_acc = up(&acc, aw, ah, *lw, *lh);
            let weight = &gw[k].2;
            acc = (0..lw * lh)
                .map(|i| up_acc[i] + (1.0 - weight[i]) * (g[i] - up_g[i]))
                .collect();
            (aw, ah) = (*lw, *lh);
        }
        acc
    };
    Planes {
        width: w,
        height: h,
        c: [channel(0), channel(1), channel(2)],
    }
}
