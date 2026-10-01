//! The development's sensor normalization and output scale, on the development executor.
//!
//! [`Normalization::run`] turns the retained integer mosaic into the float mosaic the native
//! demosaic reads: each site's black level is subtracted, the result scaled so sensor white is
//! [`SENSOR_SCALE`], and multiplied by its colour's white-balance gain. [`scale_planes`] divides
//! the demosaiced planes by the same scale. [`BlackLevels`] is the one per-site black model,
//! shared with the neutral picker.

use crate::{MosaicCorrection, RawError, RawMetadata, native_tiles};
use std::{
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
};

/// Sensor white in the normalized mosaic, the numerical contract of both native demosaics. The
/// developed planes are divided by it, so sensor white develops to 1.
///
/// Normalization clamps nothing, but the pinned RCD reads each Bayer site as
/// `LIM01(value / 65536)`: a gained site is clipped to [0, 65536/65535] of sensor white before
/// interpolation, except in the 9 px border band the border pass fills from this unclamped input.
/// Markesteijn has no input clamp, so X-Trans keeps both under-black and over-white values
/// (`bayer_input_clips_at_sensor_white_after_gain_and_x_trans_does_not` pins this).
pub(crate) const SENSOR_SCALE: f32 = 65535.0;

/// Rows in one normalization or output-scale job: about 124 thousand sites or values on the
/// X100VI, a fraction of a millisecond, so a pool thread that steals one returns to its own work
/// well within the time of a native tile job.
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
    pub(crate) fn of(metadata: &'a RawMetadata) -> Self {
        Self {
            cfa_width: usize::from(metadata.cfa_width),
            cfa_height: usize::from(metadata.cfa_height),
            black_cfa: &metadata.black_cfa,
            base: metadata.black_base,
            channels: metadata.black_channels,
            repeat_width: usize::from(metadata.black_repeat_width),
            repeat_height: usize::from(metadata.black_repeat_height),
            repeat: &metadata.black_repeat,
        }
    }

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

impl Normalization<'_> {
    /// The share of the sites in `rect`, `[x, y, width, height]` in sensor coordinates, whose
    /// normalized value is at least `fraction` of sensor white: the sites the Bayer demosaic's
    /// input clamp cuts, or nearly cuts, at these gains. It normalizes the whole mosaic as a
    /// development does, repairs included, and counts; it keeps nothing.
    pub(crate) fn share_at_or_above(
        &self,
        fraction: f32,
        rect: [usize; 4],
        lanes: usize,
        cancel: &AtomicBool,
    ) -> Result<f64, RawError> {
        let [x, y, width, height] = rect;
        let rows = self.samples.len() / self.width.max(1);
        if width == 0 || height == 0 || x + width > self.width || y + height > rows {
            return Err(RawError::InvalidInput(
                "clip share rectangle outside the mosaic",
            ));
        }
        let mosaic = self.run(lanes, cancel)?;
        let threshold = fraction * SENSOR_SCALE;
        let clipped: usize = mosaic
            .chunks_exact(self.width)
            .skip(y)
            .take(height)
            .map(|row| {
                row[x..x + width]
                    .iter()
                    .filter(|value| **value >= threshold)
                    .count()
            })
            .sum();
        Ok(clipped as f64 / (width * height) as f64)
    }
}

/// Divide the demosaiced planes, whole rows of `width` values, by [`SENSOR_SCALE`] in place, in
/// [`JOB_ROWS`]-row jobs on the development executor with `lanes` at once. Every value is divided
/// the same way, so the three contiguous planes are one pass. The first cancellation stops the
/// queue.
pub(crate) fn scale_planes(
    planes: &mut [f32],
    width: usize,
    lanes: usize,
    cancel: &AtomicBool,
) -> Result<(), RawError> {
    native_tiles::refill_each(
        lanes,
        planes.chunks_mut(width.max(1) * JOB_ROWS),
        |values| {
            if cancel.load(Ordering::Relaxed) {
                return Err(RawError::Cancelled);
            }
            for value in values {
                *value /= SENSOR_SCALE;
            }
            Ok(())
        },
    )?;
    if cancel.load(Ordering::Relaxed) {
        return Err(RawError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAYER: [u8; 4] = [0, 1, 1, 2];
    const BAYER_BLACK: [u8; 4] = [0, 1, 3, 2];

    fn normalization<'a>(
        samples: &'a [u16],
        corrections: &'a [MosaicCorrection],
    ) -> Normalization<'a> {
        Normalization {
            samples,
            corrections,
            width: 64,
            cfa: &BAYER,
            black: BlackLevels {
                cfa_width: 2,
                cfa_height: 2,
                black_cfa: &BAYER_BLACK,
                base: 64.0,
                channels: [1.0, 2.0, 3.0, 4.0],
                repeat_width: 0,
                repeat_height: 0,
                repeat: &[],
            },
            white: 4095.0,
            gains: [2.0, 1.0, 1.5],
        }
    }

    /// Each site's black is the base, its own calibration site's channel and the repeat entry;
    /// sensor white maps to the sensor scale before the gain, and a repair replaces its site.
    #[test]
    fn black_levels_scale_and_repairs_follow_each_site() {
        let samples = vec![4095_u16; 64 * 32];
        let mut repeated = normalization(&samples, &[]);
        let repeat = [0.0, 5.0, 7.0];
        repeated.black.repeat_width = 3;
        repeated.black.repeat_height = 1;
        repeated.black.repeat = &repeat;
        assert_eq!(repeated.black.at(0, 0), 65.0);
        assert_eq!(repeated.black.at(1, 0), 66.0 + 5.0);
        assert_eq!(repeated.black.at(0, 1), 68.0);
        assert_eq!(repeated.black.at(5, 1), 67.0 + 7.0);
        let repair = [MosaicCorrection {
            index: 64 * 20 + 1,
            value: 66,
        }];
        let mosaic = normalization(&samples, &repair)
            .run(1, &AtomicBool::new(false))
            .unwrap();
        let close = |actual: f32, expected: f32| (actual - expected).abs() < 0.02;
        assert!(close(mosaic[0], SENSOR_SCALE * 2.0), "{}", mosaic[0]);
        assert!(close(mosaic[1], SENSOR_SCALE), "{}", mosaic[1]);
        assert!(close(mosaic[64], SENSOR_SCALE), "{}", mosaic[64]);
        assert!(close(mosaic[65], SENSOR_SCALE * 1.5), "{}", mosaic[65]);
        assert_eq!(mosaic[64 * 20 + 1], 0.0);
    }

    /// The clip share counts the sites in the rectangle whose gained value reaches the fraction
    /// of sensor white: here the red sites at 0.6 of white, gained by 2, and nothing ungained.
    #[test]
    fn the_clip_share_counts_gained_sites_at_sensor_white_in_the_rectangle() {
        let never = AtomicBool::new(false);
        // Black 65 to 68 by site; 0.6 of the 4031-code range above black puts red at 1.2 of white
        // once gained by 2, blue at 0.9 once gained by 1.5 and green at 0.6.
        let samples: Vec<u16> = (0..64 * 32)
            .map(|index| {
                let (x, y) = (index % 64, index / 64);
                let black: f32 = 64.0 + [1.0, 2.0, 4.0, 3.0][(y % 2) * 2 + x % 2];
                (black + 0.6 * (4095.0 - black)).round() as u16
            })
            .collect();
        let bayer = normalization(&samples, &[]);
        let whole = bayer
            .share_at_or_above(0.99, [0, 0, 64, 32], 1, &never)
            .unwrap();
        assert_eq!(whole, 0.25, "the red quarter of the sites");
        let mut raised = normalization(&samples, &[]);
        raised.gains = [2.0, 1.0, 1.7];
        assert_eq!(
            raised
                .share_at_or_above(0.99, [0, 0, 64, 32], 1, &never)
                .unwrap(),
            0.5,
            "blue at 1.02 of white joins red"
        );
        // One red column in a one-pixel-wide rectangle, a green one beside it.
        assert_eq!(
            bayer
                .share_at_or_above(0.99, [2, 0, 1, 32], 1, &never)
                .unwrap(),
            0.5
        );
        assert_eq!(
            bayer
                .share_at_or_above(0.99, [1, 0, 1, 1], 1, &never)
                .unwrap(),
            0.0
        );
        assert!(matches!(
            bayer.share_at_or_above(0.99, [60, 0, 5, 1], 1, &never),
            Err(RawError::InvalidInput(_))
        ));
    }

    /// Invalid calibration, CFA or repairs fail before any mosaic is allocated, and a cancelled
    /// token stops the normalization, before and within its rows, and the output scale.
    #[test]
    fn invalid_inputs_and_cancellation_return_no_mosaic() {
        let samples = vec![2048_u16; 64 * 32];
        let never = AtomicBool::new(false);
        let mut invalid = normalization(&samples, &[]);
        invalid.white = 0.0;
        assert!(matches!(
            invalid.run(1, &never),
            Err(RawError::MissingCalibration(_))
        ));
        let cfa = [0, 1, 3, 2];
        let mut invalid = normalization(&samples, &[]);
        invalid.cfa = &cfa;
        assert_eq!(invalid.run(1, &never), Err(RawError::UnsupportedCfa));
        let black_cfa = [0, 1, 4, 2];
        let mut invalid = normalization(&samples, &[]);
        invalid.black.black_cfa = &black_cfa;
        assert_eq!(invalid.run(1, &never), Err(RawError::UnsupportedCfa));
        let invalid = normalization(&samples[..100], &[]);
        assert!(matches!(
            invalid.run(1, &never),
            Err(RawError::InvalidInput(_))
        ));
        let repair = |index| MosaicCorrection { index, value: 1 };
        for corrections in [
            vec![repair(5), repair(5)],
            vec![repair(9), repair(3)],
            vec![repair(64 * 32)],
        ] {
            assert!(matches!(
                normalization(&samples, &corrections).run(1, &never),
                Err(RawError::InvalidInput(_))
            ));
        }

        let cancelled = AtomicBool::new(true);
        let valid = normalization(&samples, &[]);
        assert_eq!(valid.run(4, &cancelled), Err(RawError::Cancelled));
        let sites = valid.sites().unwrap();
        let mut rows = vec![MaybeUninit::uninit(); 64 * JOB_ROWS];
        assert_eq!(
            valid.rows(&sites, 0, &mut rows, &cancelled),
            Err(RawError::Cancelled)
        );
        let mut planes = vec![1.0_f32; 64 * 32 * 3];
        assert_eq!(
            scale_planes(&mut planes, 64, 4, &cancelled),
            Err(RawError::Cancelled)
        );
        scale_planes(&mut planes, 64, 4, &never).unwrap();
        assert!(planes.iter().all(|value| *value == 1.0 / SENSOR_SCALE));
    }
}
