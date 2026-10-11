//! Resampling between the crop's square and the model's: area averaging down, Catmull-Rom up,
//! separable, pixel centres aligned. Sums run in `f64`; the horizontal pass's results are stored
//! at the precision named below and the vertical pass's in `f32`.
use super::{Coverage, crop::CropPlan, crop::reflect};
use crate::{
    Error,
    modules::{Region, Stage},
};
use std::ops::Range;

/// Separable resampling weights along one axis from `src` samples to `dst` samples for the
/// destination indices in `range`. Shrinking averages each destination pixel's exact footprint
/// (area resampling); enlarging uses Catmull-Rom.
#[derive(Clone, Debug)]
pub(crate) struct Taps {
    /// The first source index for each destination index; it can fall outside `0..src`.
    pub(crate) first: Vec<i64>,
    pub(crate) count: usize,
    /// `count` weights per destination index, summing to 1.
    pub(crate) weights: Vec<f64>,
}

impl Taps {
    pub(crate) fn new(src: usize, dst: usize, range: Range<usize>) -> Self {
        let f = src as f64 / dst as f64;
        let len = range.len();
        if f > 1.0 {
            let count = f.ceil() as usize + 1;
            let mut taps = Self {
                first: Vec::with_capacity(len),
                count,
                weights: Vec::with_capacity(len * count),
            };
            for i in range {
                let (a, b) = (i as f64 * f, (i + 1) as f64 * f);
                let j0 = a.floor() as i64;
                taps.first.push(j0);
                for k in 0..count as i64 {
                    let j = (j0 + k) as f64;
                    let overlap = (b.min(j + 1.0) - a.max(j)).max(0.0);
                    taps.weights.push(overlap / f);
                }
            }
            taps
        } else {
            let mut taps = Self {
                first: Vec::with_capacity(len),
                count: 4,
                weights: Vec::with_capacity(len * 4),
            };
            for i in range {
                let centre = (i as f64 + 0.5) * f;
                let u = centre - 0.5;
                let j0 = u.floor() as i64 - 1;
                taps.first.push(j0);
                for k in 0..4 {
                    taps.weights.push(catmull_rom((u - (j0 + k) as f64).abs()));
                }
            }
            taps
        }
    }

    /// The weights of destination index `i`.
    pub(crate) fn of(&self, i: usize) -> &[f64] {
        &self.weights[i * self.count..(i + 1) * self.count]
    }
}

pub(crate) fn catmull_rom(d: f64) -> f64 {
    if d < 1.0 {
        1.5 * d * d * d - 2.5 * d * d + 1.0
    } else if d < 2.0 {
        -0.5 * d * d * d + 2.5 * d * d - 4.0 * d + 2.0
    } else {
        0.0
    }
}

/// The crop's square reduced to the model's `n × n` in linear light, and the model's hole.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reduced {
    pub(crate) pixels: Vec<[f32; 3]>,
    /// 1 where any stage pixel with coverage above zero contributed to the model pixel, so no
    /// known model pixel carries what the region covers.
    pub(crate) hole: Vec<u8>,
}

/// Reduce the crop's square to `n × n` through `read`, which answers a region of the stage as
/// row-major linear pixels, asking for the square's inside (and the pixel or two past it an
/// enlarging filter reaches) a band of rows at a time, each at most `tile_pixels`; `keep` sees each
/// band as it is read, so a caller copies out what it needs of it without a second read. The
/// horizontal pass's results are held for every row read, `rows × n` pixels in `f32`, and each band
/// is dropped after its pass. `checkpoint` is called between bands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reduce(
    plan: &CropPlan,
    stage: Stage,
    n: usize,
    coverage: &Coverage,
    tile_pixels: u64,
    read: &mut dyn FnMut(Region) -> Result<Vec<[f32; 3]>, Error>,
    keep: &mut dyn FnMut(Region, &[[f32; 3]]),
    checkpoint: &dyn Fn() -> Result<(), Error>,
) -> Result<Reduced, Error> {
    let taps = Taps::new(plan.side as usize, n, 0..n);
    let k = taps.count;
    let column = |t: usize| {
        reflect(
            plan.origin.0 + taps.first[t / k] + (t % k) as i64,
            stage.width,
        )
    };
    let row = |t: usize| {
        reflect(
            plan.origin.1 + taps.first[t / k] + (t % k) as i64,
            stage.height,
        )
    };
    // Every horizontal tap's stage column, reflected into the stage.
    let columns: Vec<u32> = (0..n * k).map(column).collect();
    // What is read: the square's inside, grown to every tap with a weight, which reaches a pixel
    // or two past the square where it is enlarged (Catmull-Rom).
    let weighted = |t: &usize| taps.weights[*t] != 0.0;
    let inside = plan.inside;
    let (x0, x1) = (0..n * k)
        .filter(weighted)
        .map(|t| columns[t])
        .fold((inside.x0, inside.x1()), |(lo, hi), x| {
            (lo.min(x), hi.max(x + 1))
        });
    let (y0, y1) = (0..n * k)
        .filter(weighted)
        .map(row)
        .fold((inside.y0, inside.y1()), |(lo, hi), y| {
            (lo.min(y), hi.max(y + 1))
        });
    let span = Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    };
    let mut across = vec![[0_f32; 3]; span.height as usize * n];
    let mut across_hole = vec![0_u8; span.height as usize * n];
    let band = (tile_pixels / u64::from(span.width.max(1))).max(1) as u32;
    let mut y = span.y0;
    while y < span.y1() {
        checkpoint()?;
        let height = band.min(span.y1() - y);
        let region = Region {
            x0: span.x0,
            y0: y,
            width: span.width,
            height,
        };
        let pixels = read(region)?;
        if pixels.len() as u64 != region.pixels() {
            return Err(Error::internal(format!(
                "a stage read of {} pixels answered {}",
                region.pixels(),
                pixels.len()
            )));
        }
        keep(region, &pixels);
        for row in 0..height {
            let sy = y + row;
            let src = &pixels[(row * span.width) as usize..((row + 1) * span.width) as usize];
            let cover = coverage.row(sy);
            let dst = (sy - span.y0) as usize * n;
            for i in 0..n {
                let mut sum = [0_f64; 3];
                let mut hole = 0_u8;
                for (j, &weight) in taps.of(i).iter().enumerate() {
                    if weight == 0.0 {
                        continue;
                    }
                    let sx = columns[i * k + j];
                    let p = src[(sx - span.x0) as usize];
                    for (s, v) in sum.iter_mut().zip(p) {
                        *s += weight * f64::from(v);
                    }
                    if let Some((x0, cover)) = cover
                        && sx >= x0
                        && cover
                            .get((sx - x0) as usize)
                            .is_some_and(|&value| value > 0.0)
                    {
                        hole = 1;
                    }
                }
                across[dst + i] = sum.map(|s| s as f32);
                across_hole[dst + i] = hole;
            }
        }
        y += height;
    }
    checkpoint()?;
    let mut pixels = vec![[0_f32; 3]; n * n];
    let mut hole = vec![0_u8; n * n];
    for jy in 0..n {
        let mut sum = vec![[0_f64; 3]; n];
        for (j, &weight) in taps.of(jy).iter().enumerate() {
            if weight == 0.0 {
                continue;
            }
            let src = (row(jy * k + j) - span.y0) as usize * n;
            for i in 0..n {
                let p = across[src + i];
                for (s, v) in sum[i].iter_mut().zip(p) {
                    *s += weight * f64::from(v);
                }
                hole[jy * n + i] |= across_hole[src + i];
            }
        }
        for (out, s) in pixels[jy * n..(jy + 1) * n].iter_mut().zip(&sum) {
            *out = s.map(|v| v as f32);
        }
    }
    Ok(Reduced { pixels, hole })
}

/// One `n × n` model plane resized back to the crop's square over `window` (inside the square):
/// Catmull-Rom up, or area averaging when the square is smaller than the model, with taps past
/// the model's edge clamped to it.
pub(crate) fn upscale_plane(src: &[f32], n: usize, plan: &CropPlan, window: Region) -> Vec<f32> {
    let s = plan.side as usize;
    let cx0 = (i64::from(window.x0) - plan.origin.0) as usize;
    let cy0 = (i64::from(window.y0) - plan.origin.1) as usize;
    let (ww, wh) = (window.width as usize, window.height as usize);
    let tx = Taps::new(n, s, cx0..cx0 + ww);
    let ty = Taps::new(n, s, cy0..cy0 + wh);
    let clamp = |j: i64| j.clamp(0, n as i64 - 1) as usize;
    let m_lo = clamp(ty.first[0]);
    let m_hi = clamp(ty.first[wh - 1] + ty.count as i64 - 1) + 1;
    let mut across = vec![0_f64; (m_hi - m_lo) * ww];
    for m in m_lo..m_hi {
        let row = &src[m * n..(m + 1) * n];
        let dst = &mut across[(m - m_lo) * ww..(m - m_lo + 1) * ww];
        for (x, d) in dst.iter_mut().enumerate() {
            let mut sum = 0.0;
            for (j, &weight) in tx.of(x).iter().enumerate() {
                sum += weight * f64::from(row[clamp(tx.first[x] + j as i64)]);
            }
            *d = sum;
        }
    }
    let mut out = vec![0_f32; ww * wh];
    let mut sum = vec![0_f64; ww];
    for y in 0..wh {
        sum.fill(0.0);
        for (j, &weight) in ty.of(y).iter().enumerate() {
            if weight == 0.0 {
                continue;
            }
            let m = clamp(ty.first[y] + j as i64) - m_lo;
            for (d, v) in sum.iter_mut().zip(&across[m * ww..(m + 1) * ww]) {
                *d += weight * v;
            }
        }
        for (o, s) in out[y * ww..(y + 1) * ww].iter_mut().zip(&sum) {
            *o = *s as f32;
        }
    }
    out
}
