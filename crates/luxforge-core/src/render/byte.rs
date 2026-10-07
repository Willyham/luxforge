//! The byte domain: a JPEG's decoded 8-bit sRGB, its rows and its driver.

use super::{
    ColorRun, Compiled, MaskedInput, PixelDomain, Raster, RenderContext, RowScratch, Segment,
    SegmentRows, SpatialEntry, apply_units, bilinear, frame_mut, segment_pass, spatial,
    spatial::fill_planes, spatial_entry, zeroed_frame,
};
use crate::{
    Cancel, Error, SnapshotId, SourceImage,
    colour::srgb::{
        Quantizer, Quantizer16, decode_pixel_in, decode_table, decode16_table, quantize_pixel,
        quantizer, quantizer16,
    },
    modules::{ExactGeometry, Parallelism, Region, Stage},
};
use rayon::prelude::*;
use std::sync::Arc;

pub(crate) fn check_source(source: &SourceImage) -> Result<(), Error> {
    if source.rgba.len() != Raster::expected_len(source.width, source.height)? {
        return Err(Error::validation(
            "source pixel buffer has the wrong length",
        ));
    }
    Ok(())
}

#[inline]
pub(super) fn source_pixel(source: &SourceImage, x: u32, y: u32) -> [u8; 4] {
    let offset = ((u64::from(y) * u64::from(source.width) + u64::from(x)) * 4) as usize;
    let pixel = &source.rgba[offset..offset + 4];
    [pixel[0], pixel[1], pixel[2], pixel[3]]
}

/// The byte domain: a JPEG's decoded 8-bit sRGB, opaque. Each colour run decodes through the
/// sRGB table, runs its units in `f32` and quantizes at its end, so a point replacement, a resample,
/// a spatial operation's output and the end of the recipe are all quantization boundaries, and a
/// frame between two segments is exactly the bytes the next boundary reads.
#[derive(Clone, Copy)]
pub(crate) struct Byte<'a>(pub(crate) &'a SourceImage);

/// The encoded width of a segment's input and output frames.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FrameWidths {
    pub(crate) input: bool,
    pub(crate) output: bool,
}

/// Backwards demand propagation keeps unbroken spatial chains wide until a real narrow boundary.
pub(crate) fn byte_frame_widths(compiled: &Compiled) -> Vec<FrameWidths> {
    let mut widths = vec![FrameWidths::default(); compiled.segments.len()];
    for index in (0..widths.len()).rev() {
        let segment = &compiled.segments[index];
        let next_spatial = compiled
            .segments
            .get(index + 1)
            .is_some_and(|next| matches!(&next.entry, Some(super::Entry::Spatial(_))));
        let spatial = matches!(&segment.entry, Some(super::Entry::Spatial(_)));
        let pass_through = !segment.writes_pixels();
        let input = spatial
            && ((segment.has_color && !segment.has_pixels) || (pass_through && next_spatial));
        widths[index] = FrameWidths {
            input,
            output: next_spatial && (segment.has_color || input),
        };
    }
    widths
}

#[derive(Clone)]
pub(crate) enum ByteFrame {
    Narrow(Arc<Vec<u8>>),
    Wide(Arc<Vec<u16>>),
}

impl ByteFrame {
    pub(crate) fn wide(&self) -> bool {
        matches!(self, Self::Wide(_))
    }
    pub(crate) fn new(stage: Stage, wide: bool) -> Result<Self, Error> {
        if !wide {
            return Ok(Self::Narrow(zeroed_frame(Raster::expected_len(
                stage.width,
                stage.height,
            )?)));
        }
        let bytes = u64::from(stage.width)
            .checked_mul(u64::from(stage.height))
            .and_then(|n| n.checked_mul(6))
            .ok_or_else(|| Error::resource_limit("wide image dimensions overflow"))?;
        if bytes > crate::render::limits::MAX_FRAME_BYTES {
            return Err(Error::resource_limit(
                "wide evaluated image exceeds 512 MiB",
            ));
        }
        let len = usize::try_from(bytes / 2)
            .map_err(|_| Error::resource_limit("wide image allocation is not addressable"))?;
        Ok(Self::Wide(Arc::new(vec![0; len])))
    }
    pub(crate) fn pixel(&self, stage: Stage, x: u32, y: u32) -> [u16; 3] {
        let offset = (u64::from(y) * u64::from(stage.width) + u64::from(x)) as usize;
        match self {
            Self::Narrow(v) => {
                [v[offset * 4], v[offset * 4 + 1], v[offset * 4 + 2]].map(|v| u16::from(v) * 257)
            }
            Self::Wide(v) => [v[offset * 3], v[offset * 3 + 1], v[offset * 3 + 2]],
        }
    }
}

fn encoded(rgb: [f32; 3], wide: bool) -> [u16; 3] {
    if wide {
        quantizer16().pixel(rgb)
    } else {
        quantize_pixel(rgb).map(|v| u16::from(v) * 257)
    }
}

/// [`encoded`] through the quantizers a pass took once, rather than once a pixel: every
/// dereference of a lazy static is an atomic load the compiler cannot merge.
#[inline]
fn encoded_in(quantizers: (&Quantizer, &Quantizer16), rgb: [f32; 3], wide: bool) -> [u16; 3] {
    if wide {
        quantizers.1.pixel(rgb)
    } else {
        quantizers.0.pixel(rgb).map(|v| u16::from(v) * 257)
    }
}

fn decoded(rgb: [u16; 3]) -> [f32; 3] {
    decoded_in(decode16_table(), rgb)
}

/// [`decoded`] through the 16-bit table a pass took once.
#[inline]
fn decoded_in(table: &[f32; 65536], rgb: [u16; 3]) -> [f32; 3] {
    rgb.map(|code| table[usize::from(code)])
}

impl PixelDomain for Byte<'_> {
    type Pixel = [u16; 3];
    type SpatialFrame = ByteFrame;
    type TileOutput = ByteFrame;

    fn source_pixel(&self, x: u32, y: u32) -> Result<Self::Pixel, Error> {
        let p = source_pixel(self.0, x, y);
        Ok([p[0], p[1], p[2]].map(|v| u16::from(v) * 257))
    }
    fn replace(_: Self::Pixel, rgb: [u8; 3]) -> Self::Pixel {
        rgb.map(|v| u16::from(v) * 257)
    }
    fn colour<'r>(
        mut pixel: Self::Pixel,
        runs: impl Iterator<Item = ColorRun<'r>>,
        x: u32,
        y: u32,
        wide: bool,
    ) -> Result<Self::Pixel, Error> {
        let mut snapshot = [MaskedInput::default(); 1];
        for run in runs {
            let mut rgb = [decoded(pixel)];
            apply_units(&run, y, x, &mut rgb, &mut snapshot)?;
            pixel = encoded(rgb[0], wide && !run.followed_by_replace());
        }
        Ok(pixel)
    }
    fn colour_row<'r>(
        pixels: &mut [Self::Pixel],
        runs: impl Iterator<Item = ColorRun<'r>>,
        y: u32,
        x0: u32,
        scratch: &mut RowScratch,
        wide: bool,
    ) -> Result<(), Error> {
        let RowScratch { linear, snapshot } = scratch;
        snapshot.resize(pixels.len().max(1), MaskedInput::default());
        let (table, quantizers) = (decode16_table(), (quantizer(), quantizer16()));
        for run in runs {
            linear.clear();
            linear.extend(pixels.iter().map(|pixel| decoded_in(table, *pixel)));
            apply_units(&run, y, x0, linear, snapshot)?;
            let wide = wide && !run.followed_by_replace();
            for (pixel, rgb) in pixels.iter_mut().zip(linear.iter()) {
                *pixel = encoded_in(quantizers, *rgb, wide);
            }
        }
        Ok(())
    }
    fn blend(
        u: f64,
        v: f64,
        width: u32,
        height: u32,
        mut fetch: impl FnMut(u32, u32) -> Result<Self::Pixel, Error>,
    ) -> Result<Self::Pixel, Error> {
        let result = bilinear(decode_table(), quantizer(), u, v, width, height, |x, y| {
            let p = fetch(x, y)?;
            Ok([
                (p[0] / 257) as u8,
                (p[1] / 257) as u8,
                (p[2] / 257) as u8,
                255,
            ])
        })?;
        Ok([result[0], result[1], result[2]].map(|v| u16::from(v) * 257))
    }
    fn spatial_input(pixel: Self::Pixel) -> [f32; 3] {
        decoded(pixel)
    }
    fn linear(pixel: Self::Pixel) -> [f64; 3] {
        decoded(pixel).map(f64::from)
    }
    fn spatial_output(rgb: [f32; 3], wide: bool) -> Result<Self::Pixel, Error> {
        Ok(encoded(rgb, wide))
    }
    fn narrow(pixel: Self::Pixel) -> Self::Pixel {
        encoded(decoded(pixel), false)
    }
    fn finish_width(pixel: Self::Pixel, wide: bool) -> Result<Self::Pixel, Error> {
        if wide {
            Ok(pixel)
        } else {
            Ok(encoded(decoded(pixel), false))
        }
    }
    fn terminal(pixel: Self::Pixel) -> Result<[u8; 4], Error> {
        Ok([
            (pixel[0] / 257) as u8,
            (pixel[1] / 257) as u8,
            (pixel[2] / 257) as u8,
            255,
        ])
    }
    fn spatial_frame(stage: Stage, wide: bool) -> Result<ByteFrame, Error> {
        ByteFrame::new(stage, wide)
    }
    fn tile_output(
        region: Region,
        values: &[f32],
        tile: Region,
        parallelism: Parallelism,
        wide: bool,
    ) -> ByteFrame {
        let plane = region.pixels() as usize;
        let width = tile.width as usize;
        let fill = |row: usize, column: usize| {
            let from = (tile.y0 + row as u32 - region.y0) as usize * region.width as usize
                + (tile.x0 - region.x0) as usize
                + column;
            [values[from], values[plane + from], values[2 * plane + from]]
        };
        if wide {
            let mut pixels = vec![0u16; tile.pixels() as usize * 3];
            let quantizer = quantizer16();
            let row = |(r, pixels): (usize, &mut [u16])| {
                for (column, p) in pixels.chunks_exact_mut(3).enumerate() {
                    p.copy_from_slice(&quantizer.pixel(fill(r, column)));
                }
            };
            match parallelism {
                Parallelism::Pool => pixels.par_chunks_mut(width * 3).enumerate().for_each(row),
                Parallelism::Serial => pixels.chunks_mut(width * 3).enumerate().for_each(row),
            }
            ByteFrame::Wide(Arc::new(pixels))
        } else {
            let mut pixels = vec![0u8; tile.pixels() as usize * 4];
            let quantizer = quantizer();
            let row = |(r, pixels): (usize, &mut [u8])| {
                for (column, p) in pixels.chunks_exact_mut(4).enumerate() {
                    p[..3].copy_from_slice(&quantizer.pixel(fill(r, column)));
                    p[3] = 255;
                }
            };
            match parallelism {
                Parallelism::Pool => pixels.par_chunks_mut(width * 4).enumerate().for_each(row),
                Parallelism::Serial => pixels.chunks_mut(width * 4).enumerate().for_each(row),
            }
            ByteFrame::Narrow(Arc::new(pixels))
        }
    }
    fn write_tile(frame: &mut ByteFrame, stage: Stage, tile: Region, output: ByteFrame) {
        match (frame, output) {
            (ByteFrame::Narrow(frame), ByteFrame::Narrow(output)) => {
                write_rows(frame_mut(frame), &output, stage, tile, 4)
            }
            (ByteFrame::Wide(frame), ByteFrame::Wide(output)) => {
                write_rows(super::raster::frame_mut16(frame), &output, stage, tile, 3)
            }
            _ => unreachable!("tile and frame widths agree"),
        }
    }
    fn frame_pixel(frame: &ByteFrame, stage: Stage, x: u32, y: u32) -> Self::Pixel {
        frame.pixel(stage, x, y)
    }
}
fn write_rows<T: Copy>(
    frame: &mut [T],
    tile_pixels: &[T],
    stage: Stage,
    tile: Region,
    samples: usize,
) {
    let row_samples = tile.width as usize * samples;
    for (row, y) in (tile.y0..tile.y1()).enumerate() {
        let to = (u64::from(y) * u64::from(stage.width) + u64::from(tile.x0)) as usize * samples;
        frame[to..to + row_samples]
            .copy_from_slice(&tile_pixels[row * row_samples..(row + 1) * row_samples]);
    }
}

/// One byte segment's rows, held in the frame the pass writes: loaded through the segment's exact
/// geometry from its input frame, or left where they are when that geometry is the identity and the
/// pass writes over a frame this render already wrote. Each colour run decodes the rows it reaches,
/// evaluates them and quantizes them back.
struct ByteRows<'f> {
    /// The frame the geometry reads and its width, or `None` for a pass in place.
    input: Option<(&'f [u8], u32)>,
    geometry: ExactGeometry,
    width: usize,
    colours: bool,
}

impl<'f> ByteRows<'f> {
    fn new(segment: &Segment, input: Option<(&'f [u8], u32)>) -> Self {
        Self {
            input,
            geometry: segment.geometry,
            width: segment.width as usize,
            colours: segment.has_color,
        }
    }
}

impl SegmentRows for ByteRows<'_> {
    /// A chunk's rows decoded to linear light while a run evaluates them: at most
    /// [`super::colour_runs::COLOR_CHUNK_SCRATCH_BYTES`], allocated by the first chunk of a Rayon split and reused by
    /// the rest of that split's chunks.
    type Sample = u8;
    type Scratch = Vec<[f32; 3]>;

    fn scratch_bytes(&self, width: usize, _: usize, coloured: usize) -> usize {
        if self.colours {
            coloured * width * std::mem::size_of::<[f32; 3]>()
        } else {
            0
        }
    }

    /// Every output pixel copies exactly one input pixel. A geometry that at most translates copies
    /// each output row as one run of its input row.
    fn load(&self, _: &mut Self::Scratch, y0: u32, chunk: &mut [u8]) -> Result<(), Error> {
        let Some((input, input_width)) = self.input else {
            return Ok(());
        };
        let row_bytes = self.width * 4;
        if self.geometry.keeps_rows() {
            for (row, bytes) in chunk.chunks_exact_mut(row_bytes).enumerate() {
                let (input_x, input_y) = self.geometry.unmap(0, y0 + row as u32);
                let from = ((u64::from(input_y) * u64::from(input_width) + u64::from(input_x)) * 4)
                    as usize;
                bytes.copy_from_slice(&input[from..from + row_bytes]);
            }
            return Ok(());
        }
        for (row, bytes) in chunk.chunks_exact_mut(row_bytes).enumerate() {
            let out_y = y0 + row as u32;
            for out_x in 0..self.width as u32 {
                let (input_x, input_y) = self.geometry.unmap(out_x, out_y);
                let from = ((u64::from(input_y) * u64::from(input_width) + u64::from(input_x)) * 4)
                    as usize;
                let to = out_x as usize * 4;
                bytes[to..to + 4].copy_from_slice(&input[from..from + 4]);
            }
        }
        Ok(())
    }

    fn replace(
        &self,
        _: &mut Self::Scratch,
        chunk: &mut [u8],
        offset: usize,
        rgb: [u8; 3],
    ) -> Result<(), Error> {
        chunk[offset * 4..offset * 4 + 3].copy_from_slice(&rgb);
        Ok(())
    }

    fn run(
        &self,
        linear: &mut Self::Scratch,
        chunk: &mut [u8],
        run: &ColorRun<'_>,
        y0: u32,
        rows: std::ops::Range<usize>,
        snapshot: &mut [MaskedInput],
    ) -> Result<(), Error> {
        let row_bytes = self.width * 4;
        colour_byte_rows(
            run,
            &mut chunk[rows.start * row_bytes..rows.end * row_bytes],
            self.width,
            y0 + rows.start as u32,
            0,
            linear,
            snapshot,
        )
    }

    fn store(&self, _: &mut Self::Scratch, _: &mut [u8]) -> Result<(), Error> {
        Ok(())
    }
}

/// One colour run over `bytes`, RGBA rows of `width` pixels whose first pixel is at column `x0` of
/// row `y0` of the stage the run's segment produces: decoded through the sRGB table into `linear`,
/// every row handed to the run's units at its own coordinates, and quantized back in place, alpha
/// untouched. It is the byte domain's one row arithmetic, which a rendered chunk
/// ([`ByteRows::run`]) and a pulled row ([`Byte::colour_row`]) share; `snapshot` is a masked
/// operation's scratch for its own input and its coverage.
fn colour_byte_rows(
    run: &ColorRun<'_>,
    bytes: &mut [u8],
    width: usize,
    y0: u32,
    x0: u32,
    linear: &mut Vec<[f32; 3]>,
    snapshot: &mut [MaskedInput],
) -> Result<(), Error> {
    // The tables are taken once for the rows, not once per pixel.
    let (table, quantizer) = (decode_table(), quantizer());
    linear.clear();
    linear.extend(
        bytes
            .chunks_exact(4)
            .map(|pixel| decode_pixel_in(table, [pixel[0], pixel[1], pixel[2]])),
    );
    // Whole rows, so every unit is handed one row at a time.
    for (offset, row) in linear.chunks_mut(width).enumerate() {
        apply_units(run, y0 + offset as u32, x0, row, snapshot)?;
    }
    for (pixel, value) in bytes.chunks_exact_mut(4).zip(linear.iter()) {
        pixel[..3].copy_from_slice(&quantizer.pixel(*value));
    }
    Ok(())
}

/// The whole frame of a stack compiled against a byte source: the byte path's half of
/// [`super::Render::frame`]. The exact phase and the proxy phase differ only in how their masks were
/// compiled, so this one pass serves both. A cancelled token answers `Cancelled`, and every
/// reservation held is released on the way out.
///
/// This driver materializes every segment's output as a byte frame, which is exactly what the next
/// boundary reads: a segment's input frame is the source, or the frame its boundary writes from the
/// frame before it ([`super::Entry::byte_frame`]), and one [`segment_pass`] writes the segment's
/// frame from it through its exact geometry, replacements and colour runs. A segment whose geometry
/// is the identity writes in place over a frame this render wrote, loads the shared source's rows
/// into its new frame inside the pass, and shares its input when it writes nothing, so an identity
/// stack returns the source allocation itself. At most two frames exist at once.
pub(super) fn rasterize(
    source: &SourceImage,
    compiled: &Compiled,
    snapshot_id: SnapshotId,
    cancel: &Cancel,
    tiling: spatial::Tiling,
    context: &RenderContext,
) -> Result<Raster, Error> {
    rasterize_suffix(source, compiled, snapshot_id, cancel, tiling, context, None)
}

pub(super) fn rasterize_suffix(
    source: &SourceImage,
    compiled: &Compiled,
    snapshot_id: SnapshotId,
    cancel: &Cancel,
    tiling: spatial::Tiling,
    context: &RenderContext,
    held: Option<(usize, ByteFrame, Stage)>,
) -> Result<Raster, Error> {
    let (frame, stage) = frames(source, compiled, cancel, tiling, context, None, held)?;
    let ByteFrame::Narrow(rgba) = frame else {
        unreachable!("terminal frame is narrow")
    };
    Ok(Raster {
        width: stage.width,
        height: stage.height,
        rgba,
        source_fingerprint: source.fingerprint.clone(),
        snapshot_id,
    })
}

pub(super) fn frames(
    source: &SourceImage,
    compiled: &Compiled,
    cancel: &Cancel,
    tiling: spatial::Tiling,
    context: &RenderContext,
    stop: Option<usize>,
    held: Option<(usize, ByteFrame, Stage)>,
) -> Result<(ByteFrame, Stage), Error> {
    cancel.check()?;
    let domain = Byte(source);
    let widths = byte_frame_widths(compiled);
    let start = held.as_ref().map_or(0, |(index, _, _)| *index);
    let reused = held.is_some();
    let (mut frame, mut stage) = held.map_or_else(
        || {
            (
                ByteFrame::Narrow(source.rgba.clone()),
                Stage {
                    width: source.width,
                    height: source.height,
                },
            )
        },
        |(_, pixels, stage)| (pixels, stage),
    );
    // A held frame is the output of the entry at `start`, so that entry runs no tiles.
    let first = if reused { start + 1 } else { start };
    let last = stop.map_or(compiled.segments.len(), |stop| stop + 1);
    compiled.plan_progress(cancel, Some(stage), first..last, tiling);
    let mut shared = true;
    for (index, segment) in compiled.segments.iter().enumerate().skip(start) {
        if let Some(entry) = &segment.entry
            && !(reused && index == start)
        {
            frame = entry.byte_frame(
                &domain,
                &frame,
                stage,
                tiling,
                cancel,
                context,
                widths[index].input,
            )?;
            let held = entry.held(stage);
            stage = Stage {
                width: held.width,
                height: held.height,
            };
            shared = false;
        }
        if stop == Some(index) {
            return Ok((frame, stage));
        }
        let output_wide = widths[index].output;
        let identity = segment.geometry.is_identity(stage.width, stage.height);
        if identity && !segment.writes_pixels() && frame.wide() == output_wide {
            continue;
        }
        let band = band(compiled, index);
        if identity && frame.wide() == output_wide && !shared {
            match &mut frame {
                ByteFrame::Narrow(pixels) => segment_pass(
                    &ByteRows::new(segment, None),
                    segment,
                    frame_mut(pixels),
                    band,
                    cancel,
                    context.scratch(),
                )?,
                ByteFrame::Wide(pixels) => segment_pass(
                    &FloatRows::<Wide>::new(segment, None),
                    segment,
                    super::raster::frame_mut16(pixels),
                    band,
                    cancel,
                    context.scratch(),
                )?,
            }
        } else {
            cancel.check()?;
            let mut next = ByteFrame::new(segment.stage(), output_wide)?;
            match (&frame, &mut next) {
                (ByteFrame::Narrow(input), ByteFrame::Narrow(output)) => segment_pass(
                    &ByteRows::new(segment, Some((input, stage.width))),
                    segment,
                    frame_mut(output),
                    band,
                    cancel,
                    context.scratch(),
                )?,
                (_, ByteFrame::Narrow(output)) => segment_pass(
                    &FloatRows::<Narrow>::new(segment, Some((&frame, stage))),
                    segment,
                    frame_mut(output),
                    band,
                    cancel,
                    context.scratch(),
                )?,
                (_, ByteFrame::Wide(output)) => segment_pass(
                    &FloatRows::<Wide>::new(segment, Some((&frame, stage))),
                    segment,
                    super::raster::frame_mut16(output),
                    band,
                    cancel,
                    context.scratch(),
                )?,
            }
            frame = next;
        }
        shared = false;
        stage = segment.stage();
    }
    Ok((frame, stage))
}

/// How a float row pass stores its pixels: the narrow 8-bit frame, the wide RGB16 frame a spatial
/// chain reads, or the half floats a GPU preview's boundary holds. Each converts a whole chunk, so
/// it takes its table once for the chunk.
trait SampleStore: Send + Sync {
    type Sample: Copy + Send + Sync;
    const SAMPLES: usize;
    /// Every pixel of `samples`, appended to `rgb`.
    fn load(samples: &[Self::Sample], rgb: &mut Vec<[f32; 3]>);
    /// Every pixel of `rgb`, written over `samples`.
    fn store(samples: &mut [Self::Sample], rgb: &[[f32; 3]]);
}
/// Opaque 8-bit sRGB, four bytes a pixel.
struct Narrow;
impl SampleStore for Narrow {
    type Sample = u8;
    const SAMPLES: usize = 4;
    fn load(samples: &[u8], rgb: &mut Vec<[f32; 3]>) {
        let table = decode_table();
        rgb.extend(
            samples
                .chunks_exact(4)
                .map(|p| decode_pixel_in(table, [p[0], p[1], p[2]])),
        );
    }
    fn store(samples: &mut [u8], rgb: &[[f32; 3]]) {
        let quantizer = quantizer();
        for (p, rgb) in samples.chunks_exact_mut(4).zip(rgb) {
            p[..3].copy_from_slice(&quantizer.pixel(*rgb));
            p[3] = 255;
        }
    }
}
/// Encoded RGB16, three samples a pixel.
struct Wide;
impl SampleStore for Wide {
    type Sample = u16;
    const SAMPLES: usize = 3;
    fn load(samples: &[u16], rgb: &mut Vec<[f32; 3]>) {
        let table = decode16_table();
        rgb.extend(
            samples
                .chunks_exact(3)
                .map(|p| decoded_in(table, [p[0], p[1], p[2]])),
        );
    }
    fn store(samples: &mut [u16], rgb: &[[f32; 3]]) {
        let quantizer = quantizer16();
        for (p, rgb) in samples.chunks_exact_mut(3).zip(rgb) {
            p.copy_from_slice(&quantizer.pixel(*rgb));
        }
    }
}
/// Linear light as a JPEG's GPU preview boundary holds it ([`super::boundary`]): four
/// little-endian half floats a pixel, eight bytes, with the value unclamped and opaque alpha.
#[cfg(any(test, feature = "qualification"))]
pub(super) struct Half;
#[cfg(any(test, feature = "qualification"))]
impl SampleStore for Half {
    type Sample = u8;
    const SAMPLES: usize = super::boundary::BoundaryFormat::Half.texel_bytes();
    fn load(samples: &[u8], rgb: &mut Vec<[f32; 3]>) {
        rgb.extend(
            samples
                .chunks_exact(Self::SAMPLES)
                .map(|p| super::boundary::read_texel(super::boundary::BoundaryFormat::Half, p)),
        );
    }
    fn store(samples: &mut [u8], rgb: &[[f32; 3]]) {
        for (p, rgb) in samples.chunks_exact_mut(Self::SAMPLES).zip(rgb) {
            super::boundary::write_texel(super::boundary::BoundaryFormat::Half, p, *rgb);
        }
    }
}
struct FloatRows<'a, S> {
    segment: &'a Segment,
    input: Option<(&'a ByteFrame, Stage)>,
    sample: std::marker::PhantomData<S>,
}
impl<'a, S> FloatRows<'a, S> {
    fn new(segment: &'a Segment, input: Option<(&'a ByteFrame, Stage)>) -> Self {
        Self {
            segment,
            input,
            sample: std::marker::PhantomData,
        }
    }
}
impl<S: SampleStore> SegmentRows for FloatRows<'_, S> {
    type Sample = S::Sample;
    type Scratch = Vec<[f32; 3]>;
    fn samples_per_pixel(&self) -> usize {
        S::SAMPLES
    }
    fn scratch_bytes(&self, width: usize, rows: usize, _: usize) -> usize {
        width * rows * 12
    }
    fn load(
        &self,
        scratch: &mut Self::Scratch,
        y0: u32,
        chunk: &mut [S::Sample],
    ) -> Result<(), Error> {
        scratch.clear();
        match self.input {
            Some((input, stage)) => {
                let width = self.segment.width as usize;
                let table = decode16_table();
                scratch.extend((0..chunk.len() / S::SAMPLES).map(|offset| {
                    let (x, y) = self
                        .segment
                        .geometry
                        .unmap((offset % width) as u32, y0 + (offset / width) as u32);
                    decoded_in(table, input.pixel(stage, x, y))
                }));
            }
            None => S::load(chunk, scratch),
        }
        Ok(())
    }
    fn replace(
        &self,
        scratch: &mut Self::Scratch,
        _: &mut [S::Sample],
        offset: usize,
        rgb: [u8; 3],
    ) -> Result<(), Error> {
        scratch[offset] = decode_pixel_in(decode_table(), rgb);
        Ok(())
    }
    fn run(
        &self,
        scratch: &mut Self::Scratch,
        _: &mut [S::Sample],
        run: &ColorRun<'_>,
        y0: u32,
        rows: std::ops::Range<usize>,
        snapshot: &mut [MaskedInput],
    ) -> Result<(), Error> {
        let width = self.segment.width as usize;
        let (table, quantizer) = (decode_table(), quantizer());
        for row in rows {
            let pixels = &mut scratch[row * width..(row + 1) * width];
            apply_units(run, y0 + row as u32, 0, pixels, snapshot)?;
            if run.followed_by_replace() {
                for pixel in pixels {
                    *pixel = decode_pixel_in(table, quantizer.pixel(*pixel));
                }
            }
        }
        Ok(())
    }
    fn store(&self, scratch: &mut Self::Scratch, chunk: &mut [S::Sample]) -> Result<(), Error> {
        S::store(chunk, scratch);
        Ok(())
    }
}

/// One segment's pass into the GPU preview's boundary ([`super::boundary`]): `segment`'s rows read
/// from `input`, the `stage` frame entering it, through its exact geometry and colour runs, and
/// stored as half floats without quantizing, so a boundary inside a colour run holds the value
/// the run hands the next layer.
#[cfg(any(test, feature = "qualification"))]
pub(super) fn boundary_pass(
    segment: &Segment,
    input: &ByteFrame,
    stage: Stage,
    cancel: &Cancel,
    context: &RenderContext,
) -> Result<Vec<u8>, Error> {
    let len = super::boundary::frame_len(
        segment.width,
        segment.height,
        super::boundary::BoundaryFormat::Half,
    )?;
    let mut texels = vec![0u8; len];
    segment_pass(
        &FloatRows::<Half>::new(segment, Some((input, stage))),
        segment,
        &mut texels,
        0..segment.height as usize,
        cancel,
        context.scratch(),
    )?;
    Ok(texels)
}

/// The rows of segment `index`'s frame its colour runs reach: those the boundary after it reads
/// ([`super::Entry::reads`] over the rectangle its frame holds): a resample's taps
/// ([`crate::modules::Resample::reads`]), or every row under a spatial boundary, which reads every
/// row plus a halo; every row when nothing follows it. Everything outside the band is discarded by
/// the boundary, so leaving it uncoloured changes no output byte.
pub(super) fn band(compiled: &Compiled, index: usize) -> std::ops::Range<usize> {
    let segment = &compiled.segments[index];
    let every = 0..segment.height as usize;
    let Some(entry) = compiled
        .segments
        .get(index + 1)
        .and_then(|next| next.entry.as_ref())
    else {
        return every;
    };
    let stage = segment.stage();
    entry
        .reads(entry.held(stage), stage)
        .map_or(every, |read| read.y0 as usize..read.y1() as usize)
}

/// The byte driver's frame of the spatial boundary `entry` over `input`, the finished byte frame
/// of the `stage` it receives: its tiles read the frame through the sRGB decode table.
// Match the shared spatial_entry contract; this adapter adds the materialized input frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn spatial_frame(
    domain: &Byte<'_>,
    entry: &SpatialEntry,
    input: &ByteFrame,
    stage: Stage,
    tiling: spatial::Tiling,
    cancel: &Cancel,
    context: &RenderContext,
    wide: bool,
) -> Result<ByteFrame, Error> {
    let table = decode16_table();
    let read = |x: u32, y: u32| -> Result<[f32; 3], Error> {
        Ok(decoded_in(table, input.pixel(stage, x, y)))
    };
    spatial_entry(
        domain,
        entry,
        stage,
        tiling,
        cancel,
        context,
        wide,
        |region, planes, parallelism| fill_planes(region, planes, parallelism, read),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Entry, RenderOptions, tests::gradient};
    use super::*;
    use crate::{BASIC_EFFECT, Layer, ModuleRegistry, PRESENCE_EFFECT, Recipe};
    use serde_json::json;
    fn segment(spatial: bool, colour: bool, pixels: bool) -> Segment {
        let mut segment = Segment::new(
            spatial.then(|| Entry::spatial(crate::SpatialOperation::neutral())),
            10,
            10,
        );
        segment.has_color = colour;
        segment.has_pixels = pixels;
        segment
    }
    #[test]
    fn byte_frame_widths_follow_the_hand_off_rules() {
        let cases = [
            (
                vec![segment(false, false, false), segment(true, true, false)],
                vec![(false, false), (true, false)],
            ),
            (
                vec![segment(false, true, false), segment(true, false, false)],
                vec![(false, true), (false, false)],
            ),
            (
                vec![segment(false, false, false), segment(true, false, false)],
                vec![(false, false), (false, false)],
            ),
            (
                vec![segment(false, false, false), segment(true, true, true)],
                vec![(false, false), (false, false)],
            ),
            (
                vec![
                    segment(false, false, false),
                    segment(true, false, false),
                    segment(true, false, false),
                ],
                vec![(false, false), (true, true), (false, false)],
            ),
            (
                vec![
                    segment(false, false, false),
                    segment(true, false, false),
                    segment(true, true, false),
                ],
                vec![(false, false), (true, true), (true, false)],
            ),
        ];
        for (segments, expected) in cases {
            let compiled = Compiled {
                segments,
                layers: Box::new([]),
            };
            let actual: Vec<_> = byte_frame_widths(&compiled)
                .into_iter()
                .map(|w| (w.input, w.output))
                .collect();
            assert_eq!(actual, expected);
        }
        let mut segments = vec![segment(false, true, false), segment(false, true, false)];
        segments[1].entry = Some(Entry::resample(crate::Resample {
            map: crate::modules::Mapping::Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
            output_width: 10,
            output_height: 10,
        }));
        assert!(
            byte_frame_widths(&Compiled {
                segments,
                layers: Box::new([])
            })
            .iter()
            .all(|w| !w.input && !w.output)
        );
    }
    #[test]
    fn byte_wide_frame_len_is_checked_against_the_frame_limit() {
        assert!(
            matches!(ByteFrame::new(Stage{width:8,height:7},true).unwrap(),ByteFrame::Wide(values) if values.len()==8*7*3)
        );
        assert!(
            ByteFrame::new(
                Stage {
                    width: 16384,
                    height: 6000
                },
                true
            )
            .is_err()
        );
        assert!(
            ByteFrame::new(
                Stage {
                    width: u32::MAX,
                    height: u32::MAX
                },
                true
            )
            .is_err()
        );
    }
    fn wide_stack() -> Recipe {
        Recipe {
            layers: vec![
                Layer::new(BASIC_EFFECT, json!({"exposure":0.7,"shadows":40.0})),
                Layer::new(PRESENCE_EFFECT, json!({"texture":50.0,"clarity":30.0})),
                Layer::new(crate::MIXER_EFFECT, json!({"red-luminance":12.0})),
            ],
            ..Recipe::default()
        }
    }
    #[test]
    fn byte_point_sample_equals_rendered_byte_through_a_wide_hand_off() {
        let registry = ModuleRegistry::builtin();
        let source = gradient(67, 49);
        let context = RenderContext::new();
        let recipe = wide_stack();
        let render = super::super::render(
            &registry,
            &source,
            &recipe,
            RenderOptions::default(),
            &context,
        )
        .unwrap();
        let frame = render.frame(SnapshotId::new()).unwrap();
        for (x, y) in [(0, 0), (13, 8), (34, 24), (66, 48), (0, 48), (66, 0)] {
            assert_eq!(
                render.sample(x, y).unwrap().rgba,
                frame.pixel(x, y),
                "({x},{y})"
            );
        }
        super::super::parallel::force(Some(true));
        let pooled = render.frame(SnapshotId::new()).unwrap();
        super::super::parallel::force(Some(false));
        let serial = render.frame(SnapshotId::new()).unwrap();
        super::super::parallel::force(None);
        assert_eq!(pooled.rgba, serial.rgba);
    }
}
