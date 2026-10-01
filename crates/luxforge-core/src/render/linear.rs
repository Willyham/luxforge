//! High precision scene-linear rendering for prepared RAW sources.
//!
//! This module is deliberately independent of a RAW decoder. A decoder or source-preparation
//! worker supplies immutable planar RGB values in unbounded linear sRGB/D65. The recipe is then
//! evaluated in f64 and converted to the existing byte [`Raster`] only at the terminal boundary.
//! [`Linear`] is this path's [`PixelDomain`]: the one pipeline in [`super::pipeline`] evaluates it
//! exactly as it evaluates a JPEG's bytes, and only what a linear pixel is lives here.

use super::{
    ColorRun, Compiled, Entry, Evaluation, PixelDomain, Raster, RenderContext, ResampleEntry,
    RowScratch, Segment, SegmentRows, Taps, apply_units, segment_pass,
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Cancel, Error, LinearImage, SnapshotId,
    colour::{mat3, srgb},
    modules::{Parallelism, Region, Stage},
    source::{ViewReader, layout},
};
use std::borrow::Cow;

const MAX_RESAMPLES: usize = 1;

pub(super) fn output_len(width: u32, height: u32) -> Result<usize, Error> {
    if width == 0 || height == 0 {
        return Err(Error::validation(
            "linear output dimensions must be nonzero",
        ));
    }
    if width > luxforge_raw::MAX_SIDE || height > luxforge_raw::MAX_SIDE {
        return Err(Error::resource_limit(
            "linear output side exceeds 16384 pixels",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::resource_limit("linear output dimensions overflow"))?;
    if pixels > luxforge_raw::MAX_PIXELS as u64 {
        return Err(Error::resource_limit(
            "linear output exceeds 128 megapixels",
        ));
    }
    let bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| Error::resource_limit("linear output byte length overflow"))?;
    if bytes > luxforge_raw::MAX_FRAME_BYTES {
        return Err(Error::resource_limit("linear output exceeds 512 MiB"));
    }
    usize::try_from(bytes).map_err(|_| Error::resource_limit("linear output is not addressable"))
}

/// Per-evaluation linear settings, applied to the source before recipe content edits and never to
/// an intermediate display raster. The default is the developed planes exactly. Exposure is not
/// one of them: it is Basic's colour-stage unit on every kind, so a RAW development carries none.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinearSettings {
    /// An approximate white-balance change, applied to each source pixel: `W · p`. Only the
    /// preview of an open draft carries one, when the drafted temperature or tint asks for sensor
    /// gains the developed planes were not developed at. Every committed render, export, point
    /// sample and analysis is `None`, which is bit for bit the developed planes. See
    /// `WhiteBalanceApproximation`.
    pub white_balance: Option<WhiteBalanceApproximation>,
}

/// A RAW white-balance change approximated on planes developed at another white balance.
///
/// The retained planes are `R · D(g)` per pixel, where `D(g)` is the native demosaic of the mosaic
/// after the sensor gains `g` (camera RGB) and `R` is the camera-to-linear-sRGB matrix. The demosaic
/// is nonlinear, which is why a committed white balance redevelops the mosaic, but to first order
/// `D(g') ≈ diag(g'/g) · D(g)`. So planes developed at `g` approximate the planes at `g'` by
///
/// `W = R · diag(g'_c / g_c) · R⁻¹`
///
/// applied to every pixel. The approximation is used only for a drafted preview during a gesture:
/// nothing committed, exported, sampled or analysed is ever evaluated through it, and a frame
/// rendered with it says so. The matrix is private and only the constructors build it, so every
/// instance is finite by construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhiteBalanceApproximation {
    matrix: [[f64; 3]; 3],
}

impl WhiteBalanceApproximation {
    /// `W = R · diag(target / developed) · R⁻¹` for the camera-to-linear-sRGB matrix `R`, with
    /// `R⁻¹` computed in f64. A non-finite or non-positive gain, a non-finite `R`, an `R` that is
    /// singular (or so close to it that its inverse is meaningless) and a non-finite `W` are each
    /// refused: there is no approximation, never a wrong one.
    pub(crate) fn between(
        camera_to_srgb: [[f64; 3]; 3],
        developed: [f32; 3],
        target: [f32; 3],
    ) -> Result<Self, Error> {
        let mut ratio = [0.0; 3];
        for (channel, value) in ratio.iter_mut().enumerate() {
            let (from, to) = (f64::from(developed[channel]), f64::from(target[channel]));
            if !(from.is_finite() && to.is_finite() && from > 0.0 && to > 0.0) {
                return Err(Error::validation(
                    "white-balance gains must be finite and positive",
                ));
            }
            *value = to / from;
        }
        let inverse = mat3::hadamard_checked_inverse(camera_to_srgb)?;
        let matrix = std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                (0..3)
                    .map(|k| camera_to_srgb[row][k] * ratio[k] * inverse[k][column])
                    .sum::<f64>()
            })
        });
        Self::from_matrix(matrix)
    }

    /// An explicit linear-sRGB matrix, refused unless every entry is finite.
    pub(crate) fn from_matrix(matrix: [[f64; 3]; 3]) -> Result<Self, Error> {
        if matrix.iter().flatten().all(|value| value.is_finite()) {
            Ok(Self { matrix })
        } else {
            Err(Error::validation(
                "a white-balance approximation must be finite",
            ))
        }
    }

    /// The matrix applied to each linear-sRGB pixel, row by row.
    #[cfg(test)]
    pub(crate) fn matrix(&self) -> [[f64; 3]; 3] {
        self.matrix
    }

    #[inline]
    fn apply(&self, pixel: [f64; 3]) -> [f64; 3] {
        mat3::matvec_f64(&self.matrix, pixel)
    }

    /// A key that tells this approximation's evaluation apart from an exact one of the same
    /// recipe, for a cache keyed by recipe: the matrix's own bits.
    fn key(&self) -> String {
        self.matrix
            .iter()
            .flatten()
            .map(|value| format!("{:016x}", value.to_bits()))
            .collect()
    }
}

#[inline]
fn decode_rgb(value: [u8; 3]) -> [f64; 3] {
    value.map(srgb::decode_u8)
}

/// The terminal boundary for one channel: a finite value's forward rounding,
/// `round(255 · encode(v))`, through the guarded threshold search of [`srgb::Quantizer::rounded`]
/// (the byte resample's contract as well), and a render error for a non-finite one.
#[inline]
fn terminal_srgb(quantizer: &srgb::Quantizer, linear: f64) -> Result<u8, Error> {
    if !linear.is_finite() {
        return Err(Error::render(
            "linear evaluation produced a non-finite value",
        ));
    }
    Ok(quantizer.rounded(linear))
}

/// Refuse a stack the linear path cannot evaluate: more than one resample stage.
pub(super) fn check_resamples(compiled: &Compiled) -> Result<(), Error> {
    let resamples = compiled
        .segments
        .iter()
        .filter(|segment| segment.entry.as_ref().is_some_and(Entry::blends))
        .count();
    if resamples > MAX_RESAMPLES {
        return Err(Error::validation(
            "linear evaluation supports at most one resample stage",
        ));
    }
    Ok(())
}

/// The linear domain: a developed RAW's planes in signed unbounded linear sRGB, with any approximate
/// white balance applied to each source pixel in `f64`. A segment with colour
/// is `f32` from its entry to its end and one without stays `f64`; nothing is quantized before the
/// terminal boundary, so a replacement is decoded rather than quantized at, a resample blends in
/// `f64`, and a spatial operation's `f32` output is read back exactly. There is no alpha.
#[derive(Clone, Copy)]
pub(crate) struct Linear<'a> {
    source: &'a LinearImage,
    /// The source's view resolved once, so a point read does not recompute its layout per pixel.
    reader: ViewReader<'a>,
    /// Applied to each source pixel, when the settings carry one.
    white_balance: Option<WhiteBalanceApproximation>,
}

impl<'a> Linear<'a> {
    /// `source` under `settings`.
    pub(crate) fn new(source: &'a LinearImage, settings: LinearSettings) -> Result<Self, Error> {
        Ok(Self {
            source,
            reader: source.reader(),
            white_balance: settings.white_balance,
        })
    }

    /// The one point where the settings touch a source pixel: the pixel itself, or `W · p` under
    /// an approximate white balance. Shared by the point evaluation and the rendered rows, so both
    /// keep the same f64 arithmetic and the same failure. The developed planes are finite by
    /// construction, so only a product can fail: a finite matrix may still overflow.
    #[inline(always)]
    fn adjust_source_pixel(&self, pixel: [f64; 3]) -> Result<[f64; 3], Error> {
        // The developed planes exactly, as every exact evaluation reads them.
        let Some(balance) = &self.white_balance else {
            return Ok(pixel);
        };
        let output = balance.apply(pixel);
        if output.iter().all(|value| value.is_finite()) {
            Ok(output)
        } else {
            Err(Error::render("linear source produced a non-finite value"))
        }
    }
}

impl PixelDomain for Linear<'_> {
    type Pixel = [f64; 3];
    /// Three `f32` planes inside the RAW planar limit.
    type SpatialFrame = Vec<f32>;
    type TileOutput = (Region, Vec<f32>);

    fn fingerprint(&self) -> &str {
        self.source.fingerprint()
    }

    /// Fingerprint alone does not identify developed pixels: public callers may omit it, two
    /// developments of a file differ, and crop/orientation views share their source's identity.
    /// The estimate store is keyed by the recipe prefix, which an approximate white balance does not
    /// change: the drafted recipe names the target gains whichever planes it is evaluated over. So
    /// an approximate evaluation keys its estimates apart, and a committed render of the same
    /// recipe never takes one estimated from approximate pixels.
    fn estimate_prefix<'p>(&self, prefix_hash: &'p str) -> Cow<'p, str> {
        let input_prefix = format!(
            "{prefix_hash}+linear:{}:{:?}",
            self.source.development(),
            self.source.view(),
        );
        Cow::Owned(match self.white_balance {
            Some(balance) => format!(
                "{input_prefix}+white-balance-approximation:{}",
                balance.key()
            ),
            None => input_prefix,
        })
    }

    fn check_output(&self, width: u32, height: u32) -> Result<(), Error> {
        output_len(width, height).map(drop)
    }

    /// One source pixel through the view resolved at construction. The planes need no finiteness
    /// check here: `LinearImage::construct` scans every value unless the producer already
    /// checked them, and the only such producer, the RAW camera conversion
    /// (`convert_camera_planes` in `source.rs`), refuses a non-finite output. The planes are
    /// immutable after that, so a source row reads them unchecked too.
    #[inline(always)]
    fn source_pixel(&self, x: u32, y: u32) -> Result<[f64; 3], Error> {
        let pixel = self.reader.pixel(x, y).ok_or_else(|| {
            Error::validation(format!(
                "linear source coordinate ({x}, {y}) is outside the view"
            ))
        })?;
        self.adjust_source_pixel(pixel.map(f64::from))
    }

    #[inline]
    fn replace(_: [f64; 3], rgb: [u8; 3]) -> [f64; 3] {
        decode_rgb(rgb)
    }

    /// The colour phases are the byte path's, applied to one linear pixel: every run processes the
    /// value in place and nothing is quantized between runs, which is the whole point of the linear
    /// path: a scene value above 1 or below 0 survives to the next unit and only the terminal
    /// boundary encodes it.
    #[inline(always)]
    fn colour<'r>(
        pixel: [f64; 3],
        runs: impl Iterator<Item = ColorRun<'r>>,
        x: u32,
        y: u32,
    ) -> Result<[f64; 3], Error> {
        let mut linear = [pixel.map(|value| value as f32)];
        // One pixel of snapshot scratch on the stack: a masked operation blends against its own
        // input, and a point pulls single pixels, so nothing is allocated per pixel.
        let mut scratch = [[0.0f32; 3]; 1];
        for run in runs {
            apply_units(&run, y, x, &mut linear, &mut scratch)?;
        }
        Ok(linear[0].map(f64::from))
    }

    /// The row form of [`Self::colour`]: one `f32` row through every run, which is what the rows
    /// of a rendered segment do too.
    fn colour_row<'r>(
        pixels: &mut [[f64; 3]],
        runs: impl Iterator<Item = ColorRun<'r>>,
        y: u32,
        x0: u32,
        scratch: &mut RowScratch,
    ) -> Result<(), Error> {
        let RowScratch { linear, snapshot } = scratch;
        linear.clear();
        linear.extend(pixels.iter().map(|pixel| pixel.map(|value| value as f32)));
        snapshot.resize(pixels.len().max(1), [0.0; 3]);
        for run in runs {
            apply_units(&run, y, x0, linear, snapshot)?;
        }
        for (pixel, value) in pixels.iter_mut().zip(linear.iter()) {
            *pixel = Self::finish(value.map(f64::from))?;
        }
        Ok(())
    }

    #[inline(always)]
    fn finish(pixel: [f64; 3]) -> Result<[f64; 3], Error> {
        if pixel.iter().all(|value| value.is_finite()) {
            Ok(pixel)
        } else {
            Err(Error::render(
                "linear evaluation produced a non-finite pixel",
            ))
        }
    }

    #[inline(always)]
    fn blend(
        u: f64,
        v: f64,
        width: u32,
        height: u32,
        mut fetch: impl FnMut(u32, u32) -> Result<[f64; 3], Error>,
    ) -> Result<[f64; 3], Error> {
        if !u.is_finite() || !v.is_finite() || width == 0 || height == 0 {
            return Err(Error::render(
                "linear resample has invalid coordinates or dimensions",
            ));
        }
        let taps = Taps::new(u, v, width, height);
        let [top_left, top_right, bottom_left, bottom_right] = taps.corners;
        let [w0, w1, w2, w3] = taps.weights;
        let corners = [
            (fetch(top_left.0, top_left.1)?, w0),
            (fetch(top_right.0, top_right.1)?, w1),
            (fetch(bottom_left.0, bottom_left.1)?, w2),
            (fetch(bottom_right.0, bottom_right.1)?, w3),
        ];
        let output = std::array::from_fn(|channel| {
            corners
                .iter()
                .map(|(pixel, weight)| pixel[channel] * weight)
                .sum::<f64>()
        });
        if output.iter().all(|value: &f64| value.is_finite()) {
            Ok(output)
        } else {
            Err(Error::render("linear resample produced a non-finite value"))
        }
    }

    #[inline]
    fn spatial_input(pixel: [f64; 3]) -> [f32; 3] {
        pixel.map(|value| value as f32)
    }

    #[inline]
    fn linear(pixel: [f64; 3]) -> [f64; 3] {
        pixel
    }

    #[inline]
    fn spatial_output(rgb: [f32; 3]) -> Result<[f64; 3], Error> {
        Ok(rgb.map(f64::from))
    }

    #[inline]
    fn terminal(pixel: [f64; 3]) -> Result<[u8; 4], Error> {
        terminal_pixel(pixel)
    }

    /// The RAW planar limit applies to this float frame exactly as it does to the source's.
    fn spatial_frame(stage: Stage) -> Result<Vec<f32>, Error> {
        let (values, _) = layout(stage.width, stage.height)?;
        Ok(vec![0.0_f32; values])
    }

    /// The last unit's planes as they are: they already hold rows in the frame's planar layout, and
    /// away from the stage edges their rectangle is the tile itself, so a copy here would only move
    /// the same rows twice.
    fn tile_output(
        region: Region,
        values: Vec<f32>,
        _: Region,
        _: Parallelism,
    ) -> (Region, Vec<f32>) {
        (region, values)
    }

    /// Each of the tile's rows, one `copy_from_slice` per plane, from wherever the tile lies in the
    /// last unit's rectangle.
    fn write_tile(
        frame: &mut Vec<f32>,
        stage: Stage,
        tile: Region,
        (region, values): (Region, Vec<f32>),
    ) {
        let plane = (u64::from(stage.width) * u64::from(stage.height)) as usize;
        let source = region.pixels() as usize;
        let width = tile.width as usize;
        for y in tile.y0..tile.y1() {
            let to = (u64::from(y) * u64::from(stage.width) + u64::from(tile.x0)) as usize;
            let from =
                (y - region.y0) as usize * region.width as usize + (tile.x0 - region.x0) as usize;
            for channel in 0..3 {
                let (to, from) = (channel * plane + to, channel * source + from);
                frame[to..to + width].copy_from_slice(&values[from..from + width]);
            }
        }
    }

    #[inline]
    fn frame_pixel(frame: &Vec<f32>, stage: Stage, x: u32, y: u32) -> [f64; 3] {
        let plane = (u64::from(stage.width) * u64::from(stage.height)) as usize;
        let offset = (u64::from(y) * u64::from(stage.width) + u64::from(x)) as usize;
        [
            f64::from(frame[offset]),
            f64::from(frame[plane + offset]),
            f64::from(frame[2 * plane + offset]),
        ]
    }
}

#[inline]
pub(super) fn terminal_pixel(pixel: [f64; 3]) -> Result<[u8; 4], Error> {
    terminal_pixel_in(srgb::quantizer(), pixel)
}

/// [`terminal_pixel`] through a quantizer a row loop took once.
#[inline]
fn terminal_pixel_in(quantizer: &srgb::Quantizer, pixel: [f64; 3]) -> Result<[u8; 4], Error> {
    Ok([
        terminal_srgb(quantizer, pixel[0])?,
        terminal_srgb(quantizer, pixel[1])?,
        terminal_srgb(quantizer, pixel[2])?,
        255,
    ])
}

/// The whole frame of a frames-mode evaluation, terminally produced as bytes: the linear path's half
/// of [`super::Render::frame`], for the exact phase and the proxy phase alike.
///
/// This driver materializes only the last segment's output, as terminal bytes, in one
/// [`segment_pass`] whose rows pull their entry through the evaluation: the source's rows directly
/// when the stack is one segment read through the identity, and otherwise each pixel through the
/// geometry from the source, the evaluation's spatial frame or a resample of the segment before.
/// What lies before a resample is pulled, never materialized, because it is `f64`: its taps are
/// read one block of output pixels at a time, through the rectangle of the segment before them
/// that the block reads ([`LinearRows::load_resampled`]).
pub(super) fn rasterize(
    evaluation: &Evaluation<'_, Linear<'_>>,
    snapshot_id: SnapshotId,
    cancel: &Cancel,
    context: &RenderContext,
) -> Result<Raster, Error> {
    let stage = evaluation.stage();
    // Write into the Arc-backed frame that the raster returns, avoiding an output publication copy.
    let mut frame = super::zeroed_frame(output_len(stage.width, stage.height)?);
    let index = evaluation.compiled.segments.len() - 1;
    let segment = &evaluation.compiled.segments[index];
    let source = evaluation.domain.source;
    let reader = (segment.entry.is_none()
        && segment
            .geometry
            .is_identity(source.width(), source.height()))
    .then_some(evaluation.domain.reader);
    segment_pass(
        &LinearRows {
            evaluation,
            index,
            segment,
            reader,
            quantizer: srgb::quantizer(),
            #[cfg(test)]
            context,
        },
        segment,
        super::frame_mut(&mut frame),
        0..stage.height as usize,
        cancel,
        context.scratch(),
    )?;
    Ok(Raster {
        width: stage.width,
        height: stage.height,
        rgba: frame,
        source_fingerprint: source.fingerprint().to_owned(),
        snapshot_id,
    })
}

/// How many output columns one block of a resampled segment's rows reads its taps for at once.
const TAP_BLOCK_COLUMNS: u32 = 64;

/// The most pixels of the segment before a resample one block holds. A block of up to 16 rows by
/// 64 columns reads about 1,500 of them at a small angle and about 4,000 at the 45 degree limit,
/// with [`super::geometry::TAP_MARGIN`] on every side; a mapping that would need more than this reads its
/// taps one at a time instead.
pub(super) const TAP_BLOCK_PIXELS: u64 = 16 * 1024;

/// The last segment's rows on the linear path. A segment with colour holds its rows as `f32`
/// between its entry and the terminal boundary, exactly the value [`Linear::colour`] converts a
/// pixel to; one without colour has nothing to hold, so its entry and replacements go straight to
/// terminal bytes.
pub(super) struct LinearRows<'e, 'x, 's> {
    evaluation: &'e Evaluation<'x, Linear<'s>>,
    index: usize,
    segment: &'e Segment,
    /// The source's rows, when the segment reads the source through the identity.
    reader: Option<ViewReader<'e>>,
    /// The terminal boundary's quantizer, taken once for the pass rather than once per channel.
    quantizer: &'static srgb::Quantizer,
    #[cfg(test)]
    context: &'e RenderContext,
}

/// What one worker reuses for every chunk of linear rows it takes.
#[derive(Default)]
pub(super) struct LinearScratch {
    /// A colour segment's rows, in `f32`; empty for a segment without colour.
    rows: Vec<[f32; 3]>,
    /// The pixels of the segment before a resample that one block of taps reads.
    block: Vec<[f64; 3]>,
    /// One row of that segment's colour.
    row: RowScratch,
}

impl LinearRows<'_, '_, '_> {
    /// One viewed source row, which the reader resolves once instead of per pixel.
    fn source_row<'r>(
        reader: &'r ViewReader<'_>,
        y: u32,
    ) -> Result<impl ExactSizeIterator<Item = [f32; 3]> + 'r, Error> {
        reader
            .row(y)
            .ok_or_else(|| Error::render("linear output coordinate was outside stage"))
    }

    /// The segment's entry value under output pixel `(x, y)`, through its exact geometry.
    #[inline]
    fn entry(&self, x: u32, y: u32) -> Result<[f64; 3], Error> {
        let (input_x, input_y) = self.segment.geometry.unmap(x, y);
        self.evaluation.entry_pixel(self.index, input_x, input_y)
    }

    /// Hand one entry value to the chunk: as `f32` to a colour segment's rows, or as terminal
    /// bytes when the segment has no colour.
    #[inline]
    fn put(
        &self,
        rows: &mut [[f32; 3]],
        chunk: &mut [u8],
        offset: usize,
        pixel: [f64; 3],
    ) -> Result<(), Error> {
        if self.segment.has_color {
            rows[offset] = pixel.map(|value| value as f32);
        } else {
            chunk[offset * 4..offset * 4 + 4]
                .copy_from_slice(&terminal_pixel_in(self.quantizer, pixel)?);
        }
        Ok(())
    }

    /// The rows of a chunk whose entry is pulled one pixel at a time through the evaluation
    /// ([`Evaluation::entry_pixel`]), or read from the source's rows when the segment reads the
    /// source through the identity.
    pub(super) fn load_pulled(
        &self,
        scratch: &mut LinearScratch,
        y0: u32,
        chunk: &mut [u8],
    ) -> Result<(), Error> {
        let width = self.segment.width as usize;
        let domain = &self.evaluation.domain;
        for (row, bytes) in chunk.chunks_exact_mut(width * 4).enumerate() {
            let y = y0 + row as u32;
            let values = &mut scratch.rows;
            // Immutable source planes were checked finite on construction. A source row widens at
            // the same boundary as the point path's source pixel.
            match (&self.reader, self.segment.has_color) {
                (Some(reader), true) => {
                    for (pixel, value) in Self::source_row(reader, y)?
                        .zip(values[row * width..(row + 1) * width].iter_mut())
                    {
                        let pixel = domain.adjust_source_pixel(pixel.map(f64::from))?;
                        *value = pixel.map(|value| value as f32);
                    }
                }
                (Some(reader), false) => {
                    for (pixel, rgba) in Self::source_row(reader, y)?.zip(bytes.chunks_exact_mut(4))
                    {
                        let pixel = domain.adjust_source_pixel(pixel.map(f64::from))?;
                        rgba.copy_from_slice(&terminal_pixel_in(self.quantizer, pixel)?);
                    }
                }
                (None, true) => {
                    for (x, value) in values[row * width..(row + 1) * width]
                        .iter_mut()
                        .enumerate()
                    {
                        *value = self.entry(x as u32, y)?.map(|value| value as f32);
                    }
                }
                (None, false) => {
                    for (x, rgba) in bytes.chunks_exact_mut(4).enumerate() {
                        rgba.copy_from_slice(&terminal_pixel_in(
                            self.quantizer,
                            self.entry(x as u32, y)?,
                        )?);
                    }
                }
            }
        }
        Ok(())
    }

    /// A resampled segment's rows, in blocks of [`TAP_BLOCK_COLUMNS`] columns: each block reads
    /// the rectangle of the segment before the resample its taps need ([`ResampleEntry::reads`]),
    /// unless it holds more than [`TAP_BLOCK_PIXELS`], once, through
    /// [`Evaluation::region_in`], and blends every output pixel from it with the resample's own
    /// [`Linear::blend`]. Every tap is the value [`Evaluation::pixel_in`] answers there, so the
    /// result is [`Evaluation::entry_pixel`]'s, while each pixel before the resample is evaluated
    /// about once per block instead of once per tap and its colour runs over rows.
    pub(super) fn load_resampled(
        &self,
        entry: &ResampleEntry,
        scratch: &mut LinearScratch,
        y0: u32,
        chunk: &mut [u8],
    ) -> Result<(), Error> {
        let width = self.segment.width;
        let rows = (chunk.len() / (width as usize * 4)) as u32;
        let stage = self.evaluation.compiled.segments[self.index - 1].stage();
        let outside = || Error::render("a resample tap was outside its stage");
        let LinearScratch {
            rows: values,
            block,
            row,
        } = scratch;
        for x0 in (0..width).step_by(TAP_BLOCK_COLUMNS as usize) {
            self.evaluation.checkpoint()?;
            let columns = (width - x0).min(TAP_BLOCK_COLUMNS);
            // The block's rectangle of the resample's full output: the segment's exact geometry
            // maps the block onto one, placed at the entry window's origin.
            let local = self.segment.geometry.unmap_region(Region {
                x0,
                y0,
                width: columns,
                height: rows,
            });
            let (full_x, full_y) = entry.output_at(local.x0, local.y0);
            let window = Region {
                x0: full_x,
                y0: full_y,
                ..local
            };
            let held = entry
                .reads(window, stage)
                .filter(|region| region.pixels() <= TAP_BLOCK_PIXELS);
            if let Some(region) = held {
                self.evaluation
                    .region_in(self.index - 1, region, block, row)?;
                #[cfg(test)]
                self.context
                    .note_resample_bytes(block.capacity() * std::mem::size_of::<[f64; 3]>());
            }
            for y in y0..y0 + rows {
                for x in x0..x0 + columns {
                    let (input_x, input_y) = self.segment.geometry.unmap(x, y);
                    let (u, v) = entry.input_at(input_x, input_y);
                    let pixel =
                        Linear::blend(u, v, stage.width, stage.height, |x, y| match held {
                            Some(region) if region.contains(x, y) => {
                                Ok(block
                                    [((y - region.y0) * region.width + (x - region.x0)) as usize])
                            }
                            _ => self
                                .evaluation
                                .pixel_in(self.index - 1, x, y)?
                                .ok_or_else(outside),
                        })?;
                    let offset = ((y - y0) * width + x) as usize;
                    self.put(values, chunk, offset, pixel)?;
                }
            }
        }
        Ok(())
    }
}

impl SegmentRows for LinearRows<'_, '_, '_> {
    type Scratch = LinearScratch;

    fn scratch_bytes(&self, width: usize, rows: usize, _: usize) -> usize {
        let colour = if self.segment.has_color {
            rows * width * std::mem::size_of::<[f32; 3]>()
        } else {
            0
        };
        let taps = self.segment.entry.as_ref().map_or(0, Entry::linear_scratch);
        colour + taps
    }

    fn load(&self, scratch: &mut Self::Scratch, y0: u32, chunk: &mut [u8]) -> Result<(), Error> {
        if self.segment.has_color {
            // Every value is written below, by the rows or the resample's blocks, before anything
            // reads it, so a worker's buffer is resized and never cleared.
            scratch.rows.resize(chunk.len() / 4, [0.0; 3]);
        }
        match &self.segment.entry {
            Some(entry) => entry.load_linear(self, scratch, y0, chunk),
            None => self.load_pulled(scratch, y0, chunk),
        }
    }

    fn replace(
        &self,
        scratch: &mut Self::Scratch,
        chunk: &mut [u8],
        offset: usize,
        rgb: [u8; 3],
    ) -> Result<(), Error> {
        self.put(&mut scratch.rows, chunk, offset, decode_rgb(rgb))
    }

    fn run(
        &self,
        scratch: &mut Self::Scratch,
        _: &mut [u8],
        run: &ColorRun<'_>,
        y0: u32,
        rows: std::ops::Range<usize>,
        snapshot: &mut [[f32; 3]],
    ) -> Result<(), Error> {
        let width = self.segment.width as usize;
        // The same coordinates the byte path hands its units, so a position-dependent unit makes
        // a linear sample and a linear frame agree pixel for pixel.
        for (offset, row) in scratch.rows[rows.start * width..rows.end * width]
            .chunks_mut(width)
            .enumerate()
        {
            apply_units(run, y0 + (rows.start + offset) as u32, 0, row, snapshot)?;
        }
        Ok(())
    }

    fn store(&self, scratch: &mut Self::Scratch, chunk: &mut [u8]) -> Result<(), Error> {
        if self.segment.has_color {
            for (rgba, pixel) in chunk.chunks_exact_mut(4).zip(scratch.rows.iter()) {
                rgba.copy_from_slice(&terminal_pixel_in(self.quantizer, pixel.map(f64::from))?);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{
        SpatialMode, parallel,
        spatial::Tiling,
        testing::{
            frame_in, linear, linear_evaluation, point_evaluated, render_linear,
            render_linear_cancellable, sample_in, sample_linear,
        },
        tests::{image, varied},
    };
    use crate::{
        Layer, Recipe, RenderContext, RenderOptions, SnapshotId,
        modules::{CropPayload, ModuleRegistry},
    };
    use luxforge_raw::SPATIAL_TILE;
    use luxforge_reference::srgb as srgb_ref;
    use luxforge_testbase::Distribution;

    /// The frame `render_linear` writes, checked against the point evaluator's byte at every pixel
    /// ([`point_evaluated`]).
    fn rendered_as_the_point_evaluator(
        registry: &ModuleRegistry,
        source: &LinearImage,
        recipe: &Recipe,
        settings: LinearSettings,
        what: &str,
    ) -> Raster {
        let rendered =
            render_linear(registry, source, SnapshotId::new(), recipe, settings).unwrap();
        let (width, height, expected) = point_evaluated(
            &RenderContext::new(),
            registry,
            linear(source, settings),
            recipe,
            SpatialMode::Frames,
        )
        .unwrap();
        assert_eq!((rendered.width, rendered.height), (width, height), "{what}");
        assert!(
            rendered.rgba.as_slice() == expected.as_slice(),
            "{what}: the rows differ from the point evaluator"
        );
        rendered
    }

    fn colour_layer(effect_id: &str, payload: serde_json::Value) -> Layer {
        Layer {
            id: crate::LayerId::new(),
            effect_id: effect_id.into(),
            effect_format: crate::EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn colour_recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    #[test]
    fn source_rows_preserve_all_views_and_f64_adjustments() {
        let mut pixels: Vec<_> = (0..99)
            .map(|index| [index as f32 / 59.0 - 0.2, index as f32 / 97.0, 0.37])
            .collect();
        pixels[0] = [-0.0, 0.0, f32::from_bits(1)];
        pixels[1] = [f32::MIN, f32::MAX, -f32::from_bits(1)];
        let source = image(11, 9, &pixels);
        let original: Vec<_> = source
            .planes()
            .iter()
            .map(|value| value.to_bits())
            .collect();
        let registry = ModuleRegistry::builtin();
        let recipe = Recipe::default();
        let balance = WhiteBalanceApproximation::from_matrix([
            [1.3, 0.1, -0.05],
            [0.02, 0.97, 0.01],
            [-0.1, 0.05, 0.62],
        ])
        .unwrap();
        for orientation in 1..=8 {
            for crop in [
                [0, 0, 11, 9],
                [2, 3, 7, 4],
                [10, 8, 1, 1],
                [0, 0, 1, 9],
                [0, 0, 11, 1],
            ] {
                let view = source.with_view(crop, orientation).unwrap();
                assert!(std::ptr::eq(source.planes(), view.planes()));
                let reader = view.reader();
                assert!(reader.row(view.height()).is_none());
                for y in 0..view.height() {
                    let row = reader.row(y).unwrap();
                    assert_eq!(row.len(), view.width() as usize);
                    for (x, pixel) in row.enumerate() {
                        assert_eq!(
                            pixel.map(f32::to_bits),
                            view.pixel(x as u32, y).unwrap().map(f32::to_bits),
                            "orientation {orientation}, crop {crop:?}, ({x}, {y})"
                        );
                    }
                }
                for white_balance in [None, Some(balance)] {
                    let settings = LinearSettings { white_balance };
                    rendered_as_the_point_evaluator(
                        &registry,
                        &view,
                        &recipe,
                        settings,
                        &format!("orientation {orientation}, crop {crop:?}, settings {settings:?}"),
                    );
                }
            }
        }
        assert_eq!(
            source
                .planes()
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            original
        );
    }

    /// Forced onto the pool, every row of a view is read whole through each stride an orientation
    /// gives it, over a stage several row chunks tall.
    #[test]
    fn source_rows_preserve_complete_parallel_buffers_for_each_stride() {
        let source = varied(131, 129);
        let registry = ModuleRegistry::builtin();
        let recipe = Recipe::default();
        let settings = LinearSettings {
            white_balance: None,
        };
        parallel::force(Some(true));
        for orientation in [1, 2, 5, 7] {
            let view = source.with_view([1, 1, 128, 128], orientation).unwrap();
            let actual = rendered_as_the_point_evaluator(
                &registry,
                &view,
                &recipe,
                settings,
                &format!("orientation {orientation}"),
            );
            for (x, y) in [(0, 0), (64, 63), (127, 127)] {
                assert_eq!(
                    sample_linear(&registry, &view, &recipe, settings, x, y)
                        .unwrap()
                        .rgba,
                    actual.pixel(x, y)
                );
            }
        }
        parallel::force(None);
    }

    #[test]
    fn source_rows_keep_validation_fallback_cancellation_and_finite_errors() {
        let source = varied(9, 7);
        let registry = ModuleRegistry::developer();
        let settings = LinearSettings::default();
        for recipe in [
            Recipe {
                layers: vec![Layer::pixel(1, 1, [30, 60, 90])],
                ..Recipe::default()
            },
            Recipe {
                layers: vec![Layer::orientation(crate::Orientation {
                    mirror: false,
                    turns: 1,
                })],
                ..Recipe::default()
            },
            Recipe {
                layers: vec![Layer::crop(CropPayload {
                    angle: 5.0,
                    x: 0.2,
                    y: 0.2,
                    width: 0.6,
                    height: 0.6,
                })],
                ..Recipe::default()
            },
            cancellation_recipe(),
        ] {
            rendered_as_the_point_evaluator(&registry, &source, &recipe, settings, "fallback");
        }
        let mut unavailable = cancellation_recipe();
        unavailable.layers[0].effect_id = "unavailable.effect".into();
        let expected = registry
            .compile(source.width(), source.height(), &unavailable)
            .err()
            .unwrap();
        let actual = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &unavailable,
            settings,
        )
        .unwrap_err();
        assert_eq!(
            (actual.kind, actual.detail),
            (expected.kind, expected.detail)
        );
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(
            render_linear_cancellable(
                &registry,
                &source,
                SnapshotId::new(),
                &Recipe::default(),
                settings,
                &cancel
            )
            .unwrap_err()
            .kind,
            ErrorKind::Cancelled
        );

        // Finite WB coefficients can still overflow while evaluating a pixel. The row path must
        // keep the generic source-adjustment error, not let a later terminal check replace it.
        let overflow = LinearSettings {
            white_balance: Some(
                WhiteBalanceApproximation::from_matrix([[f64::MAX; 3]; 3]).unwrap(),
            ),
        };
        let overflowing_source = image(1, 1, &[[1.0; 3]]);
        let context = RenderContext::new();
        let generic = linear_evaluation(
            &context,
            &registry,
            &overflowing_source,
            &Recipe::default(),
            overflow,
            Tiling::Halo,
            SpatialMode::Frames,
        )
        .unwrap();
        let expected = generic.pixel(0, 0).unwrap_err();
        let actual = render_linear(
            &registry,
            &overflowing_source,
            SnapshotId::new(),
            &Recipe::default(),
            overflow,
        )
        .unwrap_err();
        assert_eq!(
            (actual.kind, actual.detail),
            (expected.kind, expected.detail)
        );
    }

    fn reference_srgb(value: f64) -> u8 {
        (srgb_ref::encode_clamped(value) * 255.0).round() as u8
    }

    #[test]
    fn terminal_quantization_preserves_forward_rounding_at_every_f64_boundary() {
        // Independent inverse transfer, including every representable neighbour around each
        // code boundary. Some of these values intentionally disagree with inverse-only lookup.
        for code in 1..=255_u32 {
            let encoded = (f64::from(code) - 0.5) / 255.0;
            let boundary = srgb_ref::decode_encoded(encoded);
            let bits = boundary.to_bits();
            for bits in bits - 128..=bits + 128 {
                let value = f64::from_bits(bits);
                assert_eq!(
                    terminal_srgb(srgb::quantizer(), value).unwrap(),
                    reference_srgb(value),
                    "code {code}, bits {bits:#018x}"
                );
            }
            // Include both sides of the guard as well as values within it. The reference is
            // always the former forward transfer, never the lookup under test.
            for delta in [-2e-12, -1e-12, -5e-13, 5e-13, 1e-12, 2e-12] {
                let value = boundary + delta;
                for value in [value.next_down(), value, value.next_up()] {
                    assert_eq!(
                        terminal_srgb(srgb::quantizer(), value).unwrap(),
                        reference_srgb(value)
                    );
                }
            }
        }
    }

    #[test]
    fn terminal_quantization_preserves_finite_domain_and_rejects_nonfinite_values() {
        for value in [
            f64::MIN,
            -1.0,
            -f64::MIN_POSITIVE,
            -f64::from_bits(1),
            -0.0,
            0.0,
            f64::from_bits(1),
            f64::MIN_POSITIVE,
            0.003_130_8_f64.next_down(),
            0.003_130_8,
            0.003_130_8_f64.next_up(),
            1.0_f64.next_down(),
            1.0,
            1.0_f64.next_up(),
            32.0,
            f64::MAX,
        ] {
            assert_eq!(
                terminal_srgb(srgb::quantizer(), value).unwrap(),
                reference_srgb(value)
            );
        }
        for step in 0..=40_000 {
            let value = f64::from(step) / 40_000.0;
            assert_eq!(
                terminal_srgb(srgb::quantizer(), value).unwrap(),
                reference_srgb(value)
            );
        }
        // Deterministic bit-pattern coverage also visits the very dark/subnormal domain that
        // a uniform sweep misses, plus signed and extended-domain source values.
        let mut bits = 0x1234_5678_9abc_def0_u64;
        for _ in 0..100_000 {
            bits = bits.wrapping_mul(6364136223846793005).wrapping_add(1);
            let value = f64::from_bits(bits);
            if value.is_finite() {
                assert_eq!(
                    terminal_srgb(srgb::quantizer(), value).unwrap(),
                    reference_srgb(value)
                );
            }
        }
        for value in [f64::NAN, -f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = terminal_srgb(srgb::quantizer(), value).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Render);
            assert_eq!(
                error.detail,
                "linear evaluation produced a non-finite value"
            );
        }
    }

    #[test]
    fn raw_planar_bound_is_separate_from_terminal_rgba_bound() {
        // This checks admission arithmetic only; LinearImage is not allocated.
        assert!(layout(16_000, 8_000).is_ok());
        assert!(output_len(16_000, 6_250).is_ok());
        assert!(output_len(16_000, 8_000).is_ok());
        assert!(output_len(16_000, 8_001).is_err());
        assert!(layout(16_384, 16_384).is_err());
    }

    /// A colour-stage layer reaches the linear path too: the Basic module's units run on the
    /// scene-linear pixel, without the 8-bit decode and quantize the JPEG path needs, and the only
    /// encoding is the terminal boundary. A RAW stack therefore never silently omits a Basic edit.
    #[test]
    fn a_colour_layer_runs_on_the_linear_pixel_and_encodes_only_at_the_boundary() {
        let source = image(2, 1, &[[0.1, 0.2, 0.3], [0.05, 0.4, 0.6]]);
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: serde_json::json!({"exposure": 1.0}),
                mask: None,
                artifacts: Vec::new(),
            }],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let raster = render_linear(
            &ModuleRegistry::builtin(),
            &source,
            SnapshotId::new(),
            &recipe,
            LinearSettings::default(),
        )
        .unwrap();
        // +1 EV doubles the linear value; the second pixel's blue clips only at the encoding.
        assert_eq!(
            raster.pixel(0, 0),
            Some([
                reference_srgb(0.2),
                reference_srgb(0.4),
                reference_srgb(0.6),
                255
            ])
        );
        assert_eq!(
            raster.pixel(1, 0),
            Some([
                reference_srgb(0.1),
                reference_srgb(0.8),
                reference_srgb(1.0),
                255
            ])
        );
    }

    #[test]
    fn planar_source_keeps_negative_and_headroom_values_until_terminal_boundary() {
        let source = image(
            2,
            2,
            &[
                [-0.25, 0.18, 1.5],
                [0.5, 0.2, -0.1],
                [1.25, 0.4, 0.75],
                [2.0, -0.5, 0.25],
            ],
        );
        assert_eq!(source.pixel(0, 0), Some([-0.25, 0.18, 1.5]));
        let raster = render_linear(
            &ModuleRegistry::builtin(),
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            LinearSettings::default(),
        )
        .unwrap();
        assert_eq!(
            raster.pixel(0, 0),
            Some([0, reference_srgb(0.18), 255, 255])
        );
        assert_eq!(
            raster.pixel(1, 1),
            Some([255, 0, reference_srgb(0.25), 255])
        );
    }

    /// Basic's exposure on the developed planes is an `f64` read of the unclipped source, clipped
    /// only at the terminal boundary: a value pushed past 1 by the gain encodes as white and a
    /// negative one as black, exactly as the reference computes it.
    #[test]
    fn basic_exposure_reads_unclipped_planes_before_terminal_clipping() {
        let source = image(1, 1, &[[0.18, -0.1, 0.6]]);
        let raster = render_linear(
            &ModuleRegistry::builtin(),
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![colour_layer(
                crate::BASIC_EFFECT,
                serde_json::json!({"exposure": 1.0}),
            )]),
            LinearSettings::default(),
        )
        .unwrap();
        assert_eq!(
            raster.pixel(0, 0),
            Some([reference_srgb(0.36), 0, 255, 255])
        );
    }

    #[test]
    fn exact_recipe_geometry_and_source_view_preserve_working_values() {
        let source = image(
            3,
            2,
            &[
                [0.1, 1.1, -0.1],
                [0.2, 1.2, -0.2],
                [0.3, 1.3, -0.3],
                [0.4, 1.4, -0.4],
                [0.5, 1.5, -0.5],
                [0.6, 1.6, -0.6],
            ],
        );
        let view = source.with_view([0, 0, 3, 2], 6).unwrap();
        assert_eq!(view.width(), 2);
        assert_eq!(view.height(), 3);
        assert_eq!(view.pixel(0, 0), Some([0.4, 1.4, -0.4]));
        assert_eq!(view.pixel(1, 2), Some([0.3, 1.3, -0.3]));
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer::orientation(crate::Orientation {
                mirror: false,
                turns: 2,
            })],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let context = RenderContext::new();
        let evaluation = linear_evaluation(
            &context,
            &ModuleRegistry::builtin(),
            &view,
            &recipe,
            LinearSettings::default(),
            Tiling::Halo,
            SpatialMode::Point,
        )
        .unwrap();
        assert_eq!(
            evaluation.pixel(1, 2).unwrap(),
            Some([f64::from(0.4_f32), f64::from(1.4_f32), f64::from(-0.4_f32),])
        );
    }

    #[test]
    fn all_eight_source_view_orientations_match_independent_literals() {
        let source = image(
            2,
            3,
            &[
                [1.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [3.0, 0.0, 0.0],
                [4.0, 0.0, 0.0],
                [5.0, 0.0, 0.0],
                [6.0, 0.0, 0.0],
            ],
        );
        let expected = [
            (1, 2, 3, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
            (2, 2, 3, vec![2.0, 1.0, 4.0, 3.0, 6.0, 5.0]),
            (3, 2, 3, vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0]),
            (4, 2, 3, vec![5.0, 6.0, 3.0, 4.0, 1.0, 2.0]),
            (5, 3, 2, vec![1.0, 3.0, 5.0, 2.0, 4.0, 6.0]),
            (6, 3, 2, vec![5.0, 3.0, 1.0, 6.0, 4.0, 2.0]),
            (7, 3, 2, vec![6.0, 4.0, 2.0, 5.0, 3.0, 1.0]),
            (8, 3, 2, vec![2.0, 4.0, 6.0, 1.0, 3.0, 5.0]),
        ];
        for (orientation, width, height, expected_red) in expected {
            let view = source.with_view([0, 0, 2, 3], orientation).unwrap();
            let mut actual = Vec::with_capacity((width * height) as usize);
            for y in 0..height {
                for x in 0..width {
                    actual.push(view.pixel(x, y).unwrap()[0]);
                }
            }
            assert_eq!(actual, expected_red, "orientation {orientation}");
        }
    }

    #[test]
    fn source_vec_storage_is_moved_without_pixel_copy_and_views_share_it() {
        let mut planes = Vec::with_capacity(12);
        planes.extend([0.0_f32, 1.0, 2.0, 3.0]);
        planes.extend([4.0, 5.0, 6.0, 7.0]);
        planes.extend([8.0, 9.0, 10.0, 11.0]);
        let pointer = planes.as_ptr();
        let source = LinearImage::new(2, 2, planes).unwrap();
        let view = source.with_view([0, 0, 2, 2], 6).unwrap();
        assert_eq!(source.planes().as_ptr(), pointer);
        assert_eq!(view.planes().as_ptr(), pointer);
        assert_eq!(view.pixel(0, 0), Some([2.0, 6.0, 10.0]));
    }

    #[test]
    fn validated_plane_adoption_keeps_layout_identity_and_shared_storage() {
        let planes = vec![0.25, 0.5, 0.75, 1.0, -0.5, 2.0];
        let pointer = planes.as_ptr();
        let source =
            LinearImage::from_validated_planes(2, 1, planes, "raw-fingerprint".into()).unwrap();
        let view = source.with_view([0, 0, 2, 1], 2).unwrap();
        assert_eq!(source.planes().as_ptr(), pointer);
        assert_eq!(view.planes().as_ptr(), pointer);
        assert_eq!(source.fingerprint(), "raw-fingerprint");
        assert_eq!(view.development(), source.development());
        assert_eq!(source.pixel(0, 0), Some([0.25, 0.75, -0.5]));
        assert_eq!(view.pixel(0, 0), Some([0.5, 1.0, 2.0]));
        assert_eq!(
            LinearImage::from_validated_planes(2, 2, vec![0.0; 6], String::new())
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
    }

    #[test]
    fn bilinear_preserves_headroom_and_point_replace_decodes_at_its_stage() {
        let corners = [
            [2.0, -1.0, 0.0],
            [0.0, 1.0, 2.0],
            [2.0, -1.0, 0.0],
            [0.0, 1.0, 2.0],
        ];
        let actual =
            Linear::blend(1.0, 1.0, 2, 2, |x, y| Ok(corners[(y * 2 + x) as usize])).unwrap();
        assert_eq!(actual, [1.0, 0.0, 1.0]);

        let precise = [
            [0.125_123_456_789, -0.543_210_987_654, 1.734_567_890_123],
            [0.912_345_678_901, 0.234_567_890_123, -0.876_543_210_987],
            [1.234_567_890_123, -1.345_678_901_234, 0.456_789_012_345],
            [-0.321_098_765_432, 0.678_901_234_567, 1.890_123_456_789],
        ];
        let actual =
            Linear::blend(1.25, 1.25, 2, 2, |x, y| Ok(precise[(y * 2 + x) as usize])).unwrap();
        let expected: [f64; 3] = std::array::from_fn(|channel| {
            precise[0][channel] * 0.0625
                + precise[1][channel] * 0.1875
                + precise[2][channel] * 0.1875
                + precise[3][channel] * 0.5625
        });
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-15);
        }

        let source = image(2, 2, &[[0.0, 0.0, 0.0]; 4]);
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer::pixel(1, 0, [128, 64, 255])],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let sample = sample_linear(
            &ModuleRegistry::developer(),
            &source,
            &recipe,
            LinearSettings::default(),
            1,
            0,
        )
        .unwrap();
        assert_eq!(
            sample.rgba,
            Some([
                reference_srgb(srgb::decode_u8(128)),
                reference_srgb(srgb::decode_u8(64)),
                255,
                255
            ])
        );
    }

    // -----------------------------------------------------------------------------------------
    // The white-balance approximation.
    // -----------------------------------------------------------------------------------------

    /// A plausible camera-to-sRGB matrix: rows sum to one, strong off-diagonal terms, invertible.
    const CAMERA: [[f64; 3]; 3] = [
        [1.72, -0.61, -0.11],
        [-0.18, 1.49, -0.31],
        [0.04, -0.52, 1.48],
    ];

    fn apply(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
        matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
    }

    /// With no approximation the evaluation is the one every committed render does: each byte is
    /// the independent `sRGB(p)` of its developed source pixel. An identity matrix, whose products
    /// are exact, renders the same bytes, so the approximation adds nothing but its matrix.
    #[test]
    fn no_approximation_is_bit_for_bit_the_developed_planes() {
        let registry = ModuleRegistry::builtin();
        let source = varied(9, 7);
        let plain = LinearSettings::default();
        let raster = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            plain,
        )
        .unwrap();
        for y in 0..7 {
            for x in 0..9 {
                let pixel = source.pixel(x, y).unwrap().map(f64::from);
                let expected = pixel.map(reference_srgb);
                assert_eq!(
                    raster.pixel(x, y),
                    Some([expected[0], expected[1], expected[2], 255]),
                    "({x}, {y})"
                );
            }
        }
        let identity = LinearSettings {
            white_balance: Some(
                WhiteBalanceApproximation::from_matrix([
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ])
                .unwrap(),
            ),
        };
        let through_identity = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            identity,
        )
        .unwrap();
        assert_eq!(through_identity.rgba, raster.rgba);
    }

    /// The approximation multiplies each source pixel by `W`: the byte is the independent
    /// `sRGB(W · p)`, so a colour layer after it — Basic's exposure among them — sees the
    /// approximated scene value exactly as it sees an exact one.
    #[test]
    fn the_approximation_applies_its_matrix_to_each_source_pixel() {
        let matrix = [[1.3, 0.1, -0.05], [0.02, 0.97, 0.01], [-0.1, 0.05, 0.62]];
        let settings = LinearSettings {
            white_balance: Some(WhiteBalanceApproximation::from_matrix(matrix).unwrap()),
        };
        let source = varied(6, 5);
        let registry = ModuleRegistry::builtin();
        let raster = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            settings,
        )
        .unwrap();
        for y in 0..5 {
            for x in 0..6 {
                let pixel = source.pixel(x, y).unwrap().map(f64::from);
                let expected = apply(matrix, pixel).map(reference_srgb);
                assert_eq!(
                    raster.pixel(x, y),
                    Some([expected[0], expected[1], expected[2], 255]),
                    "({x}, {y})"
                );
                // The point sampler takes the same path, so the readout of an approximate stack
                // would agree with its frame — though the host never asks it to.
                assert_eq!(
                    sample_linear(&registry, &source, &Recipe::default(), settings, x, y)
                        .unwrap()
                        .rgba,
                    raster.pixel(x, y)
                );
            }
        }
    }

    /// `W = R · diag(g'/g) · R⁻¹`: a camera-RGB pixel `c` developed at `g` is `R · c`, and the one
    /// developed at `g'` is, to first order, `R · diag(g'/g) · c`, which `W` must reach from the
    /// first. Equal gains are the identity to rounding.
    #[test]
    fn the_matrix_maps_one_development_onto_the_other_in_camera_space() {
        let developed = [2.1_f32, 1.0, 1.45];
        let target = [1.52_f32, 1.0, 2.37];
        let balance = WhiteBalanceApproximation::between(CAMERA, developed, target).unwrap();
        let ratio: [f64; 3] =
            std::array::from_fn(|c| f64::from(target[c]) / f64::from(developed[c]));
        for camera in [
            [0.2, 0.4, 0.1],
            [1.3, 0.05, 0.9],
            [0.0, 0.0, 1.0],
            [-0.02, 0.7, 0.33],
        ] {
            let at_developed = apply(CAMERA, camera);
            let at_target = apply(CAMERA, std::array::from_fn(|c| camera[c] * ratio[c]));
            let approximated = balance.apply(at_developed);
            for channel in 0..3 {
                assert!(
                    (approximated[channel] - at_target[channel]).abs() < 1.0e-12,
                    "{camera:?} channel {channel}: {approximated:?} against {at_target:?}"
                );
            }
        }
        let same = WhiteBalanceApproximation::between(CAMERA, developed, developed).unwrap();
        for (row, values) in same.matrix().iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                let identity = if row == column { 1.0 } else { 0.0 };
                assert!(
                    (value - identity).abs() < 1.0e-12,
                    "{row},{column}: {value}"
                );
            }
        }
    }

    /// No approximation exists for a singular or non-finite camera matrix, for a gain that is not
    /// finite and positive, or for a non-finite matrix given directly; each is refused rather than
    /// rendering a frame the matrix cannot describe.
    #[test]
    fn a_singular_matrix_or_unusable_gain_has_no_approximation() {
        let gains = [2.0_f32, 1.0, 1.5];
        let rank_two = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.5, -1.0, 0.25]];
        let error = WhiteBalanceApproximation::between(rank_two, gains, [1.0, 1.0, 1.0])
            .expect_err("a rank-two camera matrix has no inverse");
        assert_eq!(error.kind, ErrorKind::UnsupportedColor);
        // Nearly singular: a determinant of 1e-15 against unit rows.
        let nearly = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 1.0e-15]];
        assert!(WhiteBalanceApproximation::between(nearly, gains, [1.0, 1.0, 1.0]).is_err());
        let mut infinite = CAMERA;
        infinite[1][2] = f64::INFINITY;
        assert!(WhiteBalanceApproximation::between(infinite, gains, [1.0, 1.0, 1.0]).is_err());
        for bad in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            assert!(WhiteBalanceApproximation::between(CAMERA, [bad, 1.0, 1.0], gains).is_err());
            assert!(WhiteBalanceApproximation::between(CAMERA, gains, [1.0, 1.0, bad]).is_err());
        }
        let mut matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        matrix[2][0] = f64::NAN;
        assert!(WhiteBalanceApproximation::from_matrix(matrix).is_err());
    }

    #[test]
    fn malformed_sources_views_and_multiple_resamples_fail_closed() {
        assert!(LinearImage::new(2, 2, vec![0.0; 11]).is_err());
        assert!(LinearImage::new(2, 2, vec![f32::NAN; 12]).is_err());
        assert!(LinearImage::new(0, 1, Vec::<f32>::new()).is_err());
        assert!(LinearImage::new(16_385, 1, Vec::<f32>::new()).is_err());
        assert!(output_len(16_000, 8_001).is_err());
        let source = image(2, 2, &[[0.0, 0.0, 0.0]; 4]);
        assert!(source.with_view([1, 1, 2, 2], 1).is_err());
        assert!(source.with_view([u32::MAX, 0, 2, 1], 1).is_err());
        assert!(source.with_view([0, 0, 2, 2], 9).is_err());
        assert!(Linear::blend(1.0, 1.0, 2, 2, |_x, _y| Ok([f64::INFINITY; 3])).is_err());
    }

    /// A synthetic linear source with a varying value in all three channels, filled
    /// programmatically so no file is read.
    fn cancellation_image(width: u32, height: u32) -> LinearImage {
        let pixels = (width * height) as usize;
        let mut planes = Vec::with_capacity(pixels * 3);
        for channel in 0..3 {
            planes.extend((0..pixels).map(|index| ((index * (channel + 1)) % 997) as f32 / 997.0));
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:linear-cancellation").unwrap()
    }

    fn cancellation_recipe() -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: serde_json::json!({"exposure": 0.5, "contrast": 20.0, "vibrance": 30.0}),
                mask: None,
                artifacts: Vec::new(),
            }],
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    fn presence_stack(payload: serde_json::Value) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::PRESENCE_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload,
                artifacts: Vec::new(),
                mask: None,
            }],
            masks: Vec::new(),
            strokes: Default::default(),
            artifacts: Default::default(),
        }
    }

    #[test]
    fn a_point_evaluation_builds_no_frame_for_its_spatial_segment() {
        let context = RenderContext::new();
        let source = cancellation_image(96, 64);
        let registry = ModuleRegistry::builtin();
        let stack = presence_stack(serde_json::json!({"clarity": 40.0}));
        let evaluate = |mode| {
            linear_evaluation(
                &context,
                &registry,
                &source,
                &stack,
                LinearSettings::default(),
                Tiling::Halo,
                mode,
            )
            .unwrap()
        };
        let frames = evaluate(SpatialMode::Frames);
        assert!(frames.tiles.is_none());
        assert_eq!(
            frames.built.len(),
            1,
            "a render materializes the spatial output"
        );
        assert!(frames.frame.is_some());
        let point = evaluate(SpatialMode::Point);
        assert!(
            point.built.is_empty() && point.frame.is_none(),
            "a point evaluation materializes nothing"
        );
        for (x, y) in [(5, 7), (90, 60), (5, 7)] {
            assert_eq!(point.pixel(x, y).unwrap(), frames.pixel(x, y).unwrap());
        }
        assert_eq!(
            point.tiles.as_ref().unwrap().evaluated().len(),
            1,
            "one tile answers every pixel inside it"
        );
    }

    /// A global Presence layer, a colour layer and three masked Presence layers: four spatial
    /// segments, each a whole float frame on this path.
    fn four_spatial_segments() -> Recipe {
        let mask = |name: &str, x0: f64, x1: f64| {
            let mut mask = crate::Mask::new(name);
            let component = mask.next_component_name("linear");
            mask.components.push(crate::Component::new(
                component,
                crate::ComponentMode::Add,
                "linear",
                serde_json::json!({"x0": x0, "y0": 0.5, "x1": x1, "y1": 0.5}),
            ));
            mask
        };
        let masks = vec![
            mask("Mask 1", 0.2, 0.6),
            mask("Mask 2", 0.9, 0.3),
            mask("Mask 3", -0.2, 0.4),
        ];
        let layer = |effect: &str, payload, mask: Option<&crate::Mask>| Layer {
            id: crate::LayerId::new(),
            effect_id: effect.into(),
            effect_format: crate::EFFECT_FORMAT,
            payload,
            mask: mask.map(|mask| mask.id.clone()),
            artifacts: Vec::new(),
        };
        let mut layers = vec![
            layer(
                crate::PRESENCE_EFFECT,
                serde_json::json!({"clarity": 40.0, "dehaze": 20.0}),
                None,
            ),
            layer(
                crate::BASIC_EFFECT,
                serde_json::json!({"exposure": 0.3}),
                None,
            ),
        ];
        for (mask, payload) in masks.iter().zip([
            serde_json::json!({"texture": 35.0}),
            serde_json::json!({"clarity": -30.0}),
            serde_json::json!({"dehaze": 25.0}),
        ]) {
            layers.push(layer(crate::PRESENCE_EFFECT, payload, Some(mask)));
        }
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks,
            ..Recipe::default()
        }
    }

    /// Each spatial frame is built from the one before it and replaces it, so building one holds
    /// two and the evaluation keeps one, the latest; a point evaluation builds none. The counts are
    /// the frames' own reference counts, not bookkeeping: an earlier frame anything still held would
    /// be counted alive.
    #[test]
    fn a_linear_evaluation_keeps_at_most_two_spatial_frames() {
        let context = RenderContext::new();
        let source = cancellation_image(96, 64);
        let registry = ModuleRegistry::builtin();
        let stack = four_spatial_segments();
        let evaluate = |mode| {
            linear_evaluation(
                &context,
                &registry,
                &source,
                &stack,
                LinearSettings::default(),
                Tiling::Halo,
                mode,
            )
            .unwrap()
        };
        let alive = |evaluation: &Evaluation<'_, Linear<'_>>| -> Vec<bool> {
            evaluation
                .built
                .iter()
                .map(|(frame, _)| frame.strong_count() > 0)
                .collect()
        };
        let peaks = |evaluation: &Evaluation<'_, Linear<'_>>| -> Vec<usize> {
            evaluation.built.iter().map(|(_, peak)| *peak).collect()
        };

        let frames = evaluate(SpatialMode::Frames);
        let spatial: Vec<usize> = frames
            .compiled
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                segment
                    .entry
                    .as_ref()
                    .is_some_and(|entry| entry.point_tiles().is_some())
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(spatial.len(), 4, "four spatial segments");
        assert_eq!(
            peaks(&frames),
            [1, 2, 2, 2],
            "each frame is finished beside the one it reads and no other"
        );
        assert_eq!(alive(&frames), [false, false, false, true]);
        assert_eq!(
            frames.frame.as_ref().map(|frame| frame.index),
            Some(spatial[3])
        );

        // Point mode materializes no spatial segment: every one is answered from the query's tiles.
        let point = evaluate(SpatialMode::Point);
        assert!(point.built.is_empty() && point.frame.is_none());
        for (x, y) in [(0, 0), (5, 7), (48, 32), (90, 60), (95, 63)] {
            assert_eq!(point.pixel(x, y).unwrap(), frames.pixel(x, y).unwrap());
        }

        let built: Vec<_> = [&frames, &point]
            .iter()
            .flat_map(|evaluation| evaluation.built.iter().map(|(frame, _)| frame.clone()))
            .collect();
        drop((frames, point));
        assert!(
            built.iter().all(|frame| frame.strong_count() == 0),
            "nothing outlives its evaluation"
        );
    }

    /// A sample from one `Compiled` shared by several points equals a sample that compiles for
    /// itself, at every point of a small stack with a colour layer: the split
    /// A capability sample grid over a RAW stage with spatial layers answers its points through one
    /// tile cache, as the byte path's does: it materializes no spatial frame where the render of
    /// the same stack builds one per spatial layer, and every point is the rendered byte there.
    #[test]
    fn a_linear_grid_reads_tiles_and_materializes_no_spatial_frame() {
        let registry = ModuleRegistry::builtin();
        let source = cancellation_image(300, 200);
        let stack = four_spatial_segments();
        let context = RenderContext::new();
        let render = crate::render::render(
            &registry,
            crate::RenderSource::Linear {
                image: &source,
                settings: LinearSettings::default(),
            },
            &stack,
            RenderOptions::default(),
            &context,
        )
        .unwrap();
        let grid = render.grid(8, &|| Ok(())).unwrap();
        assert_eq!(context.spatial_frames(), 0, "a grid materializes no frame");
        let frame = render.frame(SnapshotId::new()).unwrap();
        assert_eq!(context.spatial_frames(), 4, "a render builds one per layer");
        for ((x, y), sampled) in
            super::super::entry::grid_centres(8, frame.width, frame.height).zip(grid)
        {
            assert_eq!(frame.pixel(x, y), Some(sampled), "grid point ({x}, {y})");
        }
    }

    /// `HostStage::sample_before`'s RAW path takes to compile a prefix once and reuse it across the
    /// points it samples reads the same values as compiling fresh for each point.
    #[test]
    fn sample_linear_compiled_from_a_shared_prefix_matches_sample_linear_per_point() {
        let registry = ModuleRegistry::builtin();
        let source = image(
            3,
            3,
            &[
                [0.1, 0.2, 0.3],
                [0.4, 0.5, 0.6],
                [0.7, 0.8, 0.9],
                [0.05, 0.15, 0.25],
                [0.35, 0.45, 0.55],
                [0.65, 0.75, 0.85],
                [0.02, 0.12, 0.22],
                [0.32, 0.42, 0.52],
                [0.62, 0.72, 0.82],
            ],
        );
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: serde_json::json!({"exposure": 0.4, "contrast": 8.0, "vibrance": -15.0}),
                mask: None,
                artifacts: Vec::new(),
            }],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let settings = LinearSettings {
            white_balance: None,
        };
        // Compiled once, as `HostStage::compiled_prefix` compiles a prefix once and clones it for
        // every point sampled from it, instead of every call in this loop compiling its own.
        let compiled = registry
            .compile_layers(
                3,
                3,
                &recipe.layers,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )
            .unwrap();
        for y in 0..3 {
            for x in 0..3 {
                let expected = sample_linear(&registry, &source, &recipe, settings, x, y).unwrap();
                let context = RenderContext::new();
                let actual = crate::render::Render::compiled(
                    crate::RenderSource::Linear {
                        image: &source,
                        settings,
                    },
                    compiled.clone(),
                    crate::RenderOptions::default(),
                    &context,
                )
                .unwrap()
                .sample(x, y)
                .unwrap();
                assert_eq!(actual.rgba, expected.rgba, "({x}, {y})");
                assert_eq!(
                    (actual.width, actual.height),
                    (expected.width, expected.height),
                    "({x}, {y})"
                );
            }
        }
    }

    /// On a real RAW file, through Presence: a point sample equals the byte `render_linear` writes
    /// there, at points spread over the stage and the far corner, in the production tiling and in
    /// 512 px tiles whatever the halo, and the timings of the tile and of the frame a whole-stage
    /// evaluation builds in each are printed. Run in release with
    /// LUXFORGE_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG:
    ///
    /// ```text
    /// LUXFORGE_RAW_FIXTURE=/path/to/file.NEF cargo test --release -p luxforge-core --lib \
    ///   a_raw_point_sample_through_presence -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_raw_point_sample_through_presence_equals_the_render() {
        use std::time::Instant;
        let path =
            std::path::PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("fixture path"));
        let bytes = std::fs::read(&path).unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let prepared =
            crate::source::RawPrepared::decode(bytes, "sha256:point-sample".into(), None, &cancel)
                .unwrap();
        let image = prepared.linear.clone().unwrap();
        let registry = ModuleRegistry::builtin();
        let settings = LinearSettings::default();
        let ms = |start: Instant| start.elapsed().as_secs_f64() * 1e3;
        println!("{}: {}x{}", path.display(), image.width(), image.height());
        for payload in [
            serde_json::json!({"clarity": 60.0}),
            serde_json::json!({"clarity": 60.0, "dehaze": 30.0}),
        ] {
            let stack = presence_stack(payload.clone());
            let context = RenderContext::new();
            let raster = frame_in(
                &context,
                &registry,
                linear(&image, settings),
                SnapshotId::new(),
                &stack,
                RenderOptions::default(),
            )
            .unwrap();
            let mut state = 0x2545_f491_4f6c_dd1d_u64;
            let mut points: Vec<(u32, u32)> = (0..40)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (
                        (state % u64::from(raster.width)) as u32,
                        ((state >> 32) % u64::from(raster.height)) as u32,
                    )
                })
                .collect();
            points.push((raster.width - 1, raster.height - 1));
            // The production tiling, and 512 px tiles whatever the halo, whose samples must equal
            // the same rendered bytes.
            for (tiling, options) in [
                (Tiling::Halo, RenderOptions::default()),
                (
                    Tiling::Fixed(SPATIAL_TILE),
                    RenderOptions::default().with_tile(SPATIAL_TILE),
                ),
            ] {
                let mut samples = Vec::new();
                for &(x, y) in &points {
                    let start = Instant::now();
                    let sampled = sample_in(
                        &context,
                        &registry,
                        linear(&image, settings),
                        &stack,
                        options.clone(),
                        x,
                        y,
                    )
                    .unwrap();
                    samples.push(ms(start));
                    assert_eq!(
                        sampled.rgba,
                        raster.pixel(x, y),
                        "{payload}, {tiling:?}, at ({x}, {y})"
                    );
                }
                let mut frames = Vec::new();
                for _ in 0..3 {
                    let start = Instant::now();
                    linear_evaluation(
                        &context,
                        &registry,
                        &image,
                        &stack,
                        settings,
                        tiling,
                        SpatialMode::Frames,
                    )
                    .unwrap();
                    frames.push(ms(start));
                }
                let samples = Distribution::of(samples).expect("points were sampled");
                let frames = Distribution::of(frames).expect("frames were evaluated");
                println!(
                    "{payload}, {tiling:?}: {} point samples equal the render; sample p50 {:.1} \
                     ms, max {:.1} ms; the spatial frame alone p50 {:.0} ms",
                    points.len(),
                    samples.p50,
                    samples.max,
                    frames.p50
                );
            }
        }
    }
}
