//! The byte domain: a JPEG's decoded 8-bit sRGB, its rows and its driver.

use super::{
    ColorRun, Compiled, Entry, PixelDomain, Raster, RenderContext, RowScratch, Segment,
    SegmentRows, SpatialEntry, apply_units, bilinear, color_pixel, frame_mut, resample_frame,
    segment_pass, spatial, spatial::fill_planes, spatial_entry, zeroed_frame,
};
use crate::{
    Cancel, Error, SnapshotId, SourceImage,
    colour::srgb::{decode_pixel, decode_pixel_in, decode_table, quantize_pixel, quantizer},
    modules::{ExactGeometry, Parallelism, Region, Stage},
};
use rayon::prelude::*;
use std::{borrow::Cow, sync::Arc};

pub(super) fn check_source(source: &SourceImage) -> Result<(), Error> {
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

impl PixelDomain for Byte<'_> {
    type Pixel = [u8; 4];
    type SpatialFrame = Arc<Vec<u8>>;
    /// A tile's RGBA rows, quantized and opaque, exactly as the frame holds them.
    type TileOutput = Vec<u8>;

    fn fingerprint(&self) -> &str {
        &self.0.fingerprint
    }

    fn estimate_prefix<'p>(&self, prefix_hash: &'p str) -> Cow<'p, str> {
        Cow::Owned(format!(
            "{prefix_hash}+byte:{}x{}:orientation:{}",
            self.0.width, self.0.height, self.0.orientation,
        ))
    }

    #[inline]
    fn source_pixel(&self, x: u32, y: u32) -> Result<[u8; 4], Error> {
        Ok(source_pixel(self.0, x, y))
    }

    #[inline]
    fn replace(mut pixel: [u8; 4], rgb: [u8; 3]) -> [u8; 4] {
        pixel[..3].copy_from_slice(&rgb);
        pixel
    }

    fn colour<'r>(
        mut pixel: [u8; 4],
        runs: impl Iterator<Item = ColorRun<'r>>,
        x: u32,
        y: u32,
    ) -> Result<[u8; 4], Error> {
        let mut rgb = [pixel[0], pixel[1], pixel[2]];
        for run in runs {
            rgb = color_pixel(rgb, &run, x, y)?;
        }
        pixel[..3].copy_from_slice(&rgb);
        Ok(pixel)
    }

    /// The row form of [`Self::colour`]: each run decodes the row, runs its units over it and
    /// quantizes it back, with the rows of a rendered segment's own arithmetic
    /// ([`colour_byte_rows`]).
    fn colour_row<'r>(
        pixels: &mut [[u8; 4]],
        runs: impl Iterator<Item = ColorRun<'r>>,
        y: u32,
        x0: u32,
        scratch: &mut RowScratch,
    ) -> Result<(), Error> {
        let width = pixels.len();
        if width == 0 {
            return Ok(());
        }
        let RowScratch { linear, snapshot } = scratch;
        snapshot.resize(width, [0.0; 3]);
        for run in runs {
            colour_byte_rows(
                &run,
                pixels.as_flattened_mut(),
                width,
                y,
                x0,
                linear,
                snapshot,
            )?;
        }
        Ok(())
    }

    fn blend(
        u: f64,
        v: f64,
        width: u32,
        height: u32,
        fetch: impl FnMut(u32, u32) -> Result<[u8; 4], Error>,
    ) -> Result<[u8; 4], Error> {
        bilinear(decode_table(), quantizer(), u, v, width, height, fetch)
    }

    #[inline]
    fn spatial_input(pixel: [u8; 4]) -> [f32; 3] {
        decode_pixel([pixel[0], pixel[1], pixel[2]])
    }

    #[inline]
    fn linear(pixel: [u8; 4]) -> [f64; 3] {
        Self::spatial_input(pixel).map(f64::from)
    }

    fn spatial_output(rgb: [f32; 3]) -> Result<[u8; 4], Error> {
        let rgb = quantize_pixel(rgb);
        Ok([rgb[0], rgb[1], rgb[2], 255])
    }

    #[inline]
    fn terminal(pixel: [u8; 4]) -> Result<[u8; 4], Error> {
        Ok(pixel)
    }

    fn spatial_frame(stage: Stage) -> Result<Arc<Vec<u8>>, Error> {
        Ok(zeroed_frame(Raster::expected_len(
            stage.width,
            stage.height,
        )?))
    }

    /// Quantized through the same exact thresholds as a colour run's end, opaque, into RGBA rows
    /// of the tile's width: on the pool under [`Parallelism::Pool`], and otherwise on the worker
    /// that ran the tile.
    fn tile_output(
        region: Region,
        values: Vec<f32>,
        tile: Region,
        parallelism: Parallelism,
    ) -> Vec<u8> {
        let mut bytes = vec![0; (tile.pixels() * 4) as usize];
        let plane = region.pixels() as usize;
        let width = tile.width as usize;
        let quantizer = quantizer();
        let row = |(row, bytes): (usize, &mut [u8])| {
            let y = tile.y0 + row as u32;
            // The tile's row inside each of the last unit's planes.
            let from =
                (y - region.y0) as usize * region.width as usize + (tile.x0 - region.x0) as usize;
            let [red, green, blue] = [0, 1, 2].map(|channel| {
                let start = channel * plane + from;
                &values[start..start + width]
            });
            for (column, pixel) in bytes.chunks_exact_mut(4).enumerate() {
                // `quantize_pixel` channel by channel, written out so this hot loop does not
                // depend on the array map being inlined into it.
                pixel[0] = quantizer.channel(f64::from(red[column]));
                pixel[1] = quantizer.channel(f64::from(green[column]));
                pixel[2] = quantizer.channel(f64::from(blue[column]));
                pixel[3] = 255;
            }
        };
        let row_bytes = width * 4;
        match parallelism {
            Parallelism::Pool => bytes.par_chunks_mut(row_bytes).enumerate().for_each(row),
            Parallelism::Serial => bytes.chunks_mut(row_bytes).enumerate().for_each(row),
        }
        bytes
    }

    fn write_tile(frame: &mut Arc<Vec<u8>>, stage: Stage, tile: Region, bytes: Vec<u8>) {
        let output = frame_mut(frame);
        let row_bytes = tile.width as usize * 4;
        for (row, y) in (tile.y0..tile.y1()).enumerate() {
            let to = ((u64::from(y) * u64::from(stage.width) + u64::from(tile.x0)) * 4) as usize;
            output[to..to + row_bytes]
                .copy_from_slice(&bytes[row * row_bytes..(row + 1) * row_bytes]);
        }
    }

    fn frame_pixel(frame: &Arc<Vec<u8>>, stage: Stage, x: u32, y: u32) -> [u8; 4] {
        let offset = ((u64::from(y) * u64::from(stage.width) + u64::from(x)) * 4) as usize;
        let pixel = &frame[offset..offset + 4];
        [pixel[0], pixel[1], pixel[2], pixel[3]]
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
        snapshot: &mut [[f32; 3]],
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
/// operation's scratch for its own input.
fn colour_byte_rows(
    run: &ColorRun<'_>,
    bytes: &mut [u8],
    width: usize,
    y0: u32,
    x0: u32,
    linear: &mut Vec<[f32; 3]>,
    snapshot: &mut [[f32; 3]],
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
/// boundary reads: a segment's input frame is the source, a resample of the frame before it or a
/// spatial operation's output, and one [`segment_pass`] writes the segment's frame from it through
/// its exact geometry, replacements and colour runs. A segment whose geometry is the identity writes
/// in place over a frame this render wrote, loads the shared source's rows into its new frame
/// inside the pass, and shares its input when it writes nothing, so an identity stack returns the
/// source allocation itself. At most two frames exist at once.
pub(super) fn rasterize(
    source: &SourceImage,
    compiled: &Compiled,
    snapshot_id: SnapshotId,
    cancel: &Cancel,
    tiling: spatial::Tiling,
    context: &RenderContext,
) -> Result<Raster, Error> {
    // A token already cancelled when the call arrives costs no frame at all.
    cancel.check()?;
    let domain = Byte(source);
    // The frame the next pass reads; `None` is the source itself. Every frame is the raster's
    // `Arc<Vec<u8>>` from the start, so the last one written is the one returned.
    let mut frame: Option<Arc<Vec<u8>>> = None;
    let (mut width, mut height) = (source.width, source.height);
    for (index, segment) in compiled.segments.iter().enumerate() {
        if let Some(entry) = &segment.entry {
            let input = frame.as_deref().unwrap_or(&source.rgba).as_slice();
            let next = match entry {
                Entry::Resample(resample) => resample_frame(
                    input,
                    width,
                    height,
                    *resample,
                    segment.entry_origin,
                    segment.resample_window(*resample),
                    cancel,
                )?,
                Entry::Spatial { .. } => {
                    let stage = Stage { width, height };
                    let offset = |x: u32, y: u32| {
                        ((u64::from(y) * u64::from(stage.width) + u64::from(x)) * 4) as usize
                    };
                    let table = decode_table();
                    let read = |x: u32, y: u32| -> Result<[f32; 3], Error> {
                        let at = offset(x, y);
                        Ok(decode_pixel_in(
                            table,
                            [input[at], input[at + 1], input[at + 2]],
                        ))
                    };
                    spatial_entry(
                        &domain,
                        SpatialEntry::of(segment).expect("a spatial entry"),
                        stage,
                        tiling,
                        cancel,
                        context,
                        |region, planes, parallelism| {
                            fill_planes(region, planes, parallelism, read)
                        },
                    )?
                }
            };
            #[cfg(test)]
            if matches!(entry, Entry::Resample(_)) {
                context.note_resample_bytes(next.len());
            }
            if let Some(resample) = entry.resample() {
                (width, height) = segment
                    .entry_window
                    .map(|window| (window.width, window.height))
                    .unwrap_or((resample.output_width, resample.output_height));
            }
            // The frame the boundary read is released before the next pass, so two frames is the
            // peak.
            frame = Some(next);
        }
        let band = band(compiled, index);
        if segment.geometry.is_identity(width, height) {
            // An identity pass with nothing to write shares its input instead of copying it. One
            // over a frame this render wrote writes it in place. The source is shared, so a pass
            // over it loads the source's rows into a new frame, chunk by chunk inside the pass.
            if segment.writes_pixels() {
                let (mut owned, input) = match frame.take() {
                    Some(owned) => (owned, None),
                    None => (
                        zeroed_frame(source.rgba.len()),
                        Some((source.rgba.as_slice(), width)),
                    ),
                };
                segment_pass(
                    &ByteRows::new(segment, input),
                    segment,
                    frame_mut(&mut owned),
                    band,
                    cancel,
                    context.scratch(),
                )?;
                frame = Some(owned);
            }
        } else {
            let input = frame.take();
            cancel.check()?;
            let mut next = zeroed_frame(Raster::expected_len(segment.width, segment.height)?);
            segment_pass(
                &ByteRows::new(
                    segment,
                    Some((input.as_deref().unwrap_or(&source.rgba).as_slice(), width)),
                ),
                segment,
                frame_mut(&mut next),
                band,
                cancel,
                context.scratch(),
            )?;
            // Released before the next pass, so two frames is the peak.
            drop(input);
            frame = Some(next);
            (width, height) = (segment.width, segment.height);
        }
    }
    Ok(Raster {
        width,
        height,
        rgba: frame.unwrap_or_else(|| source.rgba.clone()),
        source_fingerprint: source.fingerprint.clone(),
        snapshot_id,
    })
}

/// The rows of segment `index`'s frame its colour runs reach: those the resample after it reads
/// ([`crate::modules::Resample::reads`]), or every row when a spatial boundary, which reads every row plus a halo,
/// or nothing follows it. Everything outside the band is discarded by the resample, so leaving it
/// uncoloured changes no output byte.
pub(super) fn band(compiled: &Compiled, index: usize) -> std::ops::Range<usize> {
    let segment = &compiled.segments[index];
    let every = 0..segment.height as usize;
    let Some(next) = compiled.segments.get(index + 1) else {
        return every;
    };
    let Some(Entry::Resample(resample)) = next.entry else {
        return every;
    };
    resample
        .reads(
            next.entry_origin,
            next.resample_window(resample),
            segment.stage(),
        )
        .map_or(every, |read| read.y0 as usize..read.y1() as usize)
}
