//! The fill pipeline around a model, shared by every fill tier (`docs/design/ai-editing.md`,
//! "Generative fill and Replace"): it turns a region's coverage and the stage it was painted on
//! into the model's square input, and the model's answer into a candidate patch whose every
//! pixel with zero coverage is the input's own.
//!
//! 1. **Crop and reduce** ([`crop`], [`resample`]). The region's bounds grow by one region-length
//!    of context on every side (at least 48 pixels) and become a square of the grown longer side,
//!    centred on the region, slid inside the stage, and padded past an edge by reflection where the
//!    stage is smaller than it. The square's inside is read through the host-planned reference read
//!    of the repair stage's input, a band of rows at a time, and area-averaged in linear light to
//!    the model's native side (Catmull-Rom when the square is smaller). A model pixel any covered
//!    stage pixel contributed to is in the hole, which the model's input zeroes.
//! 2. **Encode** ([`encode`]). Linear light becomes the model's display-referred input through the
//!    sRGB transfer function over the context's range, with its recorded inverse.
//! 3. **Model.** The generator fills the hole at its native side.
//! 4. **Enlarge** ([`resample`]). The answer is resized back to the square's side (Catmull-Rom) over
//!    the window the later stages work on: the region's bounds and the blend's reach.
//! 5. **Grain** ([`grain`]). Grain measured in a ring around the region makes up what enlarging the
//!    answer lost, from the candidate's seed.
//! 6. **Blend** ([`blend`]). A push-pull seam correction meets the input's colour and tone at the
//!    edge, then a Laplacian pyramid carries detail across it band by band. What the region covers
//!    enters neither.
//! 7. **Patch.** The candidate is the tight bounds of the coverage: the blended fill decoded to
//!    linear light where coverage is above zero and the input's own value where it is zero, with
//!    the coverage as its alpha, so the repair stage's composite `in + c · (patch − in)` copies
//!    every zero-coverage pixel byte for byte. Its preview is that composite in 8-bit sRGB.
//!
//! The pipeline runs on the capability worker's module lane, serially: the host's shared pool has
//! no measured threshold for these passes yet, and one candidate's work is small beside its
//! generation. Its memory is bounded by the region's admission: the horizontal reduction holds
//! `inside.height × side` pixels, and every later stage works over the window, whose side is the
//! bounds' plus twice the blend's reach of at most 512 pixels.
use crate::{
    Error,
    colour::srgb,
    modules::{Region, Stage},
};

pub(crate) mod blend;
pub(crate) mod crop;
pub(crate) mod encode;
pub(crate) mod grain;
pub(crate) mod resample;
#[cfg(test)]
mod tests;

use crop::CropPlan;
use encode::Encoding;
use grain::RingGrain;

/// The longest side of a candidate's preview.
pub(crate) use crate::artifacts::candidates::MAX_PREVIEW_SIDE;

/// Three `f32` planes of one size.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Planes {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) c: [Vec<f32>; 3],
}

impl Planes {
    pub(crate) fn new(width: usize, height: usize) -> Self {
        let zero = vec![0.0; width * height];
        Self {
            width,
            height,
            c: [zero.clone(), zero.clone(), zero],
        }
    }
}

/// A region's coverage over its bounds, `0..=1` a pixel; exactly 0 outside them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Coverage {
    bounds: Region,
    values: Vec<f32>,
}

impl Coverage {
    /// Coverage over `bounds`, row-major.
    pub(crate) fn new(bounds: Region, values: Vec<f32>) -> Result<Self, Error> {
        if values.len() as u64 != bounds.pixels() {
            return Err(Error::internal(format!(
                "a coverage of {} values for {} pixels",
                values.len(),
                bounds.pixels()
            )));
        }
        Ok(Self { bounds, values })
    }

    /// The same coverage over the tight bounds of its pixels above zero; `None` when none is.
    pub(crate) fn tightened(&self) -> Option<Self> {
        let (w, b) = (self.bounds.width as usize, self.bounds);
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for (i, &value) in self.values.iter().enumerate() {
            if value > 0.0 {
                let (x, y) = ((i % w) as u32, (i / w) as u32);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
        if x0 == u32::MAX {
            return None;
        }
        let bounds = Region {
            x0: b.x0 + x0,
            y0: b.y0 + y0,
            width: x1 - x0,
            height: y1 - y0,
        };
        Some(Self {
            bounds,
            values: self.over(bounds),
        })
    }

    pub(crate) fn bounds(&self) -> Region {
        self.bounds
    }

    /// The coverage at stage pixel `(x, y)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn at(&self, x: u32, y: u32) -> f32 {
        if !self.bounds.contains(x, y) {
            return 0.0;
        }
        let b = self.bounds;
        self.values[((y - b.y0) * b.width + (x - b.x0)) as usize]
    }

    /// Stage row `y` within the bounds, with the stage column its first value is at.
    pub(crate) fn row(&self, y: u32) -> Option<(u32, &[f32])> {
        let b = self.bounds;
        (b.y0..b.y1()).contains(&y).then(|| {
            let start = ((y - b.y0) * b.width) as usize;
            (b.x0, &self.values[start..start + b.width as usize])
        })
    }

    /// The coverage over `rect`, densely.
    pub(crate) fn over(&self, rect: Region) -> Vec<f32> {
        let mut out = vec![0.0; rect.pixels() as usize];
        let b = self.bounds;
        let x0 = rect.x0.max(b.x0);
        let x1 = rect.x1().min(b.x1());
        let y0 = rect.y0.max(b.y0);
        let y1 = rect.y1().min(b.y1());
        if x0 >= x1 || y0 >= y1 {
            return out;
        }
        let len = (x1 - x0) as usize;
        for y in y0..y1 {
            let src = ((y - b.y0) * b.width + (x0 - b.x0)) as usize;
            let dst = ((y - rect.y0) * rect.width + (x0 - rect.x0)) as usize;
            out[dst..dst + len].copy_from_slice(&self.values[src..src + len]);
        }
        out
    }
}

/// Where one fill reads and works, decided from its region's bounds, the stage and the model's
/// side before anything is read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FillPlan {
    pub(crate) crop: CropPlan,
    /// The model's square side.
    pub(crate) n: usize,
    /// Stage pixels per model pixel.
    pub(crate) scale: f64,
    /// Laplacian pyramid levels.
    pub(crate) levels: usize,
    /// The window the enlargement, grain and blend work over: the bounds grown by the blend's
    /// reach, inside the square.
    pub(crate) window: Region,
    /// Width of the ring the grain is estimated in.
    pub(crate) ring_width: u32,
    /// The input the later stages read: the window and the ring's.
    pub(crate) area: Region,
}

impl FillPlan {
    pub(crate) fn new(bounds: Region, stage: Stage, n: usize) -> Self {
        let crop = crop::plan_crop(bounds, stage);
        let scale = f64::from(crop.side) / n as f64;
        let levels = crop::blend_levels(scale);
        let window = crop::grow_within(bounds, crop::blend_reach(levels), crop.inside);
        let ring_width = crop::ring_width(bounds);
        let ring = crop::grow_within(bounds, ring_width + 4, crop.inside);
        Self {
            crop,
            n,
            scale,
            levels,
            window,
            ring_width,
            area: crop::union(window, ring),
        }
    }
}

/// What the input gives every candidate of one request: read once, then shared by each.
#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub(crate) plan: FillPlan,
    pub(crate) coverage: Coverage,
    pub(crate) encoding: Encoding,
    /// The model's input, `n × n` interleaved RGB, display-referred `0..=1`, the hole zeroed.
    pub(crate) model_image: Vec<f32>,
    /// The model's mask, `n × n`: 1 in the hole, 0 elsewhere.
    pub(crate) model_mask: Vec<f32>,
    hole: Vec<u8>,
    /// The input's codes over the plan's area.
    input: Planes,
    /// The input's linear values over the coverage's bounds, which the patch holds where its
    /// coverage is zero.
    linear: Vec<[f32; 3]>,
    pub(crate) ring: Option<RingGrain>,
}

/// One finished candidate: its patch over the coverage's tight bounds and its preview.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Finished {
    /// `[x, y, width, height]` in stage pixels.
    pub(crate) rect: [u32; 4],
    /// Linear RGB, interleaved, a pixel of `rect` each.
    pub(crate) linear: Vec<f32>,
    pub(crate) coverage: Vec<f32>,
    /// 8-bit sRGB RGB of the composite over `rect`, reduced to at most 1024 on its longer side.
    pub(crate) preview: Vec<u8>,
    pub(crate) preview_width: u32,
    pub(crate) preview_height: u32,
    /// The grain σ added per channel, in codes.
    pub(crate) grain: [f32; 3],
    /// Pixels in the band the seam correction measured.
    pub(crate) seam_band: usize,
}

/// A preview's size: `width × height` reduced to at most [`MAX_PREVIEW_SIDE`] on its longer side.
pub(crate) fn preview_size(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= MAX_PREVIEW_SIDE {
        return (width, height);
    }
    let scale = |side: u32| {
        (u64::from(side) * u64::from(MAX_PREVIEW_SIDE) / u64::from(longest)).max(1) as u32
    };
    (scale(width), scale(height))
}

impl Context {
    /// Read and prepare one fill: the square's inside read through `read` in bands of at most
    /// `tile_pixels`, reduced to the model's side, encoded, and the area the later stages read
    /// kept as codes. `coverage` is over its tight bounds. `checkpoint` is called between bands.
    pub(crate) fn prepare(
        coverage: Coverage,
        stage: Stage,
        n: usize,
        tile_pixels: u64,
        read: &mut dyn FnMut(Region) -> Result<Vec<[f32; 3]>, Error>,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Self, Error> {
        let plan = FillPlan::new(coverage.bounds(), stage, n);
        let area = plan.area;
        let mut input = Planes::new(area.width as usize, area.height as usize);
        let reduced = resample::reduce(
            &plan.crop,
            stage,
            n,
            &coverage,
            tile_pixels,
            read,
            &mut |band, pixels| {
                let y0 = band.y0.max(area.y0);
                let y1 = band.y1().min(area.y1());
                for y in y0..y1 {
                    let src = ((y - band.y0) * band.width + (area.x0 - band.x0)) as usize;
                    let dst = ((y - area.y0) * area.width) as usize;
                    for (x, p) in pixels[src..src + area.width as usize].iter().enumerate() {
                        for (plane, &value) in input.c.iter_mut().zip(p) {
                            plane[dst + x] = value;
                        }
                    }
                }
            },
            checkpoint,
        )?;
        let bounds = coverage.bounds();
        let mut linear = Vec::with_capacity(bounds.pixels() as usize);
        for y in bounds.y0..bounds.y1() {
            for x in bounds.x0..bounds.x1() {
                let i = ((y - area.y0) * area.width + (x - area.x0)) as usize;
                linear.push([input.c[0][i], input.c[1][i], input.c[2][i]]);
            }
        }
        let encoding = Encoding::measure(
            reduced
                .pixels
                .iter()
                .zip(&reduced.hole)
                .filter(|(_, hole)| **hole == 0)
                .map(|(p, _)| p[0].max(p[1]).max(p[2])),
        );
        for plane in &mut input.c {
            for value in plane.iter_mut() {
                *value = encoding.code(*value);
            }
        }
        let mut model_image = Vec::with_capacity(n * n * 3);
        for (p, &hole) in reduced.pixels.iter().zip(&reduced.hole) {
            for &v in p {
                model_image.push(if hole == 0 {
                    encoding.code(v) / 255.0
                } else {
                    0.0
                });
            }
        }
        let model_mask = reduced.hole.iter().map(|&h| f32::from(h)).collect();
        let ring = grain::ring_grain(&input, area, &coverage, bounds, plan.ring_width);
        Ok(Self {
            plan,
            coverage,
            encoding,
            model_image,
            model_mask,
            hole: reduced.hole,
            input,
            linear,
            ring,
        })
    }

    /// The candidate a model answered `generated` with (`n × n` interleaved RGB, display-referred),
    /// its grain drawn from `seed`.
    pub(crate) fn finish(&self, generated: &[f32], seed: u64) -> Result<Finished, Error> {
        let FillPlan {
            crop,
            n,
            scale,
            levels,
            window,
            area,
            ..
        } = self.plan;
        if generated.len() != n * n * 3 {
            return Err(Error::render(format!(
                "the model answered {} values for a {n}x{n} fill",
                generated.len()
            )));
        }
        let mut model = Planes::new(n, n);
        for (i, p) in generated.chunks_exact(3).enumerate() {
            for (plane, &value) in model.c.iter_mut().zip(p) {
                plane[i] = value.clamp(0.0, 1.0) * 255.0;
            }
        }
        let mut fill = Planes {
            width: window.width as usize,
            height: window.height as usize,
            c: [0, 1, 2].map(|c| resample::upscale_plane(&model.c[c], n, &crop, window)),
        };
        drop(model);
        let cov = self.coverage.over(window);

        let mut added = [0.0_f32; 3];
        if let Some(ring) = &self.ring {
            let kernel = grain::grain_kernel(f64::from(ring.correlation_px));
            let gain = grain::grain_highpass_gain(&kernel, 1) as f32;
            let inner = grain::fill_stats(&fill, &cov);
            if scale > 1.0 {
                for c in 0..3 {
                    // Grain makes up only the fine texture enlarging the answer lost: the ring's
                    // high-pass energy the enlarged fill lacks.
                    let deficit = ring.highpass[c].powi(2) - inner.map_or(0.0, |s| s[c].powi(2));
                    added[c] = deficit.max(0.0).sqrt() / gain;
                }
            }
            if added.iter().any(|&a| a > 0.0) {
                grain::add_grain(
                    &mut fill,
                    &cov,
                    window,
                    self.coverage.bounds(),
                    added,
                    ring.channel_correlation,
                    &kernel,
                    seed,
                );
            }
        }

        let seam_band = blend::seam_correction(
            &mut fill,
            &self.input,
            area,
            window,
            &cov,
            &crop,
            &self.hole,
            n,
            scale,
        );
        let correction = blend::pyramid_correction(&fill, &self.input, area, window, &cov, levels);
        for c in 0..3 {
            for (v, d) in fill.c[c].iter_mut().zip(&correction.c[c]) {
                *v += d;
            }
        }
        drop(correction);

        let bounds = self.coverage.bounds();
        let pixels = bounds.pixels() as usize;
        let mut linear = Vec::with_capacity(pixels * 3);
        let mut coverage = Vec::with_capacity(pixels);
        let ww = window.width as usize;
        for y in bounds.y0..bounds.y1() {
            for x in bounds.x0..bounds.x1() {
                let i = ((y - bounds.y0) * bounds.width + (x - bounds.x0)) as usize;
                let c = self.coverage.values[i];
                let input = self.linear[i];
                let patch = if c > 0.0 {
                    let w = (y - window.y0) as usize * ww + (x - window.x0) as usize;
                    [0, 1, 2].map(|k| self.encoding.linear(fill.c[k][w]))
                } else {
                    input
                };
                linear.extend_from_slice(&patch);
                coverage.push(c);
            }
        }
        let (preview_width, preview_height) = preview_size(bounds.width, bounds.height);
        let preview = self.preview(&linear, &coverage, preview_width, preview_height);
        Ok(Finished {
            rect: [bounds.x0, bounds.y0, bounds.width, bounds.height],
            linear,
            coverage,
            preview,
            preview_width,
            preview_height,
            grain: added,
            seam_band,
        })
    }

    /// The repair stage's composite of a patch over the coverage's bounds, `in + c · (patch − in)`
    /// in `f64` and the input itself where `c` is zero, box-averaged to `width × height` and
    /// quantized to 8-bit sRGB.
    fn preview(&self, patch: &[f32], coverage: &[f32], width: u32, height: u32) -> Vec<u8> {
        let quantizer = srgb::quantizer();
        let bounds = self.coverage.bounds();
        let (bw, bh) = (u64::from(bounds.width), u64::from(bounds.height));
        let mut out = Vec::with_capacity((width * height * 3) as usize);
        for py in 0..u64::from(height) {
            let y0 = py * bh / u64::from(height);
            let y1 = ((py + 1) * bh / u64::from(height)).max(y0 + 1);
            for px in 0..u64::from(width) {
                let x0 = px * bw / u64::from(width);
                let x1 = ((px + 1) * bw / u64::from(width)).max(x0 + 1);
                let mut sum = [0.0_f64; 3];
                for y in y0..y1 {
                    for x in x0..x1 {
                        let i = (y * bw + x) as usize;
                        let c = f64::from(coverage[i]);
                        for (k, s) in sum.iter_mut().enumerate() {
                            let input = f64::from(self.linear[i][k]);
                            *s += if c > 0.0 {
                                input + c * (f64::from(patch[i * 3 + k]) - input)
                            } else {
                                input
                            };
                        }
                    }
                }
                let count = ((y1 - y0) * (x1 - x0)) as f64;
                for s in sum {
                    out.push(quantizer.channel(s / count));
                }
            }
        }
        out
    }
}
