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
//! the shared Rayon pool from the proxy pass's own measured threshold (`render::parallel`).

#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Cancel, Error, LinearImage, PreviewSource, Raster, SourceImage,
    colour::srgb::{decode_pixel_in, decode_table, quantizer},
    render::{frame_mut, zeroed_frame},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Physical pixels of the photo area the display can show. Fit proxies clamp these bounds before
/// planning. A viewport's virtual half-resolution stage instead records its full half dimensions
/// here for cache identity and admits only the requested output/source windows, so a 60 MP image
/// does not silently scale below half because its uncut virtual stage exceeds the Fit area cap.
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
    /// ([masking](../../../docs/design/masking.md#point-queries-and-proxies), proposal P5). Without
    /// that rule a hard edge would alias differently on every frame of a drag.
    pub mask: bool,
    /// A moving viewport uses a stage with about half the full output's pixels on each side.
    pub reduced_detail: bool,
}

impl ProxyApproximation {
    /// Whether this frame is approximate at all. `false` is the ordinary case: the proxy render is
    /// the exact recipe at proxy size, byte for byte with the exact recipe over the exact
    /// downscale of the source.
    pub fn is_approximate(self) -> bool {
        self.spatial || self.mask || self.reduced_detail
    }

    /// Why, in one sentence, or `None` when the frame is not approximate. Both reasons are named
    /// when both are present.
    pub fn reason(self) -> Option<String> {
        const SPATIAL: &str =
            "a spatial-stage layer's neighbourhoods scale with the stage it is rendered at";
        const MASK: &str = "a mask draws a feature narrower than two proxy pixels, so its field is \
             evaluated with a 2x2 supersample per pixel";
        let mut reasons = Vec::new();
        if self.spatial {
            reasons.push(SPATIAL);
        }
        if self.mask {
            reasons.push(MASK);
        }
        if self.reduced_detail {
            reasons.push("the viewport uses reduced detail during interaction");
        }
        (!reasons.is_empty()).then(|| reasons.join("; "))
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
    /// The rectangle of the whole proxy stage the proxy source holds, when the stack reads less
    /// than all of it — a crop, and what each boundary before it needs around what it reads — or
    /// `None` for the whole stage. A tight crop fits a small output into the bounds, which raises
    /// the scale towards one, so without a window its proxy would approach the source's own size;
    /// with one, the proxy source is the display-sized part the crop reads plus a stated margin
    /// (instant previews, "Render what the display can show").
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

impl ProxyPlan {
    /// `Some(plan)` when a proxy strictly smaller than a `source`-sized source fits `bounds` for a
    /// recipe whose full-resolution output stage is `stage`, and `None` when the scale would be one
    /// or more, which is where the exact path runs unchanged.
    ///
    /// The scale is `min(bounds.width / stage.width, bounds.height / stage.height, 1)`, so a rotated
    /// crop's output — not the source rectangle it was cut from — is what gets fitted into the
    /// bounds. Pure arithmetic: the stage comes from the compilation the caller already holds
    /// ([`crate::Render::proxy_plan`]).
    pub(crate) fn fit(source: (u32, u32), stage: (u32, u32), bounds: ProxyBounds) -> Option<Self> {
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
            window: None,
        })
    }
}

/// One cached proxy source's identity: the pixels it came from and the plan it was built to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProxyKey {
    pub identity: ProxyIdentity,
    pub plan: ProxyPlan,
}

/// One cached proxy source. Bounded by construction: at most one entry, so the memory a cache can
/// hold is one proxy and a new plan replaces the old one rather than accumulating beside it.
#[derive(Default)]
pub(crate) struct ProxyCache {
    entry: Option<(ProxyKey, PreviewSource)>,
}

impl ProxyCache {
    /// The proxy source for `key`: on a hit, the held one's pixels under the evaluation settings
    /// of `job` ([`PreviewSource::with_settings_of`]) and `false`; on a miss, what `build` returns,
    /// which the cache then holds, and `true`. A miss releases the held proxy before `build` runs,
    /// so a replacement never sits beside the proxy it replaces at the build's peak. A build that
    /// fails or is cancelled leaves the cache empty, which the next job reads as a miss and
    /// rebuilds; nothing but the preview worker that owns the cache ever reads it.
    pub(crate) fn source_for(
        &mut self,
        key: &ProxyKey,
        job: &PreviewSource,
        build: impl FnOnce() -> Result<PreviewSource, Error>,
    ) -> Result<(PreviewSource, bool), Error> {
        if let Some((held, source)) = &self.entry
            && held == key
        {
            return Ok((source.with_settings_of(job), false));
        }
        self.entry = None;
        let built = build()?;
        self.entry = Some((key.clone(), built.clone()));
        Ok((built, true))
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
    pub(crate) fn with_settings_of(&self, job: &PreviewSource) -> PreviewSource {
        match (self, job) {
            (Self::Raw { image, .. }, Self::Raw { settings, .. }) => Self::Raw {
                image: image.clone(),
                settings: *settings,
            },
            _ => self.clone(),
        }
    }

    /// Downscale this source to the plan's dimensions with a separable area average, keeping only
    /// the plan's window when it has one. A windowed proxy's pixels are the whole downscale's
    /// pixels in that window, bit for bit: each is the same weights over the same source samples in
    /// the same order, and only the source rows and columns the window covers are read.
    ///
    /// This is frame work: it runs on the caller's thread and puts its bands of output rows on the
    /// shared Rayon pool from the proxy pass's threshold of source pixels read, each worker holding
    /// one band's intermediate of about 1 MiB. Never call it on the catalog owner thread.
    pub fn proxy(&self, plan: ProxyPlan) -> Result<PreviewSource, Error> {
        self.proxy_cancellable(plan, &Cancel::never())
    }

    /// The same bounded proxy build, with a checkpoint in every horizontal and vertical row.
    /// Interactive viewport jobs pass their `abandoned` token here, including cache misses.
    pub(crate) fn proxy_cancellable(
        &self,
        plan: ProxyPlan,
        cancel: &Cancel,
    ) -> Result<PreviewSource, Error> {
        self.proxy_in_bands(plan, cancel, BAND_BYTES)
    }

    /// The proxy build with bands of about `band_bytes` of intermediate each. The pixels do not
    /// depend on the band size ([`BoxDownscale`]); the tests pass small bands to prove it.
    fn proxy_in_bands(
        &self,
        plan: ProxyPlan,
        cancel: &Cancel,
        band_bytes: usize,
    ) -> Result<PreviewSource, Error> {
        cancel.check()?;
        let (source_width, source_height) = self.dimensions();
        check_plan(plan, source_width, source_height)?;
        match self {
            Self::Jpeg(image) => Ok(PreviewSource::Jpeg(downscale_jpeg(
                image, plan, cancel, band_bytes,
            )?)),
            Self::Raw { image, settings } => Ok(PreviewSource::Raw {
                image: downscale_linear(image, plan, cancel, band_bytes)?,
                settings: *settings,
            }),
        }
    }
}

fn check_plan(plan: ProxyPlan, source_width: u32, source_height: u32) -> Result<(), Error> {
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
fn window_of(plan: ProxyPlan) -> ProxyWindow {
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
fn float_values(width: u32, height: u32, what: &str) -> Result<usize, Error> {
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

    #[cfg(test)]
    fn len(&self) -> usize {
        self.first.len()
    }
}

/// The intermediate one band of the box downscale holds: about 1 MiB of horizontally averaged rows
/// per worker, whatever the source's size.
const BAND_BYTES: usize = 1 << 20;

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
struct BoxDownscale {
    horizontal: Coverage,
    vertical: Coverage,
    window: ProxyWindow,
    /// Output rows per band; the last band may hold fewer.
    band_rows: usize,
    parallel: bool,
}

impl BoxDownscale {
    /// The coverage tables and band size for `plan` over a `source_width × source_height` source,
    /// with bands of about `band_bytes` of intermediate. `O(source + output)`, and reads no pixel.
    fn new(
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
    fn bands(&self) -> usize {
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
    fn band<Read: Fn(u32) -> [f32; 3]>(
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

/// The area average of a JPEG source, re-quantized through the render path's own threshold
/// boundary. A uniform region therefore comes out as exactly its own code, and the proxy of an
/// identity stack agrees with the exact render's arithmetic everywhere it can. The output frame is
/// written in place and returned as the proxy's pixels, with no copy.
fn downscale_jpeg(
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

/// One band of output rows of the three planes, as the zipped chunk iterators hand it over: the
/// band index and its red, green and blue slices. The serial and parallel iterators yield the same
/// shape, so one closure serves both.
type PlanarBand<'a> = (usize, ((&'a mut [f32], &'a mut [f32]), &'a mut [f32]));

/// The area average of a prepared RAW source's planes, read through its view.
///
/// The result is a smaller [`LinearImage`] with the same fingerprint and an identity view: the
/// crop and orientation of the input view are resolved by the averaging itself, so the proxy is
/// upright content with nothing left to map. Values stay unbounded linear f32, so no clipping or
/// transfer function is introduced anywhere on this path.
fn downscale_linear(
    image: &LinearImage,
    plan: ProxyPlan,
    cancel: &Cancel,
    band_bytes: usize,
) -> Result<LinearImage, Error> {
    let (width, height) = plan.source_dimensions();
    let reader = image.reader();
    let (source_width, source_height) = reader.dimensions();
    let plane_values = float_values(width, height, "proxy linear source")?;
    let plane_len = plane_values / 3;
    // Inside the view by construction: the coverage never leaves the source.
    let read_row = |y: u32| {
        let reader = &reader;
        move |x: u32| reader.pixel(x, y).unwrap_or([0.0; 3])
    };
    let downscale = BoxDownscale::new(source_width, source_height, plan, band_bytes)?;
    let mut planes = vec![0f32; plane_values];
    {
        let (red, rest) = planes.split_at_mut(plane_len);
        let (green, blue) = rest.split_at_mut(plane_len);
        let row = width as usize;
        let pass = |rows: &mut Vec<f32>, (band, ((red, green), blue)): PlanarBand<'_>| {
            downscale.band(band, rows, cancel, &read_row, |y, x, sum| {
                red[y * row + x] = sum[0] as f32;
                green[y * row + x] = sum[1] as f32;
                blue[y * row + x] = sum[2] as f32;
            })
        };
        let band_len = downscale.band_rows * row;
        if downscale.parallel {
            red.par_chunks_mut(band_len)
                .zip(green.par_chunks_mut(band_len))
                .zip(blue.par_chunks_mut(band_len))
                .enumerate()
                .try_for_each_init(Vec::new, pass)?;
        } else {
            let mut rows = Vec::new();
            red.chunks_mut(band_len)
                .zip(green.chunks_mut(band_len))
                .zip(blue.chunks_mut(band_len))
                .enumerate()
                .try_for_each(|band| pass(&mut rows, band))?;
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
        SnapshotId, Stage, colour::srgb::decode_u8,
    };
    use luxforge_reference::srgb;
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
        (srgb::encode_clamped(linear) * 255.0).round() as u8
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
            window: None,
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
        let registry = ModuleRegistry::developer();
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

    /// The cache holds one proxy under one key: the same key hits and builds nothing, and a
    /// resized window, a different plan or a different source is a miss. A miss releases the held
    /// proxy's pixels before its replacement is built, so the two never coexist, and the cache
    /// then holds the replacement alone. A build that is cancelled leaves the cache empty, which
    /// the next job reads as a miss and rebuilds.
    #[test]
    fn the_cache_holds_one_entry_and_releases_it_before_building_its_replacement() {
        let source = jpeg_source(8, 6, &[[40, 80, 120]; 48]);
        let other = jpeg_source(8, 6, &[[40, 80, 120]; 48]);
        let other = PreviewSource::Jpeg(SourceImage {
            fingerprint: "sha256:other".into(),
            ..jpeg_of(&other).clone()
        });
        let key = |source: &PreviewSource, plan: ProxyPlan| ProxyKey {
            identity: source.identity(),
            plan,
        };
        let first = key(&source, plan(4, 3, (4, 3)));
        let mut cache = ProxyCache::default();
        let (built, fresh) = cache
            .source_for(&first, &source, || source.proxy(first.plan))
            .expect("a proxy");
        assert!(fresh, "an empty cache never hits");
        let (hit, fresh) = cache
            .source_for(&first, &source, || panic!("a hit builds nothing"))
            .expect("the held proxy");
        assert!(!fresh, "the same key hits");
        assert!(std::sync::Arc::ptr_eq(
            &jpeg_of(&hit).rgba,
            &jpeg_of(&built).rgba
        ));
        drop((built, hit));

        let misses = [
            // A resized window is a miss even at the same rounded dimensions.
            (key(&source, plan(4, 3, (5, 3))), &source),
            // A different plan is a miss.
            (key(&source, plan(2, 3, (4, 3))), &source),
            // A different source identity is a miss.
            (key(&other, first.plan), &other),
        ];
        for (miss, miss_source) in misses {
            let (held, _) = cache
                .source_for(&first, &source, || source.proxy(first.plan))
                .expect("the first proxy");
            let pixels = std::sync::Arc::downgrade(&jpeg_of(&held).rgba);
            drop(held);
            let (replacement, fresh) = cache
                .source_for(&miss, miss_source, || {
                    assert!(
                        pixels.upgrade().is_none(),
                        "the held proxy is released before its replacement is built"
                    );
                    miss_source.proxy(miss.plan)
                })
                .expect("the replacement");
            assert!(fresh, "{:?} is a miss", miss.plan);
            let (_, fresh) = cache
                .source_for(&miss, miss_source, || panic!("the replacement is held"))
                .expect("the held replacement");
            assert!(!fresh);
            drop(replacement);
        }
        let (_, fresh) = cache
            .source_for(&first, &source, || source.proxy(first.plan))
            .expect("the first proxy");
        assert!(fresh, "the cache held one entry, the last replacement");

        let cancelled = Cancel::new();
        cancelled.cancel();
        let second = key(&source, plan(2, 3, (2, 3)));
        let refused = cache.source_for(&second, &source, || {
            source.proxy_cancellable(second.plan, &cancelled)
        });
        assert_eq!(
            refused.err().map(|error| error.kind),
            Some(ErrorKind::Cancelled)
        );
        let (_, fresh) = cache
            .source_for(&first, &source, || source.proxy(first.plan))
            .expect("the first proxy");
        assert!(
            fresh,
            "a cancelled build leaves the cache empty, so the next job rebuilds"
        );
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

    /// The proxy build on the generated 24 MP and 60 MP sources, which are the only inputs that
    /// say anything about cost. It is ignored by default because it needs the generated fixtures
    /// and because timing gates do not belong in CI. For the peak memory of one size, set
    /// `LUXFORGE_PROXY_BUILD_SOURCE` to that fixture's path and wrap the run in `/usr/bin/time -l`;
    /// `LUXFORGE_PROXY_BUILDS=0` decodes the source and builds nothing, which is the floor the
    /// build's own peak is read against:
    ///
    /// ```text
    /// cargo run --release --locked --package xtask -- generate-fixtures --output fixtures/generated
    /// LUXFORGE_PROXY_BUILD_SOURCE=fixtures/generated/60mp.jpg /usr/bin/time -l \
    ///   cargo test --release --package luxforge-core --lib measure_the_proxy_build -- --ignored --nocapture
    /// ```
    ///
    /// Each line ends with a hash of the last proxy's bytes, so two builds of the code can be
    /// compared for identical output on photo-sized sources as well as for cost.
    #[test]
    #[ignore = "needs fixtures/generated and is a measurement, not a gate"]
    fn measure_the_proxy_build_on_photo_sized_sources() {
        use std::hash::{Hash, Hasher};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sources: Vec<std::path::PathBuf> = match std::env::var("LUXFORGE_PROXY_BUILD_SOURCE") {
            Ok(path) => vec![root.join(path)],
            Err(_) => ["24mp.jpg", "60mp.jpg"]
                .iter()
                .map(|name| root.join("fixtures/generated").join(name))
                .collect(),
        };
        let builds = std::env::var("LUXFORGE_PROXY_BUILDS")
            .map_or(15, |count| count.parse().expect("a build count"));
        let registry = ModuleRegistry::builtin();
        let stack = recipe(vec![basic_layer(
            json!({ "exposure": 0.5, "contrast": 20 }),
        )]);
        let bounds = ProxyBounds {
            width: 2880,
            height: 1800,
        };
        for path in sources {
            let source = PreviewSource::Jpeg(
                crate::open_source(&path).expect("a generated fixture: run generate-fixtures"),
            );
            let plan = source
                .proxy_plan(&registry, &stack, bounds)
                .expect("a plan")
                .expect("a photo-sized source needs a proxy at this size");
            let mut samples = Vec::new();
            let mut hash = None;
            for _ in 0..builds {
                let start = std::time::Instant::now();
                let proxy = source.proxy(plan).expect("a proxy");
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(proxy.dimensions(), (plan.width, plan.height));
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                jpeg_of(&proxy).rgba.hash(&mut hasher);
                hash = Some(hasher.finish());
            }
            let (width, height) = source.dimensions();
            let Some(cold) = samples.first().copied() else {
                println!(
                    "{} {width}x{height}: decoded, no proxy built",
                    path.display()
                );
                continue;
            };
            // The first build carries the Rayon pool's first use and the first touch of fresh
            // buffers, which is what the first job after a window resize actually pays; the rest
            // is the steady-state cost of rebuilding one.
            let ms = luxforge_testbase::Distribution::of(samples).expect("builds ran");
            println!(
                "{} proxy build {width}x{height} -> {}x{}: cold {cold:.2} ms, warm p50 {:.2} ms, \
                 p95 {:.2} ms, min {:.2} ms, max {:.2} ms, {builds} builds, bytes {:016x}",
                path.display(),
                plan.width,
                plan.height,
                ms.p50,
                ms.p95,
                ms.min,
                ms.max,
                hash.unwrap_or_default()
            );
        }
    }

    /// What a **mask** costs the proxy phase on photo-sized sources: the render a drag presents,
    /// unmasked, masked and point sampled, and masked under the thin-feature rule, at the display
    /// bounds the owner's screen offers. Ignored by default for the same two reasons as the build
    /// measurement above:
    ///
    /// ```text
    /// cargo run --release --locked --package xtask -- generate-fixtures --output fixtures/generated
    /// cargo test --release --package luxforge-core --lib proxy:: -- --ignored --nocapture
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
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    std::hint::black_box(frame);
                }
                let ms = luxforge_testbase::Distribution::of(samples).expect("renders ran");
                println!(
                    "{fixture} proxy {}x{} {name}: p50 {:.2} ms, p95 {:.2} ms, min {:.2} ms, max {:.2} ms",
                    plan.width, plan.height, ms.p50, ms.p95, ms.min, ms.max
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
        // A window must lie inside the proxy stage and hold a pixel.
        let windowed = |x, y, width, height| ProxyPlan {
            window: Some(ProxyWindow {
                x,
                y,
                width,
                height,
            }),
            ..plan(4, 3, (4, 3))
        };
        assert!(source.proxy(windowed(1, 1, 3, 2)).is_ok());
        assert!(source.proxy(windowed(2, 1, 3, 2)).is_err());
        assert!(source.proxy(windowed(0, 2, 4, 2)).is_err());
        assert!(source.proxy(windowed(0, 0, 0, 2)).is_err());
    }

    /// A windowed proxy source is the whole downscale's pixels in its window, bit for bit, at
    /// fractional scales and for both source kinds: the same weights over the same source samples,
    /// reading only the source rows and columns the window covers.
    #[test]
    fn a_windowed_proxy_is_the_whole_downscale_in_its_window() {
        let (width, height) = (53u32, 41u32);
        let codes: Vec<[u8; 3]> = (0..width * height)
            .map(|index| {
                [
                    (index * 37 % 251) as u8,
                    (index * 11 % 239) as u8,
                    (index * 5 % 241) as u8,
                ]
            })
            .collect();
        let floats: Vec<[f32; 3]> = codes
            .iter()
            .map(|code| code.map(|value| f32::from(value) / 97.0 - 0.3))
            .collect();
        let sources = [
            jpeg_source(width, height, &codes),
            PreviewSource::Raw {
                image: raw_source(width, height, &floats)
                    .with_view([2, 1, 49, 38], 6)
                    .expect("a view"),
                settings: LinearSettings::default(),
            },
        ];
        for source in &sources {
            let (source_width, source_height) = source.dimensions();
            let whole_plan = plan(source_width * 3 / 7, source_height * 5 / 9, (1, 1));
            let whole = source.proxy(whole_plan).expect("the whole downscale");
            for (x, y, w, h) in [
                (0, 0, 1, 1),
                (3, 2, 7, 5),
                (whole_plan.width - 4, whole_plan.height - 3, 4, 3),
                (0, 0, whole_plan.width, whole_plan.height),
            ] {
                let windowed_plan = ProxyPlan {
                    window: Some(ProxyWindow {
                        x,
                        y,
                        width: w,
                        height: h,
                    }),
                    ..whole_plan
                };
                let windowed = source.proxy(windowed_plan).expect("a windowed proxy");
                assert_eq!(windowed.dimensions(), (w, h));
                for row in 0..h {
                    for column in 0..w {
                        match (&windowed, &whole) {
                            (PreviewSource::Jpeg(part), PreviewSource::Jpeg(all)) => {
                                let at = ((row * w + column) * 4) as usize;
                                let from =
                                    (((y + row) * whole_plan.width + x + column) * 4) as usize;
                                assert_eq!(
                                    part.rgba[at..at + 4],
                                    all.rgba[from..from + 4],
                                    "({column}, {row}) of {w}x{h} at ({x}, {y})"
                                );
                            }
                            (
                                PreviewSource::Raw { image: part, .. },
                                PreviewSource::Raw { image: all, .. },
                            ) => {
                                let actual = part.pixel(column, row).unwrap();
                                let expected = all.pixel(x + column, y + row).unwrap();
                                assert!(
                                    actual.map(f32::to_bits) == expected.map(f32::to_bits),
                                    "({column}, {row}) of {w}x{h} at ({x}, {y})"
                                );
                            }
                            _ => unreachable!("a proxy keeps its source kind"),
                        }
                    }
                }
            }
        }
    }

    /// The band size never changes a proxy: bands of one output row, which split every straddled
    /// source row between two bands, and bands of a few rows build the same bytes as one band over
    /// the whole window, which is one intermediate of every source row the window reads. For both
    /// source kinds, at fractional scales, whole and windowed, serially and forced onto the
    /// parallel path.
    #[test]
    fn a_proxy_is_the_same_bytes_at_any_band_size() {
        let pixels = |width: u32, height: u32| -> Vec<[u8; 3]> {
            (0..width * height)
                .map(|index| {
                    [
                        (index * 37 % 251) as u8,
                        (index * 11 % 239) as u8,
                        (index * 5 % 241) as u8,
                    ]
                })
                .collect()
        };
        let (width, height) = (53u32, 41u32);
        let floats: Vec<[f32; 3]> = pixels(width, height)
            .iter()
            .map(|code| code.map(|value| f32::from(value) / 97.0 - 0.3))
            .collect();
        let sources = [
            jpeg_source(width, height, &pixels(width, height)),
            PreviewSource::Raw {
                image: raw_source(width, height, &floats)
                    .with_view([2, 1, 49, 38], 6)
                    .expect("a view"),
                settings: LinearSettings::default(),
            },
        ];
        let bits = |source: &PreviewSource| -> Vec<u32> {
            match source {
                PreviewSource::Jpeg(image) => image.rgba.iter().map(|&code| code.into()).collect(),
                PreviewSource::Raw { image, .. } => {
                    image.planes().iter().map(|value| value.to_bits()).collect()
                }
            }
        };
        for (pooled, source) in [false, true]
            .into_iter()
            .flat_map(|pooled| sources.iter().map(move |source| (pooled, source)))
        {
            crate::render::parallel::force(Some(pooled));
            let (source_width, source_height) = source.dimensions();
            let whole = plan(source_width * 3 / 7, source_height * 5 / 9, (1, 1));
            let windows = [
                None,
                Some(ProxyWindow {
                    x: 3,
                    y: 2,
                    width: 7,
                    height: whole.height - 4,
                }),
            ];
            for window in windows {
                let plan = ProxyPlan { window, ..whole };
                let stride = plan.source_dimensions().0 as usize * 3 * std::mem::size_of::<f32>();
                let one_band = source
                    .proxy_in_bands(plan, &Cancel::never(), usize::MAX)
                    .expect("one band");
                assert_eq!(
                    BoxDownscale::new(source_width, source_height, plan, usize::MAX)
                        .expect("a downscale")
                        .bands(),
                    1
                );
                for band_bytes in [1, stride * 3, stride * 5, stride * 11] {
                    let downscale =
                        BoxDownscale::new(source_width, source_height, plan, band_bytes)
                            .expect("a downscale");
                    assert!(
                        downscale.bands() > 1,
                        "{band_bytes} bytes splits the window"
                    );
                    assert_eq!(downscale.parallel, pooled);
                    let banded = source
                        .proxy_in_bands(plan, &Cancel::never(), band_bytes)
                        .expect("a banded proxy");
                    assert_eq!(banded.dimensions(), one_band.dimensions());
                    assert!(
                        bits(&banded) == bits(&one_band),
                        "{source_width}x{source_height} to {plan:?} in bands of {band_bytes} \
                         bytes, pooled {pooled}"
                    );
                }
            }
        }
        crate::render::parallel::force(None);
    }
}
