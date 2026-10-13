//! Independent f64 reference for the guided upsampler of `docs/design/ai-editing.md#the-guided-upsampler`.
//!
//! The upsampler turns a model's probabilities at the model's own resolution into an 8-bit alpha
//! raster at the output's resolution, with its edges taken from a luminance guide at that output
//! resolution: He and Sun's fast guided filter, whose coefficients are computed on a grid
//! subsampled from the output and enlarged bilinearly before `q = a · I + b` is taken at every
//! output pixel, with the filter's statistics weighted by the prior's confidence. This file is
//! that algorithm written out in its plainest form, step by step; the production function in
//! `luxforge-core` writes the same expressions in the same order and is compared against this one
//! bit for bit. They share no code.
//!
//! Domain: every value of `prior` and `guide` is finite, `radius >= 1` and `eps > 0`; production
//! refuses anything else before it gets here. Probabilities are clamped to `[0, 1]` as they are
//! read; the guide is read as it is.

/// The guided filter's radius in subsampled samples is about this many: the coefficient grid is
/// the output subsampled by `radius / SUBSAMPLED_RADIUS`, at least 1.
pub const SUBSAMPLED_RADIUS: u32 = 4;

/// The most samples the coefficient grid holds over the whole output. The subsampling grows until
/// the output's grid fits, so the working planes are bounded whatever the output's size.
pub const GRID_MAX: u64 = 1 << 20;

/// The smallest weight a prior sample carries: the weight is `CONFIDENCE_FLOOR + |2p - 1|`, so an
/// undecided sample (`p = 0.5`) still counts, a sixteenth as much as a decided one.
pub const CONFIDENCE_FLOOR: f64 = 0.0625;

/// A raster of `f32` samples, row-major.
#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    pub width: u32,
    pub height: u32,
    pub values: &'a [f32],
}

/// The rectangle of the output the upsampler computed: everything outside it is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The subsampling factor of the coefficient grid and the filter's radius in its samples, for an
/// output of `width × height` and a radius of `radius` output pixels.
pub fn subsampling(width: u32, height: u32, radius: u32) -> (u32, u32) {
    let mut s = (radius / SUBSAMPLED_RADIUS).max(1);
    while u64::from(width.div_ceil(s)) * u64::from(height.div_ceil(s)) > GRID_MAX {
        s += 1;
    }
    let r = ((radius + s / 2) / s).max(1);
    (s, r)
}

/// One prior sample, clamped to `[0, 1]`.
fn prior_at(prior: &Plane<'_>, i: usize, j: usize) -> f64 {
    f64::from(prior.values[j * prior.width as usize + i]).clamp(0.0, 1.0)
}

/// The window: the prior's support grown by the filter's reach, clamped to the output.
///
/// The support is every output position a non-zero prior cell reaches through the bilinear
/// enlargement: cell `c` has its centre at `(c + 0.5) · W / w` and reaches one cell either side of
/// it. The filter then spreads coverage through two box passes of radius `r` samples and the
/// bilinear enlargement of the coefficients, `(2r + 2) · s` output pixels in all.
pub fn window(prior: &Plane<'_>, guide: &Plane<'_>, radius: u32) -> Option<Window> {
    let (s, r) = subsampling(guide.width, guide.height, radius);
    let mut cells: Option<(u32, u32, u32, u32)> = None;
    for j in 0..prior.height {
        for i in 0..prior.width {
            if prior.values[(j * prior.width + i) as usize] > 0.0 {
                cells = Some(match cells {
                    None => (i, i, j, j),
                    Some((x0, x1, y0, y1)) => (x0.min(i), x1.max(i), y0.min(j), y1.max(j)),
                });
            }
        }
    }
    let (cx0, cx1, cy0, cy1) = cells?;
    let sx = f64::from(guide.width) / f64::from(prior.width);
    let sy = f64::from(guide.height) / f64::from(prior.height);
    let reach = f64::from((2 * r + 2) * s);
    let x0 = ((f64::from(cx0) - 0.5) * sx - reach).floor().max(0.0) as u32;
    let x1 = ((f64::from(cx1) + 1.5) * sx + reach)
        .ceil()
        .min(f64::from(guide.width)) as u32;
    let y0 = ((f64::from(cy0) - 0.5) * sy - reach).floor().max(0.0) as u32;
    let y1 = ((f64::from(cy1) + 1.5) * sy + reach)
        .ceil()
        .min(f64::from(guide.height)) as u32;
    (x0 < x1 && y0 < y1).then_some(Window {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// The prior at a continuous output position `(px, py)`, bilinear between cell centres with the
/// edges clamped:
///
/// ```text
/// u = clamp(px * (w / W) - 0.5, 0, w - 1)
/// v = clamp(py * (h / H) - 0.5, 0, h - 1)
/// top    = p(u0, v0) + fu * (p(u1, v0) - p(u0, v0))
/// bottom = p(u0, v1) + fu * (p(u1, v1) - p(u0, v1))
/// p      = top + fv * (bottom - top)
/// ```
pub fn prior_sample(prior: &Plane<'_>, guide: &Plane<'_>, px: f64, py: f64) -> f64 {
    let kx = f64::from(prior.width) / f64::from(guide.width);
    let ky = f64::from(prior.height) / f64::from(guide.height);
    let u = (px * kx - 0.5).clamp(0.0, f64::from(prior.width - 1));
    let v = (py * ky - 0.5).clamp(0.0, f64::from(prior.height - 1));
    let u0 = u.floor();
    let v0 = v.floor();
    let fu = u - u0;
    let fv = v - v0;
    let (i0, j0) = (u0 as usize, v0 as usize);
    let i1 = (i0 + 1).min(prior.width as usize - 1);
    let j1 = (j0 + 1).min(prior.height as usize - 1);
    let top = prior_at(prior, i0, j0) + fu * (prior_at(prior, i1, j0) - prior_at(prior, i0, j0));
    let bottom = prior_at(prior, i0, j1) + fu * (prior_at(prior, i1, j1) - prior_at(prior, i0, j1));
    top + fv * (bottom - top)
}

/// The sum of `plane` (`w × h`) over the `(2r + 1)²` box around every sample, clipped at the
/// edges, by an integral image built row by row:
///
/// ```text
/// S(0, *) = S(*, 0) = 0
/// row     = running sum of plane(0..=x, y) along the row
/// S(x + 1, y + 1) = S(x + 1, y) + row
/// sum(i, j) = ((S(xb + 1, yb + 1) - S(xb + 1, ya)) - S(xa, yb + 1)) + S(xa, ya)
/// ```
///
/// with `xa = max(i - r, 0)`, `xb = min(i + r, w - 1)` and likewise for `y`.
pub fn box_sums(plane: &[f64], w: usize, h: usize, r: usize) -> Vec<f64> {
    let stride = w + 1;
    let mut integral = vec![0.0f64; stride * (h + 1)];
    for y in 0..h {
        let mut row = 0.0;
        for x in 0..w {
            row += plane[y * w + x];
            integral[(y + 1) * stride + x + 1] = integral[y * stride + x + 1] + row;
        }
    }
    let mut sums = vec![0.0f64; w * h];
    for j in 0..h {
        let ya = j.saturating_sub(r);
        let yb = (j + r).min(h - 1);
        for i in 0..w {
            let xa = i.saturating_sub(r);
            let xb = (i + r).min(w - 1);
            sums[j * w + i] = ((integral[(yb + 1) * stride + xb + 1]
                - integral[ya * stride + xb + 1])
                - integral[(yb + 1) * stride + xa])
                + integral[ya * stride + xa];
        }
    }
    sums
}

/// How many samples the clipped box around `(i, j)` covers.
pub fn box_count(i: usize, j: usize, w: usize, h: usize, r: usize) -> f64 {
    let xa = i.saturating_sub(r);
    let xb = (i + r).min(w - 1);
    let ya = j.saturating_sub(r);
    let yb = (j + r).min(h - 1);
    ((xb - xa + 1) * (yb - ya + 1)) as f64
}

/// Upsample `prior` to the guide's resolution against `guide`: the window it computed and that
/// window's alpha codes, row-major, or `None` when no prior sample is above zero.
///
/// ```text
/// grid:    block (i, j) holds output pixels [i·s, min((i+1)·s, ww)) × [j·s, min((j+1)·s, wh))
///          of the window; I = the mean of the guide over the block, summed y-major from 0.0;
///          p = prior_sample at the block's centre
/// weight:  c = CONFIDENCE_FLOOR + |2p - 1|
/// sums:    Σc, Σ(c·I), Σ(c·p), Σ((c·I)·I), Σ((c·I)·p) over each box (box_sums)
/// means:   mI = Σ(c·I) / Σc,  mp = Σ(c·p) / Σc,  cII = Σ((c·I)·I) / Σc,  cIp = Σ((c·I)·p) / Σc
/// coeffs:  var = max(cII - mI·mI, 0),  cov = cIp - mI·mp,  a = cov / (var + eps),  b = mp - a·mI
/// smooth:  ā = box_sums(a) / count,  b̄ = box_sums(b) / count
/// output:  u = clamp((x + 0.5) / s - 0.5, 0, lw - 1), likewise v; ā and b̄ bilinear there in the
///          prior's form; q = ā · guide(x, y) + b̄; code = round(clamp(q, 0, 1) · 255)
/// ```
pub fn upsample(
    prior: &Plane<'_>,
    guide: &Plane<'_>,
    radius: u32,
    eps: f64,
) -> Option<(Window, Vec<u8>)> {
    let window = window(prior, guide, radius)?;
    let (s, r) = subsampling(guide.width, guide.height, radius);
    let (s, r) = (s as usize, r as usize);
    let (ww, wh) = (window.width as usize, window.height as usize);
    let (lw, lh) = (ww.div_ceil(s), wh.div_ceil(s));
    let ow = guide.width as usize;
    let at = |x: usize, y: usize| -> f64 {
        f64::from(guide.values[(window.y as usize + y) * ow + window.x as usize + x])
    };

    let mut low_i = vec![0.0f64; lw * lh];
    let mut low_p = vec![0.0f64; lw * lh];
    for j in 0..lh {
        let (y0, y1) = (j * s, ((j + 1) * s).min(wh));
        for i in 0..lw {
            let (x0, x1) = (i * s, ((i + 1) * s).min(ww));
            let mut sum = 0.0;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += at(x, y);
                }
            }
            low_i[j * lw + i] = sum / ((y1 - y0) * (x1 - x0)) as f64;
            let px = f64::from(window.x) + (x0 + x1) as f64 / 2.0;
            let py = f64::from(window.y) + (y0 + y1) as f64 / 2.0;
            low_p[j * lw + i] = prior_sample(prior, guide, px, py);
        }
    }

    let n = lw * lh;
    let mut c = vec![0.0f64; n];
    let mut ci = vec![0.0f64; n];
    let mut cp = vec![0.0f64; n];
    let mut cii = vec![0.0f64; n];
    let mut cip = vec![0.0f64; n];
    for k in 0..n {
        let weight = CONFIDENCE_FLOOR + (2.0 * low_p[k] - 1.0).abs();
        let weighted = weight * low_i[k];
        c[k] = weight;
        ci[k] = weighted;
        cp[k] = weight * low_p[k];
        cii[k] = weighted * low_i[k];
        cip[k] = weighted * low_p[k];
    }
    let sc = box_sums(&c, lw, lh, r);
    let sci = box_sums(&ci, lw, lh, r);
    let scp = box_sums(&cp, lw, lh, r);
    let scii = box_sums(&cii, lw, lh, r);
    let scip = box_sums(&cip, lw, lh, r);
    let mut a = vec![0.0f64; n];
    let mut b = vec![0.0f64; n];
    for k in 0..n {
        let mean_i = sci[k] / sc[k];
        let mean_p = scp[k] / sc[k];
        let corr_ii = scii[k] / sc[k];
        let corr_ip = scip[k] / sc[k];
        let var = (corr_ii - mean_i * mean_i).max(0.0);
        let cov = corr_ip - mean_i * mean_p;
        a[k] = cov / (var + eps);
        b[k] = mean_p - a[k] * mean_i;
    }
    let sa = box_sums(&a, lw, lh, r);
    let sb = box_sums(&b, lw, lh, r);
    let mut mean_a = vec![0.0f64; n];
    let mut mean_b = vec![0.0f64; n];
    for j in 0..lh {
        for i in 0..lw {
            let count = box_count(i, j, lw, lh, r);
            mean_a[j * lw + i] = sa[j * lw + i] / count;
            mean_b[j * lw + i] = sb[j * lw + i] / count;
        }
    }

    let grid = |n: usize, blocks: usize| -> Vec<(usize, usize, f64)> {
        (0..n)
            .map(|i| {
                let u = ((i as f64 + 0.5) / s as f64 - 0.5).clamp(0.0, (blocks - 1) as f64);
                let u0 = u.floor();
                let i0 = u0 as usize;
                (i0, (i0 + 1).min(blocks - 1), u - u0)
            })
            .collect()
    };
    let xs = grid(ww, lw);
    let ys = grid(wh, lh);
    let mut out = Vec::with_capacity(ww * wh);
    for (y, &(v0, v1, fv)) in ys.iter().enumerate() {
        for (x, &(u0, u1, fu)) in xs.iter().enumerate() {
            let lerp = |m: &[f64]| {
                let top = m[v0 * lw + u0] + fu * (m[v0 * lw + u1] - m[v0 * lw + u0]);
                let bottom = m[v1 * lw + u0] + fu * (m[v1 * lw + u1] - m[v1 * lw + u0]);
                top + fv * (bottom - top)
            };
            let q = lerp(&mean_a) * at(x, y) + lerp(&mean_b);
            out.push((q.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    Some((window, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_is_bounded_whatever_the_output() {
        assert_eq!(subsampling(1024, 1024, 4), (1, 4));
        assert_eq!(subsampling(4096, 4096, 4), (4, 1));
        assert_eq!(subsampling(4096, 2048, 16), (4, 4));
        let (s, _) = subsampling(16384, 16384, 1);
        assert!(u64::from(16384u32.div_ceil(s)).pow(2) <= GRID_MAX);
    }

    #[test]
    fn box_sums_count_every_sample_of_the_clipped_box() {
        let plane = vec![1.0; 7 * 5];
        let sums = box_sums(&plane, 7, 5, 2);
        for j in 0..5 {
            for i in 0..7 {
                assert_eq!(sums[j * 7 + i], box_count(i, j, 7, 5, 2));
            }
        }
    }

    #[test]
    fn a_zero_prior_has_no_window_and_a_flat_guide_keeps_a_decided_prior() {
        let guide = vec![0.5f32; 64 * 64];
        let guide = Plane {
            width: 64,
            height: 64,
            values: &guide,
        };
        let zero = vec![0.0f32; 16 * 16];
        let zero = Plane {
            width: 16,
            height: 16,
            values: &zero,
        };
        assert!(upsample(&zero, &guide, 4, 1e-3).is_none());
        let one = vec![1.0f32; 16 * 16];
        let one = Plane {
            width: 16,
            height: 16,
            values: &one,
        };
        let (window, codes) = upsample(&one, &guide, 4, 1e-3).unwrap();
        assert_eq!((window.width, window.height), (64, 64));
        assert!(codes.iter().all(|&code| code == 255));
    }
}
