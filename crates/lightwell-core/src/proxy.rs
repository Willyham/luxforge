//! Display-bounded proxy sources.
//!
//! A proxy source is the prepared source downscaled once to the size the display can actually show.
//! Every layer a gesture can draft is resolution independent — the orientation layer is a mapping,
//! the crop payload is normalized to its own input stage, and the Basic and RAW development layers
//! are pointwise — so the same recipe compiles unchanged against the smaller content stage and
//! produces the same picture at display size, through the same code and the same colour arithmetic.
//! Nothing inside the colour path is approximated; the only thing that differs from the exact
//! render is the resampling the display was going to do anyway.
//!
//! Nothing here touches the catalog and nothing here belongs on the owner thread: building a proxy
//! is frame work, bounded by the same 512 MiB frame limit as a rendered raster and parallelized on
//! the shared Rayon pool above the same one-megapixel threshold as every other pass.

use crate::{
    Error, ErrorKind, LinearImage, PreviewSource, Raster, SourceImage,
    colour::srgb::{decode_pixel, quantize_channel},
    render::{frame_mut, zeroed_frame},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Above this many source pixels the two passes run on the shared Rayon pool, as every other pass
/// in the renderer does; below it they stay serial.
const PARALLEL_PROXY_PIXELS: u64 = 1_000_000;

/// The frame limit a proxy and its one intermediate are each counted against, which is the limit
/// [`Raster::expected_len`] applies to a rendered frame.
const FRAME_LIMIT_BYTES: u64 = 512 * 1024 * 1024;

/// Physical pixels of the photo area the display can show. Clamped, never refused: a caller reports
/// the window it has, and the core decides what it is willing to build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyBounds {
    pub width: u32,
    pub height: u32,
}

impl ProxyBounds {
    pub const MAX_SIDE: u32 = 4096;
    pub const MAX_PIXELS: u64 = 8_000_000;

    /// Clamp each side to [`Self::MAX_SIDE`] and scale both down uniformly until the product is
    /// within [`Self::MAX_PIXELS`]. A zero side becomes one, so the result always describes a
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

/// Why a proxy frame is an approximation of the exact render at display size, rather than the same
/// picture.
///
/// A proxy frame is normally the exact recipe at proxy size: every layer a gesture can draft is
/// resolution independent and a mask's geometry is normalized, so the same equations produce the
/// same picture at display size. Two things break that, and both are reported rather than assumed.
/// Neither one is a reason to decline the proxy: the frame is still what a drag presents, and the
/// exact phase still produces every number, the overlays and the 100% view.
///
/// One word, `approximate`, reaches the client; this is what it means in each case, so a person can
/// tell a thin mask from a spatial layer when both are present.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyApproximation {
    /// The stack compiles to a spatial operation at the proxy stage. Its neighbourhoods scale with
    /// the stage it is rendered at, so a proxy frame is close to the exact render at display size
    /// rather than equal to it. A neutral spatial layer compiles to no operation and does not set
    /// this.
    pub spatial: bool,
    /// A mask in the stack draws a feature narrower than two pixels of the proxy stage, so its
    /// field — never the effect — is evaluated with a 2 x 2 supersample per pixel
    /// ([masking](../../docs/design/masking.md#point-queries-and-proxies), proposal P5). Without
    /// that rule a hard edge would alias differently on every frame of a drag.
    pub mask: bool,
}

impl ProxyApproximation {
    /// Whether this frame is approximate at all. `false` is the ordinary case: the proxy render is
    /// the exact recipe at proxy size, byte for byte with the exact recipe over the exact
    /// downscale of the source.
    pub fn is_approximate(self) -> bool {
        self.spatial || self.mask
    }

    /// Why, in one sentence, or `None` when the frame is not approximate. Both reasons are named
    /// when both are present.
    pub fn reason(self) -> Option<&'static str> {
        const SPATIAL: &str =
            "a spatial-stage layer's neighbourhoods scale with the stage it is rendered at";
        const MASK: &str = "a mask draws a feature narrower than two proxy pixels, so its field is \
             evaluated with a 2x2 supersample per pixel";
        match (self.spatial, self.mask) {
            (false, false) => None,
            (true, false) => Some(SPATIAL),
            (false, true) => Some(MASK),
            (true, true) => Some(
                "a spatial-stage layer's neighbourhoods scale with the stage it is rendered at, \
                 and a mask draws a feature narrower than two proxy pixels, so its field is \
                 evaluated with a 2x2 supersample per pixel",
            ),
        }
    }
}

/// The proxy source dimensions a job will render against, with the bounds they were derived from.
/// The bounds are part of the plan because they are part of the cache identity: a resized window
/// produces a different plan even when the rounded dimensions happen to agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyPlan {
    pub width: u32,
    pub height: u32,
    pub bounds: ProxyBounds,
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
        /// The development the planes belong to ([`LinearImage::development`]): a process-unique
        /// number, so a redevelopment misses even when its planes reuse the old allocation's
        /// address.
        development: u64,
        crop: [u32; 4],
        orientation: u8,
    },
}

impl ProxyPlan {
    /// `Some(plan)` when a proxy strictly smaller than a `source`-sized source fits `bounds` for a
    /// recipe whose full-resolution output stage is `stage`, and `None` when the scale would be one
    /// or more, which is where the exact path runs unchanged.
    ///
    /// The scale is `min(bounds.width / stage.width, bounds.height / stage.height, 1)`, so a rotated
    /// crop's output — not the source rectangle it was cut from — is what gets fitted into the
    /// bounds. Pure arithmetic: the stage comes from the compilation the caller already holds
    /// ([`crate::Render::proxy_plan`]).
    pub fn fit(source: (u32, u32), stage: (u32, u32), bounds: ProxyBounds) -> Option<Self> {
        let bounds = bounds.clamped();
        let (source_width, source_height) = source;
        let (stage_width, stage_height) = stage;
        if stage_width == 0 || stage_height == 0 || source_width == 0 || source_height == 0 {
            return None;
        }
        let scale = (f64::from(bounds.width) / f64::from(stage_width))
            .min(f64::from(bounds.height) / f64::from(stage_height))
            .min(1.0);
        // A non-finite scale declines too: there is no proxy to describe, and the exact path is
        // always a correct answer.
        if !scale.is_finite() || scale >= 1.0 {
            return None;
        }
        let width = ((f64::from(source_width) * scale).round() as u32).clamp(1, source_width);
        let height = ((f64::from(source_height) * scale).round() as u32).clamp(1, source_height);
        if width == source_width && height == source_height {
            return None;
        }
        Some(Self {
            width,
            height,
            bounds,
        })
    }
}

/// One cached proxy source's identity: the pixels it came from and the plan it was built to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyKey {
    pub identity: ProxyIdentity,
    pub plan: ProxyPlan,
}

/// One cached proxy source. Bounded by construction: at most one entry, so the memory a cache can
/// hold is one proxy and a new plan replaces the old one rather than accumulating beside it.
#[derive(Default)]
pub struct ProxyCache {
    entry: Option<(ProxyKey, PreviewSource)>,
}

impl ProxyCache {
    /// The cached source for exactly this key, or `None`. A different identity, a different plan or
    /// different bounds is a miss; the caller rebuilds on the worker.
    pub fn get(&self, key: &ProxyKey) -> Option<&PreviewSource> {
        self.entry
            .as_ref()
            .filter(|(cached, _)| cached == key)
            .map(|(_, source)| source)
    }

    /// Hold this source under this key, replacing whatever was held before.
    pub fn insert(&mut self, key: ProxyKey, source: PreviewSource) {
        self.entry = Some((key, source));
    }
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

    /// This source's pixels under the evaluation settings of `job`. A cached proxy is keyed by its
    /// pixels alone — a RAW source's developed planes and view — while its `LinearSettings`
    /// (the exposure a RAW development layer asks for) belong to the recipe being rendered, so a
    /// cache hit takes the pixels from the cache and the settings from the job that is rendering.
    /// A JPEG carries no settings and is returned as it is.
    pub fn with_settings_of(&self, job: &PreviewSource) -> PreviewSource {
        match (self, job) {
            (Self::Raw { image, .. }, Self::Raw { settings, .. }) => Self::Raw {
                image: image.clone(),
                settings: *settings,
            },
            _ => self.clone(),
        }
    }

    /// Downscale this source to the plan's dimensions with a separable area average.
    ///
    /// This is frame work: it runs on the caller's thread and puts its two passes on the shared
    /// Rayon pool above the one-megapixel threshold. Never call it on the catalog owner thread.
    pub fn proxy(&self, plan: ProxyPlan) -> Result<PreviewSource, Error> {
        let (source_width, source_height) = self.dimensions();
        check_plan(plan, source_width, source_height)?;
        match self {
            Self::Jpeg(image) => Ok(PreviewSource::Jpeg(downscale_jpeg(
                image,
                plan.width,
                plan.height,
            )?)),
            Self::Raw { image, settings } => Ok(PreviewSource::Raw {
                image: downscale_linear(image, plan.width, plan.height)?,
                settings: *settings,
            }),
        }
    }
}

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn check_plan(plan: ProxyPlan, source_width: u32, source_height: u32) -> Result<(), Error> {
    if plan.width == 0 || plan.height == 0 {
        return Err(validation("a proxy plan's dimensions must be nonzero"));
    }
    if plan.width > source_width || plan.height > source_height {
        return Err(validation(format!(
            "a proxy plan never upscales: {}x{} from a {source_width}x{source_height} source",
            plan.width, plan.height
        )));
    }
    Ok(())
}

/// The element count of a float buffer of `width × height × 3`, refused when it would exceed the
/// frame limit. Same shape and the same error kind as [`Raster::expected_len`], which bounds the
/// byte frames beside it.
fn float_values(width: u32, height: u32, what: &str) -> Result<usize, Error> {
    let values = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ResourceLimit,
                format!("{what} dimensions overflow"),
            )
        })?;
    let bytes = values
        .checked_mul(std::mem::size_of::<f32>() as u64)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ResourceLimit,
                format!("{what} byte length overflow"),
            )
        })?;
    if bytes > FRAME_LIMIT_BYTES {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!("{what} exceeds 512 MiB"),
        ));
    }
    usize::try_from(values).map_err(|_| {
        Error::new(
            ErrorKind::ResourceLimit,
            format!("{what} is not addressable"),
        )
    })
}

/// The source samples that one output sample averages along one axis, with the fractional coverage
/// weights that make the output the exact mean over its source interval `[i·S/o, (i+1)·S/o)`.
///
/// The whole table is built once per axis and holds at most `source + output` weights, so nothing
/// here scales with the area and no weight is recomputed per row or per column.
struct Coverage {
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

    fn len(&self) -> usize {
        self.first.len()
    }
}

/// The one separable area average every proxy source is built with, whatever the source's pixels
/// are: a horizontal pass that reads each source pixel once, in linear light, into one intermediate
/// row per source row, and a vertical pass that averages those rows into each output pixel.
///
/// Both passes accumulate in f64 with the [`Coverage`] weights, so an output pixel is the exact mean
/// over its source rectangle up to the one f32 store between the passes. The two source kinds differ
/// only in how a source pixel is read — a JPEG code decoded through the render path's own sRGB
/// table, a RAW value read through its view — and in how the output is stored, which each caller
/// does with [`Self::pixel`]; the arithmetic is this one implementation.
///
/// Allocations: the intermediate of `width × source_height × 3` f32, bounded by the 512 MiB frame
/// limit. Every source pixel is read exactly once, so no per-row scratch exists at all.
struct BoxDownscale {
    vertical: Coverage,
    /// The intermediate rows: `width × 3` values per source row.
    rows: Vec<f32>,
    stride: usize,
    parallel: bool,
}

impl BoxDownscale {
    /// Run the horizontal pass. `read(x, y)` is one source pixel in linear light; it is only asked
    /// for coordinates inside the source.
    fn new(
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
        read: impl Fn(u32, u32) -> [f32; 3] + Sync,
    ) -> Result<Self, Error> {
        let intermediate_len = float_values(width, source_height, "proxy downscale intermediate")?;
        let horizontal = Coverage::new(source_width, width);
        let vertical = Coverage::new(source_height, height);
        let parallel = u64::from(source_width) * u64::from(source_height) >= PARALLEL_PROXY_PIXELS;
        let stride = width as usize * 3;
        let mut rows = vec![0f32; intermediate_len];
        let pass = |(y, row): (usize, &mut [f32])| {
            for index in 0..horizontal.len() {
                let (first, weights) = horizontal.span(index);
                let mut sum = [0f64; 3];
                for (offset, weight) in weights.iter().enumerate() {
                    let linear = read(first + offset as u32, y as u32);
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += f64::from(linear[channel]) * weight;
                    }
                }
                for (channel, value) in sum.iter().enumerate() {
                    row[index * 3 + channel] = *value as f32;
                }
            }
        };
        if parallel {
            rows.par_chunks_exact_mut(stride).enumerate().for_each(pass);
        } else {
            rows.chunks_exact_mut(stride).enumerate().for_each(pass);
        }
        Ok(Self {
            vertical,
            rows,
            stride,
            parallel,
        })
    }

    /// The vertical pass for one output pixel: the weighted mean of its column of intermediate
    /// rows, in f64, for the caller to store.
    #[inline]
    fn pixel(&self, x: usize, y: usize) -> [f64; 3] {
        let (first, weights) = self.vertical.span(y);
        let mut sum = [0f64; 3];
        for (offset, weight) in weights.iter().enumerate() {
            let at = (first as usize + offset) * self.stride + x * 3;
            for (channel, value) in sum.iter_mut().enumerate() {
                *value += f64::from(self.rows[at + channel]) * weight;
            }
        }
        sum
    }
}

/// The area average of a JPEG source, re-quantized through the render path's own threshold
/// boundary. A uniform region therefore comes out as exactly its own code, and the proxy of an
/// identity stack agrees with the exact render's arithmetic everywhere it can. The output frame is
/// written in place and returned as the proxy's pixels, with no copy.
fn downscale_jpeg(source: &SourceImage, width: u32, height: u32) -> Result<SourceImage, Error> {
    let source_len = Raster::expected_len(source.width, source.height)?;
    if source.rgba.len() != source_len {
        return Err(validation(
            "source buffer length does not match its dimensions",
        ));
    }
    let output_len = Raster::expected_len(width, height)?;
    let source_stride = source.width as usize * 4;
    let downscale = BoxDownscale::new(source.width, source.height, width, height, |x, y| {
        let at = y as usize * source_stride + x as usize * 4;
        decode_pixel([source.rgba[at], source.rgba[at + 1], source.rgba[at + 2]])
    })?;
    let mut frame = zeroed_frame(output_len);
    {
        let output = frame_mut(&mut frame);
        let pass = |(y, row): (usize, &mut [u8])| {
            for x in 0..width as usize {
                let sum = downscale.pixel(x, y);
                let pixel = &mut row[x * 4..x * 4 + 4];
                for (channel, value) in sum.iter().enumerate() {
                    pixel[channel] = quantize_channel(*value);
                }
                pixel[3] = 255;
            }
        };
        let output_stride = width as usize * 4;
        if downscale.parallel {
            output
                .par_chunks_exact_mut(output_stride)
                .enumerate()
                .for_each(pass);
        } else {
            output
                .chunks_exact_mut(output_stride)
                .enumerate()
                .for_each(pass);
        }
    }

    Ok(SourceImage {
        width,
        height,
        rgba: frame,
        fingerprint: source.fingerprint.clone(),
        orientation: source.orientation,
    })
}

/// One output row of the three planes, as the zipped chunk iterators hand it over: the row index
/// and its red, green and blue slices. The serial and parallel iterators yield the same shape, so
/// one closure serves both.
type PlanarRow<'a> = (usize, ((&'a mut [f32], &'a mut [f32]), &'a mut [f32]));

/// The area average of a prepared RAW source's planes, read through its view.
///
/// The result is a smaller [`LinearImage`] with the same fingerprint and an identity view: the
/// crop and orientation of the input view are resolved by the averaging itself, so the proxy is
/// upright content with nothing left to map. Values stay unbounded linear f32, so no clipping or
/// transfer function is introduced anywhere on this path.
fn downscale_linear(image: &LinearImage, width: u32, height: u32) -> Result<LinearImage, Error> {
    let reader = image.reader();
    let (source_width, source_height) = reader.dimensions();
    let plane_values = float_values(width, height, "proxy linear source")?;
    let plane_len = plane_values / 3;
    // Inside the view by construction: the coverage never leaves the source.
    let downscale = BoxDownscale::new(source_width, source_height, width, height, |x, y| {
        reader.pixel(x, y).unwrap_or([0.0; 3])
    })?;
    let mut planes = vec![0f32; plane_values];
    {
        let (red, rest) = planes.split_at_mut(plane_len);
        let (green, blue) = rest.split_at_mut(plane_len);
        let pass = |(y, ((red, green), blue)): PlanarRow<'_>| {
            for x in 0..width as usize {
                let sum = downscale.pixel(x, y);
                red[x] = sum[0] as f32;
                green[x] = sum[1] as f32;
                blue[x] = sum[2] as f32;
            }
        };
        let row = width as usize;
        if downscale.parallel {
            red.par_chunks_exact_mut(row)
                .zip(green.par_chunks_exact_mut(row))
                .zip(blue.par_chunks_exact_mut(row))
                .enumerate()
                .for_each(pass);
        } else {
            red.chunks_exact_mut(row)
                .zip(green.chunks_exact_mut(row))
                .zip(blue.chunks_exact_mut(row))
                .enumerate()
                .for_each(pass);
        }
    }

    LinearImage::with_fingerprint(width, height, planes, image.fingerprint())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BASIC_EFFECT, BoxRect, CROP_EFFECT, CropStage, EFFECT_FORMAT, Layer, LayerId,
        LinearSettings, Mask, ModuleRegistry, Orientation, PIXEL_EFFECT, RECIPE_FORMAT, Recipe,
        SnapshotId, Stage, colour::srgb::decode_u8, render::Cancel,
    };
    use serde_json::json;

    // -----------------------------------------------------------------------------------------
    // Independent references
    // -----------------------------------------------------------------------------------------

    /// The linear value the render path's own f32 table holds for one code: the f64 transfer
    /// function, stored as f32. Decoding through the table is what production does, so the
    /// reference has to start from the same value to be a reference and not a second algorithm.
    fn decoded(code: u8) -> f64 {
        f64::from(decode_u8(code) as f32)
    }

    /// The forward sRGB transfer function rounded to a code. This is the definition the render
    /// path's threshold table encodes; computing it directly here keeps the reference independent
    /// of that table.
    fn encoded(linear: f64) -> u8 {
        let linear = linear.clamp(0.0, 1.0);
        let value = if linear <= 0.003_130_8 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (value * 255.0).round() as u8
    }

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
        })
    }

    fn raw_source(width: u32, height: u32, pixels: &[[f32; 3]]) -> LinearImage {
        assert_eq!(pixels.len() as u64, u64::from(width) * u64::from(height));
        let mut planes = Vec::with_capacity(pixels.len() * 3);
        for channel in 0..3 {
            planes.extend(pixels.iter().map(|pixel| pixel[channel]));
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:proxy-raw").expect("an image")
    }

    fn recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    fn basic_layer(payload: serde_json::Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn presence_layer(payload: serde_json::Value) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: crate::PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
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
        }
    }

    fn jpeg_of(source: &PreviewSource) -> &SourceImage {
        match source {
            PreviewSource::Jpeg(image) => image,
            PreviewSource::Raw { .. } => panic!("a JPEG proxy stays a JPEG source"),
        }
    }

    fn raw_of(source: &PreviewSource) -> &LinearImage {
        match source {
            PreviewSource::Raw { image, .. } => image,
            PreviewSource::Jpeg(_) => panic!("a RAW proxy stays a RAW source"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // 1. Integer scales average exactly
    // -----------------------------------------------------------------------------------------

    /// An integer-scale downscale equals, per output pixel, the mean of its block's decoded linear
    /// values re-quantized at the code boundary — computed here in f64 from the transfer function
    /// itself rather than from the renderer's threshold table.
    #[test]
    fn an_integer_scale_downscale_is_the_exact_mean_of_each_block() {
        let codes: Vec<[u8; 3]> = (0..48u32)
            .map(|index| {
                [
                    (index * 5 + 3) as u8,
                    (index * 11 + 17) as u8,
                    (255 - index * 3) as u8,
                ]
            })
            .collect();
        let source = jpeg_source(8, 6, &codes);
        for (width, height) in [(4u32, 3u32), (2, 3), (1, 1)] {
            let block_width = 8 / width as usize;
            let block_height = 6 / height as usize;
            let proxy = source
                .proxy(plan(width, height, (width, height)))
                .expect("a proxy");
            let image = jpeg_of(&proxy);
            assert_eq!((image.width, image.height), (width, height));
            assert_eq!(image.fingerprint, "sha256:proxy-fixture");
            assert_eq!(image.orientation, 1);
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let mut sum = [0f64; 3];
                    for row in 0..block_height {
                        for column in 0..block_width {
                            let code =
                                codes[(y * block_height + row) * 8 + x * block_width + column];
                            for (channel, value) in sum.iter_mut().enumerate() {
                                *value += decoded(code[channel]);
                            }
                        }
                    }
                    let count = (block_width * block_height) as f64;
                    let expected = [
                        encoded(sum[0] / count),
                        encoded(sum[1] / count),
                        encoded(sum[2] / count),
                    ];
                    let at = (y * width as usize + x) * 4;
                    assert_eq!(
                        [image.rgba[at], image.rgba[at + 1], image.rgba[at + 2]],
                        expected,
                        "{width}x{height} block ({x}, {y})"
                    );
                    assert_eq!(image.rgba[at + 3], 255, "alpha is always opaque");
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // 2. Fractional coverage
    // -----------------------------------------------------------------------------------------

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

    /// A uniform image stays exactly uniform at a fractional scale, and a horizontal gradient stays
    /// monotone across it.
    #[test]
    fn a_fractional_scale_preserves_uniformity_and_monotonicity() {
        let uniform: Vec<[u8; 3]> = vec![[97, 13, 200]; 35];
        let proxy = jpeg_source(7, 5, &uniform)
            .proxy(plan(3, 2, (3, 2)))
            .expect("a proxy");
        let image = jpeg_of(&proxy);
        assert_eq!((image.width, image.height), (3, 2));
        for pixel in image.rgba.chunks_exact(4) {
            assert_eq!(pixel, [97, 13, 200, 255]);
        }

        let gradient: Vec<[u8; 3]> = (0..35)
            .map(|index| {
                let code = ((index % 7) * 36) as u8;
                [code, code, code]
            })
            .collect();
        let proxy = jpeg_source(7, 5, &gradient)
            .proxy(plan(3, 2, (3, 2)))
            .expect("a proxy");
        let image = jpeg_of(&proxy);
        for y in 0..2usize {
            let row: Vec<u8> = (0..3).map(|x| image.rgba[(y * 3 + x) * 4]).collect();
            assert!(
                row.windows(2).all(|pair| pair[0] < pair[1]),
                "a horizontal gradient stays monotone: {row:?}"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // 3. Uniform images are unchanged
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_uniform_image_is_unchanged_at_any_scale_for_both_source_kinds() {
        let jpeg = jpeg_source(12, 9, &vec![[31, 199, 4]; 108]);
        for (width, height) in [(1u32, 1u32), (2, 3), (5, 4), (11, 8), (12, 9)] {
            let image = jpeg_of(&jpeg.proxy(plan(width, height, (width, height))).unwrap()).clone();
            assert_eq!((image.width, image.height), (width, height));
            for pixel in image.rgba.chunks_exact(4) {
                assert_eq!(pixel, [31, 199, 4, 255], "{width}x{height} is not uniform");
            }
        }

        let raw = PreviewSource::Raw {
            image: raw_source(12, 9, &vec![[0.25, 1.75, -0.5]; 108]),
            settings: LinearSettings::default(),
        };
        for (width, height) in [(1u32, 1u32), (2, 3), (5, 4), (11, 8), (12, 9)] {
            let proxy = raw.proxy(plan(width, height, (width, height))).unwrap();
            let image = raw_of(&proxy);
            assert_eq!((image.width(), image.height()), (width, height));
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(
                        image.pixel(x, y),
                        Some([0.25, 1.75, -0.5]),
                        "{width}x{height} at ({x}, {y})"
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // 4. RAW through a cropped, oriented view
    // -----------------------------------------------------------------------------------------

    /// The proxy of a RAW source reads through its view: a crop and an EXIF orientation are
    /// resolved by the averaging, so the result is upright content with an identity view, the same
    /// fingerprint, and values equal to the mean of the viewed planes.
    #[test]
    fn a_raw_proxy_averages_the_viewed_planes_and_keeps_the_fingerprint() {
        let pixels: Vec<[f32; 3]> = (0..64)
            .map(|index| {
                let value = index as f32;
                [value, value * 0.5 - 3.0, 100.0 - value]
            })
            .collect();
        let base = raw_source(8, 8, &pixels);
        // A 6x4 crop at (1, 1), read with EXIF orientation 6: a quarter turn, so the viewed image
        // is 4 wide and 6 tall.
        let viewed = base.with_view([1, 1, 6, 4], 6).expect("a view");
        assert_eq!((viewed.width(), viewed.height()), (4, 6));
        let source = PreviewSource::Raw {
            image: viewed.clone(),
            settings: LinearSettings::default(),
        };

        let proxy = source.proxy(plan(2, 3, (2, 3))).expect("a proxy");
        let image = raw_of(&proxy);
        assert_eq!((image.width(), image.height()), (2, 3));
        assert_eq!(image.fingerprint(), "sha256:proxy-raw");
        assert_eq!(
            image.view(),
            ([0, 0, 2, 3], 1),
            "a proxy has an identity view"
        );

        for y in 0..3u32 {
            for x in 0..2u32 {
                let mut sum = [0f64; 3];
                for row in 0..2u32 {
                    for column in 0..2u32 {
                        let pixel = viewed
                            .pixel(x * 2 + column, y * 2 + row)
                            .expect("a viewed pixel");
                        for (channel, value) in sum.iter_mut().enumerate() {
                            *value += f64::from(pixel[channel]);
                        }
                    }
                }
                let actual = image.pixel(x, y).expect("a proxy pixel");
                for channel in 0..3 {
                    let expected = (sum[channel] / 4.0) as f32;
                    assert!(
                        (actual[channel] - expected).abs()
                            <= f32::EPSILON * expected.abs().max(1.0),
                        "({x}, {y}) channel {channel}: {} against {expected}",
                        actual[channel]
                    );
                }
            }
        }
    }

    /// The white-balance approximation is one matrix per pixel and the downscale an area average,
    /// both linear, so they commute: the proxy of planes the matrix was applied to equals the
    /// matrix applied to the proxy of the planes, to f32 rounding. That is why an approximate
    /// job's proxy phase describes the same approximation its full-size phase does, at display
    /// size, through the same filter as every other proxy.
    #[test]
    fn a_white_balance_approximation_commutes_with_the_downscale() {
        let matrix = [[1.31, 0.07, -0.03], [0.02, 0.96, 0.05], [-0.08, 0.03, 0.69]];
        let (width, height) = (37, 23);
        let pixels: Vec<[f32; 3]> = (0..width * height)
            .map(|index| {
                let value = index as f32;
                [
                    (value * 0.031) % 1.4 - 0.1,
                    (value * 0.047) % 1.2,
                    (value * 0.019) % 1.7,
                ]
            })
            .collect();
        let balanced: Vec<[f32; 3]> = pixels
            .iter()
            .map(|pixel| {
                matrix.map(|row| {
                    (row[0] * f64::from(pixel[0])
                        + row[1] * f64::from(pixel[1])
                        + row[2] * f64::from(pixel[2])) as f32
                })
            })
            .collect();
        let settings = LinearSettings::default();
        let proxy_of = |pixels: &[[f32; 3]]| {
            PreviewSource::Raw {
                image: raw_source(width, height, pixels),
                settings,
            }
            .proxy(plan(11, 7, (11, 7)))
            .expect("a proxy")
        };
        let downscaled = proxy_of(&pixels);
        let balanced_then_downscaled = proxy_of(&balanced);
        for y in 0..7 {
            for x in 0..11 {
                let proxy = raw_of(&downscaled).pixel(x, y).unwrap().map(f64::from);
                let expected = raw_of(&balanced_then_downscaled).pixel(x, y).unwrap();
                let balanced_proxy =
                    matrix.map(|row| row[0] * proxy[0] + row[1] * proxy[1] + row[2] * proxy[2]);
                for channel in 0..3 {
                    let expected = f64::from(expected[channel]);
                    assert!(
                        (balanced_proxy[channel] - expected).abs()
                            <= 1.0e-5 * expected.abs().max(1.0),
                        "({x}, {y}) channel {channel}: {} against {expected}",
                        balanced_proxy[channel]
                    );
                }
            }
        }
        // And so do the frames the two render: the approximation over the proxy, against the
        // matrix applied before the downscale, agree to one code at most — f32 rounding at a code
        // boundary, never a visible difference.
        let registry = ModuleRegistry::builtin();
        let approximate = PreviewSource::Raw {
            image: raw_of(&downscaled).clone(),
            settings: LinearSettings {
                exposure_ev: 0.0,
                white_balance: Some(crate::WhiteBalanceApproximation::from_matrix(matrix).unwrap()),
            },
        };
        let over_proxy = approximate
            .render(&registry, SnapshotId::new(), &recipe(Vec::new()))
            .unwrap();
        let before = balanced_then_downscaled
            .render(&registry, SnapshotId::new(), &recipe(Vec::new()))
            .unwrap();
        assert_eq!(over_proxy.rgba.len(), before.rgba.len());
        for (a, b) in over_proxy.rgba.iter().zip(before.rgba.iter()) {
            assert!(a.abs_diff(*b) <= 1, "{a} against {b}");
        }
    }

    // -----------------------------------------------------------------------------------------
    // 5. The plan
    // -----------------------------------------------------------------------------------------

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

    // -----------------------------------------------------------------------------------------
    // 6. Eligibility
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_pixel_stage_layer_makes_a_stack_ineligible_and_is_named() {
        let registry = ModuleRegistry::builtin();
        let eligible = recipe(vec![
            Layer::orientation(Orientation {
                mirror: true,
                turns: 3,
            }),
            basic_layer(json!({ "exposure": 0.5 })),
            fitted_crop_layer(480, 320, 4.0),
        ]);
        registry
            .proxy_eligible(&eligible)
            .expect("orientation, Basic and crop are all resolution independent");

        let ineligible = recipe(vec![
            Layer::orientation(Orientation::NEUTRAL),
            Layer::pixel(3, 4, [9, 9, 9]),
            fitted_crop_layer(480, 320, 0.0),
        ]);
        let error = registry
            .proxy_eligible(&ineligible)
            .expect_err("a point replacement addresses content pixels");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains(PIXEL_EFFECT), "{error}");
        assert!(error.detail.contains("layer 1"), "{error}");

        let unknown = recipe(vec![Layer {
            id: LayerId::new(),
            effect_id: "test.absent".into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        }]);
        let error = registry
            .proxy_eligible(&unknown)
            .expect_err("an unknown effect has no stage");
        assert!(error.detail.contains("test.absent"), "{error}");
        assert!(error.detail.contains("layer 0"), "{error}");
    }

    /// A finish-stage layer is exact at proxy scale (its mask is normalized to the output stage)
    /// and a spatial-stage layer is eligible but approximate, which the registry says separately.
    #[test]
    fn finish_layers_are_exact_and_spatial_layers_are_approximate_at_proxy_scale() {
        let registry = ModuleRegistry::builtin();
        let finish = recipe(vec![
            basic_layer(json!({ "exposure": 0.5 })),
            Layer {
                id: LayerId::new(),
                effect_id: crate::VIGNETTE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({ "amount": -40 }),
                mask: None,
                artifacts: Vec::new(),
            },
        ]);
        registry
            .proxy_eligible(&finish)
            .expect("a vignette is resolution independent");
        assert!(!registry.proxy_approximation(&finish, 96, 64).spatial);

        let spatial = recipe(vec![
            basic_layer(json!({ "exposure": 0.5 })),
            presence_layer(json!({ "clarity": 60 })),
        ]);
        registry
            .proxy_eligible(&spatial)
            .expect("a Presence stack renders through the proxy");
        assert!(
            registry.proxy_approximation(&spatial, 96, 64).spatial,
            "its neighbourhoods scale with the stage, so the proxy frame is approximate"
        );
    }

    /// A reset Presence layer is still a spatial-stage layer, but its neutral payload compiles to
    /// no operation at all: the proxy frame is the exact recipe at proxy size, byte for byte with
    /// the exact recipe over the exact downscale, and is not labelled approximate.
    #[test]
    fn a_neutral_spatial_layer_is_not_approximate_at_proxy_scale() {
        let registry = ModuleRegistry::builtin();
        for payload in [
            json!({}),
            json!({ "texture": 0, "clarity": 0, "dehaze": 0 }),
        ] {
            let reset = recipe(vec![
                basic_layer(json!({ "exposure": 0.5 })),
                presence_layer(payload.clone()),
            ]);
            registry
                .proxy_eligible(&reset)
                .expect("a Presence stack renders through the proxy");
            let approximation = registry.proxy_approximation(&reset, 96, 64);
            assert!(!approximation.spatial, "{payload}");
            assert!(!approximation.is_approximate(), "{payload}");
            assert_eq!(approximation.reason(), None, "{payload}");

            // And the frame is what the label says: the stack without its neutral layer.
            let pixels: Vec<[u8; 3]> = (0..96_u32 * 64)
                .map(|index| {
                    [
                        (index % 251) as u8,
                        (index * 7 % 253) as u8,
                        (index * 13 % 241) as u8,
                    ]
                })
                .collect();
            let source = jpeg_source(96, 64, &pixels);
            let bounds = ProxyBounds {
                width: 48,
                height: 32,
            };
            let plan = source
                .proxy_plan(&registry, &reset, bounds)
                .unwrap()
                .expect("a proxy is worthwhile");
            let proxy = source.proxy(plan).unwrap();
            let without = recipe(vec![basic_layer(json!({ "exposure": 0.5 }))]);
            assert_eq!(
                proxy
                    .render(&registry, SnapshotId::new(), &reset)
                    .unwrap()
                    .rgba,
                proxy
                    .render(&registry, SnapshotId::new(), &without)
                    .unwrap()
                    .rgba,
                "{payload}: a neutral spatial layer changes no proxy byte"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // 7. The cache
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_cache_holds_one_entry_keyed_by_identity_and_plan() {
        let source = jpeg_source(8, 6, &[[40, 80, 120]; 48]);
        let key = |plan: ProxyPlan| ProxyKey {
            identity: source.identity(),
            plan,
        };
        let first = key(plan(4, 3, (4, 3)));
        let proxy = source.proxy(first.plan).expect("a proxy");

        let mut cache = ProxyCache::default();
        assert!(cache.get(&first).is_none(), "an empty cache never hits");
        cache.insert(first.clone(), proxy);
        assert!(cache.get(&first).is_some(), "the same key hits");

        // A resized window is a miss even at the same rounded dimensions.
        assert!(cache.get(&key(plan(4, 3, (5, 3)))).is_none());
        // A different plan is a miss.
        assert!(cache.get(&key(plan(2, 3, (4, 3)))).is_none());
        // A different source identity is a miss.
        let other = jpeg_source(8, 6, &[[40, 80, 120]; 48]);
        let mut identity = other.identity();
        if let ProxyIdentity::Jpeg { fingerprint, .. } = &mut identity {
            *fingerprint = "sha256:other".into();
        }
        assert!(
            cache
                .get(&ProxyKey {
                    identity,
                    plan: first.plan
                })
                .is_none()
        );

        // A second insert replaces the first: the cache holds one proxy, never two.
        let second = key(plan(2, 3, (2, 3)));
        cache.insert(second.clone(), source.proxy(second.plan).expect("a proxy"));
        assert!(cache.get(&second).is_some());
        assert!(cache.get(&first).is_none(), "the cache holds one entry");
    }

    /// A JPEG proxy's pixels are written in the allocation the proxy source holds, with no copy
    /// after the pass, and an identity stack rendered over it returns that allocation itself.
    #[test]
    fn a_jpeg_proxy_holds_the_frame_its_pass_wrote() {
        let codes: Vec<[u8; 3]> = (0..48u32)
            .map(|index| [(index * 5) as u8, (index * 3 + 7) as u8, 200])
            .collect();
        let source = jpeg_source(8, 6, &codes);
        let (proxy, written) = crate::render::frame_writes::record(|| {
            source.proxy(plan(4, 3, (4, 3))).expect("a proxy")
        });
        let image = jpeg_of(&proxy);
        assert_eq!(written, [image.rgba.as_ptr() as usize]);
        let frame = proxy
            .render_proxy_cancellable(
                &ModuleRegistry::builtin(),
                SnapshotId::new(),
                &recipe(Vec::new()),
                &Cancel::never(),
            )
            .expect("a frame");
        assert!(std::sync::Arc::ptr_eq(&frame.rgba, &image.rgba));
    }

    /// A RAW identity follows the developed planes: redeveloping them misses, and a view change
    /// misses, while the same planes under the same view hit.
    #[test]
    fn a_raw_identity_follows_its_developed_planes() {
        let pixels: Vec<[f32; 3]> = (0..64).map(|index| [index as f32; 3]).collect();
        let image = raw_source(8, 8, &pixels);
        let source = PreviewSource::Raw {
            image: image.clone(),
            settings: LinearSettings::default(),
        };
        let same = PreviewSource::Raw {
            image: image.clone(),
            settings: LinearSettings::default(),
        };
        assert_eq!(
            source.identity(),
            same.identity(),
            "a shared plane allocation is the same source"
        );

        let viewed = PreviewSource::Raw {
            image: image.with_view([0, 0, 4, 4], 1).expect("a view"),
            settings: LinearSettings::default(),
        };
        assert_ne!(
            source.identity(),
            viewed.identity(),
            "a view change is a different source"
        );

        let redeveloped = PreviewSource::Raw {
            image: raw_source(8, 8, &pixels),
            settings: LinearSettings::default(),
        };
        assert_ne!(
            source.identity(),
            redeveloped.identity(),
            "redeveloped planes are a different source"
        );
    }

    // -----------------------------------------------------------------------------------------
    // 8. The recipe renders against the proxy
    // -----------------------------------------------------------------------------------------

    /// The whole effective recipe — orientation, a Basic colour layer and a straightened crop —
    /// renders against the proxy source through the existing compiled path, produces an output
    /// that fits the bounds, and agrees with the point sampler over that same proxy, which is the
    /// contract every point query in the host depends on.
    #[test]
    fn a_full_stack_renders_against_the_proxy_and_agrees_with_the_sampler() {
        let registry = ModuleRegistry::builtin();
        let pixels: Vec<[u8; 3]> = (0..64 * 48)
            .map(|index| {
                [
                    (index % 251) as u8,
                    ((index * 7) % 241) as u8,
                    ((index * 13) % 239) as u8,
                ]
            })
            .collect();
        let source = jpeg_source(64, 48, &pixels);
        let stack = recipe(vec![
            basic_layer(json!({ "exposure": 0.5, "contrast": 20 })),
            Layer::orientation(Orientation {
                mirror: false,
                turns: 1,
            }),
            fitted_crop_layer(48, 64, 7.0),
        ]);
        let bounds = ProxyBounds {
            width: 20,
            height: 20,
        };
        let plan = source
            .proxy_plan(&registry, &stack, bounds)
            .expect("a plan")
            .expect("a proxy is worthwhile");
        assert!(plan.width < 64 && plan.height < 48, "{plan:?}");

        let proxy = source.proxy(plan).expect("a proxy source");
        assert_eq!(proxy.dimensions(), (plan.width, plan.height));
        assert_eq!(proxy.fingerprint(), source.fingerprint());

        let raster = proxy
            .render(&registry, SnapshotId::new(), &stack)
            .expect("the stack renders at proxy size");
        assert!(
            raster.width <= bounds.width && raster.height <= bounds.height,
            "{}x{} does not fit the bounds",
            raster.width,
            raster.height
        );
        assert!(raster.width > 0 && raster.height > 0);

        for (x, y) in [
            (0, 0),
            (raster.width - 1, 0),
            (0, raster.height - 1),
            (raster.width - 1, raster.height - 1),
            (raster.width / 2, raster.height / 3),
        ] {
            let sample = proxy.sample(&registry, &stack, x, y).expect("a sample");
            assert_eq!((sample.width, sample.height), (raster.width, raster.height));
            assert_eq!(
                sample.rgba,
                raster.pixel(x, y),
                "the sampler disagrees with the rendered proxy at ({x}, {y})"
            );
        }
        // The stack the proxy renders is the stack the registry called eligible.
        registry.proxy_eligible(&stack).expect("an eligible stack");
        assert_eq!(raster.source_fingerprint, source.fingerprint());
        // The crop layer is the last one, and CROP_EFFECT is what the fitted layer carries.
        assert_eq!(stack.layers[2].effect_id, CROP_EFFECT);
    }

    // -----------------------------------------------------------------------------------------
    // The photo-sized measurement
    // -----------------------------------------------------------------------------------------

    /// The proxy build on a real 24 MP source, which is the only input that says anything about
    /// cost. It is ignored by default because it needs a generated fixture and because timing gates
    /// do not belong in CI:
    ///
    /// ```text
    /// cargo run --release --locked --package xtask -- generate-fixtures --output fixtures/generated
    /// cargo test --release --package lightwell-core --lib proxy:: -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs fixtures/generated/24mp.jpg and is a measurement, not a gate"]
    fn measure_the_proxy_build_on_a_24_megapixel_source() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/generated/24mp.jpg");
        let source =
            PreviewSource::Jpeg(crate::open_source(&path).expect("the generated 24 MP fixture"));
        let registry = ModuleRegistry::builtin();
        let stack = recipe(vec![basic_layer(
            json!({ "exposure": 0.5, "contrast": 20 }),
        )]);
        let bounds = ProxyBounds {
            width: 2880,
            height: 1800,
        };
        let plan = source
            .proxy_plan(&registry, &stack, bounds)
            .expect("a plan")
            .expect("a 24 MP source needs a proxy at this size");
        let mut samples = Vec::new();
        for _ in 0..15 {
            let start = std::time::Instant::now();
            let proxy = source.proxy(plan).expect("a proxy");
            samples.push(start.elapsed());
            assert_eq!(proxy.dimensions(), (plan.width, plan.height));
        }
        // The first build carries the Rayon pool's first use and the first touch of the two fresh
        // buffers, which is what the first job after a window resize actually pays; the rest is
        // the steady-state cost of rebuilding one.
        let cold = samples[0];
        samples.sort_unstable();
        println!(
            "24 MP proxy build {}x{} -> {}x{}: cold {:?}, warm p50 {:?}, min {:?}, max {:?}",
            source.dimensions().0,
            source.dimensions().1,
            plan.width,
            plan.height,
            cold,
            samples[samples.len() / 2],
            samples[0],
            samples[samples.len() - 1]
        );
    }

    /// What a **mask** costs the proxy phase on photo-sized sources: the render a drag presents,
    /// unmasked, masked and point sampled, and masked under the thin-feature rule, at the display
    /// bounds the owner's screen offers. Ignored by default for the same two reasons as the build
    /// measurement above:
    ///
    /// ```text
    /// cargo run --release --locked --package xtask -- generate-fixtures --output fixtures/generated
    /// cargo test --release --package lightwell-core --lib proxy:: -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs fixtures/generated and is a measurement, not a gate"]
    fn measure_the_masked_proxy_render_on_photo_sized_sources() {
        for fixture in ["24mp.jpg", "60mp.jpg"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/generated")
                .join(fixture);
            let source =
                PreviewSource::Jpeg(crate::open_source(&path).expect("a generated fixture"));
            let registry = ModuleRegistry::builtin();
            let bounds = ProxyBounds {
                width: 2880,
                height: 1800,
            };
            // Every Basic field non-neutral, so each frame runs the module's whole colour chain —
            // the stack the latency target is stated over.
            let payload = json!({
                "exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0,
                "whites": -15.0, "blacks": 15.0, "temperature": 20.0, "tint": -10.0,
                "vibrance": 30.0, "saturation": 15.0,
            });
            let unmasked = recipe(vec![basic_layer(payload.clone())]);
            // A gradient over the middle half of the frame: a mask a person would draw, whose ramp
            // is hundreds of proxy pixels wide, so it is point sampled.
            let broad = gradient_mask(0.5);
            // A gradient whose ramp is a thousandth of the frame height: about 1.8 px at a 1800 px
            // proxy, which is what trips the 2 x 2 supersample of the mask field.
            let thin = gradient_mask(0.001);
            let masked_with = |mask: &Mask| Recipe {
                format: RECIPE_FORMAT,
                layers: vec![Layer {
                    mask: Some(mask.id.clone()),
                    ..basic_layer(payload.clone())
                }],
                masks: vec![mask.clone()],
                ..Recipe::default()
            };
            let broad_stack = masked_with(&broad);
            let thin_stack = masked_with(&thin);

            let plan = source
                .proxy_plan(&registry, &unmasked, bounds)
                .expect("a plan")
                .expect("a photo-sized source needs a proxy at this size");
            let proxy = source.proxy(plan).expect("a proxy");
            let stage = Stage {
                width: plan.width,
                height: plan.height,
            };
            assert!(
                registry
                    .proxy_approximation(&broad_stack, stage.width, stage.height)
                    .mask
                    .eq(&false),
                "the broad mask must be resolvable at this proxy size"
            );
            assert!(
                registry
                    .proxy_approximation(&thin_stack, stage.width, stage.height)
                    .mask,
                "the thin mask must trip the supersample at this proxy size"
            );

            // `true` is the proxy phase's own entry point, which applies the thin-feature rule;
            // `false` is the same render with the mask point sampled, which is what the exact phase
            // does. Running one stack through both is the only comparison that isolates the rule's
            // cost: same bounds rectangle, same effect, four coverage evaluations against one.
            let measure = |name: &str, stack: &Recipe, thin_rule: bool| {
                let once = || {
                    if thin_rule {
                        proxy.render_proxy_cancellable(
                            &registry,
                            SnapshotId::new(),
                            stack,
                            &Cancel::never(),
                        )
                    } else {
                        proxy.render_cancellable(
                            &registry,
                            SnapshotId::new(),
                            stack,
                            &Cancel::never(),
                        )
                    }
                    .expect("the stack renders at proxy size")
                };
                once();
                let mut samples = Vec::new();
                for _ in 0..25 {
                    let start = std::time::Instant::now();
                    let frame = once();
                    samples.push(start.elapsed());
                    std::hint::black_box(frame);
                }
                samples.sort_unstable();
                println!(
                    "{fixture} proxy {}x{} {name}: p50 {:?}, p95 {:?}, min {:?}, max {:?}",
                    plan.width,
                    plan.height,
                    samples[samples.len() / 2],
                    samples[samples.len() * 95 / 100],
                    samples[0],
                    samples[samples.len() - 1]
                );
            };
            measure("unmasked full Basic", &unmasked, true);
            measure("broad mask, point sampled", &broad_stack, true);
            measure("thin mask, point sampled", &thin_stack, false);
            measure("thin mask, 2x2 supersampled", &thin_stack, true);
        }
    }

    /// A linear gradient down the frame whose ramp is `length` mask-space units, which is
    /// `length x stage.height` pixels of whatever stage it is compiled against.
    fn gradient_mask(length: f64) -> Mask {
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("linear");
        mask.components.push(crate::Component::new(
            name,
            crate::ComponentMode::Add,
            "linear",
            json!({"x0": 0.5, "y0": 0.5 - length / 2.0, "x1": 0.5, "y1": 0.5 + length / 2.0}),
        ));
        mask
    }

    #[test]
    fn a_plan_that_would_upscale_or_vanish_is_refused() {
        let source = jpeg_source(8, 6, &[[1, 2, 3]; 48]);
        assert!(source.proxy(plan(0, 3, (4, 3))).is_err());
        assert!(source.proxy(plan(4, 0, (4, 3))).is_err());
        assert!(source.proxy(plan(9, 3, (9, 3))).is_err());
        assert!(source.proxy(plan(4, 7, (4, 7))).is_err());
        assert!(source.proxy(plan(8, 6, (8, 6))).is_ok(), "1:1 is allowed");
    }
}
