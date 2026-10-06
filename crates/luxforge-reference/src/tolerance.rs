//! The declared tolerance of every output kind against the reference renderer, as the
//! [GPU-first design](../../../docs/design/gpu-first.md#the-contract) states it (owner,
//! 2026-10-04): the GPU is the renderer of record, and what it produces — the picture on screen at
//! rest and in motion, the histogram and clipping counts, a sample and an export — is held to a
//! declared limit against what the whole-frame CPU reference renders. The picture in motion is held
//! to the frame it settles to, by the design's recorded default of 2026-10-05, and on request to
//! the reference. This module names each [`Kind`], what it is compared with and its limit, and holds
//! the comparisons: the picture through [`reduce_srgb8`] and the [`preview_error`] measure, the
//! counts through [`histogram`], samples through [`samples`] and an export through [`export`].
//!
//! Like the rest of this crate it is written from the design, never from production code, and
//! depends on nothing. `cargo xtask gpu-qualification`, the release gate that renders the corpus
//! on the reference and on the GPU, judges every cell with these functions.

use crate::{
    preview_error::{self, Class, Rgb8, Statistics},
    srgb,
};

/// One output the renderer of record produces, each with its own reference and limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The picture on screen at rest, at every view.
    PictureAtRest,
    /// The picture on screen during a gesture: the frame a drag draws, at every view.
    PictureInMotion,
    /// The histogram's bins and the clipping counts.
    Histogram,
    /// A sample or another pixel read.
    Sample,
    /// An export.
    Export,
}

impl Kind {
    /// Every kind, in the order a report lists them.
    pub const ALL: [Self; 5] = [
        Self::PictureAtRest,
        Self::PictureInMotion,
        Self::Histogram,
        Self::Sample,
        Self::Export,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::PictureAtRest => "picture-at-rest",
            Self::PictureInMotion => "picture-in-motion",
            Self::Histogram => "histogram",
            Self::Sample => "sample",
            Self::Export => "export",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// Whether the kind is a picture on screen, which a view of the corpus measures.
    pub const fn picture(self) -> bool {
        matches!(self, Self::PictureAtRest | Self::PictureInMotion)
    }

    /// What the kind is compared with.
    pub const fn reference(self) -> &'static str {
        match self {
            Self::PictureAtRest => {
                "The reference frame reduced to the view's size: the stack's exact whole frame, as \
                 export renders it, reduced by an area-weighted box average of its linear light at \
                 Fit and at percentages below 100%, and its visible region, unreduced, at 100%"
            }
            Self::PictureInMotion => {
                "The frame the drag settles to: the CPU frame it stands in for until the GPU draws \
                 the picture at rest (the proxy at Fit and below 100%, the exact visible region at \
                 100%), then the GPU's frame at rest; or, on request, the reference frame at the \
                 view's size"
            }
            Self::Histogram => "The reference frame's counts, from the exact whole frame",
            Self::Sample => {
                "The byte on screen at that pixel, which the sample equals by construction, and \
                 the reference frame's byte there"
            }
            Self::Export => "The reference export: the stack's exact whole frame",
        }
    }

    /// The limit, as a report states it.
    pub const fn limit(self) -> &'static str {
        match self {
            Self::PictureAtRest | Self::PictureInMotion => {
                "The preview error limit of the recipe's class: mean ΔE00, worst 16 × 16 block \
                 mean ΔE00, p99 ΔE00 and signed mean ΔL*"
            }
            Self::Histogram => {
                "The earth mover's distance between the histograms within 0.25 code on each of R, \
                 G, B and luminance, and each clipping count within 0.1% of the output pixel count"
            }
            Self::Sample => {
                "Equal to the byte on screen; against the reference, the display limit of the \
                 recipe's class over the samples: mean ΔE00, p99 ΔE00 and signed mean ΔL*"
            }
            Self::Export => {
                "The display limit of the recipe's class over every pixel of the export; the same \
                 bytes from two runs on one machine and driver"
            }
        }
    }
}

// ---- The picture --------------------------------------------------------------------------------

/// A `from`-sized frame reduced to `to` by an area-weighted box average: output pixel `(x, y)`
/// covers the source rectangle from `(x·W/w, y·H/h)` to `((x+1)·W/w, (y+1)·H/h)`, and each source
/// pixel it overlaps is weighted by the area of that overlap, fractional edge pixels included.
/// `row` writes source row `y`'s values, `channels` a pixel, into the buffer it is given, so a
/// caller decodes one row at a time and nothing the size of the source is held. Answers the means,
/// `channels` a pixel, row by row.
///
/// # Panics
///
/// When a side of `from` or `to` is zero, or `channels` is.
pub fn area_average(
    from: (u32, u32),
    to: (u32, u32),
    channels: usize,
    row: impl FnMut(u32, &mut [f64]),
) -> Vec<f64> {
    area_average_rows(from, to, channels, 0..to.1, row)
}

/// [`area_average`] of the output rows `rows` alone, which a caller can split across threads: each
/// output row's means depend only on the source rows under it, so the rows of the whole reduction
/// are these, joined in order.
///
/// # Panics
///
/// When a side of `from` or `to` is zero, `channels` is, or `rows` passes `to`'s height.
pub fn area_average_rows(
    from: (u32, u32),
    to: (u32, u32),
    channels: usize,
    rows: std::ops::Range<u32>,
    mut row: impl FnMut(u32, &mut [f64]),
) -> Vec<f64> {
    assert!(
        from.0 > 0 && from.1 > 0 && to.0 > 0 && to.1 > 0 && channels > 0,
        "a reduction from {from:?} to {to:?} of {channels} channels"
    );
    assert!(
        rows.start <= rows.end && rows.end <= to.1,
        "output rows {rows:?} of {}",
        to.1
    );
    let (sw, sh) = (from.0 as usize, from.1 as usize);
    let (w, h) = (to.0 as usize, to.1 as usize);
    // Where each boundary between output pixels falls in the source, across and down.
    let columns: Vec<f64> = (0..=w).map(|x| x as f64 * sw as f64 / w as f64).collect();
    let row_edge = |y: usize| y as f64 * sh as f64 / h as f64;
    let mut values = vec![0.0; sw * channels];
    // One running sum per channel, `sw + 1` entries each: the sum of a row's first `x` values.
    let mut prefix = vec![0.0; (sw + 1) * channels];
    let (start, end) = (rows.start as usize, rows.end as usize);
    let mut sums = vec![0.0; w * (end - start) * channels];
    for oy in start..end {
        let (top, bottom) = (row_edge(oy), row_edge(oy + 1));
        let first = (top.floor() as usize).min(sh - 1);
        let last = (bottom.ceil() as usize).clamp(first + 1, sh);
        let at = oy - start;
        let output = &mut sums[at * w * channels..(at + 1) * w * channels];
        for sy in first..last {
            let overlap = bottom.min((sy + 1) as f64) - top.max(sy as f64);
            if overlap <= 0.0 {
                continue;
            }
            row(sy as u32, &mut values);
            for channel in 0..channels {
                let sums = &mut prefix[channel * (sw + 1)..(channel + 1) * (sw + 1)];
                for x in 0..sw {
                    sums[x + 1] = sums[x] + values[x * channels + channel];
                }
            }
            for ox in 0..w {
                for channel in 0..channels {
                    let sums = &prefix[channel * (sw + 1)..(channel + 1) * (sw + 1)];
                    // The sum of the row's values up to a fractional position.
                    let up_to = |at: f64| -> f64 {
                        let whole = (at.floor() as usize).min(sw);
                        let fraction = at - whole as f64;
                        sums[whole]
                            + if whole < sw {
                                fraction * values[whole * channels + channel]
                            } else {
                                0.0
                            }
                    };
                    output[ox * channels + channel] +=
                        overlap * (up_to(columns[ox + 1]) - up_to(columns[ox]));
                }
            }
        }
    }
    let area = (sw as f64 / w as f64) * (sh as f64 / h as f64);
    for sum in &mut sums {
        *sum /= area;
    }
    sums
}

/// An 8-bit sRGB frame of `from` pixels, `stride` bytes a pixel (3, or 4 with an alpha byte it
/// never reads), reduced to `to` in linear light: each channel decoded by [`srgb::decode`],
/// averaged by [`area_average`], then encoded and quantized by [`srgb::code`]. Three bytes a pixel
/// out, row by row. A frame reduced to its own size is that frame, byte for byte.
pub fn reduce_srgb8(
    pixels: &[u8],
    stride: usize,
    from: (u32, u32),
    to: (u32, u32),
) -> Result<Vec<u8>, String> {
    reduce_srgb8_rows(pixels, stride, from, to, 0..to.1)
}

/// [`reduce_srgb8`] of the output rows `rows` alone ([`area_average_rows`]).
pub fn reduce_srgb8_rows(
    pixels: &[u8],
    stride: usize,
    from: (u32, u32),
    to: (u32, u32),
    rows: std::ops::Range<u32>,
) -> Result<Vec<u8>, String> {
    let expected = from.0 as usize * from.1 as usize * stride;
    if stride < 3 || pixels.len() != expected {
        return Err(format!(
            "a {} x {} frame of {stride} bytes a pixel holds {expected} bytes, not {}",
            from.0,
            from.1,
            pixels.len()
        ));
    }
    if from.0 == 0 || from.1 == 0 || to.0 == 0 || to.1 == 0 {
        return Err(format!("no reduction from {from:?} to {to:?}"));
    }
    if rows.start > rows.end || rows.end > to.1 {
        return Err(format!("output rows {rows:?} of {}", to.1));
    }
    let width = from.0 as usize;
    if from == to {
        let (start, end) = (rows.start as usize, rows.end as usize);
        return Ok(pixels[start * width * stride..end * width * stride]
            .chunks_exact(stride)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect());
    }
    let table: Vec<f64> = (0..=255u8).map(srgb::decode).collect();
    let means = area_average_rows(from, to, 3, rows, |y, row| {
        let start = y as usize * width * stride;
        for (x, pixel) in pixels[start..start + width * stride]
            .chunks_exact(stride)
            .enumerate()
        {
            for channel in 0..3 {
                row[x * 3 + channel] = table[usize::from(pixel[channel])];
            }
        }
    });
    Ok(means.into_iter().map(srgb::code).collect())
}

/// `region` (`[x, y, width, height]`) of a `width`-wide frame, `stride` bytes a pixel, as three
/// bytes a pixel: what a view at 100% shows of the reference frame, unreduced.
pub fn region_srgb8(
    pixels: &[u8],
    stride: usize,
    width: u32,
    region: [u32; 4],
) -> Result<Vec<u8>, String> {
    let [x0, y0, w, h] = region;
    let height = if width == 0 || stride == 0 {
        0
    } else {
        pixels.len() / (width as usize * stride)
    };
    if stride < 3
        || w == 0
        || h == 0
        || x0 + w > width
        || (y0 + h) as usize > height
        || pixels.len() != width as usize * height * stride
    {
        return Err(format!(
            "the region {region:?} is empty or outside a {width}-wide frame of {height} rows"
        ));
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 3);
    for y in y0..y0 + h {
        let start = (y as usize * width as usize + x0 as usize) * stride;
        for pixel in pixels[start..start + w as usize * stride].chunks_exact(stride) {
            out.extend_from_slice(&pixel[..3]);
        }
    }
    Ok(out)
}

// ---- Histogram bins and clipping counts -----------------------------------------------------------

/// The clipping counters, in the order [`Counts::clipping`] holds them: each channel at code 0 and
/// at code 255, a pixel with any channel at 0 and at 255, with all three at 0 and at 255, and a
/// pixel with a channel at each end.
pub const CLIPPING: [&str; 11] = [
    "r0",
    "g0",
    "b0",
    "r255",
    "g255",
    "b255",
    "any_shadow",
    "any_highlight",
    "all_shadow",
    "all_highlight",
    "both",
];

/// The share of the output pixel count each clipping counter's difference may reach: 0.1%.
pub const HISTOGRAM_FRACTION: f64 = 0.001;

/// The most each of the histograms of R, G, B and luminance may move from the reference's, by the
/// earth mover's distance in codes ([`histogram_emd`]): a quarter of a code (owner, 2026-10-05).
/// A whole frame one code off is 1.0; a share of its pixels one code off is that share.
pub const HISTOGRAM_EMD_CODES: f64 = 0.25;

/// The histograms the earth mover's distance is judged on, in the order [`HistogramError::emd`]
/// holds them.
pub const HISTOGRAM_SIDES: [&str; 4] = ["r", "g", "b", "luminance"];

/// One frame's counts: each channel's 256 bins, the clipping counters of [`CLIPPING`] and the
/// luminance histogram of [`luma_bins`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counts {
    pub bins: [[u64; 256]; 3],
    pub clipping: [u64; 11],
    pub luma: [u64; 256],
}

impl Counts {
    /// The counts of an 8-bit sRGB frame, `stride` bytes a pixel, its alpha never read.
    pub fn of(pixels: &[u8], stride: usize) -> Self {
        let mut counts = Self {
            bins: [[0; 256]; 3],
            clipping: [0; 11],
            luma: luma_bins(pixels, stride),
        };
        for pixel in pixels.chunks_exact(stride.max(3)) {
            let rgb = [pixel[0], pixel[1], pixel[2]];
            for (channel, &code) in rgb.iter().enumerate() {
                counts.bins[channel][usize::from(code)] += 1;
                counts.clipping[channel] += u64::from(code == 0);
                counts.clipping[3 + channel] += u64::from(code == 255);
            }
            let shadow = rgb.contains(&0);
            let highlight = rgb.contains(&255);
            counts.clipping[6] += u64::from(shadow);
            counts.clipping[7] += u64::from(highlight);
            counts.clipping[8] += u64::from(rgb == [0; 3]);
            counts.clipping[9] += u64::from(rgb == [255; 3]);
            counts.clipping[10] += u64::from(shadow && highlight);
        }
        counts
    }

    /// The pixel count: what the red channel's bins sum to.
    pub fn pixels(&self) -> u64 {
        self.bins[0].iter().sum()
    }
}

/// How far one frame's counts are from the reference frame's: in pixels, and by the earth mover's
/// distance in codes.
#[derive(Clone, Debug, PartialEq)]
pub struct HistogramError {
    /// The output pixel count both frames hold.
    pub pixels: u64,
    /// Each channel's summed absolute bin difference. A pixel that moves from one bin to another
    /// counts twice: once where it left and once where it arrived. Reported, not judged.
    pub bins: [u64; 3],
    /// Each clipping counter's absolute difference, in the order of [`CLIPPING`].
    pub clipping: [u64; 11],
    /// The earth mover's distance in codes of each histogram of [`HISTOGRAM_SIDES`].
    pub emd: [f64; 4],
}

impl HistogramError {
    /// The clipping counters' limit, in pixels: [`HISTOGRAM_FRACTION`] of the output pixel count.
    pub fn limit(&self) -> f64 {
        HISTOGRAM_FRACTION * self.pixels as f64
    }

    /// The largest earth mover's distance, in codes, and its histogram: the first in the order of
    /// [`HISTOGRAM_SIDES`] among equal ones.
    pub fn worst_emd(&self) -> (&'static str, f64) {
        let (index, worst) =
            self.emd
                .iter()
                .enumerate()
                .fold((0, 0.0), |(best, worst), (index, emd)| {
                    if *emd > worst {
                        (index, *emd)
                    } else {
                        (best, worst)
                    }
                });
        (HISTOGRAM_SIDES[index], worst)
    }

    /// The largest channel's summed bin difference, as a share of the pixel count.
    pub fn worst_bins(&self) -> f64 {
        self.bins.iter().copied().max().unwrap_or(0) as f64 / self.pixels as f64
    }

    /// The largest clipping difference, as a share of the pixel count, and its counter: the first
    /// in the order of [`CLIPPING`] among equal ones.
    pub fn worst_clipping(&self) -> (&'static str, f64) {
        let (index, worst) =
            self.clipping
                .iter()
                .enumerate()
                .fold((0, 0), |(best, worst), (index, difference)| {
                    if *difference > worst {
                        (index, *difference)
                    } else {
                        (best, worst)
                    }
                });
        (CLIPPING[index], worst as f64 / self.pixels as f64)
    }

    /// Every histogram's earth mover's distance within [`HISTOGRAM_EMD_CODES`] and every clipping
    /// difference within [`Self::limit`], both inclusive. The summed bin difference is reported
    /// beside them and not judged.
    pub fn passed(&self) -> bool {
        let limit = self.limit();
        self.emd.iter().all(|emd| *emd <= HISTOGRAM_EMD_CODES)
            && self
                .clipping
                .iter()
                .all(|difference| *difference as f64 <= limit)
    }
}

/// `candidate`'s counts against `reference`'s. Both must count the same number of pixels in every
/// channel.
pub fn histogram(candidate: &Counts, reference: &Counts) -> Result<HistogramError, String> {
    let pixels = reference.pixels();
    for (name, counts) in [("candidate", candidate), ("reference", reference)] {
        let sums = counts.bins.map(|bins| bins.iter().sum::<u64>());
        if sums.iter().any(|sum| *sum != pixels) || pixels == 0 {
            return Err(format!(
                "the {name}'s channels count {sums:?} pixels, not {pixels} each"
            ));
        }
    }
    let bins = [0, 1, 2].map(|channel| {
        candidate.bins[channel]
            .iter()
            .zip(&reference.bins[channel])
            .map(|(a, b)| a.abs_diff(*b))
            .sum()
    });
    let clipping = std::array::from_fn(|counter| {
        candidate.clipping[counter].abs_diff(reference.clipping[counter])
    });
    if candidate.luma.iter().sum::<u64>() != pixels || reference.luma.iter().sum::<u64>() != pixels
    {
        return Err(format!(
            "the luminance histograms do not count {pixels} pixels each"
        ));
    }
    let emd = [
        histogram_emd(&candidate.bins[0], &reference.bins[0]),
        histogram_emd(&candidate.bins[1], &reference.bins[1]),
        histogram_emd(&candidate.bins[2], &reference.bins[2]),
        histogram_emd(&candidate.luma, &reference.luma),
    ];
    Ok(HistogramError {
        pixels,
        bins,
        clipping,
        emd,
    })
}

// ---- Histogram diagnostics, reported beside the gated figures and never gated -------------------

/// The earth mover's distance between two histograms of the same pixel count, in codes: the L1
/// distance of their cumulative histograms over the pixel count, which is the mean distance each
/// pixel's code must move to turn one into the other. A whole frame shifted by one code is 1.0;
/// a share `s` of the pixels shifted by one code is `s`.
pub fn histogram_emd(candidate: &[u64; 256], reference: &[u64; 256]) -> f64 {
    let pixels: u64 = reference.iter().sum();
    if pixels == 0 {
        return 0.0;
    }
    let (mut a, mut b, mut moved) = (0i128, 0i128, 0u128);
    for (x, y) in candidate.iter().zip(reference) {
        a += i128::from(*x);
        b += i128::from(*y);
        moved += (a - b).unsigned_abs();
    }
    moved as f64 / pixels as f64
}

/// The signed mean code of `candidate` less `reference`'s over the same pixel count: a systematic
/// shift shows here, a spread of shifts in both directions does not.
pub fn histogram_mean_shift(candidate: &[u64; 256], reference: &[u64; 256]) -> f64 {
    let pixels: u64 = reference.iter().sum();
    if pixels == 0 {
        return 0.0;
    }
    let sum = |bins: &[u64; 256]| -> f64 {
        bins.iter()
            .enumerate()
            .map(|(code, count)| code as f64 * *count as f64)
            .sum()
    };
    (sum(candidate) - sum(reference)) / pixels as f64
}

/// The summed absolute bin difference after both histograms are binned at `width` codes, the bins
/// aligned at code 0: as [`histogram`]'s, a pixel that moves between bins counts twice.
pub fn histogram_binned(candidate: &[u64; 256], reference: &[u64; 256], width: usize) -> u64 {
    let width = width.max(1);
    candidate
        .chunks(width)
        .zip(reference.chunks(width))
        .map(|(a, b)| a.iter().sum::<u64>().abs_diff(b.iter().sum::<u64>()))
        .sum()
}

/// The luminance histogram of an 8-bit frame, `stride` bytes a pixel: each pixel's Rec. 709
/// weighting of its three codes, rounded to a code. A diagnostic of the frames, not a figure the
/// report or the reducer holds.
pub fn luma_bins(pixels: &[u8], stride: usize) -> [u64; 256] {
    let mut bins = [0u64; 256];
    for pixel in pixels.chunks_exact(stride.max(3)) {
        let y = 0.2126 * f64::from(pixel[0])
            + 0.7152 * f64::from(pixel[1])
            + 0.0722 * f64::from(pixel[2]);
        bins[(y.round() as usize).min(255)] += 1;
    }
    bins
}

/// How many pixels of `candidate` differ from `reference`'s at the same place by their largest
/// channel difference: equal, by exactly one code, by exactly two, and by more than two. Both are
/// 8-bit frames of the same size, `stride` bytes a pixel.
pub fn code_shifts(candidate: &[u8], reference: &[u8], stride: usize) -> Result<[u64; 4], String> {
    if candidate.len() != reference.len() {
        return Err(format!(
            "the frames hold {} and {} bytes",
            candidate.len(),
            reference.len()
        ));
    }
    let mut shifts = [0u64; 4];
    for (a, b) in candidate
        .chunks_exact(stride.max(3))
        .zip(reference.chunks_exact(stride.max(3)))
    {
        let largest = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
        shifts[usize::from(largest.min(3))] += 1;
    }
    Ok(shifts)
}

// ---- Samples ------------------------------------------------------------------------------------

/// One sampled pixel: the byte the renderer of record answered, the byte on screen at that pixel
/// when the view shows it, and the reference frame's byte there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub answered: [u8; 3],
    pub on_screen: Option<[u8; 3]>,
    pub reference: [u8; 3],
}

/// How a set of samples compares, with the byte on screen and with the reference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleError {
    pub samples: u64,
    /// How many samples had a byte on screen to equal, and how many of those differ from it.
    pub on_screen: u64,
    pub unequal: u64,
    /// Against the reference: mean ΔE00, the nearest-rank p99 ΔE00, the signed mean ΔL\*
    /// (answered minus reference) and the largest ΔE00.
    pub mean: f64,
    pub p99: f64,
    pub mean_delta_l: f64,
    pub max: f64,
}

impl SampleError {
    /// Every sample equal to the byte on screen where there is one, and the samples within
    /// `class`'s display limit against the reference: its mean, its p99 and its signed mean ΔL\*.
    /// A worst block has no meaning over scattered samples, so it does not apply.
    pub fn passed(&self, class: Class) -> bool {
        let limits = class.limits();
        self.unequal == 0
            && self.mean <= limits.mean
            && self.p99 <= limits.p99
            && self.mean_delta_l.abs() <= limits.mean_delta_l
    }
}

/// The figures of `samples`, which must not be empty.
pub fn samples(samples: &[Sample]) -> Result<SampleError, String> {
    if samples.is_empty() {
        return Err("no samples to compare".to_owned());
    }
    let mut delta_e = Vec::with_capacity(samples.len());
    let mut delta_l = 0.0;
    let (mut on_screen, mut unequal) = (0, 0);
    for sample in samples {
        if let Some(screen) = sample.on_screen {
            on_screen += 1;
            unequal += u64::from(screen != sample.answered);
        }
        let (answered, reference) = (
            preview_error::lab_from_srgb8(sample.answered),
            preview_error::lab_from_srgb8(sample.reference),
        );
        delta_e.push(preview_error::ciede2000(reference, answered));
        delta_l += answered[0] - reference[0];
    }
    let count = samples.len() as f64;
    let mean = delta_e.iter().sum::<f64>() / count;
    let max = delta_e.iter().copied().fold(0.0, f64::max);
    Ok(SampleError {
        samples: samples.len() as u64,
        on_screen,
        unequal,
        mean,
        p99: preview_error::nearest_rank_99(&mut delta_e),
        mean_delta_l: delta_l / count,
        max,
    })
}

// ---- Export -------------------------------------------------------------------------------------

/// How an export compares with the reference export, and with itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportError {
    /// The four statistics over every pixel of the export.
    pub statistics: Statistics,
    /// Whether a second export of the same stack on the same machine and driver was the same
    /// bytes.
    pub repeatable: bool,
}

impl ExportError {
    /// Within `class`'s display limit over every pixel, and repeatable.
    pub fn passed(&self, class: Class) -> bool {
        self.repeatable && preview_error::verdict(&self.statistics, class).passed()
    }
}

/// `candidate`, an export, against `reference`, the reference export, over every pixel, and
/// against `repeat`, the same export made again on the same machine.
pub fn export(
    candidate: Rgb8<'_>,
    repeat: Rgb8<'_>,
    reference: Rgb8<'_>,
) -> Result<ExportError, String> {
    let whole = [0, 0, reference.width(), reference.height()];
    Ok(ExportError {
        statistics: preview_error::compare(candidate, reference, whole)?,
        repeatable: candidate == repeat,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| pixel(x, y))
            .collect()
    }

    #[test]
    fn every_kind_is_named_once_and_parses_back() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.name()), Some(kind));
            assert!(!kind.reference().is_empty() && !kind.limit().is_empty());
        }
        assert_eq!(Kind::parse("picture"), None, "the picture is two kinds");
        let pictures: Vec<Kind> = Kind::ALL.into_iter().filter(|k| k.picture()).collect();
        assert_eq!(pictures, [Kind::PictureAtRest, Kind::PictureInMotion]);
    }

    #[test]
    fn a_reduction_to_its_own_size_is_the_frame_itself() {
        let frame = rgb(7, 5, |x, y| [x as u8 * 30, y as u8 * 50, 7]);
        assert_eq!(reduce_srgb8(&frame, 3, (7, 5), (7, 5)).unwrap(), frame);
        // With an alpha byte, which is never read.
        let rgba: Vec<u8> = frame
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 99])
            .collect();
        assert_eq!(reduce_srgb8(&rgba, 4, (7, 5), (7, 5)).unwrap(), frame);
    }

    #[test]
    fn a_flat_frame_reduces_to_itself_at_any_size() {
        let flat = rgb(37, 23, |_, _| [90, 140, 200]);
        for size in [(10, 6), (7, 5), (3, 2), (1, 1), (36, 22)] {
            let reduced = reduce_srgb8(&flat, 3, (37, 23), size).unwrap();
            assert_eq!(reduced.len(), (size.0 * size.1 * 3) as usize);
            assert!(
                reduced.chunks_exact(3).all(|p| p == [90, 140, 200]),
                "{size:?}"
            );
        }
    }

    #[test]
    fn the_reduction_averages_light_rather_than_codes() {
        // A black and a white pixel average to half the light, code 188, not to code 128.
        let pair = rgb(2, 1, |x, _| [[0; 3], [255; 3]][x as usize]);
        assert_eq!(reduce_srgb8(&pair, 3, (2, 1), (1, 1)).unwrap(), [188; 3]);
    }

    #[test]
    fn the_reduction_weights_fractional_pixels_by_their_overlap() {
        // Three pixels to two: each output covers one and a half, so the middle one is shared
        // equally, and each output is (1 + 0.5 · 0) / 1.5 of white's light.
        let row = rgb(3, 1, |x, _| [[255; 3], [0; 3], [255; 3]][x as usize]);
        let expected = srgb::code(1.0 / 1.5);
        assert_eq!(
            reduce_srgb8(&row, 3, (3, 1), (2, 1)).unwrap(),
            [expected; 6]
        );
        // And down the rows alike.
        let column = rgb(1, 3, |_, y| [[255; 3], [0; 3], [255; 3]][y as usize]);
        assert_eq!(
            reduce_srgb8(&column, 3, (1, 3), (1, 2)).unwrap(),
            [expected; 6]
        );
    }

    #[test]
    fn the_area_average_is_the_mean_of_each_footprint() {
        // A 4 x 4 ramp halved: each output is the mean of its 2 x 2 block, exactly.
        let values: Vec<f64> = (0..16).map(f64::from).collect();
        let means = area_average((4, 4), (2, 2), 1, |y, row| {
            row.copy_from_slice(&values[y as usize * 4..y as usize * 4 + 4]);
        });
        assert_eq!(means, [2.5, 4.5, 10.5, 12.5]);
        // Each channel separately.
        let means = area_average((2, 1), (1, 1), 2, |_, row| {
            row.copy_from_slice(&[1.0, 10.0, 3.0, 30.0]);
        });
        assert_eq!(means, [2.0, 20.0]);
    }

    #[test]
    fn a_reduction_in_bands_of_output_rows_is_the_whole_reduction() {
        let frame = rgb(53, 41, |x, y| {
            [(x * 4) as u8, (y * 6) as u8, ((x + y) * 2) as u8]
        });
        for to in [(53, 41), (17, 13), (20, 9)] {
            let whole = reduce_srgb8(&frame, 3, (53, 41), to).unwrap();
            let mut joined = Vec::new();
            for rows in [0..to.1 / 3, to.1 / 3..to.1 / 3 + 1, to.1 / 3 + 1..to.1] {
                joined.extend(reduce_srgb8_rows(&frame, 3, (53, 41), to, rows).unwrap());
            }
            assert_eq!(joined, whole, "{to:?}");
        }
        assert!(reduce_srgb8_rows(&frame, 3, (53, 41), (17, 13), 5..14).is_err());
    }

    #[test]
    fn a_region_is_cut_unreduced() {
        let frame = rgb(4, 3, |x, y| [x as u8, y as u8, 9]);
        let rgba: Vec<u8> = frame
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        assert_eq!(
            region_srgb8(&rgba, 4, 4, [1, 1, 2, 2]).unwrap(),
            [1, 1, 9, 2, 1, 9, 1, 2, 9, 2, 2, 9]
        );
        assert!(region_srgb8(&rgba, 4, 4, [3, 0, 2, 1]).is_err());
        assert!(region_srgb8(&rgba, 4, 4, [0, 2, 1, 2]).is_err());
    }

    #[test]
    fn counts_bin_every_channel_and_classify_every_clipped_pixel() {
        let frame = [0, 0, 0, 255, 255, 255, 0, 128, 255, 10, 20, 30];
        let counts = Counts::of(&frame, 3);
        assert_eq!(counts.pixels(), 4);
        assert_eq!(counts.bins[0][0], 2);
        assert_eq!(counts.bins[1][128], 1);
        assert_eq!(counts.bins[2][255], 2);
        // r0 g0 b0 r255 g255 b255 any_shadow any_highlight all_shadow all_highlight both
        assert_eq!(counts.clipping, [2, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1]);
    }

    #[test]
    fn the_histogram_is_held_to_a_quarter_code_and_clipping_to_a_tenth_of_a_percent() {
        let pixels = 4000;
        let reference = Counts::of(&vec![100; pixels * 3], 3);
        // A quarter of the pixels one code up: a quarter of a code by the earth mover's distance on
        // every channel and on luminance, at the limit, which is inclusive, while the summed bin
        // difference, reported and not judged, is half the pixel count.
        let mut moved = vec![100; pixels * 3];
        moved[..pixels / 4 * 3].fill(101);
        let error = histogram(&Counts::of(&moved, 3), &reference).unwrap();
        assert_eq!(error.bins, [2000; 3]);
        assert_eq!(error.emd, [0.25; 4]);
        assert_eq!(error.worst_emd(), ("r", 0.25));
        assert!(error.passed());
        assert_eq!(error.worst_bins(), 0.5);
        // One pixel more moves past it.
        moved[..(pixels / 4 + 1) * 3].fill(101);
        assert!(
            !histogram(&Counts::of(&moved, 3), &reference)
                .unwrap()
                .passed()
        );
        // A tenth of the pixels two codes up is a fifth of a code, within it.
        let mut far = vec![100; pixels * 3];
        far[..pixels / 10 * 3].fill(102);
        let error = histogram(&Counts::of(&far, 3), &reference).unwrap();
        assert!((error.emd[0] - 0.2).abs() < 1e-12 && error.passed());
        assert_eq!(error.limit(), 4.0);
        // Clipping is held to the same limit, counter by counter.
        let mut clipped = vec![100; pixels * 3];
        clipped[..15].fill(0);
        let error = histogram(&Counts::of(&clipped, 3), &reference).unwrap();
        assert_eq!(error.worst_clipping(), ("r0", 5.0 / 4000.0));
        assert!(!error.passed());
        // Frames of different sizes are refused rather than compared.
        assert!(histogram(&Counts::of(&[1; 9], 3), &reference).is_err());
    }

    #[test]
    fn samples_must_equal_the_screen_and_meet_the_display_limit() {
        let near = Sample {
            answered: [120, 120, 120],
            on_screen: Some([120, 120, 120]),
            reference: [120, 120, 120],
        };
        let error = samples(&[near; 25]).unwrap();
        assert_eq!((error.samples, error.on_screen, error.unequal), (25, 25, 0));
        assert!(error.passed(Class::Pointwise), "{error:?}");
        // One sample that is not the byte on screen fails, however close to the reference.
        let off = Sample {
            on_screen: Some([121, 120, 120]),
            ..near
        };
        let error = samples(&[near, off]).unwrap();
        assert_eq!(error.unequal, 1);
        assert!(!error.passed(Class::Spatial));
        // A sample far from the reference fails on its p99.
        let far = Sample {
            reference: [160, 120, 120],
            ..near
        };
        let mut set = vec![near; 24];
        set.push(far);
        let error = samples(&set).unwrap();
        assert!(error.p99 > Class::Spatial.limits().p99, "{error:?}");
        assert!(!error.passed(Class::Spatial));
        assert!(samples(&[]).is_err());
    }

    #[test]
    fn an_export_is_held_to_the_display_limit_and_to_itself() {
        let reference = rgb(20, 20, |x, y| [(x * 10) as u8, (y * 10) as u8, 50]);
        let same = Rgb8::new(20, 20, &reference).unwrap();
        let error = export(same, same, same).unwrap();
        assert!(error.passed(Class::Pointwise));
        assert_eq!(error.statistics.mean, 0.0);
        // The same bytes against the reference, but a repeat that differs: not repeatable.
        let mut other = reference.clone();
        other[0] ^= 1;
        let repeat = Rgb8::new(20, 20, &other).unwrap();
        let error = export(same, repeat, same).unwrap();
        assert!(!error.repeatable && !error.passed(Class::Spatial));
    }

    /// A share of a frame moved by one code is that share in codes by the earth mover's distance,
    /// counts twice in the summed bin difference, and mostly vanishes when the bins are widened;
    /// a far move costs its distance.
    #[test]
    fn the_histogram_diagnostics_measure_how_far_counts_moved() {
        let mut reference = [0u64; 256];
        reference[100] = 600;
        reference[101] = 400;
        let mut candidate = reference;
        // 100 pixels from 100 to 101: a tenth of the frame by one code.
        candidate[100] -= 100;
        candidate[101] += 100;
        assert!((histogram_emd(&candidate, &reference) - 0.1).abs() < 1e-12);
        assert!((histogram_mean_shift(&candidate, &reference) - 0.1).abs() < 1e-12);
        assert_eq!(
            histogram(
                &Counts {
                    bins: [candidate; 3],
                    clipping: [0; 11],
                    luma: candidate,
                },
                &Counts {
                    bins: [reference; 3],
                    clipping: [0; 11],
                    luma: reference,
                }
            )
            .unwrap()
            .bins[0],
            200
        );
        // 100 and 101 share a bin of two codes and of four.
        assert_eq!(histogram_binned(&candidate, &reference, 2), 0);
        assert_eq!(histogram_binned(&candidate, &reference, 4), 0);
        assert_eq!(histogram_binned(&candidate, &reference, 1), 200);
        // Ten pixels moved by fifty codes are half a code by the distance.
        let mut far = reference;
        far[100] -= 10;
        far[150] += 10;
        assert!((histogram_emd(&far, &reference) - 0.5).abs() < 1e-12);
        assert_eq!(histogram_binned(&far, &reference, 4), 20);
        // Pixel by pixel, by the largest channel difference.
        let a = [10u8, 10, 10, 0, 20, 20, 20, 0, 30, 30, 30, 0, 40, 40, 40, 0];
        let b = [10u8, 10, 10, 0, 21, 20, 20, 0, 30, 28, 30, 0, 40, 40, 47, 0];
        assert_eq!(code_shifts(&a, &b, 4).unwrap(), [1, 1, 1, 1]);
        assert!(code_shifts(&a, &b[..8], 4).is_err());
        // Luminance by Rec. 709 weights over the codes.
        let luma = luma_bins(&[255, 255, 255, 0, 0, 0, 0, 0], 4);
        assert_eq!((luma[255], luma[0]), (1, 1));
    }
}
