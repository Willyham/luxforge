//! An export's output from the reference renderer's banded mode (`docs/design/export-settings.md`,
//! "Paged sources"): what [`Banded`] renders of a paged source's output stage, band by band, as the
//! bands of the export's [`OutputSpec`], every one of them the whole-frame reference's output
//! (`tiles::output::render_output`) over its rows, code for code, without the stage or the output
//! ever held whole.
//!
//! - **Full size, 8 bits.** The banded mode's own bands ([`Banded::bands`]), each the whole
//!   render's rows byte for byte.
//! - **Full size, 16 bits.** Bands of the wide terminal ([`Banded::wide`]), the values the whole
//!   frame's `Render::wide_frame` holds there, made opaque as `WideTerminal::into_rgba16` makes the
//!   whole frame's.
//! - **Smaller.** A streaming separable Lanczos-3 in linear light ([`Resampled`]): the whole
//!   frame's own taps (`lanczos_taps`), each stage row decoded and resampled across by the whole
//!   frame's [`across_row`] once, as its band of the wide terminal arrives, and each output row
//!   summed down by its [`down_row`] and encoded by its encoder, so every output code is the whole
//!   frame's resample's.
//! - **Sharpened.** Any of these, then sharpened as it arrives ([`SharpenStream`]), each row from
//!   the unsharpened rows by the whole frame's own row functions.
//!
//! # Bounds
//!
//! Beside the banded mode's own (a band of at most the evaluated-frame limit, its windows within
//! [`super::bands::BAND_WINDOW_BYTES`]):
//!
//! - a 16-bit band of the wide terminal and its opaque codes, together within the evaluated-frame
//!   limit (`6 + 8` bytes a pixel);
//! - a resized output's one band of the wide terminal (within the evaluated-frame limit at six bytes
//!   a pixel) and its rolling window of stage rows resampled across: the rows the vertical taps of
//!   [`RESAMPLE_BAND_ROWS`] output rows read, about `(16 + 6) × scale` rows of `24 × the output's
//!   width` bytes, so about `(16 + 6) × 24 × the stage's width` bytes at any reduction (34 MB at
//!   65,000 pixels), plus one band of output rows;
//! - the output sharpening's rows not yet handed on (a band and the kernel's radius of rows before
//!   it) and its rolling rows of luma ([`SharpenStream`]).
//!
//! Every band and every group of output rows checks the cancellation before it starts.
use super::{
    bands::Banded,
    export_output::{
        RESAMPLE_BAND_ROWS, Taps, WideTerminal, across_row, down_row, encode8, encode16,
        lanczos_taps,
    },
    parallel::{RenderPass, pooled},
};
use crate::{
    Cancel, Error,
    colour::srgb::decode16_table,
    modules::Region,
    tiles::{Depth, OutputSpec, Pixels, sharpen::SharpenStream},
};
use rayon::prelude::*;
use std::collections::VecDeque;

/// The bands of an export's output, from the top, each its row count and its pixels at the
/// output's depth.
pub(crate) type OutputBands<'a> = Box<dyn Iterator<Item = Result<(u32, Pixels), Error>> + 'a>;

/// `output` of `banded`'s output stage, in bands from the top, each the whole-frame reference's
/// output over its rows ([module docs](self)); `cancel` is checked before every band and group of
/// rows. An output larger than the stage or empty is refused with `validation`, as the whole-frame
/// reference refuses it.
pub(crate) fn output_bands<'b>(
    banded: &'b Banded<'_>,
    output: OutputSpec,
    cancel: &'b Cancel,
) -> Result<OutputBands<'b>, Error> {
    let stage = banded.stage();
    output.check(stage.width, stage.height)?;
    let bands: OutputBands<'b> = if !output.is_full(stage.width, stage.height) {
        Box::new(Resampled::new(banded, output, cancel)?)
    } else {
        match output.depth {
            Depth::Eight => Box::new(
                banded
                    .bands()
                    .map(|band| band.map(|(rows, rgba)| (rows, Pixels::Eight(rgba)))),
            ),
            Depth::Sixteen => Box::new(Wide16::new(banded, cancel)),
        }
    };
    Ok(match output.sharpen {
        None => bands,
        Some(sharpen) => Box::new(Sharpened {
            bands,
            stream: SharpenStream::new(
                sharpen.kernel(),
                (output.width, output.height),
                output.depth,
            ),
            ready: VecDeque::new(),
            cancel,
            failed: false,
        }),
    })
}

/// A full-size 16-bit output in bands of the wide terminal, each made opaque as the whole frame's
/// is.
struct Wide16<'b, 'a> {
    banded: &'b Banded<'a>,
    cancel: &'b Cancel,
    rows: u32,
    y0: u32,
    failed: bool,
}

impl<'b, 'a> Wide16<'b, 'a> {
    fn new(banded: &'b Banded<'a>, cancel: &'b Cancel) -> Self {
        Self {
            banded,
            cancel,
            // The wide band and its opaque codes beside it, six and eight bytes a pixel.
            rows: banded.wide_band_rows(6 + 8),
            y0: 0,
            failed: false,
        }
    }

    fn band(&mut self) -> Result<(u32, Pixels), Error> {
        self.cancel.check()?;
        let stage = self.banded.stage();
        let rows = self.rows.min(stage.height - self.y0);
        let wide = self.banded.wide(Region {
            x0: 0,
            y0: self.y0,
            width: stage.width,
            height: rows,
        })?;
        self.y0 += rows;
        let codes = WideTerminal::bytes(stage.width, rows, wide).into_rgba16(self.cancel)?;
        Ok((rows, Pixels::Sixteen(codes)))
    }
}

impl Iterator for Wide16<'_, '_> {
    type Item = Result<(u32, Pixels), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.y0 >= self.banded.stage().height {
            return None;
        }
        let band = self.band();
        self.failed = band.is_err();
        Some(band)
    }
}

/// A resized output, resampled as its stage's wide terminal arrives in bands
/// ([module docs](self)): [`RESAMPLE_BAND_ROWS`] output rows at a time, each group from the stage
/// rows its taps read, resampled across once and kept while a later group reads them.
struct Resampled<'b, 'a> {
    banded: &'b Banded<'a>,
    cancel: &'b Cancel,
    depth: Depth,
    columns: Vec<Taps>,
    rows: Vec<Taps>,
    /// Stage rows in one band of the wide terminal.
    band_rows: u32,
    pool: bool,
    table: &'static [f32; 65536],
    /// The band of the wide terminal being read: its first row, its row count and its codes.
    held: Option<(u32, u32, WideTerminal)>,
    /// The stage rows resampled across, from row `first` on.
    across: VecDeque<Vec<[f64; 3]>>,
    first: u32,
    /// The next output row.
    next: u32,
    failed: bool,
    /// The most stage rows resampled across held at once, for the tests that hold it to its bound.
    #[cfg(test)]
    peak: usize,
}

impl<'b, 'a> Resampled<'b, 'a> {
    fn new(banded: &'b Banded<'a>, output: OutputSpec, cancel: &'b Cancel) -> Result<Self, Error> {
        let stage = banded.stage();
        Ok(Self {
            banded,
            cancel,
            depth: output.depth,
            columns: lanczos_taps(stage.width, output.width)?,
            rows: lanczos_taps(stage.height, output.height)?,
            band_rows: banded.wide_band_rows(6),
            pool: pooled(
                RenderPass::Resample,
                u64::from(output.width) * u64::from(output.height),
            ),
            table: decode16_table(),
            held: None,
            across: VecDeque::new(),
            first: 0,
            next: 0,
            failed: false,
            #[cfg(test)]
            peak: 0,
        })
    }

    /// Stage rows `first + across.len()` to `end` resampled across, each band of the wide terminal
    /// read once, as its first row is needed, and let go before the next is read.
    fn read_until(&mut self, end: u32) -> Result<(), Error> {
        let stage = self.banded.stage();
        while self.first + (self.across.len() as u32) < end {
            let y = self.first + self.across.len() as u32;
            if !matches!(self.held, Some((y0, rows, _)) if (y0..y0 + rows).contains(&y)) {
                self.held = None;
                self.cancel.check()?;
                let rows = self.band_rows.min(stage.height - y);
                let wide = self.banded.wide(Region {
                    x0: 0,
                    y0: y,
                    width: stage.width,
                    height: rows,
                })?;
                self.held = Some((y, rows, WideTerminal::bytes(stage.width, rows, wide)));
            }
            let (y0, rows, terminal) = self.held.as_ref().expect("a band of the wide terminal");
            let upto = end.min(y0 + rows);
            let (columns, table, width) = (&self.columns, self.table, stage.width as usize);
            let resample = |row: u32| {
                let mut line = Vec::with_capacity(width);
                terminal.decode_row(row - y0, table, &mut line);
                let mut across = vec![[0.0f64; 3]; columns.len()];
                across_row(&line, columns, &mut across);
                across
            };
            let fresh: Vec<Vec<[f64; 3]>> = if self.pool {
                (y..upto).into_par_iter().map(resample).collect()
            } else {
                (y..upto).map(resample).collect()
            };
            self.across.extend(fresh);
        }
        Ok(())
    }

    /// The next group of output rows.
    fn group(&mut self) -> Result<(u32, Pixels), Error> {
        self.cancel.check()?;
        let height = self.rows.len() as u32;
        let (o0, o1) = (self.next, (self.next + RESAMPLE_BAND_ROWS).min(height));
        let taps = &self.rows[o0 as usize..o1 as usize];
        let start = taps.iter().map(|taps| taps.first).min().unwrap_or(0);
        let end = taps
            .iter()
            .map(|taps| taps.first + taps.weights.len() as u32)
            .max()
            .unwrap_or(start);
        // The rows above the group's taps are read by no later group: the taps only move down.
        while self.first < start && !self.across.is_empty() {
            self.across.pop_front();
            self.first += 1;
        }
        if self.across.is_empty() {
            self.first = start;
        }
        self.read_until(end)?;
        #[cfg(test)]
        {
            self.peak = self.peak.max(self.across.len());
        }
        let rows = (o1 - o0) as usize;
        let pixels = match self.depth {
            Depth::Eight => {
                let mut rgba = vec![0u8; rows * self.columns.len() * 4];
                self.down(o0, &mut rgba, encode8());
                Pixels::Eight(rgba)
            }
            Depth::Sixteen => {
                let mut rgba = vec![0u16; rows * self.columns.len() * 4];
                self.down(o0, &mut rgba, encode16());
                Pixels::Sixteen(rgba)
            }
        };
        self.next = o1;
        Ok((o1 - o0, pixels))
    }

    /// Output rows from `o0` into `out`, each summed down from the rows resampled across.
    fn down<T: Copy + Send>(
        &self,
        o0: u32,
        out: &mut [T],
        encode: impl Fn([f64; 3]) -> [T; 4] + Sync,
    ) {
        let width = self.columns.len();
        let (across, first) = (&self.across, self.first);
        let row = |y: u32| -> &[[f64; 3]] { &across[(y - first) as usize] };
        let write = |(offset, pixels): (usize, &mut [T])| {
            let mut sums = vec![[0.0f64; 3]; width];
            down_row(
                &self.rows[o0 as usize + offset],
                row,
                &mut sums,
                pixels,
                &encode,
            );
        };
        if self.pool {
            out.par_chunks_mut(width * 4).enumerate().for_each(write);
        } else {
            out.chunks_mut(width * 4).enumerate().for_each(write);
        }
    }
}

impl Iterator for Resampled<'_, '_> {
    type Item = Result<(u32, Pixels), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.next as usize >= self.rows.len() {
            return None;
        }
        let group = self.group();
        self.failed = group.is_err();
        if self.failed || self.next as usize >= self.rows.len() {
            // Nothing more is read: the last band and the rows across go now.
            self.held = None;
            self.across.clear();
        }
        Some(group)
    }
}

/// An output's bands sharpened as they arrive ([`SharpenStream`]).
struct Sharpened<'b> {
    bands: OutputBands<'b>,
    stream: SharpenStream,
    /// The sharpened bands not yet taken.
    ready: VecDeque<(u32, Pixels)>,
    cancel: &'b Cancel,
    failed: bool,
}

impl Iterator for Sharpened<'_> {
    type Item = Result<(u32, Pixels), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(band) = self.ready.pop_front() {
                return Some(Ok(band));
            }
            if self.failed {
                return None;
            }
            let pushed = match self.bands.next()? {
                Ok((rows, pixels)) => self.stream.push(rows, pixels, self.cancel),
                Err(error) => Err(error),
            };
            match pushed {
                Ok(ready) => self.ready.extend(ready),
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "bands_output_tests.rs"]
mod tests;
