//! High precision scene-linear rendering for prepared RAW sources.
//!
//! This module is deliberately independent of a RAW decoder. A decoder or source-preparation
//! worker supplies immutable planar RGB values in unbounded linear sRGB/D65. The recipe is then
//! evaluated in f64 and converted to the existing byte [`Raster`] only at the terminal boundary.
//! [`Linear`] is this path's [`PixelDomain`]: the one pipeline in [`super::pipeline`] evaluates it
//! exactly as it evaluates a JPEG's bytes, and only what a linear pixel is lives here.

use super::{
    ColorRun, Compiled, Entry, Evaluation, MaskedInput, PixelDomain, Raster, RenderContext,
    ResampleEntry, RowScratch, Segment, SegmentRows, Taps, apply_units, color_runs,
    pipeline::PlaneRow, segment_pass, spatial,
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Cancel, Error, LinearImage, SnapshotId,
    colour::{mat3, srgb},
    modules::{ExactGeometry, Parallelism, Region, Stage},
    source::{ViewReader, Walk, layout},
};
use rayon::prelude::*;
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

    /// The matrix applied to each linear-sRGB pixel, row by row: what a GPU plan of a drafted RAW
    /// preview applies to each source texel, narrowed to `f32`.
    pub(crate) fn matrix(&self) -> [[f64; 3]; 3] {
        self.matrix
    }

    #[inline]
    fn apply(&self, pixel: [f64; 3]) -> [f64; 3] {
        mat3::matvec_f64(&self.matrix, pixel)
    }

    /// `W · p` for one source pixel, refused when a product is not finite: the developed planes
    /// are finite, but a finite matrix may still overflow. The one adjustment a source pixel
    /// takes, on the point path and on every row, with the same `f64` arithmetic and failure.
    #[inline(always)]
    fn adjust(&self, pixel: [f64; 3]) -> Result<[f64; 3], Error> {
        let output = self.apply(pixel);
        if output.iter().all(|value| value.is_finite()) {
            Ok(output)
        } else {
            Err(Error::render("linear source produced a non-finite value"))
        }
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

/// What the terminal boundary refuses a non-finite value with.
const NON_FINITE_TERMINAL: &str = "linear evaluation produced a non-finite value";

/// The terminal boundary for one channel: a finite value's forward rounding,
/// `round(255 · encode(v))`, through the guarded threshold search of [`srgb::Quantizer::rounded`]
/// (the byte resample's contract as well), and a render error for a non-finite one.
#[inline]
fn terminal_srgb(quantizer: &srgb::Quantizer, linear: f64) -> Result<u8, Error> {
    if !linear.is_finite() {
        return Err(Error::render(NON_FINITE_TERMINAL));
    }
    Ok(quantizer.rounded(linear))
}

/// [`terminal_pixel_in`] for a pixel whose values are `f32` — a colour segment's rows, and the
/// planes and spatial frames a segment without colour or white balance copies — without widening
/// it to `f64`: the same refusal of a non-finite value, then [`srgb::Quantizer::pixel`]. For an
/// `f32` the guard band [`srgb::Quantizer::rounded`] keeps around each threshold holds only the
/// threshold's own `f32` and the one below it, where the two quantizers give the same code, so
/// every byte is the one [`terminal_pixel_in`] writes (the `colour::srgb` tests prove it).
#[inline]
fn terminal_f32(quantizer: &srgb::Quantizer, pixel: [f32; 3]) -> Result<[u8; 4], Error> {
    if !pixel.iter().all(|value| value.is_finite()) {
        return Err(Error::render(NON_FINITE_TERMINAL));
    }
    let [red, green, blue] = quantizer.pixel(pixel);
    Ok([red, green, blue, 255])
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

/// The estimate prefix of a development `development` seen through `view` under an approximate
/// `white_balance`: what the linear domain's estimates are keyed by.
fn estimate_prefix(
    prefix_hash: &str,
    development: u64,
    view: ([u32; 4], u8),
    white_balance: Option<WhiteBalanceApproximation>,
) -> String {
    let input_prefix = format!("{prefix_hash}+linear:{development}:{view:?}");
    match white_balance {
        Some(balance) => format!(
            "{input_prefix}+white-balance-approximation:{}",
            balance.key()
        ),
        None => input_prefix,
    }
}

/// The linear domain: a developed RAW's planes in signed unbounded linear sRGB, with any approximate
/// white balance applied to each source pixel in `f64`. A segment with colour is `f32` from its
/// entry to its end; one without hands its entry to the terminal as it is, `f32` read from the
/// planes or a spatial frame and `f64` from a blend, a white-balance product or a replacement.
/// Nothing is quantized before the terminal boundary, so a replacement is decoded rather than
/// quantized at, a resample blends in `f64`, and a spatial operation's `f32` output is read back
/// exactly. There is no alpha.
#[derive(Clone, Copy)]
pub(crate) struct Linear<'a> {
    source: &'a LinearImage,
    /// The source's view resolved once, so a point read does not recompute its layout per pixel
    /// and a row walks it ([`Linear::entry_planes`]).
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

    /// The one point where the settings touch a source pixel on the point path: the pixel itself,
    /// or `W · p` under an approximate white balance ([`WhiteBalanceApproximation::adjust`], which
    /// the rows apply to each pixel they read too).
    #[inline(always)]
    fn adjust_source_pixel(&self, pixel: [f64; 3]) -> Result<[f64; 3], Error> {
        match &self.white_balance {
            // The developed planes exactly, as every exact evaluation reads them.
            None => Ok(pixel),
            Some(balance) => balance.adjust(pixel),
        }
    }

    /// The planes segment `index` of `evaluation` reads its entry from by rows: the source's, for
    /// the first segment, under the evaluation's white balance, or the spatial frame the evaluation
    /// holds for the segment's spatial entry. `None` for a resample, whose taps blend the segment
    /// before it, and for a spatial entry whose frame the evaluation does not hold: those are
    /// pulled one pixel at a time ([`Evaluation::entry_pixel`]).
    pub(super) fn entry_planes<'e>(
        evaluation: &'e Evaluation<'_, Self>,
        index: usize,
    ) -> Option<EntryPlanes<'e>> {
        match &evaluation.compiled.segments[index].entry {
            None => Some(EntryPlanes {
                reader: evaluation.domain.reader,
                white_balance: evaluation.domain.white_balance,
            }),
            Some(Entry::Spatial(_)) => {
                let frame = evaluation
                    .frame
                    .as_ref()
                    .filter(|frame| frame.index == index)?;
                let stage = evaluation.compiled.segments[index - 1].stage();
                Some(EntryPlanes {
                    reader: ViewReader::over(&frame.planes, stage.width, stage.height),
                    white_balance: None,
                })
            }
            Some(Entry::Resample(_)) => None,
        }
    }

    /// Every colour run of `runs` over one `f32` row, then the refusal of a non-finite value that
    /// [`PixelDomain::finish`] makes of each pixel: what [`PixelDomain::colour_row`] does once a
    /// row is `f32`, and what a row read from planes does without widening it first.
    fn colour_f32<'r>(
        row: &mut [[f32; 3]],
        runs: impl Iterator<Item = ColorRun<'r>>,
        y: u32,
        x0: u32,
        snapshot: &mut Vec<MaskedInput>,
    ) -> Result<(), Error> {
        snapshot.resize(row.len().max(1), MaskedInput::default());
        for run in runs {
            apply_units(&run, y, x0, row, snapshot)?;
        }
        finite_f32(row, NON_FINITE_PIXEL)
    }
}

/// What [`Linear::finish`] refuses a non-finite pixel with.
const NON_FINITE_PIXEL: &str = "linear evaluation produced a non-finite pixel";

/// `Ok` when every value of `row` is finite, else a render error with `detail`. An `f32` is
/// finite exactly when its `f64` widening is, so this is the check [`Linear::finish`] makes of the
/// same values widened.
#[inline]
fn finite_f32(row: &[[f32; 3]], detail: &str) -> Result<(), Error> {
    // The conjunction over every value, without the short circuit `all` would take, so it
    // vectorises; the answer is the same.
    let finite = row
        .as_flattened()
        .iter()
        .fold(true, |finite, value| finite & value.is_finite());
    if finite {
        Ok(())
    } else {
        Err(Error::render(detail))
    }
}

/// The planes a linear segment's entry is read from by rows ([`Linear::entry_planes`]): the
/// source through its view, or a spatial frame. Both hold `f32`, so a rectangle of a segment's
/// output is walked through its exact geometry and the view at once ([`ViewReader::walk`]) and
/// its values copied, rather than each pulled through [`Evaluation::entry_pixel`] with the
/// geometry's unmap, the view's orientation and a widening to `f64` per pixel.
#[derive(Clone, Copy)]
pub(super) struct EntryPlanes<'a> {
    reader: ViewReader<'a>,
    /// Applied to each source pixel read, in `f64`, when the settings carry one; a frame's
    /// values are read as they are.
    white_balance: Option<WhiteBalanceApproximation>,
}

impl EntryPlanes<'_> {
    /// The pixels `block` of a segment's output reads through its exact `geometry`, or `None`
    /// when it is empty or would read outside the planes, which the per-pixel pull reports.
    fn walk(&self, geometry: ExactGeometry, block: Region) -> Option<Walk> {
        if block.is_empty() {
            return None;
        }
        let (across, down) = geometry.unmap_steps();
        let at = geometry.unmap(block.x0, block.y0);
        self.reader
            .walk(at, across, down, block.width, block.height)
    }

    /// The values of `walk`, row-major into `row`, as a colour run reads them: the planes' own
    /// `f32` values, or each source pixel's [`WhiteBalanceApproximation::adjust`] narrowed to
    /// `f32`, exactly as [`Linear::colour`] narrows the pixel it is handed.
    fn read_f32(&self, walk: Walk, row: &mut Vec<[f32; 3]>) -> Result<(), Error> {
        row.clear();
        match &self.white_balance {
            None => self.reader.visit(walk, |_, rgb| {
                row.push(rgb);
                Ok(())
            }),
            Some(balance) => self.reader.visit(walk, |_, rgb| {
                row.push(
                    balance
                        .adjust(rgb.map(f64::from))?
                        .map(|value| value as f32),
                );
                Ok(())
            }),
        }
    }

    /// Each value of `walk`, with its row-major offset, as [`Evaluation::entry_pixel`] answers it
    /// there: the plane's `f32` widened, or the source pixel's
    /// [`WhiteBalanceApproximation::adjust`].
    fn visit_f64(
        &self,
        walk: Walk,
        mut visit: impl FnMut(usize, [f64; 3]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match &self.white_balance {
            None => self
                .reader
                .visit(walk, |offset, rgb| visit(offset, rgb.map(f64::from))),
            Some(balance) => self.reader.visit(walk, |offset, rgb| {
                visit(offset, balance.adjust(rgb.map(f64::from))?)
            }),
        }
    }
}

impl PixelDomain for Linear<'_> {
    type Pixel = [f64; 3];
    /// Three `f32` planes inside the RAW planar limit.
    type SpatialFrame = Vec<f32>;
    /// The tile's own three planes.
    type TileOutput = Vec<f32>;

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
        Cow::Owned(estimate_prefix(
            prefix_hash,
            self.source.development(),
            self.source.view(),
            self.white_balance,
        ))
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
        _: bool,
    ) -> Result<[f64; 3], Error> {
        let mut linear = [pixel.map(|value| value as f32)];
        // One pixel of snapshot scratch on the stack: a masked operation blends against its own
        // input and decides its coverage from it, and a point pulls single pixels, so nothing is
        // allocated per pixel.
        let mut scratch = [MaskedInput::default(); 1];
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
        _: bool,
    ) -> Result<(), Error> {
        let RowScratch { linear, snapshot } = scratch;
        linear.clear();
        linear.extend(pixels.iter().map(|pixel| pixel.map(|value| value as f32)));
        Self::colour_f32(linear, runs, y, x0, snapshot)?;
        for (pixel, value) in pixels.iter_mut().zip(linear.iter()) {
            *pixel = value.map(f64::from);
        }
        Ok(())
    }

    #[inline(always)]
    fn finish(pixel: [f64; 3]) -> Result<[f64; 3], Error> {
        if pixel.iter().all(|value| value.is_finite()) {
            Ok(pixel)
        } else {
            Err(Error::render(NON_FINITE_PIXEL))
        }
    }

    /// A segment without replacements whose entry is planes ([`Linear::entry_planes`]) reads each
    /// row of `region` by walking them: a colour segment's row as `f32` straight into the colour
    /// scratch, through its runs and the finite check, then widened once; one without colour each
    /// value as its entry pixel and [`Self::finish`]. Row by row, so each row's entry, colour and
    /// check run in the order the pull runs them and the first error is the pull's.
    fn region_rows(
        evaluation: &Evaluation<'_, Self>,
        index: usize,
        region: Region,
        out: &mut Vec<[f64; 3]>,
        scratch: &mut RowScratch,
    ) -> Option<Result<(), Error>> {
        let segment = &evaluation.compiled.segments[index];
        if segment.has_pixels {
            return None;
        }
        let planes = Self::entry_planes(evaluation, index)?;
        let walk = planes.walk(segment.geometry, region)?;
        Some((0..walk.rows()).try_for_each(|row| {
            let line = walk.row(row);
            if segment.has_color {
                let RowScratch { linear, snapshot } = &mut *scratch;
                planes.read_f32(line, linear)?;
                let y = region.y0 + row as u32;
                Self::colour_f32(linear, color_runs(segment), y, region.x0, snapshot)?;
                out.extend(linear.iter().map(|value| value.map(f64::from)));
                Ok(())
            } else {
                planes.visit_f64(line, |_, pixel| {
                    out.push(Self::finish(pixel)?);
                    Ok(())
                })
            }
        }))
    }

    /// A segment without replacements whose entry is planes ([`Linear::entry_planes`]) fills each
    /// row of the planes a spatial operation reads by walking them, in `f32` from the planes to
    /// the spatial input: a colour segment's row through its runs and the finite check, one
    /// without colour each value checked as [`Self::finish`] checks it and narrowed as
    /// [`Self::spatial_input`] narrows it. Row by row, on the pool under [`Parallelism::Pool`].
    fn fill_rows(
        evaluation: &Evaluation<'_, Self>,
        index: usize,
        region: Region,
        planes: &mut [f32],
        parallelism: Parallelism,
    ) -> Option<Result<(), Error>> {
        let segment = &evaluation.compiled.segments[index];
        if segment.has_pixels {
            return None;
        }
        let entry = Self::entry_planes(evaluation, index)?;
        let walk = entry.walk(segment.geometry, region)?;
        let len = region.pixels() as usize;
        let width = region.width as usize;
        let (red, rest) = planes[..3 * len].split_at_mut(len);
        let (green, blue) = rest.split_at_mut(len);
        let row = |scratch: &mut RowScratch,
                   (row, ((red, green), blue)): PlaneRow<'_>|
         -> Result<(), Error> {
            let line = walk.row(row);
            let mut put = |column: usize, [r, g, b]: [f32; 3]| {
                red[column] = r;
                green[column] = g;
                blue[column] = b;
            };
            if segment.has_color {
                let RowScratch { linear, snapshot } = scratch;
                entry.read_f32(line, linear)?;
                let y = region.y0 + row as u32;
                Self::colour_f32(linear, color_runs(segment), y, region.x0, snapshot)?;
                for (column, rgb) in linear.iter().enumerate() {
                    put(column, *rgb);
                }
                Ok(())
            } else if entry.white_balance.is_none() {
                entry.reader.visit(line, |column, rgb| {
                    finite_f32(&[rgb], NON_FINITE_PIXEL)?;
                    put(column, rgb);
                    Ok(())
                })
            } else {
                entry.visit_f64(line, |column, pixel| {
                    put(column, Self::spatial_input(Self::finish(pixel)?));
                    Ok(())
                })
            }
        };
        Some(match parallelism {
            Parallelism::Pool => red
                .par_chunks_mut(width)
                .zip(green.par_chunks_mut(width))
                .zip(blue.par_chunks_mut(width))
                .enumerate()
                .try_for_each_init(RowScratch::default, row),
            Parallelism::Serial => {
                let mut scratch = RowScratch::default();
                red.chunks_mut(width)
                    .zip(green.chunks_mut(width))
                    .zip(blue.chunks_mut(width))
                    .enumerate()
                    .try_for_each(|item| row(&mut scratch, item))
            }
        })
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
    fn spatial_output(rgb: [f32; 3], _: bool) -> Result<[f64; 3], Error> {
        Ok(rgb.map(f64::from))
    }

    #[inline]
    fn terminal(pixel: [f64; 3]) -> Result<[u8; 4], Error> {
        terminal_pixel(pixel)
    }

    /// The RAW planar limit applies to this float frame exactly as it does to the source's.
    fn spatial_frame(stage: Stage, _: bool) -> Result<Vec<f32>, Error> {
        let (values, _) = layout(stage.width, stage.height)?;
        Ok(vec![0.0_f32; values])
    }

    /// The tile's own three planes, cut out of the last unit's rectangle in the slot the tile ran
    /// in, which its next tile overwrites: one row per plane appended, in the parallel phase,
    /// without zero-filling first ([`spatial::cut_out`]). Only the tile is copied, never the halo
    /// an edge tile's rectangle keeps.
    fn tile_output(
        region: Region,
        values: &[f32],
        tile: Region,
        _: Parallelism,
        _: bool,
    ) -> Vec<f32> {
        spatial::cut_out(region, values, tile)
    }

    /// Each of the tile's rows, one `copy_from_slice` per plane, from the tile's own planes.
    fn write_tile(frame: &mut Vec<f32>, stage: Stage, tile: Region, values: Vec<f32>) {
        let plane = (u64::from(stage.width) * u64::from(stage.height)) as usize;
        let source = tile.pixels() as usize;
        let width = tile.width as usize;
        for (row, y) in (tile.y0..tile.y1()).enumerate() {
            let to = (u64::from(y) * u64::from(stage.width) + u64::from(tile.x0)) as usize;
            let from = row * width;
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
/// [`segment_pass`] whose rows read their entry through the segment's exact geometry: the source's
/// planes or the evaluation's spatial frame by rows ([`LinearRows::load_pulled`]), or a resample
/// of the segment before. What lies before a resample is pulled, never materialized, because it is
/// `f64`: its taps are read one block of output pixels at a time, through the rectangle of the
/// segment before them that the block reads ([`LinearRows::load_resampled`]).
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
    segment_pass(
        &LinearRows {
            evaluation,
            index,
            segment,
            planes: Linear::entry_planes(evaluation, index),
            output: LinearOutput::Terminal(srgb::quantizer()),
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
    /// The planes the segment's entry is read from by rows, when it is the source or a spatial
    /// frame the evaluation holds ([`Linear::entry_planes`]).
    planes: Option<EntryPlanes<'e>>,
    /// What each finished pixel is written as.
    output: LinearOutput,
    #[cfg(test)]
    context: &'e RenderContext,
}

/// What a pass of [`LinearRows`] writes: terminal bytes through the output quantizer, taken once
/// for the pass rather than once per channel, or the `f32` texels a GPU preview's boundary holds
/// ([`super::boundary`]), unquantized.
#[derive(Clone, Copy)]
pub(super) enum LinearOutput {
    Terminal(&'static srgb::Quantizer),
    #[cfg(any(test, feature = "qualification"))]
    Boundary,
}

impl LinearOutput {
    /// Bytes per pixel of the frame the pass writes.
    fn bytes(self) -> usize {
        match self {
            Self::Terminal(_) => 4,
            #[cfg(any(test, feature = "qualification"))]
            Self::Boundary => super::boundary::BoundaryFormat::Float.texel_bytes(),
        }
    }

    /// Write one finished pixel at the start of `bytes`.
    #[inline]
    fn write(self, bytes: &mut [u8], pixel: [f64; 3]) -> Result<(), Error> {
        match self {
            Self::Terminal(quantizer) => {
                bytes[..4].copy_from_slice(&terminal_pixel_in(quantizer, pixel)?);
            }
            #[cfg(any(test, feature = "qualification"))]
            Self::Boundary => {
                super::boundary::write_texel(
                    super::boundary::BoundaryFormat::Float,
                    bytes,
                    pixel.map(|value| value as f32),
                );
            }
        }
        Ok(())
    }
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
            let bytes = self.output.bytes();
            self.output
                .write(&mut chunk[offset * bytes..(offset + 1) * bytes], pixel)?;
        }
        Ok(())
    }

    /// The rows of a chunk whose entry is the source or a spatial frame: walked through the
    /// segment's exact geometry and the view at once and copied ([`Self::load_walk`]), or, where
    /// the evaluation holds no planes for the entry, pulled one pixel at a time through it
    /// ([`Evaluation::entry_pixel`]), which is the reference the rows are held to.
    pub(super) fn load_pulled(
        &self,
        scratch: &mut LinearScratch,
        y0: u32,
        chunk: &mut [u8],
    ) -> Result<(), Error> {
        let width = self.segment.width as usize;
        let pixel_bytes = self.output.bytes();
        let rows = chunk.len() / (width * pixel_bytes);
        let block = Region {
            x0: 0,
            y0,
            width: self.segment.width,
            height: rows as u32,
        };
        if let Some(planes) = &self.planes
            && let Some(walk) = planes.walk(self.segment.geometry, block)
        {
            return self.load_walk(planes, walk, &mut scratch.rows, chunk);
        }
        let values = &mut scratch.rows;
        for (row, bytes) in chunk.chunks_exact_mut(width * pixel_bytes).enumerate() {
            let y = y0 + row as u32;
            if self.segment.has_color {
                for (x, value) in values[row * width..(row + 1) * width]
                    .iter_mut()
                    .enumerate()
                {
                    *value = self.entry(x as u32, y)?.map(|value| value as f32);
                }
            } else {
                for (x, rgba) in bytes.chunks_exact_mut(pixel_bytes).enumerate() {
                    self.output.write(rgba, self.entry(x as u32, y)?)?;
                }
            }
        }
        Ok(())
    }

    /// A chunk walked from its entry's planes, each value placed at its offset in the chunk.
    /// Without a white balance the values stay `f32` from the planes to the colour rows, the
    /// terminal bytes ([`terminal_f32`]) or the boundary texels; with one, each pixel takes its
    /// `f64` [`WhiteBalanceApproximation::adjust`] and overflow error, as the pull does, before it
    /// is narrowed or written. The test is made once per chunk, not per pixel. The source planes
    /// are finite by construction; a frame's values reach the same checks the pull's do.
    fn load_walk(
        &self,
        planes: &EntryPlanes<'_>,
        walk: Walk,
        values: &mut [[f32; 3]],
        chunk: &mut [u8],
    ) -> Result<(), Error> {
        let bytes = self.output.bytes();
        let reader = &planes.reader;
        match (self.segment.has_color, &planes.white_balance, self.output) {
            (true, None, _) => reader.visit(walk, |offset, rgb| {
                values[offset] = rgb;
                Ok(())
            }),
            (true, Some(balance), _) => reader.visit(walk, |offset, rgb| {
                values[offset] = balance
                    .adjust(rgb.map(f64::from))?
                    .map(|value| value as f32);
                Ok(())
            }),
            (false, None, LinearOutput::Terminal(quantizer)) => {
                reader.visit(walk, |offset, rgb| {
                    chunk[offset * 4..offset * 4 + 4]
                        .copy_from_slice(&terminal_f32(quantizer, rgb)?);
                    Ok(())
                })
            }
            #[cfg(any(test, feature = "qualification"))]
            (false, None, LinearOutput::Boundary) => reader.visit(walk, |offset, rgb| {
                super::boundary::write_texel(
                    super::boundary::BoundaryFormat::Float,
                    &mut chunk[offset * bytes..(offset + 1) * bytes],
                    rgb,
                );
                Ok(())
            }),
            (false, Some(balance), output) => reader.visit(walk, |offset, rgb| {
                output.write(
                    &mut chunk[offset * bytes..(offset + 1) * bytes],
                    balance.adjust(rgb.map(f64::from))?,
                )
            }),
        }
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
        let rows = (chunk.len() / (width as usize * self.output.bytes())) as u32;
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
    type Sample = u8;
    type Scratch = LinearScratch;

    fn samples_per_pixel(&self) -> usize {
        self.output.bytes()
    }

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
            scratch
                .rows
                .resize(chunk.len() / self.output.bytes(), [0.0; 3]);
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
        snapshot: &mut [MaskedInput],
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

    /// A resample entry, whose taps [`Self::load_resampled`] pulls block by block through the
    /// segment before it; a spatial entry is read from its frame.
    fn pulled(&self) -> Option<(&ResampleEntry, &Segment)> {
        match &self.segment.entry {
            Some(Entry::Resample(entry)) => {
                Some((entry, &self.evaluation.compiled.segments[self.index - 1]))
            }
            _ => None,
        }
    }

    fn store(&self, scratch: &mut Self::Scratch, chunk: &mut [u8]) -> Result<(), Error> {
        if self.segment.has_color {
            match self.output {
                LinearOutput::Terminal(quantizer) => {
                    for (rgba, pixel) in chunk.chunks_exact_mut(4).zip(scratch.rows.iter()) {
                        rgba.copy_from_slice(&terminal_f32(quantizer, *pixel)?);
                    }
                }
                #[cfg(any(test, feature = "qualification"))]
                LinearOutput::Boundary => {
                    let format = super::boundary::BoundaryFormat::Float;
                    for (texel, pixel) in chunk
                        .chunks_exact_mut(format.texel_bytes())
                        .zip(scratch.rows.iter())
                    {
                        super::boundary::write_texel(format, texel, *pixel);
                    }
                }
            }
        }
        Ok(())
    }
}

/// One segment's pass into the GPU preview's boundary ([`super::boundary`]): `segment`, which
/// stands in for segment `index` of `evaluation`'s compilation and reads what that segment's entry
/// reads, written as `f32` texels without quantizing, so a boundary inside a colour run holds the
/// value the run hands the next layer, exactly.
#[cfg(any(test, feature = "qualification"))]
pub(super) fn boundary_pass(
    evaluation: &Evaluation<'_, Linear<'_>>,
    index: usize,
    segment: &Segment,
    cancel: &Cancel,
    context: &RenderContext,
) -> Result<Vec<u8>, Error> {
    let mut texels = vec![
        0u8;
        super::boundary::frame_len(
            segment.width,
            segment.height,
            super::boundary::BoundaryFormat::Float,
        )?
    ];
    segment_pass(
        &LinearRows {
            evaluation,
            index,
            segment,
            planes: Linear::entry_planes(evaluation, index),
            output: LinearOutput::Boundary,
            #[cfg(test)]
            context,
        },
        segment,
        &mut texels,
        0..segment.height as usize,
        cancel,
        context.scratch(),
    )?;
    Ok(texels)
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

    /// A `width` × `height` source whose every value is distinct within its plane, spread over
    /// `[-0.1, 1.15]` so the terminal clips at both ends and neighbouring bytes differ.
    fn distinct(width: u32, height: u32) -> LinearImage {
        const PRIME: usize = 10_007;
        let pixels = (width * height) as usize;
        assert!(pixels <= PRIME);
        let planes: Vec<f32> = (0..3)
            .flat_map(|channel| {
                (0..pixels).map(move |index| {
                    let place = (index * 7_919 + channel * 3_331) % PRIME;
                    place as f32 / 8_000.0 - 0.1
                })
            })
            .collect();
        LinearImage::with_fingerprint(width, height, planes, "sha256:linear-distinct").unwrap()
    }

    /// The approximation every white-balanced case below renders under.
    fn balance() -> WhiteBalanceApproximation {
        WhiteBalanceApproximation::from_matrix([
            [1.3, 0.1, -0.05],
            [0.02, 0.97, 0.01],
            [-0.1, 0.05, 0.62],
        ])
        .unwrap()
    }

    /// The exact geometry variants a stack below ends with: none, every orientation, exact crops
    /// touching the top-left edges, the bottom-right edges and none, and crops between turns.
    fn geometry_variants() -> Vec<Vec<Layer>> {
        let crop = |x: f64, y: f64, width: f64, height: f64| {
            Layer::crop(CropPayload {
                angle: 0.0,
                x,
                y,
                width,
                height,
            })
        };
        let orientation =
            |mirror: bool, turns: u8| Layer::orientation(crate::Orientation { mirror, turns });
        let mut variants = vec![Vec::new()];
        for turns in 0..4 {
            for mirror in [false, true] {
                variants.push(vec![orientation(mirror, turns)]);
            }
        }
        variants.push(vec![crop(0.0, 0.0, 0.6, 0.7)]);
        variants.push(vec![crop(0.4, 0.3, 0.6, 0.7)]);
        variants.push(vec![crop(0.15, 0.2, 0.55, 0.45)]);
        variants.push(vec![orientation(false, 1), crop(0.0, 0.25, 0.7, 0.75)]);
        variants.push(vec![crop(0.3, 0.0, 0.7, 0.6), orientation(true, 1)]);
        variants.push(vec![
            orientation(true, 3),
            crop(0.1, 0.1, 0.5, 0.8),
            orientation(false, 2),
        ]);
        variants
    }

    /// Basic's colour units, so a colour segment runs real units over its rows.
    fn basic() -> Layer {
        colour_layer(
            crate::BASIC_EFFECT,
            serde_json::json!({"exposure": 0.4, "contrast": 15.0, "vibrance": 20.0}),
        )
    }

    /// Every chunk of the last segment of `evaluation`, loaded by the rows that walk the entry's
    /// planes and by the rows that pull each pixel through [`Evaluation::entry_pixel`]
    /// (`planes: None`, the per-pixel reference), with and without colour and into terminal bytes
    /// and boundary texels, in chunks of several heights with a partial last chunk: bit for bit
    /// the same. The two loads start from different garbage, so a value one of them leaves
    /// unwritten shows.
    fn rows_load_as_pulled(evaluation: &Evaluation<'_, Linear<'_>>, what: &str) {
        let index = evaluation.compiled.segments.len() - 1;
        let planes = Linear::entry_planes(evaluation, index);
        assert!(planes.is_some(), "{what}: the entry is read by rows");
        let context = RenderContext::new();
        for has_color in [false, true] {
            let mut segment = evaluation.compiled.segments[index].clone();
            segment.has_color = has_color;
            for output in [
                LinearOutput::Terminal(srgb::quantizer()),
                LinearOutput::Boundary,
            ] {
                let rows = |planes| LinearRows {
                    evaluation,
                    index,
                    segment: &segment,
                    planes,
                    output,
                    context: &context,
                };
                let (walked, pulled) = (rows(planes), rows(None));
                let row_bytes = segment.width as usize * output.bytes();
                let standard = crate::render::color_chunk_rows(segment.width);
                for chunk_rows in [standard, 1, 3, 7] {
                    for y0 in (0..segment.height).step_by(chunk_rows) {
                        let count = (chunk_rows as u32).min(segment.height - y0) as usize;
                        let mut scratch = [LinearScratch::default(), LinearScratch::default()];
                        scratch[0].rows =
                            vec![[f32::from_bits(0x7fc0_0001); 3]; count * segment.width as usize];
                        scratch[1].rows =
                            vec![[f32::from_bits(0x7fc0_0002); 3]; count * segment.width as usize];
                        let mut chunks =
                            [vec![0x00; count * row_bytes], vec![0xff; count * row_bytes]];
                        let [walked_scratch, pulled_scratch] = &mut scratch;
                        let [walked_chunk, pulled_chunk] = &mut chunks;
                        walked.load(walked_scratch, y0, walked_chunk).unwrap();
                        pulled.load(pulled_scratch, y0, pulled_chunk).unwrap();
                        let what = format!(
                            "{what}: colour {has_color}, {} output, chunks of {chunk_rows} at row \
                             {y0}",
                            output.bytes()
                        );
                        if has_color {
                            let bits = |rows: &[[f32; 3]]| -> Vec<[u32; 3]> {
                                rows.iter().map(|pixel| pixel.map(f32::to_bits)).collect()
                            };
                            assert!(
                                bits(&walked_scratch.rows) == bits(&pulled_scratch.rows),
                                "{what}"
                            );
                        } else {
                            assert!(walked_chunk == pulled_chunk, "{what}");
                        }
                    }
                }
            }
        }
    }

    /// For every segment of `evaluation` whose entry is read by rows, the rows a neighbourhood
    /// read and a spatial operation's input take from the planes ([`Linear::region_rows`],
    /// [`Linear::fill_rows`]) against the same reads pulled pixel by pixel
    /// ([`Evaluation::region_pulled`], [`Evaluation::fill_pulled`]), over the whole stage, a
    /// corner pixel, edge strips and an interior rectangle, serially and on the pool: bit for bit
    /// the same.
    fn regions_read_as_pulled(evaluation: &Evaluation<'_, Linear<'_>>, what: &str) {
        let mut read = 0;
        for (index, segment) in evaluation.compiled.segments.iter().enumerate() {
            if segment.has_pixels || Linear::entry_planes(evaluation, index).is_none() {
                continue;
            }
            read += 1;
            let (width, height) = (segment.width, segment.height);
            let regions = [
                Region::whole(segment.stage()),
                Region {
                    x0: width - 1,
                    y0: height - 1,
                    width: 1,
                    height: 1,
                },
                Region {
                    x0: 0,
                    y0: 0,
                    width,
                    height: height.min(3),
                },
                Region {
                    x0: width - width.min(2),
                    y0: 0,
                    width: width.min(2),
                    height,
                },
                Region {
                    x0: width / 4,
                    y0: height / 3,
                    width: (width / 2).max(1),
                    height: (height / 3).max(1),
                },
            ];
            for region in regions {
                let what = format!("{what}: segment {index}, {region:?}");
                let (mut walked, mut pulled) = (Vec::new(), Vec::new());
                let mut scratch = RowScratch::default();
                Linear::region_rows(evaluation, index, region, &mut walked, &mut scratch)
                    .expect("read by rows")
                    .unwrap();
                evaluation
                    .region_pulled(index, region, &mut pulled, &mut scratch)
                    .unwrap();
                let bits = |pixels: &[[f64; 3]]| -> Vec<[u64; 3]> {
                    pixels.iter().map(|pixel| pixel.map(f64::to_bits)).collect()
                };
                assert!(bits(&walked) == bits(&pulled), "{what}: region");
                for parallelism in [Parallelism::Serial, Parallelism::Pool] {
                    let len = 3 * region.pixels() as usize;
                    let (mut walked, mut pulled) = (vec![1.5_f32; len], vec![-2.5_f32; len]);
                    Linear::fill_rows(evaluation, index, region, &mut walked, parallelism)
                        .expect("filled by rows")
                        .unwrap();
                    evaluation
                        .fill_pulled(index, region, &mut pulled, parallelism)
                        .unwrap();
                    let bits = |values: &[f32]| -> Vec<u32> {
                        values.iter().map(|value| value.to_bits()).collect()
                    };
                    assert!(
                        bits(&walked) == bits(&pulled),
                        "{what}: planes, {parallelism:?}"
                    );
                }
            }
        }
        assert!(read > 0, "{what}: some segment is read by rows");
    }

    /// The rows read from the developed planes through every source orientation and view crop,
    /// every recipe orientation, exact crops touching each edge and crops between turns, with and
    /// without colour and with and without an approximate white balance, are what the per-pixel
    /// pull reads: the rendered frame is the point evaluator's at every pixel, each chunk's rows
    /// are bit for bit the pulled ones, and so are a neighbourhood read and a spatial input.
    #[test]
    fn rows_through_every_view_and_exact_geometry_are_the_pulled_pixels() {
        let registry = ModuleRegistry::developer();
        let source = distinct(41, 29);
        for orientation in 1..=8 {
            for crop in [[0, 0, 41, 29], [3, 2, 35, 25]] {
                let view = source.with_view(crop, orientation).unwrap();
                for geometry in geometry_variants() {
                    for colour in [false, true] {
                        let mut layers = geometry.clone();
                        if colour {
                            layers.insert(layers.len() / 2, basic());
                        }
                        let recipe = colour_recipe(layers);
                        for white_balance in [None, Some(balance())] {
                            let settings = LinearSettings { white_balance };
                            let what = format!(
                                "orientation {orientation}, crop {crop:?}, {:?}, colour \
                                 {colour}, balance {}",
                                recipe
                                    .layers
                                    .iter()
                                    .map(|layer| &layer.payload)
                                    .collect::<Vec<_>>(),
                                white_balance.is_some()
                            );
                            rendered_as_the_point_evaluator(
                                &registry, &view, &recipe, settings, &what,
                            );
                            let context = RenderContext::new();
                            let evaluation = linear_evaluation(
                                &context,
                                &registry,
                                &view,
                                &recipe,
                                settings,
                                Tiling::Halo,
                                SpatialMode::Frames,
                            )
                            .unwrap();
                            rows_load_as_pulled(&evaluation, &what);
                            regions_read_as_pulled(&evaluation, &what);
                        }
                    }
                }
            }
        }
    }

    /// A spatial frame followed by exact geometry, with and without colour after it, is read by
    /// rows as the per-pixel pull reads it, in the frame's rows and in a neighbourhood or spatial
    /// input read from it, under portrait and landscape source views.
    #[test]
    fn rows_from_a_spatial_frame_through_exact_geometry_are_the_pulled_pixels() {
        let registry = ModuleRegistry::developer();
        let source = distinct(41, 29);
        for orientation in [1, 3, 6, 7] {
            let view = source.with_view([2, 1, 37, 27], orientation).unwrap();
            for geometry in geometry_variants() {
                for colour in [false, true] {
                    let mut layers = vec![colour_layer(
                        crate::PRESENCE_EFFECT,
                        serde_json::json!({"clarity": 35.0, "texture": -20.0}),
                    )];
                    layers.extend(geometry.iter().cloned());
                    if colour {
                        layers.push(basic());
                    }
                    let recipe = colour_recipe(layers);
                    let what = format!(
                        "orientation {orientation}, {:?}, colour {colour}",
                        recipe
                            .layers
                            .iter()
                            .map(|layer| &layer.payload)
                            .collect::<Vec<_>>()
                    );
                    let settings = LinearSettings::default();
                    rendered_as_the_point_evaluator(&registry, &view, &recipe, settings, &what);
                    let context = RenderContext::new();
                    let evaluation = linear_evaluation(
                        &context,
                        &registry,
                        &view,
                        &recipe,
                        settings,
                        Tiling::Halo,
                        SpatialMode::Frames,
                    )
                    .unwrap();
                    assert!(
                        matches!(evaluation.compiled.last().entry, Some(Entry::Spatial(_))),
                        "{what}"
                    );
                    rows_load_as_pulled(&evaluation, &what);
                    regions_read_as_pulled(&evaluation, &what);
                }
            }
        }
    }

    /// A white balance whose product overflows fails with the point path's own error on every
    /// row path: through an orientation and a crop, with and without colour, and in the input a
    /// spatial operation reads.
    #[test]
    fn a_white_balance_overflow_is_the_point_error_on_every_row_path() {
        let registry = ModuleRegistry::developer();
        let source = image(9, 7, &[[1.0; 3]; 63]);
        let overflow = LinearSettings {
            white_balance: Some(
                WhiteBalanceApproximation::from_matrix([[f64::MAX; 3]; 3]).unwrap(),
            ),
        };
        let presence = colour_layer(crate::PRESENCE_EFFECT, serde_json::json!({"clarity": 35.0}));
        for layers in [
            vec![Layer::orientation(crate::Orientation {
                mirror: true,
                turns: 1,
            })],
            vec![
                Layer::crop(CropPayload {
                    angle: 0.0,
                    x: 0.2,
                    y: 0.1,
                    width: 0.6,
                    height: 0.8,
                }),
                basic(),
            ],
            vec![presence.clone()],
            vec![basic(), presence],
        ] {
            let recipe = colour_recipe(layers);
            let context = RenderContext::new();
            let expected = linear_evaluation(
                &context,
                &registry,
                &source,
                &Recipe::default(),
                overflow,
                Tiling::Halo,
                SpatialMode::Point,
            )
            .unwrap()
            .pixel(0, 0)
            .unwrap_err();
            let actual = render_linear(&registry, &source, SnapshotId::new(), &recipe, overflow)
                .unwrap_err();
            assert_eq!(
                (actual.kind, actual.detail),
                (expected.kind, expected.detail),
                "{:?}",
                recipe.layers
            );
        }
    }

    /// The terminal of an `f32` pixel is the `f64` terminal of the same values, byte for byte and
    /// error for error: at, below and above every output threshold's `f32`, across `[-1, 2]`, at
    /// the edges of `f32`, and for each non-finite value in each channel.
    #[test]
    fn the_f32_terminal_is_the_f64_terminal_of_the_same_values() {
        let quantizer = srgb::quantizer();
        let mut values = vec![
            f32::MIN,
            -1.0,
            -f32::MIN_POSITIVE,
            -f32::from_bits(1),
            -0.0,
            0.0,
            f32::from_bits(1),
            f32::MIN_POSITIVE,
            0.003_130_8,
            1.0_f32.next_down(),
            1.0,
            1.0_f32.next_up(),
            32.0,
            f32::MAX,
        ];
        for threshold in srgb::output_thresholds() {
            let mut value = threshold.next_down().next_down();
            for _ in 0..5 {
                values.push(value);
                value = value.next_up();
            }
        }
        values.extend((0..=30_000).map(|step| step as f32 / 10_000.0 - 1.0));
        for (index, &value) in values.iter().enumerate() {
            // Beside two other values, so a channel taken for another shows.
            let pixel = [
                value,
                values[(index + 1) % values.len()],
                values[(index + 7) % values.len()],
            ];
            assert_eq!(
                terminal_f32(quantizer, pixel).unwrap(),
                terminal_pixel_in(quantizer, pixel.map(f64::from)).unwrap(),
                "{pixel:?}"
            );
        }
        for bad in [f32::NAN, -f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for channel in 0..3 {
                let mut pixel = [0.5; 3];
                pixel[channel] = bad;
                let narrow = terminal_f32(quantizer, pixel).unwrap_err();
                let wide = terminal_pixel_in(quantizer, pixel.map(f64::from)).unwrap_err();
                assert_eq!((narrow.kind, narrow.detail), (wide.kind, wide.detail));
            }
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

    /// A point evaluation materializes nothing, so a pixel through a spatial segment is refused
    /// there rather than evaluated in a tile of its own; the frame evaluation materializes the
    /// segment's output once and answers it.
    #[test]
    fn a_point_evaluation_refuses_a_pixel_through_a_spatial_segment() {
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
        assert_eq!(
            frames.built.len(),
            1,
            "a render materializes the spatial output"
        );
        assert!(frames.frame.is_some());
        for (x, y) in [(5, 7), (90, 60)] {
            assert!(frames.pixel(x, y).unwrap().is_some());
        }
        let point = evaluate(SpatialMode::Point);
        assert!(
            point.built.is_empty() && point.frame.is_none(),
            "a point evaluation materializes nothing"
        );
        let refused = point.pixel(5, 7).unwrap_err();
        assert_eq!(refused.kind, crate::ErrorKind::Internal, "{refused}");
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
    /// two and the evaluation keeps one, the latest. The counts are
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
                    .is_some_and(|entry| entry.spatial_operation().is_some())
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

        let built: Vec<_> = frames
            .built
            .iter()
            .map(|(frame, _)| frame.clone())
            .collect();
        drop(frames);
        assert!(
            built.iter().all(|frame| frame.strong_count() == 0),
            "nothing outlives its evaluation"
        );
    }

    /// A sample from one `Compiled` shared by several points equals a sample that compiles for
    /// itself, at every point of a small stack with a colour layer: the split
    /// A capability sample grid over a RAW stage with spatial layers is the reference's read: it
    /// materializes each spatial layer's frame once, as the render of the same stack does, and
    /// every point is the rendered byte there.
    #[test]
    fn a_linear_grid_reads_the_reference_frames() {
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
        assert_eq!(context.spatial_frames(), 4, "a grid builds one per layer");
        let frame = render.frame(SnapshotId::new()).unwrap();
        assert_eq!(context.spatial_frames(), 8, "and so does a render");
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
