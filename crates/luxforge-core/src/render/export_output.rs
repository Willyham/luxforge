//! An export's output from the reference renderer (`docs/design/export-settings.md`, "Rendering"):
//! the output stage's terminal encoded once at 16 bits, and an output smaller than the stage
//! resampled from it in linear light.
//!
//! - **The wide terminal.** [`WideTerminal`] holds the values the 8-bit frame quantizes to its
//!   codes, quantized instead by the 16-bit output quantizer ([`super::Render::wide_frame`]): the
//!   last colour run's, or the last spatial operation's, encoded once at 16 bits. A 16-bit output
//!   is these codes, opaque ([`WideTerminal::into_rgba16`]); a resized output resamples them, so
//!   nothing an export writes is quantized to 8 bits before it is resampled.
//! - **The resample.** A separable Lanczos-3 filter in linear light whose support scales with the
//!   reduction ([`lanczos_taps`]): each output column's and row's taps are the stage pixels within
//!   three of its own pixels of its centre, weighted by the kernel at their distance in output
//!   pixels and normalized to sum to one, the stage's edge dropping the taps past it. Each output
//!   pixel is `Σ_y w_y · (Σ_x w_x · v(x, y))` in `f64`, both sums in ascending order, from the
//!   wide terminal's codes decoded through the 16-bit table, and is then clamped to `[0, 1]`,
//!   ringing included, and encoded once by the output quantizer of its depth ([`resample`]).
//! - **Bounds.** The output frame is at most the stage's size, held to the evaluated-frame limit
//!   at its depth (512 MiB). The rows are resampled in bands of [`RESAMPLE_BAND_ROWS`] output rows,
//!   on the shared pool from the resample pass's threshold, each band holding only the stage rows
//!   its taps read resampled across, `f64` a channel: about `(16 + 6) × the stage's width` values
//!   for a band at any reduction. Every band checks the cancellation before it starts.

use super::{
    Raster,
    parallel::{RenderPass, pooled},
};
use crate::{
    Cancel, Error,
    colour::srgb::{decode16_table, quantizer, quantizer16},
    tiles::{Depth, OutputSpec, Pixels},
};
use rayon::prelude::*;

/// The lobes of the resampling kernel: Lanczos-3.
pub const LANCZOS_LOBES: f64 = 3.0;

/// How many output rows one band of a resample writes ([module docs](self), "Bounds").
pub const RESAMPLE_BAND_ROWS: u32 = 16;

/// An output stage's terminal values encoded once at 16 bits: red, green and blue codes a pixel,
/// row by row ([module docs](self)). The byte domain hands its wide frame's codes over as they are;
/// the linear path's pass writes them as native-order bytes, which are read as they lie.
pub struct WideTerminal {
    width: u32,
    height: u32,
    samples: WideSamples,
}

enum WideSamples {
    Words(Vec<u16>),
    Bytes(Vec<u8>),
}

impl WideTerminal {
    /// A terminal of three codes a pixel.
    pub(crate) fn words(width: u32, height: u32, rgb: Vec<u16>) -> Self {
        debug_assert_eq!(rgb.len(), width as usize * height as usize * 3);
        Self {
            width,
            height,
            samples: WideSamples::Words(rgb),
        }
    }

    /// A terminal of three native-order codes a pixel, six bytes.
    pub(crate) fn bytes(width: u32, height: u32, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(bytes.len(), width as usize * height as usize * 6);
        Self {
            width,
            height,
            samples: WideSamples::Bytes(bytes),
        }
    }

    /// The codes of the pixel at `index`, counted row by row.
    #[inline]
    pub(crate) fn codes(&self, index: usize) -> [u16; 3] {
        match &self.samples {
            WideSamples::Words(rgb) => [rgb[index * 3], rgb[index * 3 + 1], rgb[index * 3 + 2]],
            WideSamples::Bytes(bytes) => {
                let at = index * 6;
                std::array::from_fn(|channel| {
                    u16::from_ne_bytes([bytes[at + channel * 2], bytes[at + channel * 2 + 1]])
                })
            }
        }
    }

    /// Row `y` in linear light, each code decoded through `table`, written over `into`.
    fn decode_row(&self, y: u32, table: &[f32; 65536], into: &mut Vec<[f32; 3]>) {
        let width = self.width as usize;
        let first = y as usize * width;
        into.clear();
        into.extend(
            (first..first + width)
                .map(|index| self.codes(index).map(|code| table[usize::from(code)])),
        );
    }

    /// The opaque 16-bit output of these codes: red, green, blue and 65,535 a pixel, row by row,
    /// held to the evaluated-frame limit at eight bytes a pixel. The byte domain's codes are
    /// widened in their own allocation, which grows by a third; the linear path's are read into a
    /// new one. Checks `cancel` before every row.
    pub(crate) fn into_rgba16(self, cancel: &Cancel) -> Result<Vec<u16>, Error> {
        let samples = Raster::expected_len16(self.width, self.height)?;
        let width = self.width as usize;
        match self.samples {
            WideSamples::Words(mut rgb) => {
                let pixels = rgb.len() / 3;
                rgb.resize(samples, 0);
                // From the last pixel back, so no code is overwritten before it is moved.
                for row in (0..self.height as usize).rev() {
                    cancel.check()?;
                    for index in (row * width..(row + 1) * width).rev() {
                        rgb.copy_within(index * 3..index * 3 + 3, index * 4);
                        rgb[index * 4 + 3] = u16::MAX;
                    }
                }
                debug_assert_eq!(pixels * 4, rgb.len());
                Ok(rgb)
            }
            WideSamples::Bytes(bytes) => {
                let mut rgba = Vec::with_capacity(samples);
                for row in bytes.chunks_exact(width * 6) {
                    cancel.check()?;
                    for pixel in row.chunks_exact(6) {
                        rgba.extend([
                            u16::from_ne_bytes([pixel[0], pixel[1]]),
                            u16::from_ne_bytes([pixel[2], pixel[3]]),
                            u16::from_ne_bytes([pixel[4], pixel[5]]),
                            u16::MAX,
                        ]);
                    }
                }
                Ok(rgba)
            }
        }
    }
}

/// One output column's or row's taps: the stage pixels from `first` on, each weighted, the weights
/// summing to one.
#[derive(Clone, Debug, PartialEq)]
pub struct Taps {
    pub first: u32,
    pub weights: Vec<f64>,
}

/// The Lanczos-3 kernel at `x`, in output pixels: `sinc(x) · sinc(x / 3)` inside three, zero
/// outside, one at zero and zero at every other whole number.
fn lanczos(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    if x.abs() >= LANCZOS_LOBES || x.fract() == 0.0 {
        return 0.0;
    }
    let pi_x = std::f64::consts::PI * x;
    LANCZOS_LOBES * pi_x.sin() * (pi_x / LANCZOS_LOBES).sin() / (pi_x * pi_x)
}

/// The taps of each of `to` output pixels along a side of `from` stage pixels, `to` at most
/// `from`: the separable Lanczos-3 filter whose support scales with the reduction ([module
/// docs](self)). Output pixel `i`'s centre lies at `(i + ½) · from / to` stage pixels; stage pixel
/// `k`, whose centre lies at `k + ½`, is weighted by the kernel at its distance from that centre in
/// output pixels, and the taps the side holds are normalized to sum to one, their own sum taken in
/// ascending order. A side kept at its size is its own pixels, each with the weight one. Refused,
/// with `validation`, for an empty side or one larger than `from`.
pub fn lanczos_taps(from: u32, to: u32) -> Result<Vec<Taps>, Error> {
    if from == 0 || to == 0 || to > from {
        return Err(Error::validation(format!(
            "an output side of {to} pixels cannot be resampled from {from}: it is never larger \
             than the stage, nor empty"
        )));
    }
    if to == from {
        return Ok((0..to)
            .map(|first| Taps {
                first,
                weights: vec![1.0],
            })
            .collect());
    }
    let scale = f64::from(from) / f64::from(to);
    let radius = LANCZOS_LOBES * scale;
    Ok((0..to)
        .map(|index| {
            let centre = (f64::from(index) + 0.5) * scale;
            let first = (centre - radius).floor().max(0.0) as u32;
            let end = ((centre + radius).ceil() as u64).min(u64::from(from)) as u32;
            let mut weights: Vec<f64> = (first..end)
                .map(|k| lanczos((f64::from(k) + 0.5 - centre) / scale))
                .collect();
            let sum: f64 = weights.iter().sum();
            for weight in &mut weights {
                *weight /= sum;
            }
            Taps { first, weights }
        })
        .collect())
}

/// `terminal` resampled to `output`, which is at most its size on both sides, and encoded once at
/// the output's depth ([module docs](self)): opaque RGBA rows, 8-bit codes through the output
/// quantizer or 16-bit codes through the 16-bit one, each value clamped to `[0, 1]` first. A side
/// kept at the stage's size is copied through weights of one, exactly. The output frame is held to
/// the evaluated-frame limit at its depth; `cancel` is checked before every band.
pub(crate) fn resample(
    terminal: &WideTerminal,
    output: OutputSpec,
    cancel: &Cancel,
) -> Result<Pixels, Error> {
    let columns = lanczos_taps(terminal.width, output.width)?;
    let rows = lanczos_taps(terminal.height, output.height)?;
    match output.depth {
        Depth::Eight => {
            let mut rgba = vec![0u8; Raster::expected_len(output.width, output.height)?];
            let quantizer = quantizer();
            resample_into(terminal, (&columns, &rows), &mut rgba, cancel, |sum| {
                [
                    quantizer.channel(sum[0]),
                    quantizer.channel(sum[1]),
                    quantizer.channel(sum[2]),
                    u8::MAX,
                ]
            })?;
            Ok(Pixels::Eight(rgba))
        }
        Depth::Sixteen => {
            let mut rgba = vec![0u16; Raster::expected_len16(output.width, output.height)?];
            let quantizer = quantizer16();
            resample_into(terminal, (&columns, &rows), &mut rgba, cancel, |sum| {
                [
                    quantizer.channel(sum[0] as f32),
                    quantizer.channel(sum[1] as f32),
                    quantizer.channel(sum[2] as f32),
                    u16::MAX,
                ]
            })?;
            Ok(Pixels::Sixteen(rgba))
        }
    }
}

/// [`resample`] into `out`, opaque RGBA rows of `columns.len()` pixels, each pixel's linear sum
/// encoded by `encode`: band by band, each band on the pool past the resample pass's threshold.
fn resample_into<T: Copy + Send>(
    terminal: &WideTerminal,
    (columns, rows): (&[Taps], &[Taps]),
    out: &mut [T],
    cancel: &Cancel,
    encode: impl Fn([f64; 3]) -> [T; 4] + Sync,
) -> Result<(), Error> {
    let width = columns.len();
    let table = decode16_table();
    let band = |(index, chunk): (usize, &mut [T])| -> Result<(), Error> {
        cancel.check()?;
        let start = index * RESAMPLE_BAND_ROWS as usize;
        let taps = &rows[start..start + chunk.len() / (width * 4)];
        // The stage rows the band's taps read, each resampled across once.
        let first = taps.iter().map(|taps| taps.first).min().unwrap_or(0);
        let end = taps
            .iter()
            .map(|taps| taps.first + taps.weights.len() as u32)
            .max()
            .unwrap_or(first);
        let mut line = Vec::with_capacity(terminal.width as usize);
        let mut across = vec![[0.0f64; 3]; (end - first) as usize * width];
        for (y, resampled) in (first..end).zip(across.chunks_exact_mut(width)) {
            terminal.decode_row(y, table, &mut line);
            for (value, taps) in resampled.iter_mut().zip(columns) {
                let read = &line[taps.first as usize..];
                let mut sum = [0.0f64; 3];
                for (weight, pixel) in taps.weights.iter().zip(read) {
                    for channel in 0..3 {
                        sum[channel] += weight * f64::from(pixel[channel]);
                    }
                }
                *value = sum;
            }
        }
        // Down each column, every output pixel's sum taken in ascending stage rows.
        let mut sums = vec![[0.0f64; 3]; width];
        for (taps, pixels) in taps.iter().zip(chunk.chunks_exact_mut(width * 4)) {
            sums.fill([0.0; 3]);
            for (row, weight) in taps.weights.iter().enumerate() {
                let at = (taps.first - first) as usize + row;
                for (sum, value) in sums.iter_mut().zip(&across[at * width..(at + 1) * width]) {
                    for channel in 0..3 {
                        sum[channel] += weight * value[channel];
                    }
                }
            }
            for (pixel, sum) in pixels.chunks_exact_mut(4).zip(&sums) {
                pixel.copy_from_slice(&encode(*sum));
            }
        }
        Ok(())
    };
    let samples = RESAMPLE_BAND_ROWS as usize * width * 4;
    let pixels = (out.len() / 4) as u64;
    if pooled(RenderPass::Resample, pixels) {
        out.par_chunks_mut(samples).enumerate().try_for_each(band)
    } else {
        out.chunks_mut(samples).enumerate().try_for_each(band)
    }
}

#[cfg(test)]
#[path = "export_output_tests.rs"]
mod tests;
