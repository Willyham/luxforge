//! The development's sensor normalization and output scale, on the development executor.
//!
//! [`Normalization::run`] turns the retained integer mosaic into the float mosaic the native
//! demosaic reads: each site's black level is subtracted, the result scaled so sensor white is
//! [`SENSOR_SCALE`], and multiplied by its colour's white-balance gain. [`scale_planes`] divides
//! the demosaiced planes by the same scale. [`BlackLevels`] is the one per-site black model,
//! shared with the neutral picker.

use crate::{MosaicCorrection, RawError, native_tiles};
use std::{
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
};

/// Sensor white in the normalized mosaic, the numerical contract of both native demosaics. The
/// developed planes are divided by it, so sensor white develops to 1.
pub(crate) const SENSOR_SCALE: f32 = 65535.0;

/// Rows in one normalization job: about 124 thousand sites on the X100VI, a fraction of a
/// millisecond, so a pool thread that steals one returns to its own work well within the time of
/// a native tile job.
const JOB_ROWS: usize = 16;

/// The largest number of sparse sensor repairs a development accepts.
const MAX_CORRECTIONS: usize = 65_536;

/// The decoder's per-site black calibration, as `interpret` validated it at decode. Sites are
/// anchored at sensor `(0, 0)`; the CFA and the repeat pattern each tile the sensor.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlackLevels<'a> {
    pub cfa_width: usize,
    pub cfa_height: usize,
    /// LibRaw's calibration site IDs, 0 to 3: a Bayer frame's two green sites stay distinct.
    pub black_cfa: &'a [u8],
    pub base: f32,
    pub channels: [f32; 4],
    /// Zero width and height when there is no repeat pattern.
    pub repeat_width: usize,
    pub repeat_height: usize,
    pub repeat: &'a [f32],
}

impl<'a> BlackLevels<'a> {
    /// The calibration site ID of sensor site `(x, y)`.
    fn site(&self, x: usize, y: usize) -> u8 {
        self.black_cfa[(y % self.cfa_height) * self.cfa_width + x % self.cfa_width]
    }

    /// The black level of sensor site `(x, y)`: the base plus the site's channel black, plus the
    /// repeat pattern's entry, added in `f32` in that order. LibRaw's calibration values are
    /// integers, so each sum is exact. The site ID must be at most 3, which decode validates.
    pub(crate) fn at(&self, x: usize, y: usize) -> f32 {
        let mut black = self.base + self.channels[usize::from(self.site(x, y))];
        if self.repeat_width != 0 && self.repeat_height != 0 {
            black +=
                self.repeat[(y % self.repeat_height) * self.repeat_width + x % self.repeat_width];
        }
        black
    }
}

/// One development's normalization inputs.
pub(crate) struct Normalization<'a> {
    pub samples: &'a [u16],
    /// Sorted, unique sensor offsets whose value replaces the retained sample in this development
    /// only.
    pub corrections: &'a [MosaicCorrection],
    pub width: usize,
    /// Red, green and blue are 0, 1 and 2; the same size as the black model's CFA.
    pub cfa: &'a [u8],
    pub black: BlackLevels<'a>,
    pub white: f32,
    pub gains: [f32; 3],
}

/// Every site's black level, white scale and gain over one period of the CFA and black patterns,
/// each row repeated to at least 64 sites so a row's inner loop is long enough to vectorise.
struct Sites {
    width: usize,
    height: usize,
    black: Vec<f32>,
    scale: Vec<f32>,
    gain: Vec<f32>,
}

impl Sites {
    fn index(&self, x: usize, y: usize) -> usize {
        (y % self.height) * self.width + x % self.width
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b) * b
}

impl Normalization<'_> {
    /// The per-site values, computed as the development applies them: `black` from
    /// [`BlackLevels::at`], then `SENSOR_SCALE / (white - black)` in `f32`. Each site's CFA
    /// channel, calibration ID and denominator are checked here, once per site of the period.
    fn sites(&self) -> Result<Sites, RawError> {
        let black = &self.black;
        if black.cfa_width == 0
            || black.cfa_height == 0
            || self.cfa.len() != black.cfa_width * black.cfa_height
            || black.black_cfa.len() != self.cfa.len()
            || black.repeat.len() != black.repeat_width * black.repeat_height
        {
            return Err(RawError::UnsupportedCfa);
        }
        let (repeat_width, repeat_height) = if black.repeat_width != 0 && black.repeat_height != 0 {
            (black.repeat_width, black.repeat_height)
        } else {
            (1, 1)
        };
        let period = lcm(black.cfa_width, repeat_width);
        let width = period * 64_usize.div_ceil(period);
        let height = lcm(black.cfa_height, repeat_height);
        let mut sites = Sites {
            width,
            height,
            black: Vec::with_capacity(width * height),
            scale: Vec::with_capacity(width * height),
            gain: Vec::with_capacity(width * height),
        };
        for y in 0..height {
            for x in 0..width {
                let channel = usize::from(
                    self.cfa[(y % black.cfa_height) * black.cfa_width + x % black.cfa_width],
                );
                if channel > 2 || black.site(x, y) > 3 {
                    return Err(RawError::UnsupportedCfa);
                }
                let level = black.at(x, y);
                let denominator = self.white - level;
                if !denominator.is_finite() || denominator <= 0.0 {
                    return Err(RawError::MissingCalibration("black/white denominator"));
                }
                sites.black.push(level);
                sites.scale.push(SENSOR_SCALE / denominator);
                sites.gain.push(self.gains[channel]);
            }
        }
        Ok(sites)
    }

    /// The normalized float mosaic: `((sample - black) * scale) * gain` for every site, in `f32`,
    /// in that order, with each sparse repair's value in place of its retained sample. The rows
    /// run in [`JOB_ROWS`]-row jobs on the development executor with `lanes` at once, checking
    /// cancellation before every row.
    ///
    /// The mosaic is allocated without being initialised and every element is written exactly
    /// once (a repaired site twice) by the job that owns its rows; it is adopted only once every
    /// job has returned successfully.
    pub(crate) fn run(&self, lanes: usize, cancel: &AtomicBool) -> Result<Vec<f32>, RawError> {
        let n = self.samples.len();
        if self.width == 0 || !n.is_multiple_of(self.width) {
            return Err(RawError::InvalidInput("mosaic is not whole rows"));
        }
        if self.corrections.len() > MAX_CORRECTIONS
            || self
                .corrections
                .last()
                .is_some_and(|last| last.index as usize >= n)
            || self
                .corrections
                .windows(2)
                .any(|pair| pair[0].index >= pair[1].index)
        {
            return Err(RawError::InvalidInput("invalid sparse mosaic corrections"));
        }
        let sites = self.sites()?;
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let mut mosaic = Vec::new();
        mosaic
            .try_reserve_exact(n)
            .map_err(|_| RawError::ResourceLimit("normalized mosaic allocation"))?;
        let jobs = mosaic.spare_capacity_mut()[..n]
            .chunks_mut(self.width * JOB_ROWS)
            .enumerate();
        native_tiles::refill_each(lanes, jobs, |(job, rows)| {
            self.rows(&sites, job * JOB_ROWS, rows, cancel)
        })?;
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        // SAFETY: the executor returned success, so every job ran to completion, and the jobs'
        // disjoint slices cover the first `n` elements, each of which its job wrote.
        unsafe { mosaic.set_len(n) };
        Ok(mosaic)
    }

    /// Normalize the whole rows from `first_row` into `out`.
    fn rows(
        &self,
        sites: &Sites,
        first_row: usize,
        out: &mut [MaybeUninit<f32>],
        cancel: &AtomicBool,
    ) -> Result<(), RawError> {
        let width = self.width;
        let start = first_row * width;
        for (index, out_row) in out.chunks_exact_mut(width).enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(RawError::Cancelled);
            }
            let y = first_row + index;
            let samples = &self.samples[y * width..(y + 1) * width];
            let row = sites.index(0, y);
            let black = &sites.black[row..row + sites.width];
            let scale = &sites.scale[row..row + sites.width];
            let gain = &sites.gain[row..row + sites.width];
            for (out, samples) in out_row
                .chunks_mut(sites.width)
                .zip(samples.chunks(sites.width))
            {
                for ((((out, &sample), &black), &scale), &gain) in
                    out.iter_mut().zip(samples).zip(black).zip(scale).zip(gain)
                {
                    out.write(((f32::from(sample) - black) * scale) * gain);
                }
            }
        }
        let end = start + out.len();
        let first = self
            .corrections
            .partition_point(|correction| (correction.index as usize) < start);
        for correction in self.corrections[first..]
            .iter()
            .take_while(|correction| (correction.index as usize) < end)
        {
            let index = correction.index as usize;
            let site = sites.index(index % width, index / width);
            out[index - start].write(
                ((f32::from(correction.value) - sites.black[site]) * sites.scale[site])
                    * sites.gain[site],
            );
        }
        Ok(())
    }
}
