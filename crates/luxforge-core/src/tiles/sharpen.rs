//! Output sharpening (`docs/design/export-settings.md`, "Output sharpening"): an export setting,
//! never a recipe layer, applied to the output's own codes after resizing, at the final pixel size.
//!
//! - **Domain.** The output's encoded codes at its depth, 0–255 or 0–65,535: the sRGB-encoded
//!   (gamma) values the encoder is handed, after the reference's resample or the GPU's cut has
//!   encoded them once. Nothing is decoded to linear light.
//! - **Luminance only.** Each pixel's luma is `Y′ = (2126 R′ + 7152 G′ + 722 B′) / 10000`, the
//!   Rec. 709 weights on the encoded codes. An unsharp mask of `Y′` gives one change `d` a pixel,
//!   which is added to all three channels alike. The differences between the channels, and so
//!   every chroma of `Y′CbCr`, are unchanged, so it adds no colour fringes.
//! - **The unsharp mask.** `d = round(a · (Y′ − G ∗ Y′))`, `G` a separable Gaussian of standard
//!   deviation `σ` truncated at `r = ⌈3σ⌉` taps a side, rounded to whole weights summing to
//!   [`WEIGHT_SUM`] (4,096) on each axis, and `a` the amount as a whole percent. Rounding is half
//!   away from zero. Every sum is in whole numbers (`i64`), so the separable blur equals the
//!   two-dimensional one exactly and the result never depends on the band or thread order.
//! - **Borders and clamping.** A tap past the image's edge reads the edge pixel (clamp to edge).
//!   `d` is then limited to `[−min(R′, G′, B′), M − max(R′, G′, B′)]`, `M` the depth's largest
//!   code, so no channel clips and the chroma stays exact even on a saturated edge, where the
//!   sharpening is held back instead.
//! - **Radius and amount** ([`OutputSharpen::kernel`]). Screen uses `σ` = 0.6 px. Glossy and Matte
//!   derive `σ` from the output density, `ppi / 360` and `ppi / 240` (Matte the stronger: about
//!   0.07 mm and 0.11 mm on paper), at [`DEFAULT_PRINT_DENSITY`] (300 ppi) when the settings name
//!   none, and `σ` is held to [`SIGMA_RANGE`]. Low, Standard and High scale the medium's amount:
//!   [`amount_percent`].
//! - **Bounds.** [`sharpen`] works in place on the output frame, a band of [`SHARPEN_BAND_ROWS`]
//!   rows at a time, holding the luma and the sums across of the band's rows and its `r` rows
//!   either side, about `(64 + 2r) × width × 12` bytes, never a second frame. Each band checks the
//!   cancellation before it starts.

use super::output::{Depth, Pixels};
use crate::{
    Cancel, Error,
    render::parallel::{RenderPass, pooled},
};
use rayon::prelude::*;
use std::collections::VecDeque;

/// The output sharpening's medium: what the export will be seen on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SharpenMedium {
    Screen,
    Matte,
    Glossy,
}

impl SharpenMedium {
    pub const ALL: [Self; 3] = [Self::Screen, Self::Matte, Self::Glossy];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Matte => "matte",
            Self::Glossy => "glossy",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|medium| medium.as_str() == value)
    }

    /// Whether its radius follows the output density: Matte and Glossy paper.
    pub fn print(self) -> bool {
        self != Self::Screen
    }
}

/// How strongly the output is sharpened for its medium.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SharpenAmount {
    Low,
    Standard,
    High,
}

impl SharpenAmount {
    pub const ALL: [Self; 3] = [Self::Low, Self::Standard, Self::High];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Standard => "standard",
            Self::High => "high",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|amount| amount.as_str() == value)
    }
}

/// An export's output sharpening, as its settings name it: `{medium, amount}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sharpen {
    pub medium: SharpenMedium,
    pub amount: SharpenAmount,
}

/// The density Matte and Glossy sharpen for when the settings name none.
pub const DEFAULT_PRINT_DENSITY: u16 = 300;

/// Screen's standard deviation, in output pixels.
pub const SCREEN_SIGMA: f64 = 0.6;

/// Glossy's and Matte's standard deviation, in inches: `ppi / 360` and `ppi / 240` pixels.
pub const GLOSSY_SIGMA_INCHES: f64 = 1.0 / 360.0;
pub const MATTE_SIGMA_INCHES: f64 = 1.0 / 240.0;

/// The standard deviations a kernel may take, in output pixels.
pub const SIGMA_RANGE: (f64, f64) = (0.5, 3.0);

/// What each axis's whole weights sum to.
pub const WEIGHT_SUM: i64 = 4096;

/// The luma weights, in ten-thousandths: Rec. 709's on the encoded codes.
pub const LUMA_WEIGHTS: [i64; 3] = [2126, 7152, 722];

/// What the luma weights sum to.
pub const LUMA_SUM: i64 = 10_000;

/// How many output rows [`sharpen`] writes at a time.
pub const SHARPEN_BAND_ROWS: u32 = 64;

/// The amount, as a whole percent of the high-pass, of `medium` at `amount`: Screen 40, 70 and
/// 110; Glossy 50, 90 and 140; Matte 60, 110 and 170, for Low, Standard and High.
pub const fn amount_percent(medium: SharpenMedium, amount: SharpenAmount) -> i64 {
    let [low, standard, high] = match medium {
        SharpenMedium::Screen => [40, 70, 110],
        SharpenMedium::Glossy => [50, 90, 140],
        SharpenMedium::Matte => [60, 110, 170],
    };
    match amount {
        SharpenAmount::Low => low,
        SharpenAmount::Standard => standard,
        SharpenAmount::High => high,
    }
}

/// What an export renderer is asked to sharpen with: the setting and the density its radius
/// follows, the settings' own or [`DEFAULT_PRINT_DENSITY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutputSharpen {
    pub sharpen: Sharpen,
    pub pixels_per_inch: u16,
}

impl OutputSharpen {
    /// `sharpen` at the settings' density, `None` for [`DEFAULT_PRINT_DENSITY`].
    pub fn new(sharpen: Sharpen, pixels_per_inch: Option<u16>) -> Self {
        Self {
            sharpen,
            pixels_per_inch: pixels_per_inch.unwrap_or(DEFAULT_PRINT_DENSITY).max(1),
        }
    }

    /// The standard deviation of its Gaussian, in output pixels ([module docs](self)).
    pub fn sigma(&self) -> f64 {
        let density = f64::from(self.pixels_per_inch);
        let sigma = match self.sharpen.medium {
            SharpenMedium::Screen => SCREEN_SIGMA,
            SharpenMedium::Glossy => density * GLOSSY_SIGMA_INCHES,
            SharpenMedium::Matte => density * MATTE_SIGMA_INCHES,
        };
        sigma.clamp(SIGMA_RANGE.0, SIGMA_RANGE.1)
    }

    /// Its kernel: the whole Gaussian weights of one axis and the amount.
    pub fn kernel(&self) -> SharpenKernel {
        SharpenKernel::gaussian(
            self.sigma(),
            amount_percent(self.sharpen.medium, self.sharpen.amount),
        )
    }
}

/// An unsharp mask's constants: one axis's whole weights, `2r + 1` of them from `−r` to `r`,
/// summing to [`WEIGHT_SUM`], and the amount as a whole percent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharpenKernel {
    pub weights: Vec<i64>,
    pub percent: i64,
}

impl SharpenKernel {
    /// A Gaussian of `sigma` truncated at `⌈3σ⌉` taps a side, each weight `exp(−k² / 2σ²)` over
    /// their sum, scaled to [`WEIGHT_SUM`] and rounded, the centre taking what rounding left over.
    pub fn gaussian(sigma: f64, percent: i64) -> Self {
        let radius = (3.0 * sigma).ceil() as i64;
        let raw: Vec<f64> = (-radius..=radius)
            .map(|k| (-((k * k) as f64) / (2.0 * sigma * sigma)).exp())
            .collect();
        let total: f64 = raw.iter().sum();
        let mut weights: Vec<i64> = raw
            .iter()
            .map(|weight| (weight / total * WEIGHT_SUM as f64).round() as i64)
            .collect();
        let sum: i64 = weights.iter().sum();
        weights[radius as usize] += WEIGHT_SUM - sum;
        Self { weights, percent }
    }

    /// Taps a side, `r`: the rows and columns a pixel's change reads either side of it.
    pub fn radius(&self) -> u32 {
        (self.weights.len() / 2) as u32
    }

    /// The change of a pixel whose luma, times [`LUMA_SUM`], is `luma` and whose blurred luma, times
    /// [`LUMA_SUM`] and [`WEIGHT_SUM`] squared, is `blurred`: `a · (Y′ − G ∗ Y′)` in codes,
    /// rounded half away from zero.
    #[inline]
    pub fn change(&self, luma: i64, blurred: i64) -> i64 {
        let high = luma * WEIGHT_SUM * WEIGHT_SUM - blurred;
        let scaled = self.percent * high;
        let denominator = 100 * LUMA_SUM * WEIGHT_SUM * WEIGHT_SUM;
        let magnitude = (scaled.abs() + denominator / 2) / denominator;
        if scaled < 0 { -magnitude } else { magnitude }
    }
}

/// The codes of one depth that [`sharpen`] reads and writes.
trait Code: Copy + Send + Sync {
    const MAX: i64;
    fn get(self) -> i64;
    fn set(value: i64) -> Self;
}

impl Code for u8 {
    const MAX: i64 = u8::MAX as i64;
    fn get(self) -> i64 {
        i64::from(self)
    }
    fn set(value: i64) -> Self {
        value as u8
    }
}

impl Code for u16 {
    const MAX: i64 = u16::MAX as i64;
    fn get(self) -> i64 {
        i64::from(self)
    }
    fn set(value: i64) -> Self {
        value as u16
    }
}

/// The luma of an RGBA pixel, times [`LUMA_SUM`].
#[inline]
fn luma<T: Code>(pixel: &[T]) -> i64 {
    LUMA_WEIGHTS[0] * pixel[0].get()
        + LUMA_WEIGHTS[1] * pixel[1].get()
        + LUMA_WEIGHTS[2] * pixel[2].get()
}

/// `pixel` with `change` added to its three channels, limited so none leaves `[0, MAX]`.
#[inline]
fn apply<T: Code>(pixel: &mut [T], change: i64) {
    let (r, g, b) = (pixel[0].get(), pixel[1].get(), pixel[2].get());
    let low = -r.min(g).min(b);
    let high = T::MAX - r.max(g).max(b);
    let change = change.clamp(low, high);
    pixel[0] = T::set(r + change);
    pixel[1] = T::set(g + change);
    pixel[2] = T::set(b + change);
}

/// `pixels`, an output of `width` × `height` opaque RGBA pixels, sharpened in place by `kernel`
/// ([module docs](self)), band by band; `cancel` is checked before each band.
pub fn sharpen(
    pixels: &mut Pixels,
    (width, height): (u32, u32),
    kernel: &SharpenKernel,
    cancel: &Cancel,
) -> Result<(), Error> {
    sharpen_in_bands(pixels, (width, height), kernel, SHARPEN_BAND_ROWS, cancel)
}

/// [`sharpen`] in bands of `band` rows, which a test sets small to cross many band edges.
pub(crate) fn sharpen_in_bands(
    pixels: &mut Pixels,
    size: (u32, u32),
    kernel: &SharpenKernel,
    band: u32,
    cancel: &Cancel,
) -> Result<(), Error> {
    match pixels {
        Pixels::Eight(samples) => sharpen_codes(samples, size, kernel, band, cancel),
        Pixels::Sixteen(samples) => sharpen_codes(samples, size, kernel, band, cancel),
    }
}

/// One source row's luma and its sums across, kept while a band's rows read it.
struct Row {
    luma: Vec<i64>,
    across: Vec<i64>,
}

impl Row {
    /// The luma of `line`, a row of opaque RGBA codes, and its blur across by `weights`, `radius`
    /// taps a side, a tap past either end reading the edge pixel.
    fn of<T: Code>(line: &[T], weights: &[i64], radius: usize) -> Self {
        let w = line.len() / 4;
        let luma: Vec<i64> = line.chunks_exact(4).map(luma).collect();
        let across = (0..w)
            .map(|x| {
                weights
                    .iter()
                    .enumerate()
                    .map(|(k, weight)| {
                        let at = (x + k).saturating_sub(radius).min(w - 1);
                        weight * luma[at]
                    })
                    .sum()
            })
            .collect();
        Self { luma, across }
    }
}

/// Sharpen `line`, row `y` of an output `h` rows high, in place by `kernel`, from `kept`, the rows
/// of the unsharpened output from row `first` on, which hold every row its kernel reads (`y − r`
/// to `y + r`, held to the output's edges). The one place a row is sharpened, for the whole frame
/// ([`sharpen`]) and for an output that arrives in bands ([`SharpenStream`]) alike.
#[inline]
fn sharpen_line<T: Code>(
    line: &mut [T],
    y: usize,
    h: usize,
    (kept, first): (&[Row], usize),
    kernel: &SharpenKernel,
) {
    let radius = kernel.radius() as usize;
    let own = &kept[y - first].luma;
    for (x, pixel) in line.chunks_exact_mut(4).enumerate() {
        let blurred: i64 = kernel
            .weights
            .iter()
            .enumerate()
            .map(|(k, weight)| {
                let at = (y + k).saturating_sub(radius).min(h - 1);
                weight * kept[at - first].across[x]
            })
            .sum();
        apply(pixel, kernel.change(own[x], blurred));
    }
}

fn sharpen_codes<T: Code>(
    samples: &mut [T],
    (width, height): (u32, u32),
    kernel: &SharpenKernel,
    band: u32,
    cancel: &Cancel,
) -> Result<(), Error> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return Ok(());
    }
    if samples.len() != w * h * 4 {
        return Err(Error::internal(format!(
            "a {width}x{height} output holds {} samples, not {}",
            samples.len(),
            w * h * 4
        )));
    }
    let radius = kernel.radius() as usize;
    let weights = &kernel.weights;
    let band = (band.max(1)) as usize;
    let pool = pooled(RenderPass::Spatial, u64::from(width) * u64::from(height));
    // The rows read from the source before any is written: from `first` on, `rows[i]` is row
    // `first + i`.
    let mut kept: VecDeque<Row> = VecDeque::new();
    let mut first = 0usize;
    let read = |y: usize, samples: &[T]| -> Row {
        Row::of(&samples[y * w * 4..(y + 1) * w * 4], weights, radius)
    };
    let mut top = 0usize;
    while top < h {
        cancel.check()?;
        let bottom = (top + band).min(h);
        // Every source row the band reads, `top − r` to `bottom + r`, held before its rows are
        // written: those above were read before the band above was written.
        let needed = top.saturating_sub(radius);
        while first < needed {
            kept.pop_front();
            first += 1;
        }
        let have = first + kept.len();
        let until = (bottom + radius).min(h);
        if until > have {
            let source: &[T] = samples;
            let fresh: Vec<Row> = if pool {
                (have..until)
                    .into_par_iter()
                    .map(|y| read(y, source))
                    .collect()
            } else {
                (have..until).map(|y| read(y, source)).collect()
            };
            kept.extend(fresh);
        }
        let kept_rows = kept.make_contiguous();
        let write = |(offset, line): (usize, &mut [T])| {
            sharpen_line(line, top + offset, h, (kept_rows, first), kernel);
        };
        let lines = &mut samples[top * w * 4..bottom * w * 4];
        if pool {
            lines.par_chunks_mut(w * 4).enumerate().for_each(write);
        } else {
            lines.chunks_mut(w * 4).enumerate().for_each(write);
        }
        top = bottom;
    }
    Ok(())
}

/// Output sharpening of an output that arrives in bands from the top, as the banded reference
/// renders a paged source's (`docs/design/export-settings.md`, "Paged sources"): each band's rows
/// are sharpened in place once the rows their kernel reads below them have arrived, and handed on
/// in order. Every row is computed from the unsharpened rows exactly as [`sharpen`] computes the
/// whole frame's, by the same [`Row`]s and the same line, so the stream is the whole frame
/// sharpened, code for code, whatever its bands.
///
/// It holds the rows received and not yet handed on, at most the newest band and the kernel's
/// radius of rows before it (an output's last `r` rows wait for the band below them), and the luma
/// and sums across of [`SHARPEN_BAND_ROWS`] rows and `r` either side, `(64 + 2r) × width × 16`
/// bytes. Each group of rows checks the cancellation before it starts.
pub(crate) struct SharpenStream {
    codes: StreamCodes,
}

enum StreamCodes {
    Eight(StreamRows<u8>),
    Sixteen(StreamRows<u16>),
}

impl SharpenStream {
    /// A stream of a `width` × `height` output at `depth`, sharpened by `kernel`.
    pub(crate) fn new(kernel: SharpenKernel, (width, height): (u32, u32), depth: Depth) -> Self {
        let (width, height) = (width as usize, height as usize);
        let codes = match depth {
            Depth::Eight => StreamCodes::Eight(StreamRows::new(kernel, width, height)),
            Depth::Sixteen => StreamCodes::Sixteen(StreamRows::new(kernel, width, height)),
        };
        Self { codes }
    }

    /// Take the next band of `rows` rows of the output, and answer the bands now sharpened, in
    /// order from the top: none, or every row up to the radius's rows above the newest band's
    /// last, or every row left once the last band arrives. A band of another depth, or rows past
    /// the output's, is refused as `internal`.
    pub(crate) fn push(
        &mut self,
        rows: u32,
        pixels: Pixels,
        cancel: &Cancel,
    ) -> Result<Vec<(u32, Pixels)>, Error> {
        match (&mut self.codes, pixels) {
            (StreamCodes::Eight(stream), Pixels::Eight(codes)) => Ok(stream
                .push(rows as usize, codes, cancel)?
                .into_iter()
                .map(|(rows, codes)| (rows, Pixels::Eight(codes)))
                .collect()),
            (StreamCodes::Sixteen(stream), Pixels::Sixteen(codes)) => Ok(stream
                .push(rows as usize, codes, cancel)?
                .into_iter()
                .map(|(rows, codes)| (rows, Pixels::Sixteen(codes)))
                .collect()),
            _ => Err(Error::internal(
                "a band of another depth reached the output sharpening",
            )),
        }
    }
}

/// [`SharpenStream`] at one depth.
struct StreamRows<T> {
    kernel: SharpenKernel,
    width: usize,
    height: usize,
    pool: bool,
    /// The unsharpened rows' [`Row`]s from row `first` on.
    kept: VecDeque<Row>,
    first: usize,
    /// The rows received and not yet handed on, in buffers `(first row, codes)` in order.
    held: VecDeque<(usize, Vec<T>)>,
    /// How many rows have arrived, and how many are sharpened.
    received: usize,
    written: usize,
}

impl<T: Code> StreamRows<T> {
    fn new(kernel: SharpenKernel, width: usize, height: usize) -> Self {
        let pool = pooled(RenderPass::Spatial, width as u64 * height as u64);
        Self {
            kernel,
            width,
            height,
            pool,
            kept: VecDeque::new(),
            first: 0,
            held: VecDeque::new(),
            received: 0,
            written: 0,
        }
    }

    fn push(
        &mut self,
        rows: usize,
        codes: Vec<T>,
        cancel: &Cancel,
    ) -> Result<Vec<(u32, Vec<T>)>, Error> {
        let stride = self.width * 4;
        if codes.len() != rows * stride || self.received + rows > self.height {
            return Err(Error::internal(format!(
                "a band of {rows} rows of {} samples does not follow row {} of a {}x{} output",
                codes.len(),
                self.received,
                self.width,
                self.height
            )));
        }
        if rows > 0 {
            self.held.push_back((self.received, codes));
            self.received += rows;
        }
        let radius = self.kernel.radius() as usize;
        // Every row whose kernel's rows below it have arrived.
        let limit = if self.received == self.height {
            self.height
        } else {
            self.received.saturating_sub(radius)
        };
        let Self {
            kernel,
            height,
            pool,
            kept,
            first,
            held,
            written,
            ..
        } = self;
        let (h, pool) = (*height, *pool);
        while *written < limit {
            cancel.check()?;
            let top = *written;
            let bottom = (top + SHARPEN_BAND_ROWS as usize).min(limit);
            // The rows the group reads, `top − r` to `bottom + r`, each read before it is written:
            // those above were read before the group above was written.
            let needed = top.saturating_sub(radius);
            while *first < needed && !kept.is_empty() {
                kept.pop_front();
                *first += 1;
            }
            if kept.is_empty() {
                *first = needed;
            }
            let have = *first + kept.len();
            let until = (bottom + radius).min(h);
            if until > have {
                let line = |y: usize| -> &[T] {
                    let (y0, codes) = held
                        .iter()
                        .find(|(y0, codes)| (*y0..*y0 + codes.len() / stride).contains(&y))
                        .expect("a row read before it is handed on");
                    &codes[(y - y0) * stride..(y - y0 + 1) * stride]
                };
                let read = |y: usize| Row::of(line(y), &kernel.weights, radius);
                let fresh: Vec<Row> = if pool {
                    (have..until).into_par_iter().map(read).collect()
                } else {
                    (have..until).map(read).collect()
                };
                kept.extend(fresh);
            }
            let kept_rows = kept.make_contiguous();
            let reads = (&*kept_rows, *first);
            for (y0, codes) in held.iter_mut() {
                let (from, to) = (top.max(*y0), bottom.min(*y0 + codes.len() / stride));
                if from >= to {
                    continue;
                }
                let lines = &mut codes[(from - *y0) * stride..(to - *y0) * stride];
                let write = |(offset, line): (usize, &mut [T])| {
                    sharpen_line(line, from + offset, h, reads, kernel);
                };
                if pool {
                    lines.par_chunks_mut(stride).enumerate().for_each(write);
                } else {
                    lines.chunks_mut(stride).enumerate().for_each(write);
                }
            }
            *written = bottom;
        }
        // Hand on every sharpened row, a buffer split where the rows still waiting begin: what
        // stays is at most the radius's rows.
        let mut done = Vec::new();
        while let Some((y0, codes)) = held.pop_front() {
            let end = y0 + codes.len() / stride;
            if end <= limit {
                done.push(((end - y0) as u32, codes));
            } else {
                if y0 < limit {
                    let mut codes = codes;
                    let waiting = codes.split_off((limit - y0) * stride);
                    done.push(((limit - y0) as u32, codes));
                    held.push_front((limit, waiting));
                } else {
                    held.push_front((y0, codes));
                }
                break;
            }
        }
        Ok(done)
    }
}

#[cfg(test)]
#[path = "sharpen_tests.rs"]
mod tests;
