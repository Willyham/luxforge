//! Grain: estimated in a ring around the region, re-synthesised in the fill where enlarging the
//! model's output lost it. All values are the fill's codes ([`super::encode`]).
//!
//! Per-channel noise is estimated in the flattest parts of the ring with a robust high-pass
//! estimate (a clipped RMS of Immerkær's kernel) at two tap spacings, whose ratio gives the grain's
//! spatial correlation. The same estimate on the enlarged fill shows what the fill already has, and
//! grain makes up the deficit: deterministic Gaussian grain from the candidate's seed, filtered to
//! the ring's correlation, correlated between channels as the ring is, and added where coverage is
//! above zero.
use super::{Coverage, Planes};
use crate::modules::Region;

/// Fewest high-pass samples a grain estimate is made from.
pub(crate) const MIN_SAMPLES: usize = 64;
/// Most high-pass samples read; larger areas are read on a stride.
pub(crate) const MAX_SAMPLES: usize = 1 << 20;
/// Side of the blocks the ring is split into to find its flattest parts.
const BLOCK: usize = 16;
/// Share of the ring's blocks, flattest first, that noise is estimated from.
const FLAT_SHARE: f64 = 0.3;
/// Largest spatial correlation grain is synthesised with, as a Gaussian σ in pixels.
pub(crate) const MAX_CORRELATION_PX: f64 = 1.5;
/// Rows of grain generated at once, so its fields stay a band's size whatever the region's.
const GRAIN_BAND: u32 = 256;

/// Grain measured in the ring around the region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RingGrain {
    /// Robust σ of the spacing-1 high-pass per channel.
    pub(crate) highpass: [f32; 3],
    /// Spatial correlation: the σ in pixels of the Gaussian that, filtering white noise, gives the
    /// ring's ratio of spacing-1 to spacing-2 high-pass.
    pub(crate) correlation_px: f32,
    /// Mean correlation of the high-pass between channels, in `0..=0.98`.
    pub(crate) channel_correlation: f32,
}

/// Immerkær's noise kernel `[1 −2 1; −2 4 −2; 1 −2 1]` with its taps `s` pixels apart, at `(x, y)`.
/// It ignores linear shading. On white noise of σ 1 its response has σ 6 at any spacing; on
/// spatially correlated noise the spacing-1 response falls further than the spacing-2 one.
pub(crate) fn highpass(get: impl Fn(usize, usize) -> f32, x: usize, y: usize, s: usize) -> f32 {
    let (x0, x1, y0, y1) = (x - s, x + s, y - s, y + s);
    get(x0, y0) + get(x1, y0) + get(x0, y1) + get(x1, y1)
        - 2.0 * (get(x, y0) + get(x0, y) + get(x1, y) + get(x, y1))
        + 4.0 * get(x, y)
}

/// A robust σ: the RMS deviation from the median of the samples within three σ of it, the σ for
/// the cut being `1.4826 × median absolute deviation` (at least half a code), corrected for the cut
/// (a Gaussian keeps 97.33% of its variance within ±3σ). Reorders `samples`.
pub(crate) fn robust_sigma(samples: &mut [f32]) -> f32 {
    let mid = samples.len() / 2;
    let median = *samples.select_nth_unstable_by(mid, f32::total_cmp).1;
    let mut dev: Vec<f32> = samples.iter().map(|v| (v - median).abs()).collect();
    let mad = *dev.select_nth_unstable_by(mid, f32::total_cmp).1;
    let cut = 3.0 * (1.4826 * mad).max(0.5);
    let (mut sum, mut n) = (0_f64, 0_usize);
    for &d in &dev {
        if d <= cut {
            sum += f64::from(d).powi(2);
            n += 1;
        }
    }
    ((sum / n.max(1) as f64) / 0.9733).sqrt() as f32
}

/// A Gaussian of σ `px` whose 2-D outer product has unit energy, so white noise of σ 1 filtered by
/// it keeps σ 1. Below 0.05 px it is the identity: white noise.
pub(crate) fn grain_kernel(px: f64) -> Vec<f64> {
    if px < 0.05 {
        return vec![1.0];
    }
    let r = (3.0 * px).ceil() as i64;
    let k: Vec<f64> = (-r..=r)
        .map(|d| (-((d * d) as f64) / (2.0 * px * px)).exp())
        .collect();
    let energy: f64 = k.iter().map(|v| v * v).sum();
    k.iter().map(|v| v / energy.sqrt()).collect()
}

/// The σ of the spacing-`s` high-pass on unit-σ grain made with `kernel`. Both are separable, so
/// the response's variance is `(Σ (d_s ⊛ k)²)²` with `d_s = [1, 0…, −2, 0…, 1]`.
pub(crate) fn grain_highpass_gain(kernel: &[f64], s: usize) -> f64 {
    let len = kernel.len() as i64;
    let at = |j: i64| {
        if (0..len).contains(&j) {
            kernel[j as usize]
        } else {
            0.0
        }
    };
    let s = s as i64;
    (-s..len + s)
        .map(|i| {
            let v = at(i - s) - 2.0 * at(i) + at(i + s);
            v * v
        })
        .sum()
}

/// The grain correlation σ whose spacing-1 to spacing-2 high-pass ratio is `ratio` (1 for white
/// noise, falling as the correlation grows), by bisection within `0..=MAX_CORRELATION_PX`.
pub(crate) fn correlation_for_ratio(ratio: f64) -> f64 {
    let r = |px: f64| {
        let k = grain_kernel(px);
        grain_highpass_gain(&k, 1) / grain_highpass_gain(&k, 2)
    };
    if ratio >= 1.0 {
        return 0.0;
    }
    if ratio <= r(MAX_CORRELATION_PX) {
        return MAX_CORRELATION_PX;
    }
    let (mut lo, mut hi) = (0.0, MAX_CORRELATION_PX);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if r(mid) > ratio {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Separable dilation of a boolean grid by a square of radius `r`.
pub(crate) fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut rows = vec![false; w * h];
    for (src, out) in mask.chunks_exact(w).zip(rows.chunks_exact_mut(w)) {
        let mut count = src[..r.min(w)].iter().filter(|&&v| v).count();
        for x in 0..w {
            if x + r < w && src[x + r] {
                count += 1;
            }
            if x > r && src[x - r - 1] {
                count -= 1;
            }
            out[x] = count > 0;
        }
    }
    let mut counts = vec![0_u32; w];
    for row in rows.chunks_exact(w).take(r.min(h)) {
        for (n, &v) in counts.iter_mut().zip(row) {
            *n += u32::from(v);
        }
    }
    let mut out = vec![false; w * h];
    for y in 0..h {
        if y + r < h {
            for (n, &v) in counts.iter_mut().zip(&rows[(y + r) * w..(y + r + 1) * w]) {
                *n += u32::from(v);
            }
        }
        if y > r {
            for (n, &v) in counts.iter_mut().zip(&rows[(y - r - 1) * w..(y - r) * w]) {
                *n -= u32::from(v);
            }
        }
        for (o, &n) in out[y * w..(y + 1) * w].iter_mut().zip(&counts) {
            *o = n > 0;
        }
    }
    out
}

/// Samples of one block of the ring.
#[derive(Default)]
struct Block {
    h1: [Vec<f32>; 3],
    h2: [Vec<f32>; 3],
}

/// Grain in the ring of `width` around the region's `bounds`, read from `input`, the codes over
/// `area`: pixels with coverage 0 within `width` of the region (a square neighbourhood) whose 9×9
/// neighbourhood is clear of it and not clipped. The ring is split into 16 px blocks and noise is
/// read from the flattest 30% of them, ranked by the energy of the spacing-4 high-pass (where
/// texture shows, and nearly independent of the spacing-1 and spacing-2 samples, so the ranking
/// does not bias their ratio), so texture and edges do not pass for grain. `None` when the ring
/// holds too few samples.
pub(crate) fn ring_grain(
    input: &Planes,
    area: Region,
    coverage: &Coverage,
    bounds: Region,
    width: u32,
) -> Option<RingGrain> {
    let win = super::crop::grow_within(bounds, width + 4, area);
    let (ww, wh) = (win.width as usize, win.height as usize);
    if ww < 9 || wh < 9 {
        return None;
    }
    let covered: Vec<bool> = coverage.over(win).iter().map(|&v| v > 0.0).collect();
    let near = dilate(&covered, ww, wh, width as usize);
    let blocked = dilate(&covered, ww, wh, 4);
    let (ox, oy) = ((win.x0 - area.x0) as usize, (win.y0 - area.y0) as usize);
    let aw = input.width;
    let (bw, bh) = (ww.div_ceil(BLOCK), wh.div_ceil(BLOCK));
    let step = ((ww * wh) as f64 / MAX_SAMPLES as f64).ceil().max(1.0) as usize;
    let mut blocks: Vec<(f64, Block)> = Vec::new();
    for b in (0..bw * bh).step_by(step) {
        let (bx, by) = (b % bw * BLOCK, b / bw * BLOCK);
        let mut block = Block::default();
        let mut texture = 0_f64;
        for y in by.max(4)..(by + BLOCK).min(wh - 4) {
            for x in bx.max(4)..(bx + BLOCK).min(ww - 4) {
                let i = y * ww + x;
                if !near[i] || blocked[i] {
                    continue;
                }
                let (gx, gy) = (ox + x, oy + y);
                if (0..3).any(|c| {
                    let v = input.c[c][gy * aw + gx];
                    v <= 2.0 || v >= 253.0
                }) {
                    continue;
                }
                for c in 0..3 {
                    let plane = &input.c[c];
                    let get = |px: usize, py: usize| plane[py * aw + px];
                    block.h1[c].push(highpass(get, gx, gy, 1));
                    block.h2[c].push(highpass(get, gx, gy, 2));
                    texture += f64::from(highpass(get, gx, gy, 4)).powi(2);
                }
            }
        }
        let count = block.h1[0].len();
        if count < BLOCK * BLOCK / 4 {
            continue;
        }
        blocks.push((texture / count as f64, block));
    }
    blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let want = ((blocks.len() as f64 * FLAT_SHARE).ceil() as usize).max(1);
    let mut pooled = Block::default();
    for (k, (_, block)) in blocks.iter().enumerate() {
        if k >= want && pooled.h1[0].len() >= MIN_SAMPLES {
            break;
        }
        for c in 0..3 {
            pooled.h1[c].extend_from_slice(&block.h1[c]);
            pooled.h2[c].extend_from_slice(&block.h2[c]);
        }
    }
    if pooled.h1[0].len() < MIN_SAMPLES {
        return None;
    }
    let raw = pooled.h1.clone();
    let mut highpass = [0.0_f32; 3];
    let mut coarse = 0.0_f64;
    for ((hp, h1), h2) in highpass.iter_mut().zip(&mut pooled.h1).zip(&mut pooled.h2) {
        *hp = robust_sigma(h1);
        coarse += f64::from(robust_sigma(h2));
    }
    let fine: f64 = highpass.iter().map(|&v| f64::from(v)).sum();
    let ratio = if coarse > 0.0 { fine / coarse } else { 1.0 };
    Some(RingGrain {
        highpass,
        correlation_px: correlation_for_ratio(ratio) as f32,
        channel_correlation: channel_correlation(&raw, highpass),
    })
}

/// Mean pairwise Pearson correlation of the high-pass between channels, over samples within three
/// robust σ in every channel (so edges do not dominate), clamped to `0..=0.98`.
fn channel_correlation(samples: &[Vec<f32>; 3], sigma: [f32; 3]) -> f32 {
    let keep: Vec<usize> = (0..samples[0].len())
        .filter(|&i| (0..3).all(|c| samples[c][i].abs() <= 3.0 * sigma[c].max(1e-3)))
        .collect();
    if keep.len() < MIN_SAMPLES {
        return 0.0;
    }
    let mean = |c: usize| {
        keep.iter().map(|&i| f64::from(samples[c][i])).sum::<f64>() / keep.len() as f64
    };
    let m = [mean(0), mean(1), mean(2)];
    let cov = |a: usize, b: usize| {
        keep.iter()
            .map(|&i| (f64::from(samples[a][i]) - m[a]) * (f64::from(samples[b][i]) - m[b]))
            .sum::<f64>()
    };
    let var = [cov(0, 0), cov(1, 1), cov(2, 2)];
    let mut total = 0.0;
    for (a, b) in [(0, 1), (0, 2), (1, 2)] {
        let d = (var[a] * var[b]).sqrt();
        total += if d > 0.0 { cov(a, b) / d } else { 0.0 };
    }
    ((total / 3.0) as f32).clamp(0.0, 0.98)
}

/// The robust spacing-1 high-pass σ of planes where coverage is 1 over the whole 3×3
/// neighbourhood, per channel; `None` with too few such pixels.
pub(crate) fn fill_stats(fill: &Planes, cov: &[f32]) -> Option<[f32; 3]> {
    let (w, h) = (fill.width, fill.height);
    if w < 3 || h < 3 {
        return None;
    }
    let stride = ((w * h) as f64 / MAX_SAMPLES as f64)
        .sqrt()
        .ceil()
        .max(1.0) as usize;
    let mut samples: [Vec<f32>; 3] = Default::default();
    for y in (1..h - 1).step_by(stride) {
        for x in (1..w - 1).step_by(stride) {
            let full = (0..3).all(|dy| (0..3).all(|dx| cov[(y + dy - 1) * w + x + dx - 1] >= 1.0));
            if !full {
                continue;
            }
            for (c, s) in samples.iter_mut().enumerate() {
                let p = &fill.c[c];
                s.push(highpass(|px, py| p[py * w + px], x, y, 1));
            }
        }
    }
    if samples[0].len() < MIN_SAMPLES {
        return None;
    }
    let mut out = [0.0_f32; 3];
    for c in 0..3 {
        out[c] = robust_sigma(&mut samples[c]);
    }
    Some(out)
}

/// SplitMix64's finaliser.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A standard normal value determined by the seed, a stream and absolute stage coordinates, so
/// grain does not depend on the order or window it is generated in.
pub(crate) fn gaussian(seed: u64, stream: u64, x: i64, y: i64) -> f64 {
    let key = mix(seed ^ mix(stream.wrapping_mul(0x9e37_79b9_7f4a_7c15)))
        ^ mix((x as u64) ^ (y as u64).rotate_left(32));
    let a = mix(key);
    let b = mix(key ^ 0xd1b5_4a32_d192_ed03);
    let u1 = ((a >> 11) as f64 + 1.0) / (1_u64 << 53) as f64;
    let u2 = (b >> 11) as f64 / (1_u64 << 53) as f64;
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// A unit-σ Gaussian field over `rect`, white noise filtered by the separable `kernel`.
pub(crate) fn grain_field(seed: u64, stream: u64, rect: Region, kernel: &[f64]) -> Vec<f32> {
    let r = kernel.len() / 2;
    let (w, h) = (rect.width as usize, rect.height as usize);
    let (pw, ph) = (w + 2 * r, h + 2 * r);
    let mut white = vec![0_f64; pw * ph];
    for y in 0..ph {
        for x in 0..pw {
            white[y * pw + x] = gaussian(
                seed,
                stream,
                i64::from(rect.x0) + x as i64 - r as i64,
                i64::from(rect.y0) + y as i64 - r as i64,
            );
        }
    }
    let mut rows = vec![0_f64; w * ph];
    for y in 0..ph {
        for x in 0..w {
            rows[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(j, k)| k * white[y * pw + x + j])
                .sum();
        }
    }
    let mut out = vec![0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(j, k)| k * rows[(y + j) * w + x])
                .sum::<f64>() as f32;
        }
    }
    out
}

/// Adds grain made with `kernel`, of per-channel σ `sigma` and inter-channel correlation `rho`, to
/// the fill over `window` wherever its coverage `cov` is above zero, generating it over the
/// region's `bounds` a band of rows at a time. The grain at a pixel depends only on the seed and
/// its stage coordinates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_grain(
    fill: &mut Planes,
    cov: &[f32],
    window: Region,
    bounds: Region,
    sigma: [f32; 3],
    rho: f32,
    kernel: &[f64],
    seed: u64,
) {
    let (a, b) = (rho.sqrt(), (1.0 - rho).sqrt());
    let ww = window.width as usize;
    let bw = bounds.width as usize;
    let ox = (bounds.x0 - window.x0) as usize;
    let mut y0 = bounds.y0;
    while y0 < bounds.y1() {
        let band = Region {
            x0: bounds.x0,
            y0,
            width: bounds.width,
            height: GRAIN_BAND.min(bounds.y1() - y0),
        };
        let shared = grain_field(seed, 0, band, kernel);
        let oy = (y0 - window.y0) as usize;
        for c in 0..3 {
            if sigma[c] <= 0.0 {
                continue;
            }
            let own = grain_field(seed, c as u64 + 1, band, kernel);
            for y in 0..band.height as usize {
                for x in 0..bw {
                    let i = (y + oy) * ww + x + ox;
                    if cov[i] > 0.0 {
                        let g = y * bw + x;
                        fill.c[c][i] += sigma[c] * (a * shared[g] + b * own[g]);
                    }
                }
            }
        }
        y0 += band.height;
    }
}
