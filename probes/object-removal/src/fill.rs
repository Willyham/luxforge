//! Filling a removal region: crop with context, run the inpainting model at its native size,
//! bring the result back to full resolution with matching grain, and composite it so that every
//! pixel outside the region is byte-identical to the input.
//!
//! The stages, each timed in the [`Report`]:
//!
//! 1. **Crop and resample.** The region's bounds grow by `max(min_margin, margin × longer side)`
//!    on every side and become a square of the grown longer side, centred on the region and slid
//!    inside the image. Where the image is smaller than the square on an axis, the square is
//!    padded past the edge by reflection (image and region alike), never stretched, so resizing
//!    to the model's square keeps geometry undistorted. The square is area-averaged down to the
//!    model size (Catmull-Rom when it is smaller than the model). A model pixel is part of the
//!    hole when any source pixel with non-zero coverage contributed to it, so no known model pixel
//!    carries the object; the hole is zeroed in the model's input. Growing the hole further, to
//!    give the seam correction a wider band, cost the model context next to the region's edge and
//!    measured worse on the synthetic ground truth, so it is not grown.
//! 2. **Model.** LaMa takes RGB as `code / 255` and the hole as 1, and returns `0..255` codes. The
//!    pinned export zeroes the hole and keeps the known pixels itself (`fill_lama_contract`).
//! 3. **Upscale.** The output is resized back to the square's size (Catmull-Rom), only over the
//!    window the later stages need.
//! 4. **Grain.** Per-channel noise is estimated in the flattest parts of a ring around the region
//!    with a robust high-pass estimate (a clipped RMS of Immerkær's kernel) at two
//!    tap spacings, whose ratio gives the grain's spatial correlation. The same estimate on the
//!    upscaled fill shows what the fill already has. When the model output was enlarged
//!    (`scale > 1`), grain makes up the deficit: deterministic Gaussian grain from `seed`, filtered
//!    to the ring's correlation, correlated between channels as the ring is, and added where
//!    coverage is non-zero.
//! 5. **Blend.** First a seam correction: the difference between the input and the fill in the
//!    band of background the model regenerated around the region is extended into the region by
//!    pyramid push-pull and added, so the fill's colour and tone meet the input's at the edge.
//!    Then a Laplacian pyramid carries the remaining difference across the region's edge band by
//!    band, detail over a few pixels and coarser bands further. The input's pixels under the
//!    region enter neither. Without the pyramid blend the fill meets the input only through the
//!    region's own soft edge.
//! 6. **Composite.** `input × (1 − coverage) + blended × coverage`: pixels with coverage 0 are
//!    copied byte for byte and pixels with coverage 255 are fully replaced.

use std::ops::Range;
use std::time::Instant;

use crate::image::Rgb8;
use crate::mask::{Mask, RectPx};
use crate::runtime::{self, Session};
use crate::tensor::Tensor;

#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Context around the region's bounds, as a share of its longer side. At 1.0 the region spans
    /// about a third of the model's input. OpenStill's 0.4, where it spans over half, makes the
    /// pinned LaMa leave dark blobs in flat areas (measured by the channel-order test's natural
    /// holes and recorded in the fill evidence).
    pub margin: f32,
    /// Context never smaller than this, in source pixels.
    pub min_margin: u32,
    /// The model's fixed square input size.
    pub model_size: u32,
    /// Re-synthesise grain matched to the ring around the region.
    pub grain: bool,
    /// Blend with the pyramid seam correction and Laplacian detail blend instead of a plain alpha
    /// over the region's own feathered edge.
    pub pyramid_blend: bool,
    /// Seed for grain synthesis; the same seed gives the same bytes.
    pub seed: u64,
    /// Feed the model BGR instead of RGB. OpenCV's reference feeds BGR, but LaMa trains on RGB, the
    /// export's graph does not swap channels, and the quadrant test favours RGB at the default
    /// margin, so the default is RGB.
    pub bgr: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            margin: 1.0,
            min_margin: 48,
            model_size: 512,
            grain: true,
            pyramid_blend: true,
            seed: 0,
            bgr: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// The crop sent to the model, in source pixels.
    pub crop: RectPx,
    /// Source pixels per model pixel.
    pub scale: f32,
    /// Estimated per-channel grain standard deviation in the ring, in 8-bit codes.
    pub grain_sigma: [f32; 3],
    /// Stage name and milliseconds, in order.
    pub timings_ms: Vec<(String, f64)>,
}

/// Where the model's square sits in the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropPlan {
    /// The region's tight bounds.
    pub bounds: RectPx,
    /// Context added on every side of the bounds before squaring, in source pixels.
    pub context: u32,
    /// The square's top-left corner; negative where it is padded past the top or left edge.
    pub origin: (i64, i64),
    /// The square's side in source pixels.
    pub side: u32,
    /// The part of the square inside the image (the report's `crop`).
    pub inside: RectPx,
}

impl CropPlan {
    /// Reflected padding outside the image: left, top, right, bottom.
    pub fn padding(&self) -> [u32; 4] {
        let side = self.side as i64;
        let i = self.inside;
        [
            (i.x as i64 - self.origin.0) as u32,
            (i.y as i64 - self.origin.1) as u32,
            (self.origin.0 + side - (i.x + i.width) as i64) as u32,
            (self.origin.1 + side - (i.y + i.height) as i64) as u32,
        ]
    }
}

/// What `remove` decided and produced besides its result, for inspection and evidence.
#[derive(Clone, Debug)]
pub struct Details {
    pub plan: CropPlan,
    /// The model's input at `model_size`², shown in RGB whichever order was fed, hole zeroed.
    pub model_input: Rgb8,
    /// The model's hole at `model_size`²: 255 where the model was asked to fill.
    pub model_mask: Vec<u8>,
    /// The model's output at `model_size`², in RGB, clamped to 8 bits.
    pub model_output: Rgb8,
    /// The smallest and largest raw value the model returned.
    pub output_range: (f32, f32),
    /// The window the upscale, grain, blend and composite worked over: the region's bounds plus
    /// the blend's reach, inside the crop.
    pub window: RectPx,
    /// Laplacian pyramid levels, 0 for the plain blend.
    pub levels: usize,
    /// Width of the ring the grain was estimated in.
    pub ring_width: u32,
    /// Robust grain σ of the upscaled fill before grain, per channel, in 8-bit codes.
    pub fill_sigma: [f32; 3],
    /// Grain σ added, per channel (0 where the fill already matched the ring or grain was off).
    pub grain_added: [f32; 3],
    /// Correlation of grain between channels, estimated in the ring.
    pub grain_correlation: f32,
    /// Spatial correlation of grain, as the σ in pixels of the Gaussian it is synthesised with,
    /// estimated in the ring (0 for white noise).
    pub grain_correlation_px: f32,
    /// Source pixels in the band the seam correction measured (0 for the plain blend).
    pub seam_band: usize,
}

/// Removes the masked region. `lama` is a loaded LaMa session (`image`, `mask` → `output`).
pub fn remove(
    image: &Rgb8,
    region: &Mask,
    lama: &mut dyn Session,
    params: &Params,
) -> Result<(Rgb8, Report), String> {
    remove_detailed(image, region, lama, params).map(|(out, report, _)| (out, report))
}

/// [`remove`], also returning the model's input and output and the decisions it made.
pub fn remove_detailed(
    image: &Rgb8,
    region: &Mask,
    lama: &mut dyn Session,
    params: &Params,
) -> Result<(Rgb8, Report, Details), String> {
    let size = (image.width, image.height);
    if region.image != size {
        return Err(format!(
            "the region is for a {}×{} image, not {}×{}",
            region.image.0, region.image.1, size.0, size.1
        ));
    }
    if params.model_size < 8 || params.margin.is_nan() || params.margin < 0.0 {
        return Err("model_size must be at least 8 and margin non-negative".into());
    }
    let bounds = region.bounds().ok_or("the region is empty")?;
    let n = params.model_size as usize;
    let mut clock = Stopwatch::new();

    // 1. Crop and resample.
    let plan = plan_crop(bounds, size, params);
    let scale = plan.side as f32 / n as f32;
    let (model_in, hole) = model_input(image, region, &plan, n);
    let order = if params.bgr { [2, 1, 0] } else { [0, 1, 2] };
    let mut image_t = Vec::with_capacity(3 * n * n);
    for &c in &order {
        image_t.extend(model_in.c[c].iter().map(|v| v / 255.0));
    }
    let image_t = Tensor::f32([1, 3, n, n], image_t);
    let mask_t = Tensor::f32([1, 1, n, n], hole.iter().map(|&h| h as f32).collect());
    clock.lap("crop and resample");

    // 2. Model.
    let mut outputs = lama.run(&[("image", &image_t), ("mask", &mask_t)])?;
    let raw = runtime::take(&mut outputs, "output")?.into_f32()?;
    if raw.len() != 3 * n * n {
        return Err(format!(
            "the model returned {} values, expected 3 × {n}²",
            raw.len()
        ));
    }
    let output_range = raw
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    let mut model_out = Planes::new(n, n);
    for (k, &c) in order.iter().enumerate() {
        model_out.c[c] = raw[k * n * n..(k + 1) * n * n]
            .iter()
            .map(|v| v.clamp(0.0, 255.0))
            .collect();
    }
    clock.lap("model");

    // 3. Upscale, over the window the blend can reach.
    let levels = if params.pyramid_blend {
        blend_levels(scale)
    } else {
        0
    };
    let reach = if levels > 0 { 4u32 << levels } else { 0 };
    let window = grow_within(bounds, reach, plan.inside);
    let mut fill = upscale(&model_out, &plan, window);
    let cov = coverage(region, window);
    clock.lap("upscale");

    // 4. Grain.
    let ring_width = ring_width(bounds);
    let ring = ring_grain(image, region, bounds, ring_width);
    let inner = fill_stats(&fill, &cov);
    let mut grain_sigma = [0.0f32; 3];
    let mut fill_sigma = [0.0f32; 3];
    let mut grain_added = [0.0f32; 3];
    let (mut grain_correlation, mut grain_correlation_px) = (0.0, 0.0);
    if let Some(ring) = &ring {
        grain_correlation = ring.channel_correlation;
        grain_correlation_px = ring.correlation_px;
        let kernel = grain_kernel(ring.correlation_px as f64);
        let gain = grain_highpass_gain(&kernel, 1) as f32;
        for c in 0..3 {
            grain_sigma[c] = ring.highpass[c] / gain;
            fill_sigma[c] = inner.map_or(0.0, |s| s[c] / gain);
            // Grain makes up only the fine texture that enlarging the model's output lost: the
            // ring's high-pass energy the upscaled fill lacks.
            if params.grain && scale > 1.0 {
                let deficit = ring.highpass[c].powi(2) - inner.map_or(0.0, |s| s[c].powi(2));
                grain_added[c] = deficit.max(0.0).sqrt() / gain;
            }
        }
        if grain_added.iter().any(|&a| a > 0.0) {
            add_grain(
                &mut fill,
                &cov,
                window,
                bounds,
                grain_added,
                grain_correlation,
                &kernel,
                params.seed,
            );
        }
    }
    clock.lap("grain");

    // 5. Blend.
    let mut seam_band = 0;
    if levels > 0 {
        seam_band = seam_correction(&mut fill, image, window, &cov, &plan, &hole, n, scale);
        let correction = pyramid_correction(&fill, image, window, &cov, levels);
        for c in 0..3 {
            for (v, d) in fill.c[c].iter_mut().zip(&correction.c[c]) {
                *v += d;
            }
        }
    }
    clock.lap("blend");

    // 6. Composite.
    let mut out = image.clone();
    composite(&mut out, &fill, window, &cov);
    clock.lap("composite");

    let report = Report {
        crop: plan.inside,
        scale,
        grain_sigma,
        timings_ms: clock.laps,
    };
    let details = Details {
        plan,
        model_input: model_in.to_rgb8(),
        model_mask: hole.iter().map(|&h| h * 255).collect(),
        model_output: model_out.to_rgb8(),
        output_range,
        window,
        levels,
        ring_width,
        fill_sigma,
        grain_added,
        grain_correlation,
        grain_correlation_px,
        seam_band,
    };
    Ok((out, report, details))
}

/// The square crop for a region's bounds: context of `max(min_margin, margin × longer side)`,
/// squared to the grown longer side, centred on the region and slid inside the image where it
/// fits; where it does not, it spans the axis and is padded past the edges by reflection.
pub fn plan_crop(bounds: RectPx, image: (u32, u32), params: &Params) -> CropPlan {
    let longer = bounds.width.max(bounds.height);
    let context = params
        .min_margin
        .max((params.margin * longer as f32).round() as u32);
    let side = longer + 2 * context;
    let axis = |start: u32, len: u32, extent: u32| -> i64 {
        let (s, e) = (side as i64, extent as i64);
        let centred = start as i64 + (len as i64 - s).div_euclid(2);
        if s <= e {
            centred.clamp(0, e - s)
        } else {
            centred.clamp(e - s, 0)
        }
    };
    let origin = (
        axis(bounds.x, bounds.width, image.0),
        axis(bounds.y, bounds.height, image.1),
    );
    let x0 = origin.0.max(0) as u32;
    let y0 = origin.1.max(0) as u32;
    let x1 = (origin.0 + side as i64).min(image.0 as i64) as u32;
    let y1 = (origin.1 + side as i64).min(image.1 as i64) as u32;
    CropPlan {
        bounds,
        context,
        origin,
        side,
        inside: RectPx {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        },
    }
}

/// Pyramid levels for a fill enlarged by `scale`: the coarsest band spans a few model pixels, so
/// the mismatch a 512-px fill carries fades out over its own scale.
fn blend_levels(scale: f32) -> usize {
    (scale.max(1.0).log2().round() as usize + 3).clamp(3, 7)
}

/// Ring width for the grain estimate: `max(16, 0.25 × region size)`, the size being the geometric
/// mean of the bounds' sides so that a long thin region does not take a huge ring.
fn ring_width(bounds: RectPx) -> u32 {
    let size = ((bounds.width as f64) * (bounds.height as f64)).sqrt();
    ((0.25 * size).round() as u32).max(16)
}

// ---------------------------------------------------------------------------------------------
// Geometry and sampling.

/// Reflects an index into `0..n` (OpenCV's `BORDER_REFLECT_101`: `… 2 1 | 0 1 2 … n−1 | n−2 …`),
/// repeating for pads longer than the image.
pub(crate) fn reflect(i: i64, n: u32) -> u32 {
    let n = n as i64;
    if (0..n).contains(&i) {
        return i as u32;
    }
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let m = i.rem_euclid(period);
    (if m < n { m } else { period - m }) as u32
}

/// `r` grown by `by` on every side, clamped to `within`.
fn grow_within(r: RectPx, by: u32, within: RectPx) -> RectPx {
    let x0 = r.x.saturating_sub(by).max(within.x);
    let y0 = r.y.saturating_sub(by).max(within.y);
    let x1 = (r.x + r.width + by).min(within.x + within.width);
    let y1 = (r.y + r.height + by).min(within.y + within.height);
    RectPx {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// The region's coverage over `rect`, densely.
fn coverage(region: &Mask, rect: RectPx) -> Vec<u8> {
    let mut out = vec![0u8; rect.width as usize * rect.height as usize];
    let rw = region.window;
    let x0 = rect.x.max(rw.x);
    let x1 = (rect.x + rect.width).min(rw.x + rw.width);
    let y0 = rect.y.max(rw.y);
    let y1 = (rect.y + rect.height).min(rw.y + rw.height);
    if x0 >= x1 || y0 >= y1 {
        return out;
    }
    let len = (x1 - x0) as usize;
    for y in y0..y1 {
        let src = (y - rw.y) as usize * rw.width as usize + (x0 - rw.x) as usize;
        let dst = (y - rect.y) as usize * rect.width as usize + (x0 - rect.x) as usize;
        out[dst..dst + len].copy_from_slice(&region.data[src..src + len]);
    }
    out
}

/// Separable resampling weights along one axis from `src` samples to `dst` samples, pixel centres
/// aligned (`align_corners = false`), for the destination indices in `range`. Shrinking averages
/// each destination pixel's exact footprint (area resampling); enlarging uses Catmull-Rom.
#[derive(Clone, Debug)]
struct Taps {
    /// The first source index for each destination index; it can fall outside `0..src`.
    first: Vec<i64>,
    count: usize,
    /// `count` weights per destination index, summing to 1.
    weights: Vec<f32>,
}

fn taps(src: usize, dst: usize, range: Range<usize>) -> Taps {
    let f = src as f64 / dst as f64;
    let len = range.len();
    if f > 1.0 {
        let count = f.ceil() as usize + 1;
        let mut t = Taps {
            first: Vec::with_capacity(len),
            count,
            weights: Vec::with_capacity(len * count),
        };
        for i in range {
            let (a, b) = (i as f64 * f, (i + 1) as f64 * f);
            let j0 = a.floor() as i64;
            t.first.push(j0);
            for k in 0..count as i64 {
                let j = (j0 + k) as f64;
                let overlap = (b.min(j + 1.0) - a.max(j)).max(0.0);
                t.weights.push((overlap / f) as f32);
            }
        }
        t
    } else {
        let mut t = Taps {
            first: Vec::with_capacity(len),
            count: 4,
            weights: Vec::with_capacity(len * 4),
        };
        for i in range {
            let u = (i as f64 + 0.5) * f - 0.5;
            let j0 = u.floor() as i64 - 1;
            t.first.push(j0);
            for k in 0..4 {
                t.weights
                    .push(catmull_rom((u - (j0 + k) as f64).abs()) as f32);
            }
        }
        t
    }
}

fn catmull_rom(d: f64) -> f64 {
    if d < 1.0 {
        1.5 * d * d * d - 2.5 * d * d + 1.0
    } else if d < 2.0 {
        -0.5 * d * d * d + 2.5 * d * d - 4.0 * d + 2.0
    } else {
        0.0
    }
}

/// Three f32 planes of one size.
#[derive(Clone, Debug)]
struct Planes {
    width: usize,
    height: usize,
    c: [Vec<f32>; 3],
}

impl Planes {
    fn new(width: usize, height: usize) -> Self {
        let z = vec![0.0; width * height];
        Self {
            width,
            height,
            c: [z.clone(), z.clone(), z],
        }
    }

    fn to_rgb8(&self) -> Rgb8 {
        let mut data = Vec::with_capacity(self.width * self.height * 3);
        for i in 0..self.width * self.height {
            for c in &self.c {
                data.push(c[i].round().clamp(0.0, 255.0) as u8);
            }
        }
        Rgb8::new(self.width as u32, self.height as u32, data)
    }
}

/// The model's input image (codes `0..255`, hole zeroed) and hole (0 or 1) at `n`². A model pixel
/// is in the hole when any source pixel that contributed to it has non-zero coverage.
fn model_input(image: &Rgb8, region: &Mask, plan: &CropPlan, n: usize) -> (Planes, Vec<u8>) {
    let s = plan.side as usize;
    let (w, h) = (image.width, image.height);
    let t = taps(s, n, 0..n);
    let k = t.count;
    let rw = region.window;
    // Source column and region-window column for every horizontal tap.
    let mut xs = Vec::with_capacity(n * k);
    let mut xcov = Vec::with_capacity(n * k);
    for i in 0..n {
        for j in 0..k {
            let sx = reflect(plan.origin.0 + t.first[i] + j as i64, w);
            xs.push(sx as usize);
            xcov.push(if sx >= rw.x && sx < rw.x + rw.width {
                (sx - rw.x) as usize
            } else {
                usize::MAX
            });
        }
    }
    // Crop rows the vertical taps read (Catmull-Rom reads one beyond each end).
    let y_lo = t.first[0];
    let y_hi = t.first[n - 1] + k as i64;
    let rows = (y_hi - y_lo) as usize;
    let mut tmp = vec![0f32; rows * n * 3];
    let mut tmp_hole = vec![0u8; rows * n];
    let row_len = w as usize * 3;
    par_rows(&mut tmp, &mut tmp_hole, n, |r, out, out_hole| {
        let sy = reflect(plan.origin.1 + y_lo + r as i64, h);
        let src = &image.data[sy as usize * row_len..][..row_len];
        let cov_row = (sy >= rw.y && sy < rw.y + rw.height)
            .then(|| &region.data[(sy - rw.y) as usize * rw.width as usize..][..rw.width as usize]);
        for i in 0..n {
            let (mut a0, mut a1, mut a2) = (0f32, 0f32, 0f32);
            let mut hole = 0u8;
            for j in 0..k {
                let wgt = t.weights[i * k + j];
                if wgt == 0.0 {
                    continue;
                }
                let sx = xs[i * k + j];
                let p = &src[sx * 3..sx * 3 + 3];
                a0 += wgt * p[0] as f32;
                a1 += wgt * p[1] as f32;
                a2 += wgt * p[2] as f32;
                if let Some(cr) = cov_row {
                    let cx = xcov[i * k + j];
                    if cx != usize::MAX && cr[cx] > 0 {
                        hole = 1;
                    }
                }
            }
            out[i * 3] = a0;
            out[i * 3 + 1] = a1;
            out[i * 3 + 2] = a2;
            out_hole[i] = hole;
        }
    });
    let mut img = Planes::new(n, n);
    let mut hole = vec![0u8; n * n];
    for jy in 0..n {
        for j in 0..k {
            let wgt = t.weights[jy * k + j];
            if wgt == 0.0 {
                continue;
            }
            let r = (t.first[jy] + j as i64 - y_lo) as usize;
            let src = &tmp[r * n * 3..(r + 1) * n * 3];
            let src_hole = &tmp_hole[r * n..(r + 1) * n];
            let dst = jy * n;
            for i in 0..n {
                img.c[0][dst + i] += wgt * src[i * 3];
                img.c[1][dst + i] += wgt * src[i * 3 + 1];
                img.c[2][dst + i] += wgt * src[i * 3 + 2];
                hole[dst + i] |= src_hole[i];
            }
        }
    }
    for c in 0..3 {
        for (v, &hl) in img.c[c].iter_mut().zip(&hole) {
            *v = if hl != 0 { 0.0 } else { v.clamp(0.0, 255.0) };
        }
    }
    (img, hole)
}

/// Runs `f(row, values, holes)` over rows of two parallel buffers, split across threads.
fn par_rows<F>(values: &mut [f32], holes: &mut [u8], n: usize, f: F)
where
    F: Fn(usize, &mut [f32], &mut [u8]) + Sync,
{
    let rows = holes.len() / n;
    let threads = std::thread::available_parallelism()
        .map_or(1, |t| t.get())
        .min(rows.div_ceil(32))
        .max(1);
    let per = rows.div_ceil(threads);
    std::thread::scope(|scope| {
        for (chunk, (v, hl)) in values
            .chunks_mut(per * n * 3)
            .zip(holes.chunks_mut(per * n))
            .enumerate()
        {
            let f = &f;
            scope.spawn(move || {
                for (r, (vr, hr)) in v.chunks_mut(n * 3).zip(hl.chunks_mut(n)).enumerate() {
                    f(chunk * per + r, vr, hr);
                }
            });
        }
    });
}

/// The model's output resized back to the crop's square, evaluated over `window` only.
fn upscale(model: &Planes, plan: &CropPlan, window: RectPx) -> Planes {
    let (w, h) = (window.width as usize, window.height as usize);
    Planes {
        width: w,
        height: h,
        c: [0, 1, 2].map(|c| upscale_plane(&model.c[c], model.width, plan, window)),
    }
}

/// One `n`² model plane resized back to the crop's square over `window`.
fn upscale_plane(src: &[f32], n: usize, plan: &CropPlan, window: RectPx) -> Vec<f32> {
    let s = plan.side as usize;
    let cx0 = (window.x as i64 - plan.origin.0) as usize;
    let cy0 = (window.y as i64 - plan.origin.1) as usize;
    let (ww, wh) = (window.width as usize, window.height as usize);
    let tx = taps(n, s, cx0..cx0 + ww);
    let ty = taps(n, s, cy0..cy0 + wh);
    let clamp = |j: i64| j.clamp(0, n as i64 - 1) as usize;
    let m_lo = clamp(ty.first[0]);
    let m_hi = clamp(ty.first[wh - 1] + ty.count as i64 - 1) + 1;
    let mut tmp = vec![0f32; (m_hi - m_lo) * ww];
    for m in m_lo..m_hi {
        let row = &src[m * n..(m + 1) * n];
        let dst = &mut tmp[(m - m_lo) * ww..(m - m_lo + 1) * ww];
        for (x, d) in dst.iter_mut().enumerate() {
            let mut acc = 0.0;
            for j in 0..tx.count {
                acc += tx.weights[x * tx.count + j] * row[clamp(tx.first[x] + j as i64)];
            }
            *d = acc;
        }
    }
    let mut out = vec![0f32; ww * wh];
    for y in 0..wh {
        let dst = &mut out[y * ww..(y + 1) * ww];
        for j in 0..ty.count {
            let wgt = ty.weights[y * ty.count + j];
            if wgt == 0.0 {
                continue;
            }
            let m = clamp(ty.first[y] + j as i64) - m_lo;
            for (d, v) in dst.iter_mut().zip(&tmp[m * ww..(m + 1) * ww]) {
                *d += wgt * v;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Grain.

/// Fewest high-pass samples a grain estimate is made from.
const MIN_SAMPLES: usize = 64;
/// Most high-pass samples read; larger areas are read on a stride.
const MAX_SAMPLES: usize = 1 << 20;
/// Side of the blocks the ring is split into to find its flattest parts.
const BLOCK: usize = 16;
/// Share of the ring's blocks, flattest first, that noise is estimated from.
const FLAT_SHARE: f64 = 0.3;
/// Largest spatial correlation grain is synthesised with, as a Gaussian σ in pixels.
const MAX_CORRELATION_PX: f64 = 1.5;

/// Grain measured in the ring around the region.
#[derive(Clone, Copy, Debug)]
struct RingGrain {
    /// Robust σ of the spacing-1 high-pass per channel.
    highpass: [f32; 3],
    /// Spatial correlation: the σ in pixels of the Gaussian that, filtering white noise, gives the
    /// ring's ratio of spacing-1 to spacing-2 high-pass.
    correlation_px: f32,
    /// Mean correlation of the high-pass between channels, in `0..=0.98`.
    channel_correlation: f32,
}

/// Immerkær's noise kernel `[1 −2 1; −2 4 −2; 1 −2 1]` with its taps `s` pixels apart, at `(x, y)`.
/// It ignores linear shading. On white noise of σ 1 its response has σ 6 at any spacing; on
/// spatially correlated noise the spacing-1 response falls further than the spacing-2 one.
fn highpass(get: impl Fn(usize, usize) -> f32, x: usize, y: usize, s: usize) -> f32 {
    let (x0, x1, y0, y1) = (x - s, x + s, y - s, y + s);
    get(x0, y0) + get(x1, y0) + get(x0, y1) + get(x1, y1)
        - 2.0 * (get(x, y0) + get(x0, y) + get(x1, y) + get(x, y1))
        + 4.0 * get(x, y)
}

/// A robust σ: the RMS deviation from the median of the samples within three σ of it, the σ for
/// the cut being `1.4826 × median absolute deviation`, corrected for the cut (a Gaussian keeps
/// 97.33% of its variance within ±3σ). The median absolute deviation alone is robust but, on the
/// integer high-pass of 8-bit images, quantized to whole codes, which misreads fine grain by 10%.
fn robust_sigma(samples: &mut [f32]) -> f32 {
    let mid = samples.len() / 2;
    let median = *samples.select_nth_unstable_by(mid, f32::total_cmp).1;
    let mut dev: Vec<f32> = samples.iter().map(|v| (v - median).abs()).collect();
    let mad = *dev.select_nth_unstable_by(mid, f32::total_cmp).1;
    let cut = 3.0 * (1.4826 * mad).max(0.5);
    let (mut sum, mut n) = (0f64, 0usize);
    for &d in &dev {
        if d <= cut {
            sum += (d as f64).powi(2);
            n += 1;
        }
    }
    ((sum / n.max(1) as f64) / 0.9733).sqrt() as f32
}

/// A Gaussian of σ `px` whose 2-D outer product has unit energy, so white noise of σ 1 filtered by
/// it keeps σ 1. Below 0.05 px it is the identity: white noise.
fn grain_kernel(px: f64) -> Vec<f64> {
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
fn grain_highpass_gain(kernel: &[f64], s: usize) -> f64 {
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
fn correlation_for_ratio(ratio: f64) -> f64 {
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
    // Rows: a running count over the window [x − r, x + r].
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
    // Columns: a running count per column, one row at a time.
    let mut counts = vec![0u32; w];
    for row in rows.chunks_exact(w).take(r.min(h)) {
        for (n, &v) in counts.iter_mut().zip(row) {
            *n += v as u32;
        }
    }
    let mut out = vec![false; w * h];
    for y in 0..h {
        if y + r < h {
            for (n, &v) in counts.iter_mut().zip(&rows[(y + r) * w..(y + r + 1) * w]) {
                *n += v as u32;
            }
        }
        if y > r {
            for (n, &v) in counts.iter_mut().zip(&rows[(y - r - 1) * w..(y - r) * w]) {
                *n -= v as u32;
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

/// Grain in the ring of width `width` around the region: pixels with coverage 0 within `width` of
/// it (a square neighbourhood) whose 9×9 neighbourhood is clear of it and not clipped. The ring is
/// split into 16 px blocks and noise is read from the flattest 30% of them, ranked by the energy of
/// the spacing-4 high-pass (where texture shows, and nearly independent of the spacing-1 and
/// spacing-2 samples, so the ranking does not bias their ratio), so texture and edges do not pass
/// for grain.
fn ring_grain(image: &Rgb8, region: &Mask, bounds: RectPx, width: u32) -> Option<RingGrain> {
    let win = bounds.grown(width + 4, (image.width, image.height));
    let (ww, wh) = (win.width as usize, win.height as usize);
    if ww < 9 || wh < 9 {
        return None;
    }
    let covered: Vec<bool> = coverage(region, win).iter().map(|&v| v > 0).collect();
    let near = dilate(&covered, ww, wh, width as usize);
    let blocked = dilate(&covered, ww, wh, 4);
    let iw = image.width as usize;
    let (bw, bh) = (ww.div_ceil(BLOCK), wh.div_ceil(BLOCK));
    let step = ((ww * wh) as f64 / MAX_SAMPLES as f64).ceil().max(1.0) as usize;
    let mut blocks: Vec<(f64, Block)> = Vec::new();
    for b in (0..bw * bh).step_by(step) {
        let (bx, by) = (b % bw * BLOCK, b / bw * BLOCK);
        let mut block = Block::default();
        let mut texture = 0f64;
        for y in by.max(4)..(by + BLOCK).min(wh - 4) {
            for x in bx.max(4)..(bx + BLOCK).min(ww - 4) {
                let i = y * ww + x;
                if !near[i] || blocked[i] {
                    continue;
                }
                let (gx, gy) = (win.x as usize + x, win.y as usize + y);
                let p = &image.data[(gy * iw + gx) * 3..][..3];
                if p.iter().any(|&v| v <= 2 || v >= 253) {
                    continue;
                }
                for c in 0..3 {
                    let get = |px: usize, py: usize| image.data[(py * iw + px) * 3 + c] as f32;
                    block.h1[c].push(highpass(get, gx, gy, 1));
                    block.h2[c].push(highpass(get, gx, gy, 2));
                    texture += (highpass(get, gx, gy, 4) as f64).powi(2);
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
    let mut highpass = [0.0f32; 3];
    let mut coarse = 0.0f64;
    for ((hp, h1), h2) in highpass.iter_mut().zip(&mut pooled.h1).zip(&mut pooled.h2) {
        *hp = robust_sigma(h1);
        coarse += robust_sigma(h2) as f64;
    }
    let fine: f64 = highpass.iter().map(|&v| v as f64).sum();
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
    let mean =
        |c: usize| keep.iter().map(|&i| samples[c][i] as f64).sum::<f64>() / keep.len() as f64;
    let m = [mean(0), mean(1), mean(2)];
    let cov = |a: usize, b: usize| {
        keep.iter()
            .map(|&i| (samples[a][i] as f64 - m[a]) * (samples[b][i] as f64 - m[b]))
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

/// The robust spacing-1 high-pass σ of planes where coverage is 255 over the whole 3×3
/// neighbourhood.
fn fill_stats(fill: &Planes, cov: &[u8]) -> Option<[f32; 3]> {
    let (w, h) = (fill.width, fill.height);
    if w < 3 || h < 3 {
        return None;
    }
    let stride = ((w * h) as f64 / MAX_SAMPLES as f64).sqrt().ceil().max(1.0) as usize;
    let mut samples: [Vec<f32>; 3] = Default::default();
    for y in (1..h - 1).step_by(stride) {
        for x in (1..w - 1).step_by(stride) {
            let full = (0..3).all(|dy| (0..3).all(|dx| cov[(y + dy - 1) * w + x + dx - 1] == 255));
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
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        out[c] = robust_sigma(&mut samples[c]);
    }
    Some(out)
}

/// The robust spacing-1 high-pass σ inside a region's fully covered pixels, per channel, divided
/// by the kernel's white-noise gain of 6: a white-noise-equivalent grain level in 8-bit codes, for
/// comparing a fill's fine texture with the background it replaced.
pub fn highpass_sigma_inside(image: &Rgb8, region: &Mask) -> Option<[f32; 3]> {
    let bounds = region.bounds()?;
    let cov = coverage(region, bounds);
    let mut planes = Planes::new(bounds.width as usize, bounds.height as usize);
    for y in 0..bounds.height {
        for x in 0..bounds.width {
            let p = image.pixel(bounds.x + x, bounds.y + y);
            for (plane, v) in planes.c.iter_mut().zip(p) {
                plane[(y * bounds.width + x) as usize] = v as f32;
            }
        }
    }
    fill_stats(&planes, &cov).map(|s| s.map(|v| v / 6.0))
}

/// SplitMix64's finaliser.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A standard normal value determined by the seed, a stream and absolute image coordinates, so
/// grain does not depend on the order or window it is generated in.
fn gaussian(seed: u64, stream: u64, x: i64, y: i64) -> f64 {
    let key = mix(seed ^ mix(stream.wrapping_mul(0x9e37_79b9_7f4a_7c15)))
        ^ mix((x as u64) ^ (y as u64).rotate_left(32));
    let a = mix(key);
    let b = mix(key ^ 0xd1b5_4a32_d192_ed03);
    let u1 = ((a >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
    let u2 = (b >> 11) as f64 / (1u64 << 53) as f64;
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// A unit-σ Gaussian field over `rect`, white noise filtered by the separable `kernel`.
fn grain_field(seed: u64, stream: u64, rect: RectPx, kernel: &[f64]) -> Vec<f32> {
    let r = kernel.len() / 2;
    let (w, h) = (rect.width as usize, rect.height as usize);
    let (pw, ph) = (w + 2 * r, h + 2 * r);
    let mut white = vec![0f64; pw * ph];
    for y in 0..ph {
        for x in 0..pw {
            white[y * pw + x] = gaussian(
                seed,
                stream,
                rect.x as i64 + x as i64 - r as i64,
                rect.y as i64 + y as i64 - r as i64,
            );
        }
    }
    let mut rows = vec![0f64; w * ph];
    for y in 0..ph {
        for x in 0..w {
            rows[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(j, k)| k * white[y * pw + x + j])
                .sum();
        }
    }
    let mut out = vec![0f32; w * h];
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
/// the fill (over `window`) wherever coverage is non-zero, generating it over the region's
/// `bounds` only.
#[allow(clippy::too_many_arguments)]
fn add_grain(
    fill: &mut Planes,
    cov: &[u8],
    window: RectPx,
    bounds: RectPx,
    sigma: [f32; 3],
    rho: f32,
    kernel: &[f64],
    seed: u64,
) {
    // The shared field and each channel's own, generated in parallel.
    let [shared, r, g, b] = std::thread::scope(|scope| {
        let handles = [0u64, 1, 2, 3]
            .map(|stream| scope.spawn(move || grain_field(seed, stream, bounds, kernel)));
        handles.map(|t| t.join().expect("grain thread"))
    });
    let own = [r, g, b];
    let (a, b) = (rho.sqrt(), (1.0 - rho).sqrt());
    let (bw, ww) = (bounds.width as usize, window.width as usize);
    let (ox, oy) = (
        (bounds.x - window.x) as usize,
        (bounds.y - window.y) as usize,
    );
    for c in 0..3 {
        if sigma[c] <= 0.0 {
            continue;
        }
        for y in 0..bounds.height as usize {
            for x in 0..bw {
                let i = (y + oy) * ww + x + ox;
                if cov[i] > 0 {
                    let g = y * bw + x;
                    fill.c[c][i] += sigma[c] * (a * shared[g] + b * own[c][g]);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Blend and composite.

/// The low-frequency seam correction, returning the band's size in pixels. The hole upscaled with
/// the fill's own filter, `α`, is 1 inside the region and fades over a band of background just
/// outside it (the resampling filters reach a pixel or two past the region both ways), where the
/// fill is `α` parts regenerated content `G` and the rest the input's own pixels, so
/// `I − F ≈ α (I − G)`. Push-pull on a pyramid fits `I − G` there by least squares
/// (values `α (I − F)`, weights `α²`) and extends it smoothly into the region; adding `α` times it
/// makes the fill meet the input without a step in colour or tone. It holds nothing finer than 8
/// source pixels or two model pixels, so texture differences in the band are not carried in and
/// detail is left to the detail blend. The band has coverage 0, so the input under the region
/// never enters.
#[allow(clippy::too_many_arguments)]
fn seam_correction(
    fill: &mut Planes,
    image: &Rgb8,
    window: RectPx,
    cov: &[u8],
    plan: &CropPlan,
    hole: &[u8],
    n: usize,
    scale: f32,
) -> usize {
    let (w, h) = (fill.width, fill.height);
    let hole_f: Vec<f32> = hole.iter().map(|&v| v as f32).collect();
    let alpha = upscale_plane(&hole_f, n, plan, window);
    let weight: Vec<f32> = alpha
        .iter()
        .zip(cov)
        .map(|(&a, &r)| {
            if r == 0 && a > 0.2 {
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
    let mut weights = vec![(w, h, weight)];
    while weights.len() < 24 {
        let (pw, ph, p) = weights.last().expect("level 0");
        if *pw == 1 && *ph == 1 {
            break;
        }
        let next = down(p, *pw, *ph);
        weights.push(next);
    }
    // A colour cast lives at the model's scale and above; texture differences in the band must not
    // be carried in, so nothing finer than 8 source pixels or two model pixels survives.
    let skip = ((2.0 * scale).log2().ceil().max(3.0) as usize).min(weights.len() - 1);
    let iw = image.width as usize;
    let weights = &weights;
    let fill_ref = &*fill;
    let alpha_ref = &alpha;
    let channel = |c: usize| -> Vec<f32> {
        let mut d = vec![0f32; w * h];
        for y in 0..h {
            let row = ((window.y as usize + y) * iw + window.x as usize) * 3;
            for x in 0..w {
                let i = y * w + x;
                if weights[0].2[i] > 0.0 {
                    // The fill there is `α` regenerated, so `I − F ≈ α (I − G)`: weighted by `α²`,
                    // push-pull fits the regenerated content's difference `I − G` by least squares.
                    let a = alpha_ref[i].min(1.0);
                    d[i] = a * (image.data[row + x * 3 + c] as f32 - fill_ref.c[c][i]);
                }
            }
        }
        push_pull(d, weights, skip)
    };
    let corrections: [Vec<f32>; 3] = std::thread::scope(|scope| {
        let handles = [0, 1, 2].map(|c| scope.spawn(move || channel(c)));
        handles.map(|t| t.join().expect("seam thread"))
    });
    for (plane, corr) in fill.c.iter_mut().zip(&corrections) {
        for ((v, d), &a) in plane.iter_mut().zip(corr).zip(&alpha) {
            *v += a * d;
        }
    }
    band
}

/// Push-pull interpolation: values known where the level-0 weight is positive (zero elsewhere),
/// extended everywhere by normalised averages on a binomial pyramid, each level taking its own
/// estimate where it has support and the coarser level's where it has little. The finest `skip`
/// levels only upsample, so the result holds no detail finer than `2^skip` pixels.
fn push_pull(values: Vec<f32>, weights: &[(usize, usize, Vec<f32>)], skip: usize) -> Vec<f32> {
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
/// by band: fine bands over a few pixels, coarse ones further. The input's pixels under full
/// coverage are multiplied by 0 before any band is built, so the object never enters the blend.
fn pyramid_correction(
    fill: &Planes,
    image: &Rgb8,
    window: RectPx,
    cov: &[u8],
    levels: usize,
) -> Planes {
    let (w, h) = (fill.width, fill.height);
    let weight: Vec<f32> = cov.iter().map(|&v| v as f32 / 255.0).collect();
    let mut gw = vec![(w, h, weight)];
    for _ in 0..levels {
        let (pw, ph, p) = gw.last().expect("level 0");
        let (nw, nh, d) = down(p, *pw, *ph);
        gw.push((nw, nh, d));
    }
    let iw = image.width as usize;
    let gw = &gw;
    // One channel's correction; the three run in parallel.
    let channel = |c: usize| -> Vec<f32> {
        let mut diff = vec![0f32; w * h];
        for y in 0..h {
            let row = ((window.y as usize + y) * iw + window.x as usize) * 3;
            for x in 0..w {
                let i = y * w + x;
                let r = gw[0].2[i];
                diff[i] = (1.0 - r) * (image.data[row + x * 3 + c] as f32 - fill.c[c][i]);
            }
        }
        // Gaussian pyramid of the difference, then Laplacian bands.
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
    let [r, g, b] = std::thread::scope(|scope| {
        let handles = [0, 1, 2].map(|c| scope.spawn(move || channel(c)));
        handles.map(|t| t.join().expect("pyramid thread"))
    });
    Planes {
        width: w,
        height: h,
        c: [r, g, b],
    }
}

const BINOMIAL: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Blurs with the 5-tap binomial and keeps every second sample (reflected edges).
fn down(src: &[f32], w: usize, h: usize) -> (usize, usize, Vec<f32>) {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let rx = |i: i64| reflect(i, w as u32) as usize;
    let ry = |i: i64| reflect(i, h as u32) as usize;
    let mut rows = vec![0f32; nw * h];
    for y in 0..h {
        for x in 0..nw {
            let cx = 2 * x as i64;
            rows[y * nw + x] = (0..5)
                .map(|j| BINOMIAL[j] * src[y * w + rx(cx + j as i64 - 2)])
                .sum();
        }
    }
    let mut out = vec![0f32; nw * nh];
    for y in 0..nh {
        let cy = 2 * y as i64;
        for (j, b) in BINOMIAL.iter().enumerate() {
            let sy = ry(cy + j as i64 - 2);
            for x in 0..nw {
                out[y * nw + x] += b * rows[sy * nw + x];
            }
        }
    }
    (nw, nh, out)
}

/// Burt and Adelson's expand from `(w, h)` to `(tw, th)`: even samples take `[1, 6, 1] / 8` of the
/// coarse neighbours, odd samples their mean (clamped edges).
fn up(src: &[f32], w: usize, h: usize, tw: usize, th: usize) -> Vec<f32> {
    let expand = |i: usize, len: usize| -> [(usize, f32); 3] {
        let c = |j: i64| j.clamp(0, len as i64 - 1) as usize;
        let k = (i / 2) as i64;
        if i.is_multiple_of(2) {
            [(c(k - 1), 0.125), (c(k), 0.75), (c(k + 1), 0.125)]
        } else {
            [(c(k), 0.5), (c(k + 1), 0.5), (c(k), 0.0)]
        }
    };
    let mut rows = vec![0f32; tw * h];
    for y in 0..h {
        for x in 0..tw {
            rows[y * tw + x] = expand(x, w)
                .iter()
                .map(|&(j, wgt)| wgt * src[y * w + j])
                .sum();
        }
    }
    let mut out = vec![0f32; tw * th];
    for y in 0..th {
        for (j, wgt) in expand(y, h) {
            if wgt == 0.0 {
                continue;
            }
            for x in 0..tw {
                out[y * tw + x] += wgt * rows[j * tw + x];
            }
        }
    }
    out
}

/// `input × (1 − r) + blended × r` over the window: coverage 0 leaves the input's bytes as they are,
/// coverage 255 takes the blended value alone.
fn composite(out: &mut Rgb8, blended: &Planes, window: RectPx, cov: &[u8]) {
    let iw = out.width as usize;
    let w = window.width as usize;
    for y in 0..window.height as usize {
        let row = ((window.y as usize + y) * iw + window.x as usize) * 3;
        for x in 0..w {
            let i = y * w + x;
            let r = cov[i];
            if r == 0 {
                continue;
            }
            let r = r as f32 / 255.0;
            for c in 0..3 {
                let p = &mut out.data[row + x * 3 + c];
                let v = *p as f32 * (1.0 - r) + blended.c[c][i] * r;
                *p = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

struct Stopwatch {
    last: Instant,
    laps: Vec<(String, f64)>,
}

impl Stopwatch {
    fn new() -> Self {
        Self {
            last: Instant::now(),
            laps: Vec::new(),
        }
    }

    fn lap(&mut self, name: &str) {
        let now = Instant::now();
        self.laps
            .push((name.into(), (now - self.last).as_secs_f64() * 1e3));
        self.last = now;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runtime::{Backend, Io};

    /// Stands in for LaMa: reproduces the known pixels as codes and fills the hole with a gradient,
    /// recording the tensors it was given.
    #[derive(Default)]
    pub(crate) struct FakeLama {
        pub images: Vec<Vec<f32>>,
        pub masks: Vec<Vec<f32>>,
        /// Fill the hole with this colour instead of a gradient.
        pub hole_colour: Option<[f32; 3]>,
    }

    impl Session for FakeLama {
        fn backend(&self) -> Backend {
            Backend::OrtCpu
        }

        fn inputs(&self) -> &[Io] {
            &[]
        }

        fn outputs(&self) -> &[Io] {
            &[]
        }

        fn run(&mut self, inputs: &[(&str, &Tensor)]) -> Result<Vec<(String, Tensor)>, String> {
            let get = |name: &str| {
                inputs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, t)| *t)
                    .ok_or(format!("no input {name}"))
            };
            let (image, mask) = (get("image")?, get("mask")?);
            let n = image.shape[3];
            let (img, m) = (image.as_f32()?, mask.as_f32()?);
            let mut out = vec![0f32; 3 * n * n];
            for c in 0..3 {
                for i in 0..n * n {
                    let x = (i % n) as f32 / n as f32;
                    out[c * n * n + i] = if m[i] > 0.5 {
                        self.hole_colour
                            .map_or(40.0 + 120.0 * x + 30.0 * c as f32, |h| h[c])
                    } else {
                        img[c * n * n + i] * 255.0
                    };
                }
            }
            self.images.push(img.to_vec());
            self.masks.push(m.to_vec());
            Ok(vec![("output".into(), Tensor::f32([1, 3, n, n], out))])
        }
    }

    /// A deterministic noisy image: a smooth gradient plus uniform noise of ±`amp`.
    pub(crate) fn noise_image(w: u32, h: u32, amp: f32, seed: u64) -> Rgb8 {
        let mut state = seed.wrapping_add(1);
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    state = mix(state);
                    let u = (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
                    let base = 60.0 + 120.0 * (x as f32 / w as f32) + 20.0 * c as f32
                        - 30.0 * (y as f32 / h as f32);
                    data.push((base + 2.0 * amp * u).round().clamp(0.0, 255.0) as u8);
                }
            }
        }
        Rgb8::new(w, h, data)
    }

    /// A whole-image mask from a coverage function.
    pub(crate) fn mask_from(size: (u32, u32), f: impl Fn(u32, u32) -> u8) -> Mask {
        let mut data = Vec::with_capacity((size.0 * size.1) as usize);
        for y in 0..size.1 {
            for x in 0..size.0 {
                data.push(f(x, y));
            }
        }
        Mask::new(size, rect(0, 0, size.0, size.1), data)
    }

    /// A rectangle with a soft edge of `soft` pixels inside it.
    fn soft_rect(size: (u32, u32), r: RectPx, soft: f32) -> Mask {
        mask_from(size, |x, y| {
            if !r.contains(x, y) {
                return 0;
            }
            let d = [
                x - r.x,
                r.x + r.width - 1 - x,
                y - r.y,
                r.y + r.height - 1 - y,
            ]
            .into_iter()
            .min()
            .unwrap() as f32;
            (255.0 * ((d + 1.0) / (soft + 1.0)).min(1.0)).round() as u8
        })
    }

    pub(crate) fn rect(x: u32, y: u32, width: u32, height: u32) -> RectPx {
        RectPx {
            x,
            y,
            width,
            height,
        }
    }

    fn all_params() -> Vec<Params> {
        let mut out = Vec::new();
        for grain in [false, true] {
            for pyramid_blend in [false, true] {
                out.push(Params {
                    grain,
                    pyramid_blend,
                    seed: 7,
                    ..Params::default()
                });
            }
        }
        out
    }

    fn assert_exact_outside(input: &Rgb8, out: &Rgb8, region: &Mask, what: &str) {
        let mut changed_inside = 0;
        for y in 0..input.height {
            for x in 0..input.width {
                let cov = region.get(x, y);
                if cov == 0 {
                    assert_eq!(
                        input.pixel(x, y),
                        out.pixel(x, y),
                        "{what}: pixel ({x}, {y}) changed outside the region"
                    );
                } else if cov == 255 && input.pixel(x, y) != out.pixel(x, y) {
                    changed_inside += 1;
                }
            }
        }
        assert!(
            changed_inside > 0,
            "{what}: nothing inside the region changed"
        );
    }

    #[test]
    fn fill_leaves_zero_coverage_pixels_byte_identical() {
        let size = (300, 200);
        let image = noise_image(size.0, size.1, 30.0, 1);
        let (w, h) = size;
        let cases: Vec<(&str, Mask)> = vec![
            (
                "interior soft disc",
                mask_from(size, |x, y| {
                    let d = ((x as f32 - 150.0).powi(2) + (y as f32 - 100.0).powi(2)).sqrt();
                    (255.0 * (34.0 - d).clamp(0.0, 4.0) / 4.0) as u8
                }),
            ),
            ("left edge", soft_rect(size, rect(0, 50, 20, 100), 3.0)),
            (
                "right edge",
                soft_rect(size, rect(w - 15, 30, 15, 120), 0.0),
            ),
            ("top edge", soft_rect(size, rect(40, 0, 200, 10), 2.0)),
            (
                "bottom edge",
                soft_rect(size, rect(60, h - 12, 90, 12), 0.0),
            ),
            ("top-left corner", soft_rect(size, rect(0, 0, 40, 40), 2.0)),
            (
                "bottom-right corner",
                soft_rect(size, rect(w - 50, h - 30, 50, 30), 0.0),
            ),
            ("full-width band", soft_rect(size, rect(0, 90, w, 20), 3.0)),
            (
                "all but a border",
                soft_rect(size, rect(2, 2, w - 4, h - 4), 0.0),
            ),
            ("single pixel", soft_rect(size, rect(299, 199, 1, 1), 0.0)),
        ];
        for (what, region) in &cases {
            for params in all_params() {
                let mut lama = FakeLama::default();
                let (out, report) = remove(&image, region, &mut lama, &params).unwrap();
                assert_exact_outside(&image, &out, region, &format!("{what} {params:?}"));
                assert_eq!(report.timings_ms.len(), 6);
            }
        }
    }

    #[test]
    fn fill_of_a_region_far_larger_than_the_model_is_exact_outside() {
        // 1500 × 1100 with a 0.4 margin's context of 600 makes a 2700 px square: larger than both
        // image sides, so it is padded by reflection, centred on the region (centre 1250, 850).
        let size = (2600, 1800);
        let image = noise_image(size.0, size.1, 20.0, 2);
        let region = soft_rect(size, rect(500, 300, 1500, 1100), 6.0);
        for params in all_params() {
            let params = Params {
                margin: 0.4,
                ..params
            };
            let mut lama = FakeLama::default();
            let (out, report, details) =
                remove_detailed(&image, &region, &mut lama, &params).unwrap();
            assert_exact_outside(&image, &out, &region, &format!("{params:?}"));
            assert_eq!(details.plan.side, 2700);
            assert_eq!(details.plan.padding(), [100, 500, 0, 400]);
            assert!((report.scale - 2700.0 / 512.0).abs() < 1e-6);
            assert_eq!(report.crop, rect(0, 0, 2600, 1800));
        }
    }

    #[test]
    fn fill_never_reads_the_input_under_full_coverage() {
        // Whatever the input holds where coverage is 255, the result is the same.
        let size = (900, 700);
        let image = noise_image(size.0, size.1, 25.0, 3);
        let region = mask_from(size, |x, y| {
            let d = ((x as f32 - 420.0).powi(2) / 1.6 + (y as f32 - 330.0).powi(2)).sqrt();
            (255.0 * (260.0 - d).clamp(0.0, 8.0) / 8.0) as u8
        });
        let mut other = image.clone();
        for y in 0..size.1 {
            for x in 0..size.0 {
                if region.get(x, y) == 255 {
                    let i = ((y * size.0 + x) * 3) as usize;
                    other.data[i..i + 3].copy_from_slice(&[255, (x % 256) as u8, 0]);
                }
            }
        }
        for params in all_params() {
            let a = remove(&image, &region, &mut FakeLama::default(), &params)
                .unwrap()
                .0;
            let b = remove(&other, &region, &mut FakeLama::default(), &params)
                .unwrap()
                .0;
            assert!(a == b, "{params:?}: the object leaked into the fill");
        }
    }

    #[test]
    fn fill_is_deterministic_for_a_seed() {
        let size = (900, 800);
        let image = noise_image(size.0, size.1, 30.0, 4);
        let region = soft_rect(size, rect(300, 250, 320, 300), 4.0);
        let params = Params {
            seed: 11,
            ..Params::default()
        };
        let (a, _, details) =
            remove_detailed(&image, &region, &mut FakeLama::default(), &params).unwrap();
        assert!(
            details.grain_added.iter().all(|&s| s > 1.0),
            "the flat fake fill needs grain: {details:?}"
        );
        let (b, _) = remove(&image, &region, &mut FakeLama::default(), &params).unwrap();
        assert!(a == b, "the same seed gave different bytes");
        let other = Params { seed: 12, ..params };
        let (c, _) = remove(&image, &region, &mut FakeLama::default(), &other).unwrap();
        assert!(a != c, "a different seed gave the same bytes");
    }

    #[test]
    fn fill_adds_no_grain_when_the_model_output_is_not_enlarged() {
        // A 100 px region takes a 300 px square: the model output is reduced, not enlarged.
        let size = (600, 400);
        let image = noise_image(size.0, size.1, 30.0, 5);
        let region = soft_rect(size, rect(250, 150, 100, 100), 0.0);
        let (_, report, details) = remove_detailed(
            &image,
            &region,
            &mut FakeLama::default(),
            &Params::default(),
        )
        .unwrap();
        assert!(report.scale < 1.0);
        assert!(report.grain_sigma.iter().all(|&s| s > 5.0), "{report:?}");
        assert_eq!(details.grain_added, [0.0; 3]);
    }

    #[test]
    fn fill_crop_is_a_centred_square_with_context() {
        // The default margin of 1.0: context equal to the longer side.
        let c = plan_crop(rect(1000, 1000, 100, 50), (4000, 3000), &Params::default());
        assert_eq!((c.context, c.side), (100, 300));
        assert_eq!(c.inside, rect(900, 875, 300, 300));
        let p = Params {
            margin: 0.4,
            ..Params::default()
        };
        // Interior: context is the 48 px minimum, the square is the grown longer side.
        let c = plan_crop(rect(1000, 1000, 100, 50), (4000, 3000), &p);
        assert_eq!(c.context, 48);
        assert_eq!(c.side, 196);
        assert_eq!(c.origin, (952, 927));
        assert_eq!(c.inside, rect(952, 927, 196, 196));
        assert_eq!(c.padding(), [0; 4]);
        // Context from the margin: 0.4 × 1000.
        let c = plan_crop(rect(1000, 500, 1000, 400), (4000, 3000), &p);
        assert_eq!((c.context, c.side), (400, 1800));
        assert_eq!(c.origin, (600, 0));
        assert_eq!(c.inside, rect(600, 0, 1800, 1800));
        // Near an edge the square slides inside the image rather than shrinking.
        let c = plan_crop(rect(10, 500, 100, 100), (4000, 3000), &p);
        assert_eq!(c.origin, (0, 452));
        assert_eq!(c.inside, rect(0, 452, 196, 196));
        let c = plan_crop(rect(3950, 2950, 50, 50), (4000, 3000), &p);
        assert_eq!(c.inside, rect(3854, 2854, 146, 146));
        // Larger than the image: padded by reflection, centred on the region.
        let c = plan_crop(rect(100, 50, 400, 300), (600, 400), &p);
        assert_eq!((c.context, c.side), (160, 720));
        assert_eq!(c.origin, (-60, -160));
        assert_eq!(c.inside, rect(0, 0, 600, 400));
        assert_eq!(c.padding(), [60, 160, 60, 160]);
        // Larger on one axis only: square on the other, padded on this one.
        let c = plan_crop(rect(1000, 20, 300, 60), (4000, 200), &p);
        assert_eq!(c.side, 540);
        assert_eq!(c.inside, rect(880, 0, 540, 200));
        assert_eq!(c.padding(), [0, 220, 0, 120]);
        // Every case contains the region.
        for (b, size) in [
            (rect(0, 0, 1, 1), (10, 10)),
            (rect(9, 9, 1, 1), (10, 10)),
            (rect(0, 0, 10, 10), (10, 10)),
            (rect(3, 0, 5, 700), (20, 700)),
        ] {
            let c = plan_crop(b, size, &p);
            assert!(c.origin.0 <= b.x as i64 && c.origin.1 <= b.y as i64);
            assert!(c.origin.0 + c.side as i64 >= (b.x + b.width) as i64);
            assert!(c.origin.1 + c.side as i64 >= (b.y + b.height) as i64);
        }
    }

    #[test]
    fn fill_seam_correction_takes_out_a_colour_offset() {
        // A flat image whose fake fill is 30 codes too bright everywhere in the hole: the plain
        // blend keeps the offset, the pyramid blend's seam correction removes it.
        let size = (800, 600);
        let image = Rgb8::new(size.0, size.1, [100u8, 120, 140].repeat(800 * 600));
        let region = soft_rect(size, rect(300, 200, 200, 160), 0.0);
        let mut errors = Vec::new();
        for pyramid_blend in [false, true] {
            let mut lama = FakeLama {
                hole_colour: Some([130.0, 150.0, 170.0]),
                ..FakeLama::default()
            };
            let params = Params {
                grain: false,
                pyramid_blend,
                ..Params::default()
            };
            let (out, _, details) = remove_detailed(&image, &region, &mut lama, &params).unwrap();
            let (mut sum, mut count) = (0f64, 0usize);
            for y in 0..size.1 {
                for x in 0..size.0 {
                    if region.get(x, y) > 0 {
                        for c in 0..3 {
                            sum += (out.pixel(x, y)[c] as f64 - image.pixel(x, y)[c] as f64).abs();
                            count += 1;
                        }
                    }
                }
            }
            errors.push((sum / count as f64, details.seam_band));
        }
        assert!(errors[0].0 > 29.0, "{errors:?}");
        assert!(errors[1].0 < 0.5 && errors[1].1 > 0, "{errors:?}");
    }

    #[test]
    fn fill_dilation_matches_brute_force() {
        let (w, h) = (37usize, 23usize);
        let mut state = 5u64;
        let mask: Vec<bool> = (0..w * h)
            .map(|_| {
                state = mix(state);
                state.is_multiple_of(17)
            })
            .collect();
        for r in [0usize, 1, 3, 10, 40] {
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
    fn fill_reflects_past_the_edges() {
        let r: Vec<u32> = (-5..9).map(|i| reflect(i, 4)).collect();
        assert_eq!(r, [1, 2, 3, 2, 1, 0, 1, 2, 3, 2, 1, 0, 1, 2]);
        assert_eq!(reflect(-7, 1), 0);
        assert_eq!(reflect(12, 1), 0);
    }

    #[test]
    fn fill_model_input_holes_every_covered_pixel() {
        let size = (1400, 900);
        let image = noise_image(size.0, size.1, 10.0, 6);
        // A thin diagonal line, 300 × 150 px so its 900 px square fits the image unpadded: area
        // resampling must not skip it.
        let region = mask_from(size, |x, y| {
            let d = (x as f32 * 0.5 - y as f32 + 100.0).abs();
            if (500..800).contains(&x) && d < 0.6 {
                255
            } else {
                0
            }
        });
        let mut lama = FakeLama::default();
        let (_, _, details) =
            remove_detailed(&image, &region, &mut lama, &Params::default()).unwrap();
        let n = 512usize;
        let mask = &lama.masks[0];
        let img = &lama.images[0];
        assert!(mask.iter().all(|&m| m == 0.0 || m == 1.0));
        assert!(img.iter().all(|&v| (0.0..=1.0).contains(&v)));
        let plan = details.plan;
        assert_eq!(plan.padding(), [0; 4]);
        let scale = plan.side as f64 / n as f64;
        let mut touched = vec![false; n * n];
        for y in 0..size.1 {
            for x in 0..size.0 {
                if region.get(x, y) == 0 {
                    continue;
                }
                let mx = ((x as i64 - plan.origin.0) as f64 / scale) as usize;
                let my = ((y as i64 - plan.origin.1) as f64 / scale) as usize;
                touched[my * n + mx] = true;
                assert_eq!(mask[my * n + mx], 1.0, "({x}, {y}) is not in the hole");
                for c in 0..3 {
                    assert_eq!(img[c * n * n + my * n + mx], 0.0, "the hole is not zeroed");
                }
            }
        }
        // And the hole is the line's model pixels grown by the resampling filter's pixel of reach,
        // no more.
        let near = dilate(&touched, n, n, 1);
        for (i, &m) in mask.iter().enumerate() {
            assert!(
                m == 0.0 || near[i],
                "model pixel {i} is in the hole, far from the line"
            );
        }
    }

    #[test]
    fn fill_pyramid_collapses_exactly() {
        let (w, h) = (97usize, 61usize);
        let image = noise_image(w as u32, h as u32, 40.0, 8);
        let mut fill = Planes::new(w, h);
        for c in 0..3 {
            for (i, v) in fill.c[c].iter_mut().enumerate() {
                *v = 50.0 + (i % 17) as f32 * 3.0 + c as f32;
            }
        }
        let window = rect(0, 0, w as u32, h as u32);
        // Nothing covered: the correction is the whole difference, so the blend is the input.
        let none = vec![0u8; w * h];
        let d = pyramid_correction(&fill, &image, window, &none, 4);
        for c in 0..3 {
            for i in 0..w * h {
                let want = image.data[i * 3 + c] as f32 - fill.c[c][i];
                assert!((d.c[c][i] - want).abs() < 1e-3, "{c} {i}");
            }
        }
        // Everything covered: no correction.
        let all = vec![255u8; w * h];
        let d = pyramid_correction(&fill, &image, window, &all, 4);
        assert!(d.c.iter().flatten().all(|v| v.abs() < 1e-6));
    }

    /// A grey image with grain of `sigma` made with a Gaussian of `px`, shared between channels
    /// when `shared`.
    fn grain_image(size: u32, sigma: f32, px: f64, shared: bool) -> Rgb8 {
        let k = grain_kernel(px);
        let r = rect(0, 0, size, size);
        let fields: Vec<Vec<f32>> = (0..3)
            .map(|c| grain_field(21, if shared { 0 } else { c }, r, &k))
            .collect();
        let mut data = Vec::new();
        for i in 0..(size * size) as usize {
            for (field, base) in fields.iter().zip([128.0f32, 120.0, 110.0]) {
                data.push((base + sigma * field[i]).round().clamp(0.0, 255.0) as u8);
            }
        }
        Rgb8::new(size, size, data)
    }

    #[test]
    fn fill_grain_estimate_recovers_level_and_correlation() {
        // The synthetic field has unit σ whatever its correlation.
        for px in [0.0, 0.7, 1.2] {
            let f = grain_field(5, 0, rect(0, 0, 400, 400), &grain_kernel(px));
            let var = f.iter().map(|v| (v * v) as f64).sum::<f64>() / f.len() as f64;
            assert!(
                (var.sqrt() - 1.0).abs() < 0.02,
                "{px}: unit field σ {}",
                var.sqrt()
            );
        }
        let region = soft_rect((600, 600), rect(200, 200, 200, 200), 0.0);
        let bounds = region.bounds().unwrap();
        // White noise, independent between channels.
        let white = grain_image(600, 4.0, 0.0, false);
        let g = ring_grain(&white, &region, bounds, ring_width(bounds)).unwrap();
        let gain = grain_highpass_gain(&grain_kernel(g.correlation_px as f64), 1) as f32;
        assert!(g.correlation_px < 0.2, "{g:?}");
        assert!(g.channel_correlation < 0.1, "{g:?}");
        for c in 0..3 {
            assert!(
                (g.highpass[c] / gain - 4.0).abs() < 0.2,
                "{g:?} σ {}",
                g.highpass[c] / gain
            );
        }
        // Correlated grain, shared between channels.
        let grainy = grain_image(600, 5.0, 0.8, true);
        let g = ring_grain(&grainy, &region, bounds, ring_width(bounds)).unwrap();
        let gain = grain_highpass_gain(&grain_kernel(g.correlation_px as f64), 1) as f32;
        assert!((g.correlation_px - 0.8).abs() < 0.06, "{g:?}");
        assert!(g.channel_correlation > 0.9, "{g:?}");
        for c in 0..3 {
            assert!(
                (g.highpass[c] / gain - 5.0).abs() < 0.3,
                "{g:?} σ {}",
                g.highpass[c] / gain
            );
        }
    }

    #[test]
    fn fill_refuses_an_empty_or_mismatched_region() {
        let image = noise_image(64, 48, 5.0, 9);
        let empty = mask_from((64, 48), |_, _| 0);
        assert!(remove(&image, &empty, &mut FakeLama::default(), &Params::default()).is_err());
        let other = mask_from((48, 64), |_, _| 255);
        assert!(remove(&image, &other, &mut FakeLama::default(), &Params::default()).is_err());
    }
}

/// What the pinned LaMa export does with its inputs, measured rather than assumed.
#[cfg(test)]
mod contract {
    use std::path::PathBuf;

    use serde_json::json;

    use super::tests::{mask_from, rect};
    use super::*;
    use crate::commands::fill::{evidence_dir, write_json};
    use crate::models;
    use crate::runtime::Backend;

    fn run(lama: &mut dyn Session, planes: &Planes, hole: &[u8], n: usize) -> Vec<f32> {
        let mut image = Vec::with_capacity(3 * n * n);
        for c in &planes.c {
            image.extend(c.iter().map(|v| v / 255.0));
        }
        let image = Tensor::f32([1, 3, n, n], image);
        let mask = Tensor::f32([1, 1, n, n], hole.iter().map(|&h| h as f32).collect());
        let mut out = lama.run(&[("image", &image), ("mask", &mask)]).unwrap();
        runtime::take(&mut out, "output")
            .unwrap()
            .into_f32()
            .unwrap()
    }

    /// Mean and maximum absolute difference over the model pixels `keep` selects.
    fn diff(a: &[f32], b: &[f32], keep: impl Fn(usize) -> bool, n: usize) -> (f64, f32) {
        let (mut sum, mut max, mut count) = (0f64, 0f32, 0usize);
        for c in 0..3 {
            for i in (0..n * n).filter(|&i| keep(i)) {
                let d = (a[c * n * n + i] - b[c * n * n + i]).abs();
                sum += d as f64;
                max = max.max(d);
                count += 1;
            }
        }
        (sum / count.max(1) as f64, max)
    }

    #[test]
    #[ignore = "needs the pinned models and corpus"]
    fn fill_lama_contract() {
        let corpus = std::env::var_os("LUXFORGE_REMOVAL_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from("/Users/will/projects/lightwell/private/object-removal/corpus")
            });
        let image = Rgb8::load(&corpus.join("sapa-road.jpg"))
            .unwrap()
            .crop(rect(566, 1176, 2048, 2048));
        let size = (image.width, image.height);
        let hole_rect = rect(870, 930, 300, 200);
        let region = mask_from(size, |x, y| if hole_rect.contains(x, y) { 255 } else { 0 });
        let plan = plan_crop(hole_rect, size, &Params::default());
        let n = 512;
        let (zeroed, hole) = model_input(&image, &region, &plan, n);
        let (content, none) = model_input(&image, &mask_from(size, |_, _| 0), &plan, n);
        assert!(none.iter().all(|&h| h == 0));
        let mut white = content.clone();
        for c in &mut white.c {
            for (v, &hl) in c.iter_mut().zip(&hole) {
                if hl != 0 {
                    *v = 255.0;
                }
            }
        }
        let path = models::LAMA.verified(&models::directory(None)).unwrap();
        let mut lama = runtime::open(Backend::OrtCpu, &path).unwrap().0;
        let a = run(lama.as_mut(), &zeroed, &hole, n);
        let b = run(lama.as_mut(), &content, &hole, n);
        let c = run(lama.as_mut(), &white, &hole, n);
        let again = run(lama.as_mut(), &zeroed, &hole, n);
        let inside = |i: usize| hole[i] != 0;
        let holes: Vec<bool> = hole.iter().map(|&h| h != 0).collect();
        let near = dilate(&holes, n, n, 2);
        let known = |i: usize| !near[i];
        let mut codes = Vec::with_capacity(3 * n * n);
        for p in &zeroed.c {
            codes.extend_from_slice(p);
        }
        let range = |v: &[f32]| {
            v.iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &x| {
                    (lo.min(x), hi.max(x))
                })
        };
        let report = json!({
            "hole_model_pixels": hole.iter().filter(|&&h| h != 0).count(),
            "output_range_zeroed": range(&a),
            "output_range_content": range(&b),
            "output_range_white": range(&c),
            "inside_hole_zeroed_vs_content_mean_max": diff(&a, &b, inside, n),
            "inside_hole_zeroed_vs_white_mean_max": diff(&a, &c, inside, n),
            "known_pixels_output_vs_input_codes_mean_max": diff(&a, &codes, known, n),
            "repeat_run_mean_max": diff(&a, &again, |_| true, n),
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        write_json(
            &evidence_dir("contract").join("lama-contract.json"),
            &report,
        )
        .unwrap();
    }
}
