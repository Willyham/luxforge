//! The view's bounds and the area average that reduces a stage to them: the bounds a view draws in
//! ([`ProxyBounds`]), the reduced stage fitted to them ([`ProxyPlan`], which the GPU's reduced
//! stage plans in `render::gpu::fit`), the area-average weights the GPU reduces its source with
//! ([`area_coverage`], [`ProxyPlan::coverage`]), a prepared source's identity ([`ProxyIdentity`])
//! and the reduction of the reference renderer's finished frame to the view ([`reduce_raster`]).
//! The names say "proxy" for history: the CPU proxy itself, the drag path of a session the GPU does
//! not draw at all, is `crate::cpu_proxy`, which builds its sources with this module's area average.
//!
//! Nothing here touches the catalog and nothing here belongs on the owner thread: a reduction is
//! frame work, bounded by the same 512 MiB frame limit as a rendered raster and parallelized on the
//! shared Rayon pool from the proxy pass's own measured threshold (`render::parallel`).

#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Cancel, Error, PreviewSource, Raster, SourceImage,
    colour::srgb::{decode_pixel_in, decode_table, quantizer},
    render::{frame_mut, zeroed_frame},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Physical pixels of the photo area the display can show: the bounds a reference frame is reduced
/// to, a drag's CPU proxy is rendered at and the GPU's whole-frame plans are planned at. A plan
/// clamps them before planning ([`Self::clamped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyBounds {
    pub width: u32,
    pub height: u32,
}

impl ProxyBounds {
    pub(crate) const MAX_SIDE: u32 = 4096;
    pub(crate) const MAX_PIXELS: u64 = 8_000_000;

    /// Clamp each side to `Self::MAX_SIDE` and scale both down uniformly until the product is
    /// within `Self::MAX_PIXELS`. A zero side becomes one, so the result always describes a
    /// buildable rectangle and no caller has to handle a refusal.
    pub fn clamped(self) -> Self {
        let mut width = self.width.clamp(1, Self::MAX_SIDE);
        let mut height = self.height.clamp(1, Self::MAX_SIDE);
        let pixels = u64::from(width) * u64::from(height);
        if pixels > Self::MAX_PIXELS {
            // Uniform in both axes, so the aspect ratio the caller asked for survives: each side is
            // multiplied by the square root of the area ratio and floored.
            let scale = (Self::MAX_PIXELS as f64 / pixels as f64).sqrt();
            width = ((f64::from(width) * scale).floor() as u32).max(1);
            height = ((f64::from(height) * scale).floor() as u32).max(1);
            // Flooring cannot raise the product above the limit in exact arithmetic; this settles
            // the last pixel when the square root rounded upwards, and runs at most a few times.
            while u64::from(width) * u64::from(height) > Self::MAX_PIXELS {
                if width >= height {
                    width -= 1;
                } else {
                    height -= 1;
                }
            }
        }
        Self { width, height }
    }
}

/// The proxy stage a job renders against, with the bounds it was derived from and the window of it
/// the proxy source holds. The bounds are part of the plan because they are part of the cache
/// identity: a resized window produces a different plan even when the rounded dimensions happen to
/// agree. So is the window: a crop that reads another part of the stage misses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyPlan {
    /// The whole proxy stage: the source scaled by the proxy scale, which every normalized payload,
    /// mask and spatial radius of the stack is resolved against.
    pub width: u32,
    pub height: u32,
    pub bounds: ProxyBounds,
    /// The rectangle of the whole reduced stage a GPU plan's reduced source holds, when the stack
    /// reads less than all of it — a crop, and what each boundary before it needs around what it
    /// reads — or `None` for the whole stage (the GPU window walk). A CPU proxy
    /// always holds its whole stage (`ProxyPlan::whole_within`).
    pub(crate) window: Option<ProxyWindow>,
}

/// A rectangle of the whole proxy stage, in its pixels: the part a windowed proxy source holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyWindow {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ProxyPlan {
    /// The dimensions of the proxy source this plan builds: the window's, or the whole stage's.
    pub(crate) fn source_dimensions(&self) -> (u32, u32) {
        self.window.map_or((self.width, self.height), |window| {
            (window.width, window.height)
        })
    }

    /// The same plan over the whole proxy stage, with no window: the exact downscale a windowed
    /// proxy is a part of, which the proofs render against.
    pub(crate) fn whole(self) -> Self {
        Self {
            window: None,
            ..self
        }
    }

    /// The weights this plan's build averages a `source`-sized source with, across and then down
    /// ([`Coverage`]), for the GPU's reduction of the source it holds, which averages with the
    /// build's own weights (`docs/design/gpu-preview.md`, "The GPU source"). `O(source + output)`,
    /// and reads no pixel; a plan that does not fit the source is refused as its build refuses it.
    pub fn coverage(&self, source: (u32, u32)) -> Result<[ProxyCoverage; 2], Error> {
        check_plan(*self, source.0, source.1)?;
        let of = |coverage: Coverage| ProxyCoverage {
            first: coverage.first,
            offsets: coverage
                .offsets
                .into_iter()
                .map(|offset| offset as u32)
                .collect(),
            weights: coverage.weights,
        };
        Ok([
            of(Coverage::new(source.0, self.width)),
            of(Coverage::new(source.1, self.height)),
        ])
    }

    /// The rectangle `[x, y, width, height]` of the whole proxy stage the proxy source holds: the
    /// window, or the whole stage.
    pub fn held(&self) -> [u32; 4] {
        let window = window_of(*self);
        [window.x, window.y, window.width, window.height]
    }
}

/// One axis of an area average of `from` samples into `to`, `to` at most `from`: the weights a
/// proxy's build reduces a source with, which the picture at rest reduces the output stage to the
/// view's size with too. `O(from + to)`.
pub fn area_coverage(from: u32, to: u32) -> ProxyCoverage {
    let coverage = Coverage::new(from.max(1), to.clamp(1, from.max(1)));
    ProxyCoverage {
        first: coverage.first,
        offsets: coverage
            .offsets
            .into_iter()
            .map(|offset| offset as u32)
            .collect(),
        weights: coverage.weights,
    }
}

/// One axis of a proxy's area average, as its build weighs it: for each output index the first
/// source index it reads, and the half-open range of `weights` that belongs to it — each weight
/// the share of the output's interval `[i·S/o, (i+1)·S/o)` its source sample covers, over the
/// interval's length — so an output is the exact mean over its source interval.
#[derive(Clone, Debug, PartialEq)]
pub struct ProxyCoverage {
    pub first: Vec<u32>,
    /// One more than the output's length: output `i`'s weights are `offsets[i]..offsets[i + 1]`.
    pub offsets: Vec<u32>,
    pub weights: Vec<f64>,
}

/// What identifies a prepared source's pixels for cache purposes.
///
/// A JPEG source is identified by its fingerprint, its dimensions and the EXIF orientation it
/// carries. A RAW source is identified by the address of its developed plane allocation and the
/// view over it, so a white-balance redevelopment — which produces new planes — misses, and a crop
/// or orientation change of the view misses as well.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyIdentity {
    Jpeg {
        fingerprint: String,
        width: u32,
        height: u32,
        orientation: u8,
    },
    Raw {
        fingerprint: String,
        /// The development the planes belong to (`LinearImage::development`): a process-unique
        /// number, so a redevelopment misses even when its planes reuse the old allocation's
        /// address.
        development: u64,
        crop: [u32; 4],
        orientation: u8,
    },
}

impl PreviewSource {
    /// What this source's pixels are, for cache purposes. `O(1)` and reads no pixels.
    pub fn identity(&self) -> ProxyIdentity {
        match self {
            Self::Jpeg(image) => ProxyIdentity::Jpeg {
                fingerprint: image.fingerprint.clone(),
                width: image.width,
                height: image.height,
                orientation: image.orientation,
            },
            Self::Raw { image, .. } => {
                let (crop, orientation) = image.view();
                ProxyIdentity::Raw {
                    fingerprint: image.fingerprint().to_owned(),
                    development: image.development(),
                    crop,
                    orientation,
                }
            }
        }
    }
}

pub(crate) fn check_plan(
    plan: ProxyPlan,
    source_width: u32,
    source_height: u32,
) -> Result<(), Error> {
    if plan.width == 0 || plan.height == 0 {
        return Err(Error::validation(
            "a proxy plan's dimensions must be nonzero",
        ));
    }
    if plan.width > source_width || plan.height > source_height {
        return Err(Error::validation(format!(
            "a proxy plan never upscales: {}x{} from a {source_width}x{source_height} source",
            plan.width, plan.height
        )));
    }
    if let Some(window) = plan.window
        && (window.width == 0
            || window.height == 0
            || u64::from(window.x) + u64::from(window.width) > u64::from(plan.width)
            || u64::from(window.y) + u64::from(window.height) > u64::from(plan.height))
    {
        return Err(Error::validation(format!(
            "a proxy window {}x{} at ({}, {}) lies outside its {}x{} proxy stage",
            window.width, window.height, window.x, window.y, plan.width, plan.height
        )));
    }
    Ok(())
}

/// The window a plan builds, as a rectangle of its proxy stage: the plan's own, or the whole stage.
pub(crate) fn window_of(plan: ProxyPlan) -> ProxyWindow {
    plan.window.unwrap_or(ProxyWindow {
        x: 0,
        y: 0,
        width: plan.width,
        height: plan.height,
    })
}

/// The element count of a float buffer of `width × height × 3`, refused when it would exceed the
/// frame limit. Same shape and the same error kind as [`Raster::expected_len`], which bounds the
/// byte frames beside it.
pub(crate) fn float_values(width: u32, height: u32, what: &str) -> Result<usize, Error> {
    let values = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| Error::resource_limit(format!("{what} dimensions overflow")))?;
    let bytes = values
        .checked_mul(std::mem::size_of::<f32>() as u64)
        .ok_or_else(|| Error::resource_limit(format!("{what} byte length overflow")))?;
    if bytes > luxforge_raw::MAX_FRAME_BYTES {
        return Err(Error::resource_limit(format!("{what} exceeds 512 MiB")));
    }
    usize::try_from(values).map_err(|_| Error::resource_limit(format!("{what} is not addressable")))
}

/// The source samples that one output sample averages along one axis, with the fractional coverage
/// weights that make the output the exact mean over its source interval `[i·S/o, (i+1)·S/o)`.
///
/// The whole table is built once per axis and holds at most `source + output` weights, so nothing
/// here scales with the area and no weight is recomputed per row or per column.
pub(crate) struct Coverage {
    /// Per output index, the first source index it reads.
    first: Vec<u32>,
    /// Per output index, the half-open range of `weights` that belongs to it.
    offsets: Vec<usize>,
    weights: Vec<f64>,
}

impl Coverage {
    /// `source` and `output` are both at least one, and `output` is at most `source`.
    fn new(source: u32, output: u32) -> Self {
        debug_assert!(output >= 1 && source >= output);
        let ratio = f64::from(source) / f64::from(output);
        let mut first = Vec::with_capacity(output as usize);
        let mut offsets = Vec::with_capacity(output as usize + 1);
        let mut weights = Vec::with_capacity(source as usize + output as usize);
        offsets.push(0);
        for index in 0..output {
            let start = f64::from(index) * ratio;
            let end = f64::from(index + 1) * ratio;
            let begin = (start.floor() as u32).min(source - 1);
            let last = (end.ceil() as u32).clamp(begin + 1, source);
            first.push(begin);
            for sample in begin..last {
                let low = start.max(f64::from(sample));
                let high = end.min(f64::from(sample + 1));
                weights.push((high - low).max(0.0) / ratio);
            }
            offsets.push(weights.len());
        }
        Self {
            first,
            offsets,
            weights,
        }
    }

    /// The first source index and the consecutive weights for one output index.
    #[inline]
    fn span(&self, index: usize) -> (u32, &[f64]) {
        (
            self.first[index],
            &self.weights[self.offsets[index]..self.offsets[index + 1]],
        )
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.first.len()
    }
}

/// The intermediate one band of the box downscale holds: about 1 MiB of horizontally averaged rows
/// per worker, whatever the source's size.
pub(crate) const BAND_BYTES: usize = 1 << 20;

/// The one separable area average every proxy source is built with, whatever the source's pixels
/// are: a horizontal pass that reads each source pixel in linear light into one intermediate row
/// per source row, and a vertical pass that averages those rows into each output pixel.
///
/// Both passes accumulate in f64 with the [`Coverage`] weights, so an output pixel is the exact mean
/// over its source rectangle up to the one f32 store between the passes. The two source kinds differ
/// only in how a source pixel is read — a JPEG code decoded through the render path's own sRGB
/// table, a RAW value read through its view — and in how the output is stored, which each caller
/// does in the closure it hands [`Self::band`]; the arithmetic is this one implementation.
///
/// The window's output rows run in bands. A band runs the horizontal pass over only the source rows
/// its output rows average, into the intermediate its worker holds, then the vertical pass over
/// them. Two neighbouring bands share at most the one source row that straddles their boundary,
/// which each averages horizontally with the same weights in the same order, so it is the same f32
/// row in both; and each output pixel's vertical sum runs over its own span in the same order in
/// whatever band it falls. A band boundary therefore changes no sum, and the proxy is the same
/// bytes at any band size.
///
/// Allocations: one intermediate per worker, of `window width × 3` f32 per source row a band
/// reads. A band holds as many output rows as keep that near [`BAND_BYTES`], and at least one, so
/// the intermediate is about 1 MiB, or one output row's source rows when that is larger — about
/// one source row's worth of pixels, since the proxy keeps the source's aspect. Every source pixel
/// the window reads is read once, besides the shared boundary rows, so no other scratch exists.
pub(crate) struct BoxDownscale {
    horizontal: Coverage,
    vertical: Coverage,
    window: ProxyWindow,
    /// Output rows per band; the last band may hold fewer.
    pub(crate) band_rows: usize,
    pub(crate) parallel: bool,
}

impl BoxDownscale {
    /// The coverage tables and band size for `plan` over a `source_width × source_height` source,
    /// with bands of about `band_bytes` of intermediate. `O(source + output)`, and reads no pixel.
    pub(crate) fn new(
        source_width: u32,
        source_height: u32,
        plan: ProxyPlan,
        band_bytes: usize,
    ) -> Result<Self, Error> {
        let window = window_of(plan);
        let horizontal = Coverage::new(source_width, plan.width);
        let vertical = Coverage::new(source_height, plan.height);
        let stride_bytes = window.width as usize * 3 * std::mem::size_of::<f32>();
        // `n` output rows read at most `n · ratio + 2` source rows, so a band of this many output
        // rows reads about `band_bytes` of them.
        let ratio = f64::from(source_height) / f64::from(plan.height);
        let budget_rows = (band_bytes / stride_bytes).max(1);
        let band_rows = (((budget_rows as f64 - 2.0) / ratio).floor() as usize)
            .clamp(1, window.height as usize);
        let downscale = Self {
            horizontal,
            vertical,
            window,
            band_rows,
            parallel: false,
        };
        // The largest band's intermediate, refused past the frame limit like any frame.
        let widest = (0..downscale.bands())
            .map(|band| {
                let (first, end) = downscale.source_rows(band);
                end - first
            })
            .max()
            .unwrap_or(0);
        float_values(window.width, widest, "proxy downscale intermediate")?;
        // The source pixels the window reads, which is the work: the whole source for a whole
        // proxy, and about the window's share of it for a windowed one.
        let (first_row, _) = downscale.source_rows(0);
        let (_, end_row) = downscale.source_rows(downscale.bands() - 1);
        let (column_first, _) = downscale.horizontal.span(window.x as usize);
        let (column_last, column_weights) = downscale
            .horizontal
            .span((window.x + window.width - 1) as usize);
        let read_columns = column_last + column_weights.len() as u32 - column_first;
        Ok(Self {
            parallel: crate::render::parallel::pooled(
                crate::render::parallel::RenderPass::Proxy,
                u64::from(read_columns) * u64::from(end_row - first_row),
            ),
            ..downscale
        })
    }

    /// How many bands the window's output rows make.
    pub(crate) fn bands(&self) -> usize {
        (self.window.height as usize).div_ceil(self.band_rows)
    }

    /// The half-open range of source rows band `band`'s output rows average, and nothing else.
    fn source_rows(&self, band: usize) -> (u32, u32) {
        let first = self.window.y as usize + band * self.band_rows;
        let last = (first + self.band_rows).min((self.window.y + self.window.height) as usize) - 1;
        let (last_first, last_weights) = self.vertical.span(last);
        (
            self.vertical.span(first).0,
            last_first + last_weights.len() as u32,
        )
    }

    /// Both passes for band `band`, in `rows`, the intermediate its worker holds: the horizontal
    /// pass over the band's source rows, then hands `store(y, x, sum)` the weighted mean of each
    /// output pixel's column of intermediate rows, in f64, where `y` counts from the band's first
    /// output row. `row(y)` is source row `y`'s reader, taken once per row, and the reader's
    /// `read(x)` is one of its pixels in linear light; it is only asked for coordinates inside the
    /// source. Resolving the row once lets a reader index one row's slice rather than the whole
    /// source for every pixel. `cancel` is checked once per source row and once per output row.
    pub(crate) fn band<Read: Fn(u32) -> [f32; 3]>(
        &self,
        band: usize,
        rows: &mut Vec<f32>,
        cancel: &Cancel,
        row: &impl Fn(u32) -> Read,
        mut store: impl FnMut(usize, usize, [f64; 3]),
    ) -> Result<(), Error> {
        let window = self.window;
        let stride = window.width as usize * 3;
        let (first_row, end_row) = self.source_rows(band);
        let len = (end_row - first_row) as usize * stride;
        // Every value of the band's rows is written below before it is read, so a held buffer is
        // only grown, never cleared.
        if rows.len() < len {
            rows.resize(len, 0.0);
        }
        let rows = &mut rows[..len];
        for (y, out) in rows.chunks_exact_mut(stride).enumerate() {
            cancel.check()?;
            let read = row(first_row + y as u32);
            for column in 0..window.width as usize {
                let (first, weights) = self.horizontal.span(window.x as usize + column);
                let mut sum = [0f64; 3];
                for (offset, weight) in weights.iter().enumerate() {
                    let linear = read(first + offset as u32);
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += f64::from(linear[channel]) * weight;
                    }
                }
                for (channel, value) in sum.iter().enumerate() {
                    out[column * 3 + channel] = *value as f32;
                }
            }
        }
        let first_output = window.y as usize + band * self.band_rows;
        let count = self
            .band_rows
            .min((window.y + window.height) as usize - first_output);
        for y in 0..count {
            cancel.check()?;
            // The row's span and its slice of the intermediate are resolved once per row, not
            // once per pixel.
            let (first, weights) = self.vertical.span(first_output + y);
            let start = (first - first_row) as usize * stride;
            let span = &rows[start..start + weights.len() * stride];
            for x in 0..window.width as usize {
                let mut sum = [0f64; 3];
                for (offset, weight) in weights.iter().enumerate() {
                    let at = offset * stride + x * 3;
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += f64::from(span[at + channel]) * weight;
                    }
                }
                store(y, x, sum);
            }
        }
        Ok(())
    }
}

/// Finished pixels reduced by the same linear-light area average a source proxy is built with
/// ([`crate::render::reduce_to_view`]): the source allocation is borrowed through its Arc, and only
/// the reduced raster is allocated.
pub(crate) fn reduce_raster(
    raster: &Raster,
    plan: ProxyPlan,
    cancel: &Cancel,
) -> Result<Raster, Error> {
    check_plan(plan, raster.width, raster.height)?;
    if plan.window.is_some()
        || u64::from(plan.width) * u64::from(plan.height) > ProxyBounds::MAX_PIXELS
    {
        return Err(Error::resource_limit(
            "the view's frame exceeds the 8 MP bound",
        ));
    }
    let source = SourceImage {
        width: raster.width,
        height: raster.height,
        rgba: raster.rgba.clone(),
        fingerprint: raster.source_fingerprint.clone(),
        orientation: 1,
        capture: Default::default(),
    };
    let reduced = downscale_jpeg(&source, plan, cancel, BAND_BYTES)?;
    Ok(Raster {
        width: reduced.width,
        height: reduced.height,
        rgba: reduced.rgba,
        source_fingerprint: raster.source_fingerprint.clone(),
        snapshot_id: raster.snapshot_id.clone(),
    })
}

/// The area average of a JPEG source, re-quantized through the render path's own threshold
/// boundary. A uniform region therefore comes out as exactly its own code, and the proxy of an
/// identity stack agrees with the exact render's arithmetic everywhere it can. The output frame is
/// written in place and returned as the proxy's pixels, with no copy.
pub(crate) fn downscale_jpeg(
    source: &SourceImage,
    plan: ProxyPlan,
    cancel: &Cancel,
    band_bytes: usize,
) -> Result<SourceImage, Error> {
    let (width, height) = plan.source_dimensions();
    let source_len = Raster::expected_len(source.width, source.height)?;
    if source.rgba.len() != source_len {
        return Err(Error::validation(
            "source buffer length does not match its dimensions",
        ));
    }
    let output_len = Raster::expected_len(width, height)?;
    let source_stride = source.width as usize * 4;
    // The tables are taken once for the build, not once per pixel.
    let (table, quantizer) = (decode_table(), quantizer());
    let read_row = |y: u32| {
        let start = y as usize * source_stride;
        let bytes = &source.rgba[start..start + source_stride];
        move |x: u32| {
            let at = x as usize * 4;
            decode_pixel_in(table, [bytes[at], bytes[at + 1], bytes[at + 2]])
        }
    };
    let downscale = BoxDownscale::new(source.width, source.height, plan, band_bytes)?;
    let mut frame = zeroed_frame(output_len);
    {
        let output = frame_mut(&mut frame);
        let output_stride = width as usize * 4;
        let pass = |rows: &mut Vec<f32>, (band, out): (usize, &mut [u8])| {
            downscale.band(band, rows, cancel, &read_row, |y, x, sum| {
                let pixel = &mut out[y * output_stride + x * 4..][..4];
                for (channel, value) in sum.iter().enumerate() {
                    pixel[channel] = quantizer.channel(*value);
                }
                pixel[3] = 255;
            })
        };
        let band_len = downscale.band_rows * output_stride;
        if downscale.parallel {
            output
                .par_chunks_mut(band_len)
                .enumerate()
                .try_for_each_init(Vec::new, pass)?;
        } else {
            let mut rows = Vec::new();
            output
                .chunks_mut(band_len)
                .enumerate()
                .try_for_each(|band| pass(&mut rows, band))?;
        }
    }

    Ok(SourceImage {
        width,
        height,
        rgba: frame,
        fingerprint: source.fingerprint.clone(),
        orientation: source.orientation,
        capture: source.capture.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BoxRect, CropStage, Layer, ModuleRegistry, Orientation, RECIPE_FORMAT, Recipe, SnapshotId,
    };

    fn jpeg_source(width: u32, height: u32, pixels: &[[u8; 3]]) -> PreviewSource {
        assert_eq!(pixels.len() as u64, u64::from(width) * u64::from(height));
        let mut rgba = Vec::with_capacity(pixels.len() * 4);
        for pixel in pixels {
            rgba.extend_from_slice(pixel);
            rgba.push(255);
        }
        PreviewSource::Jpeg(SourceImage {
            width,
            height,
            rgba: rgba.into(),
            fingerprint: "sha256:proxy-fixture".into(),
            orientation: 1,
            capture: Default::default(),
        })
    }

    /// A source whose dimensions are photo sized and whose buffer is empty: `proxy_plan` compiles
    /// a recipe and reads no pixels, which is exactly what this proves.
    fn dimensions_only(width: u32, height: u32) -> PreviewSource {
        PreviewSource::Jpeg(SourceImage {
            width,
            height,
            rgba: Vec::new().into(),
            fingerprint: "sha256:dimensions-only".into(),
            orientation: 1,
            capture: Default::default(),
        })
    }

    fn recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    /// The payload a crop draft would commit: the whole rotated box fitted onto the stage and
    /// normalized, which is a rectangle the crop contract accepts at any angle.
    fn fitted_crop_layer(width: u32, height: u32, angle: f64) -> Layer {
        let stage = CropStage {
            width,
            height,
            angle,
        };
        let (box_width, box_height) = stage.bounding_box();
        let fitted = stage.fit_about_center(BoxRect {
            x: 0.0,
            y: 0.0,
            width: box_width,
            height: box_height,
        });
        Layer::crop(fitted.normalized(&stage))
    }

    fn plan(width: u32, height: u32, bounds: (u32, u32)) -> ProxyPlan {
        ProxyPlan {
            width,
            height,
            bounds: ProxyBounds {
                width: bounds.0,
                height: bounds.1,
            },
            window: None,
        }
    }

    fn jpeg_of(source: &PreviewSource) -> &SourceImage {
        match source {
            PreviewSource::Jpeg(image) => image,
            PreviewSource::Raw { .. } => panic!("a JPEG proxy stays a JPEG source"),
        }
    }

    /// The coverage weights of every output sample sum to one, at fractional and integer scales
    /// alike, which is what makes an average an average rather than a gain.
    #[test]
    fn fractional_coverage_weights_sum_to_one_per_output_sample() {
        for (source, output) in [
            (7u32, 3u32),
            (5, 2),
            (8, 3),
            (6, 4),
            (8, 4),
            (9, 9),
            (13, 1),
        ] {
            let coverage = Coverage::new(source, output);
            assert_eq!(coverage.len(), output as usize);
            let mut covered = vec![0f64; source as usize];
            for index in 0..output as usize {
                let (first, weights) = coverage.span(index);
                let sum: f64 = weights.iter().sum();
                assert!(
                    (sum - 1.0).abs() < 1e-12,
                    "{source}->{output} sample {index} weighs {sum}"
                );
                assert!(!weights.is_empty());
                assert!(first as usize + weights.len() <= source as usize);
                for (offset, weight) in weights.iter().enumerate() {
                    assert!(*weight > 0.0 || weights.len() == 1);
                    // A weight is a fraction of one source sample divided by the ratio, so
                    // multiplying by the ratio puts the contributions back into source units.
                    covered[first as usize + offset] +=
                        weight * f64::from(source) / f64::from(output);
                }
            }
            // Every source sample is fully spent across the outputs: the weights partition the
            // source, so nothing is read twice at full strength and nothing is skipped.
            for (index, weight) in covered.iter().enumerate() {
                assert!(
                    (weight - 1.0).abs() < 1e-9,
                    "{source}->{output} source sample {index} contributed {weight}"
                );
            }
        }
    }

    #[test]
    fn a_plan_fits_the_recipes_output_stage_into_the_clamped_bounds() {
        let registry = ModuleRegistry::builtin();
        let source = dimensions_only(6000, 4000);
        let identity = recipe(Vec::new());

        let bounds = ProxyBounds {
            width: 2000,
            height: 1500,
        };
        let plan = source
            .proxy_plan(&registry, &identity, bounds)
            .expect("a plan")
            .expect("a proxy is worthwhile");
        assert_eq!((plan.width, plan.height), (2000, 1333));
        assert_eq!(plan.bounds, bounds);

        // Bounds at least as large as the stage: no proxy, and the exact path runs unchanged. The
        // stage here is inside the clamps, so nothing but the scale decides.
        let small = dimensions_only(3000, 2000);
        for bounds in [(3000u32, 2000u32), (4000, 2000), (3000, 2600)] {
            assert!(
                small
                    .proxy_plan(
                        &registry,
                        &identity,
                        ProxyBounds {
                            width: bounds.0,
                            height: bounds.1
                        }
                    )
                    .expect("a decision")
                    .is_none(),
                "{bounds:?} is not smaller than the 3000x2000 stage"
            );
        }

        // Out-of-range bounds are clamped, not refused: 4096 per side and then 8 megapixels.
        let plan = source
            .proxy_plan(
                &registry,
                &identity,
                ProxyBounds {
                    width: 10000,
                    height: 10000,
                },
            )
            .expect("a plan")
            .expect("a proxy is worthwhile");
        assert_eq!(
            plan.bounds,
            ProxyBounds {
                width: 2828,
                height: 2828
            }
        );
        assert!(u64::from(plan.bounds.width) * u64::from(plan.bounds.height) <= 8_000_000);
        assert_eq!((plan.width, plan.height), (2828, 1885));
    }

    /// A rotated crop's proxy is scaled so that the crop's own output — not the rectangle it was
    /// cut from — fits the bounds.
    #[test]
    fn a_rotated_crop_fits_its_output_stage_into_the_bounds() {
        let registry = ModuleRegistry::builtin();
        let source = dimensions_only(6000, 4000);
        let stack = recipe(vec![
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            }),
            // The orientation layer turns the stage a quarter, so the crop plans against 4000x6000.
            fitted_crop_layer(4000, 6000, 7.0),
        ]);
        let bounds = ProxyBounds {
            width: 1600,
            height: 1200,
        };
        let full = registry
            .compile(6000, 4000, &stack)
            .expect("a compiled stack")
            .stage();
        assert!(
            full.width > bounds.width || full.height > bounds.height,
            "the full stage is larger than the bounds"
        );
        let plan = source
            .proxy_plan(&registry, &stack, bounds)
            .expect("a plan")
            .expect("a proxy is worthwhile");
        let proxy_stage = registry
            .compile(plan.width, plan.height, &stack)
            .expect("the same stack at proxy size")
            .stage();
        assert!(
            proxy_stage.width <= bounds.width && proxy_stage.height <= bounds.height,
            "the proxy output stage {}x{} does not fit {}x{}",
            proxy_stage.width,
            proxy_stage.height,
            bounds.width,
            bounds.height
        );
        // And it is not gratuitously small: it fills at least one of the two bounds.
        assert!(
            proxy_stage.width + 2 >= bounds.width || proxy_stage.height + 2 >= bounds.height,
            "the proxy output stage {}x{} wastes the bounds {}x{}",
            proxy_stage.width,
            proxy_stage.height,
            bounds.width,
            bounds.height
        );
    }

    #[test]
    fn bounds_are_clamped_to_a_side_and_an_area() {
        assert_eq!(
            ProxyBounds {
                width: 0,
                height: 0
            }
            .clamped(),
            ProxyBounds {
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            ProxyBounds {
                width: 1920,
                height: 1080
            }
            .clamped(),
            ProxyBounds {
                width: 1920,
                height: 1080
            }
        );
        let clamped = ProxyBounds {
            width: 10000,
            height: 10000,
        }
        .clamped();
        assert!(clamped.width <= ProxyBounds::MAX_SIDE && clamped.height <= ProxyBounds::MAX_SIDE);
        assert!(
            u64::from(clamped.width) * u64::from(clamped.height) <= ProxyBounds::MAX_PIXELS,
            "{clamped:?}"
        );
        // The side cap applies first, then the area is scaled down uniformly, so a very wide
        // request keeps its aspect ratio rather than losing one axis to the cap alone.
        let wide = ProxyBounds {
            width: 20000,
            height: 3000,
        }
        .clamped();
        assert!(wide.width <= ProxyBounds::MAX_SIDE && wide.height <= ProxyBounds::MAX_SIDE);
        assert!(u64::from(wide.width) * u64::from(wide.height) <= ProxyBounds::MAX_PIXELS);
        let capped = f64::from(ProxyBounds::MAX_SIDE) / 3000.0;
        let ratio = f64::from(wide.width) / f64::from(wide.height);
        assert!(
            (ratio - capped).abs() < 0.01,
            "{wide:?} does not keep the clamped aspect ratio {capped}"
        );
    }

    #[test]
    fn restoration_settled_reducer_matches_linear_area_reference_and_preserves_identity() {
        let pixels: Vec<[u8; 3]> = (0..53 * 41)
            .map(|i| {
                [
                    (i * 37 % 251) as u8,
                    (i * 11 % 239) as u8,
                    (i * 5 % 241) as u8,
                ]
            })
            .collect();
        let source = jpeg_source(53, 41, &pixels);
        let jpeg = jpeg_of(&source);
        let raster = Raster {
            width: 53,
            height: 41,
            rgba: jpeg.rgba.clone(),
            source_fingerprint: jpeg.fingerprint.clone(),
            snapshot_id: SnapshotId::new(),
        };
        for (w, h) in [(1, 1), (17, 13), (26, 20), (52, 40)] {
            let plan = plan(w, h, (w, h));
            let reduced = reduce_raster(&raster, plan, &Cancel::never()).unwrap();
            let expected = source.proxy(plan).unwrap();
            assert_eq!(reduced.rgba.as_ref(), jpeg_of(&expected).rgba.as_ref());
            assert_eq!(reduced.snapshot_id, raster.snapshot_id);
            assert_eq!(reduced.source_fingerprint, raster.source_fingerprint);
            assert_eq!((reduced.width, reduced.height), (w, h));
        }
        assert!(std::sync::Arc::ptr_eq(&raster.rgba, &jpeg.rgba));
        let cancel = Cancel::never();
        cancel.cancel();
        assert_eq!(
            reduce_raster(&raster, plan(17, 13, (17, 13)), &cancel)
                .unwrap_err()
                .kind,
            ErrorKind::Cancelled
        );
    }
}
