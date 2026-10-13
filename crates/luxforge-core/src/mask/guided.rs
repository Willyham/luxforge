//! The guided upsampler (`docs/design/ai-editing.md#the-guided-upsampler`): a model's
//! probabilities at the model's own resolution made into an 8-bit alpha raster at the output's
//! resolution, with its edges taken from a luminance guide at that output resolution.
//!
//! It is He and Sun's fast guided filter. The filter's coefficients are computed on a grid
//! subsampled from the output, with each box statistic weighted by how decided the prior is there
//! (`c = 1/16 + |2p − 1|`), so a sample the model was unsure of takes its value from the guide and
//! its decided neighbours rather than from its own half-way probability. The coefficients are then
//! smoothed, enlarged bilinearly and applied as `q = ā · I + b̄` at every output pixel against the
//! full-resolution guide, which is where an edge the model's reduction lost is recovered.
//!
//! **Transcription.** This is the production transcription of the `f64` reference in
//! `crates/luxforge-reference/src/guided.rs`, written in the same expressions and the same order so
//! the codes are bit-identical to it, not merely close: block means summed y-major from `0.0`, the
//! prior read bilinearly in the reference's spelling, box sums by an integral image built row by
//! row, the four-term difference in the reference's grouping, and the output rounded once.
//!
//! **Memory.** Everything is allocated over the **window**: the prior's support grown by the
//! filter's reach, clamped to the output. Nothing is allocated over the whole output. The working
//! planes live on the coefficient grid, which the subsampling keeps within [`GRID_MAX`] samples
//! however large the output is: at most nine `f64` planes of it and one integral image, 80 MiB at
//! the bound, which a 4096² output reaches only when its whole frame is the window; the result is
//! the window's codes, one byte a pixel.
use crate::{Cancel, Error};

/// The guided filter's radius in subsampled samples is about this many: the coefficient grid is
/// the output subsampled by `radius / SUBSAMPLED_RADIUS`, at least 1.
pub const SUBSAMPLED_RADIUS: u32 = 4;

/// The most samples the coefficient grid holds over the whole output.
pub const GRID_MAX: u64 = 1 << 20;

/// The weight an undecided prior sample still carries, beside `|2p − 1|`.
pub const CONFIDENCE_FLOOR: f64 = 0.0625;

/// The longest side an output may have: the selection artifact's cap, and well past any removal
/// region's window.
pub const OUTPUT_SIDE_MAX: u32 = 16384;

/// The default regularization, in squared luminance.
pub const EPS_DEFAULT: f64 = 1e-3;

/// A raster of `f32` samples, row-major: a prior of probabilities in `[0, 1]` (read clamped), or a
/// guide of luminance.
#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    pub width: u32,
    pub height: u32,
    pub values: &'a [f32],
}

impl Plane<'_> {
    fn check(&self, what: &str) -> Result<(), Error> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::validation(format!("the {what} is empty")));
        }
        if self.width > OUTPUT_SIDE_MAX || self.height > OUTPUT_SIDE_MAX {
            return Err(Error::resource_limit(format!(
                "the {what} is {}x{}, past the {OUTPUT_SIDE_MAX} px side",
                self.width, self.height
            )));
        }
        if self.values.len() != self.width as usize * self.height as usize {
            return Err(Error::validation(format!(
                "the {what} holds {} samples for {}x{}",
                self.values.len(),
                self.width,
                self.height
            )));
        }
        if !self.values.iter().all(|value| value.is_finite()) {
            return Err(Error::validation(format!(
                "the {what} holds a non-finite sample"
            )));
        }
        Ok(())
    }
}

/// The filter's settings: its radius in output pixels and its regularization.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub radius: u32,
    pub eps: f64,
}

impl Params {
    /// One prior cell in output pixels, at least 2, with the default regularization: the radius
    /// the prototype measured on the object removal corpus.
    pub fn for_sizes(prior: (u32, u32), output: (u32, u32)) -> Self {
        let cell = (f64::from(output.0) / f64::from(prior.0.max(1)))
            .max(f64::from(output.1) / f64::from(prior.1.max(1)));
        Self {
            radius: (cell.round() as u32).max(2),
            eps: EPS_DEFAULT,
        }
    }
}

/// The rectangle of the output the upsampler computed, in output pixels: everything outside it is
/// zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// What the upsampler made: its window and that window's alpha codes, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upsampled {
    pub window: Window,
    pub codes: Vec<u8>,
}

impl Upsampled {
    /// The window's codes placed in a zero raster of the whole output, `width × height` bytes: the
    /// alpha artifact a selection publishes.
    pub fn into_raster(self, width: u32, height: u32) -> Vec<u8> {
        let mut raster = vec![0u8; width as usize * height as usize];
        let (x, w) = (self.window.x as usize, self.window.width as usize);
        for (row, line) in self.codes.chunks_exact(w).enumerate() {
            let start = (self.window.y as usize + row) * width as usize + x;
            raster[start..start + w].copy_from_slice(line);
        }
        raster
    }
}

/// The subsampling factor of the coefficient grid and the filter's radius in its samples.
pub fn subsampling(width: u32, height: u32, radius: u32) -> (u32, u32) {
    let mut s = (radius / SUBSAMPLED_RADIUS).max(1);
    while u64::from(width.div_ceil(s)) * u64::from(height.div_ceil(s)) > GRID_MAX {
        s += 1;
    }
    let r = ((radius + s / 2) / s).max(1);
    (s, r)
}

/// The prior's support grown by the filter's reach, clamped to the output; `None` when no prior
/// sample is above zero. `O(prior)`; allocates nothing.
pub fn window(prior: &Plane<'_>, output: (u32, u32), radius: u32) -> Option<Window> {
    let (s, r) = subsampling(output.0, output.1, radius);
    let mut cells: Option<(u32, u32, u32, u32)> = None;
    for (j, row) in prior.values.chunks_exact(prior.width as usize).enumerate() {
        for (i, value) in row.iter().enumerate() {
            if *value > 0.0 {
                let (i, j) = (i as u32, j as u32);
                cells = Some(match cells {
                    None => (i, i, j, j),
                    Some((x0, x1, y0, y1)) => (x0.min(i), x1.max(i), y0.min(j), y1.max(j)),
                });
            }
        }
    }
    let (cx0, cx1, cy0, cy1) = cells?;
    let sx = f64::from(output.0) / f64::from(prior.width);
    let sy = f64::from(output.1) / f64::from(prior.height);
    let reach = f64::from((2 * r + 2) * s);
    let x0 = ((f64::from(cx0) - 0.5) * sx - reach).floor().max(0.0) as u32;
    let x1 = ((f64::from(cx1) + 1.5) * sx + reach)
        .ceil()
        .min(f64::from(output.0)) as u32;
    let y0 = ((f64::from(cy0) - 0.5) * sy - reach).floor().max(0.0) as u32;
    let y1 = ((f64::from(cy1) + 1.5) * sy + reach)
        .ceil()
        .min(f64::from(output.1)) as u32;
    (x0 < x1 && y0 < y1).then_some(Window {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// The prior, clamped to `[0, 1]`, bilinear between cell centres with the edges clamped, read at
/// continuous output positions. `kx` and `ky` are `w / W` and `h / H`, hoisted.
struct PriorReader<'a> {
    prior: Plane<'a>,
    kx: f64,
    ky: f64,
}

impl PriorReader<'_> {
    fn at(&self, i: usize, j: usize) -> f64 {
        f64::from(self.prior.values[j * self.prior.width as usize + i]).clamp(0.0, 1.0)
    }

    fn sample(&self, px: f64, py: f64) -> f64 {
        let u = (px * self.kx - 0.5).clamp(0.0, f64::from(self.prior.width - 1));
        let v = (py * self.ky - 0.5).clamp(0.0, f64::from(self.prior.height - 1));
        let u0 = u.floor();
        let v0 = v.floor();
        let fu = u - u0;
        let fv = v - v0;
        let (i0, j0) = (u0 as usize, v0 as usize);
        let i1 = (i0 + 1).min(self.prior.width as usize - 1);
        let j1 = (j0 + 1).min(self.prior.height as usize - 1);
        let top = self.at(i0, j0) + fu * (self.at(i1, j0) - self.at(i0, j0));
        let bottom = self.at(i0, j1) + fu * (self.at(i1, j1) - self.at(i0, j1));
        top + fv * (bottom - top)
    }
}

/// The box sums of one coefficient-grid plane, through one reused integral image.
struct Boxes {
    w: usize,
    h: usize,
    r: usize,
    integral: Vec<f64>,
}

impl Boxes {
    fn new(w: usize, h: usize, r: usize) -> Self {
        Self {
            w,
            h,
            r,
            integral: vec![0.0; (w + 1) * (h + 1)],
        }
    }

    /// `out[k]` = the sum of `plane` over the clipped `(2r + 1)²` box around sample `k`.
    fn sums(&mut self, plane: &[f64], out: &mut [f64]) {
        let (w, h, r) = (self.w, self.h, self.r);
        let stride = w + 1;
        let integral = &mut self.integral;
        for y in 0..h {
            let mut row = 0.0;
            for x in 0..w {
                row += plane[y * w + x];
                integral[(y + 1) * stride + x + 1] = integral[y * stride + x + 1] + row;
            }
        }
        for j in 0..h {
            let ya = j.saturating_sub(r);
            let yb = (j + r).min(h - 1);
            for i in 0..w {
                let xa = i.saturating_sub(r);
                let xb = (i + r).min(w - 1);
                out[j * w + i] = ((integral[(yb + 1) * stride + xb + 1]
                    - integral[ya * stride + xb + 1])
                    - integral[(yb + 1) * stride + xa])
                    + integral[ya * stride + xa];
            }
        }
    }

    fn count(&self, i: usize, j: usize) -> f64 {
        let xa = i.saturating_sub(self.r);
        let xb = (i + self.r).min(self.w - 1);
        let ya = j.saturating_sub(self.r);
        let yb = (j + self.r).min(self.h - 1);
        ((xb - xa + 1) * (yb - ya + 1)) as f64
    }
}

/// Upsample `prior` to the resolution of `guide` against it, with `params`: the window and its
/// codes, or `None` when no prior sample is above zero.
///
/// Refused by name before anything is allocated: an empty or oversized plane, a sample count that
/// is not the plane's area, a non-finite sample, a radius of zero or a regularization that is not
/// a positive finite number. `cancel` is checked once per coefficient-grid row and once per output
/// row. Deterministic: identical inputs give identical codes on every host.
pub fn upsample(
    prior: Plane<'_>,
    guide: Plane<'_>,
    params: Params,
    cancel: &Cancel,
) -> Result<Option<Upsampled>, Error> {
    prior.check("prior")?;
    guide.check("guide")?;
    if params.radius == 0 {
        return Err(Error::validation("the guided filter's radius must be positive"));
    }
    if !(params.eps.is_finite() && params.eps > 0.0) {
        return Err(Error::validation(
            "the guided filter's regularization must be a positive number",
        ));
    }
    let Some(window) = window(&prior, (guide.width, guide.height), params.radius) else {
        return Ok(None);
    };
    let (s, r) = subsampling(guide.width, guide.height, params.radius);
    let (s, r) = (s as usize, r as usize);
    let (ww, wh) = (window.width as usize, window.height as usize);
    let (lw, lh) = (ww.div_ceil(s), wh.div_ceil(s));
    let n = lw * lh;
    let ow = guide.width as usize;
    let (wx, wy) = (window.x as usize, window.y as usize);
    let at = |x: usize, y: usize| f64::from(guide.values[(wy + y) * ow + wx + x]);
    let reader = PriorReader {
        prior,
        kx: f64::from(prior.width) / f64::from(guide.width),
        ky: f64::from(prior.height) / f64::from(guide.height),
    };

    // Block means of the guide, accumulated in the reference's per-block order: each block's
    // pixels arrive y-major, then x, from 0.0.
    let mut low_i = vec![0.0f64; n];
    for y in 0..wh {
        let row = &mut low_i[(y / s) * lw..(y / s + 1) * lw];
        for x in 0..ww {
            row[x / s] += at(x, y);
        }
    }
    let mut low_p = vec![0.0f64; n];
    for j in 0..lh {
        cancel.check()?;
        let (y0, y1) = (j * s, ((j + 1) * s).min(wh));
        for i in 0..lw {
            let (x0, x1) = (i * s, ((i + 1) * s).min(ww));
            let k = j * lw + i;
            low_i[k] /= ((y1 - y0) * (x1 - x0)) as f64;
            let px = f64::from(window.x) + (x0 + x1) as f64 / 2.0;
            let py = f64::from(window.y) + (y0 + y1) as f64 / 2.0;
            low_p[k] = reader.sample(px, py);
        }
    }

    // The five weighted statistics, one plane at a time through one integral image.
    let mut boxes = Boxes::new(lw, lh, r);
    let mut plane = vec![0.0f64; n];
    let mut weighted_sums = |statistic: &dyn Fn(f64, f64) -> f64| -> Result<Vec<f64>, Error> {
        cancel.check()?;
        for k in 0..n {
            plane[k] = statistic(low_i[k], low_p[k]);
        }
        let mut sums = vec![0.0f64; n];
        boxes.sums(&plane, &mut sums);
        Ok(sums)
    };
    let weight = |p: f64| CONFIDENCE_FLOOR + (2.0 * p - 1.0).abs();
    let sc = weighted_sums(&|_, p| weight(p))?;
    let sci = weighted_sums(&|i, p| weight(p) * i)?;
    let scp = weighted_sums(&|_, p| weight(p) * p)?;
    let mut scii = weighted_sums(&|i, p| (weight(p) * i) * i)?;
    let mut scip = weighted_sums(&|i, p| (weight(p) * i) * p)?;
    drop(weighted_sums);
    drop(low_p);
    // a and b take the places of the two second moments.
    for k in 0..n {
        let mean_i = sci[k] / sc[k];
        let mean_p = scp[k] / sc[k];
        let corr_ii = scii[k] / sc[k];
        let corr_ip = scip[k] / sc[k];
        let var = (corr_ii - mean_i * mean_i).max(0.0);
        let cov = corr_ip - mean_i * mean_p;
        let a = cov / (var + params.eps);
        scii[k] = a;
        scip[k] = mean_p - a * mean_i;
    }
    let (a, b) = (scii, scip);
    drop((sc, sci, scp));
    cancel.check()?;
    let mut mean_a = low_i;
    boxes.sums(&a, &mut mean_a);
    let mut mean_b = plane;
    boxes.sums(&b, &mut mean_b);
    drop((a, b));
    for j in 0..lh {
        for i in 0..lw {
            let count = boxes.count(i, j);
            mean_a[j * lw + i] /= count;
            mean_b[j * lw + i] /= count;
        }
    }
    drop(boxes);

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
    let mut codes = Vec::with_capacity(ww * wh);
    for (y, &(v0, v1, fv)) in ys.iter().enumerate() {
        cancel.check()?;
        for (x, &(u0, u1, fu)) in xs.iter().enumerate() {
            let lerp = |m: &[f64]| {
                let top = m[v0 * lw + u0] + fu * (m[v0 * lw + u1] - m[v0 * lw + u0]);
                let bottom = m[v1 * lw + u0] + fu * (m[v1 * lw + u1] - m[v1 * lw + u0]);
                top + fv * (bottom - top)
            };
            let q = lerp(&mean_a) * at(x, y) + lerp(&mean_b);
            codes.push((q.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    Ok(Some(Upsampled { window, codes }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn plane(width: u32, height: u32, values: &[f32]) -> Plane<'_> {
        Plane {
            width,
            height,
            values,
        }
    }

    #[test]
    fn malformed_inputs_are_refused_by_name_before_anything_runs() {
        let prior = vec![0.5f32; 4];
        let guide = vec![0.5f32; 16];
        let params = Params {
            radius: 2,
            eps: EPS_DEFAULT,
        };
        let never = Cancel::never();
        let error = upsample(plane(2, 2, &prior), plane(4, 3, &guide), params, &never).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("guide holds 16 samples for 4x3"));
        let error = upsample(
            plane(2, 2, &prior),
            plane(4, 4, &guide),
            Params { radius: 0, ..params },
            &never,
        )
        .unwrap_err();
        assert!(error.detail.contains("radius"));
        let nan = [f32::NAN, 0.0, 0.0, 0.0];
        let error = upsample(plane(2, 2, &nan), plane(4, 4, &guide), params, &never).unwrap_err();
        assert!(error.detail.contains("prior holds a non-finite sample"));
    }

    #[test]
    fn a_cancelled_upsample_answers_cancelled() {
        let prior = vec![1.0f32; 16];
        let guide = vec![0.5f32; 64 * 64];
        let cancel = Cancel::new();
        cancel.cancel();
        let error = upsample(
            plane(4, 4, &prior),
            plane(64, 64, &guide),
            Params::for_sizes((4, 4), (64, 64)),
            &cancel,
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
    }

    #[test]
    fn the_raster_places_the_window_and_leaves_the_rest_zero() {
        let upsampled = Upsampled {
            window: Window {
                x: 1,
                y: 2,
                width: 2,
                height: 1,
            },
            codes: vec![7, 9],
        };
        let raster = upsampled.into_raster(4, 4);
        assert_eq!(raster.iter().filter(|&&code| code != 0).count(), 2);
        assert_eq!((raster[9], raster[10]), (7, 9));
    }
}
