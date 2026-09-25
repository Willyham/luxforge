//! Bounded neutral-patch sampling directly from the retained sensor mosaic.
//!
//! [`RawSource::neutral_gains_at`] maps an upright default-crop point to the sensor through the
//! crop and orientation, reads one fixed 13×13 neighbourhood, applies the decoder's CFA and
//! per-site black calibration, and returns green-normalised sensor gains. It never demosaics or
//! builds a frame. The mosaic and calibration it reads were validated once at decode, so the
//! sampler checks only what a patch itself can get wrong: its bounds, dark or clipped sites, and
//! the gains it solves.

use crate::{MAX_GAIN, MosaicCorrection, RawError, RawSource};

/// The radius of the fixed sensor-space neutral-patch neighbourhood.
const PATCH_RADIUS: u32 = 6;
/// The side length of the fixed neutral-patch neighbourhood.
const PATCH_SIDE: u32 = PATCH_RADIUS * 2 + 1;
/// Samples at or below this normalised value are too dark.
///
/// `fixed_patch_rejects_bounds_dark_clipped_and_unusable_inputs` below exercises both bounds. The
/// UI may choose when to offer the picker, but it must not weaken this source-stage rejection once
/// sampling is requested.
const DARK_THRESHOLD: f64 = 0.01;
/// Samples at or above this normalised value are clipped or too close to clipping.
const CLIPPED_THRESHOLD: f64 = 0.995;

fn unusable(message: impl Into<String>) -> RawError {
    RawError::NeutralPatch(message.into())
}

/// The retained integer mosaic and the calibration that interprets one sensor sample. CFA and
/// black-repeat coordinates are anchored at sensor `(0, 0)`.
#[derive(Clone, Copy)]
struct Mosaic<'a> {
    samples: &'a [u16],
    corrections: &'a [MosaicCorrection],
    width: u32,
    height: u32,
    cfa_width: u8,
    cfa_height: u8,
    /// Red, green and blue are 0, 1 and 2.
    cfa: &'a [u8],
    /// Native CFA site IDs that select per-site black calibration; Bayer green sites stay
    /// distinct (usually IDs 1 and 3).
    black_cfa: &'a [u8],
    black_base: f32,
    black_channels: [f32; 4],
    black_repeat_width: u8,
    black_repeat_height: u8,
    black_repeat: &'a [f32],
    sensor_white: f32,
}

impl RawSource {
    /// The green-normalised gains `[green/red, 1, green/blue]` that make the fixed 13×13 pre-white-
    /// balance sensor patch around an upright default-crop point neutral.
    ///
    /// The point is mapped through the default crop and EXIF orientation. A corrected DNG then
    /// maps each CFA site through its channel's optical warp and weighs it by the spatial gain;
    /// dark and clipped rejection uses the sensor value before that gain, so optical gain above
    /// one is not taken for saturation. The result is finite, positive and at most the
    /// development's gain bound. Bounded point work: at most 169 sites, no frame allocated.
    pub fn neutral_gains_at(&self, x: u32, y: u32) -> Result<[f32; 3], RawError> {
        let metadata = &self.metadata;
        let crop = metadata.default_crop;
        let (out_w, out_h) = if (5..=8).contains(&metadata.exif_orientation) {
            (crop.height, crop.width)
        } else {
            (crop.width, crop.height)
        };
        if x >= out_w || y >= out_h {
            return Err(unusable("neutral picker point outside upright RAW image"));
        }
        let (sx, sy) = match metadata.exif_orientation {
            1 => (x, y),
            2 => (crop.width - 1 - x, y),
            3 => (crop.width - 1 - x, crop.height - 1 - y),
            4 => (x, crop.height - 1 - y),
            5 => (y, x),
            6 => (y, crop.height - 1 - x),
            7 => (crop.width - 1 - y, crop.height - 1 - x),
            8 => (crop.width - 1 - y, x),
            _ => return Err(RawError::InvalidInput("orientation")),
        };
        let mosaic = Mosaic {
            samples: &self.mosaic,
            corrections: &self.mosaic_corrections,
            width: metadata.sensor_width,
            height: metadata.sensor_height,
            cfa_width: metadata.cfa_width,
            cfa_height: metadata.cfa_height,
            cfa: &metadata.cfa,
            black_cfa: &metadata.black_cfa,
            black_base: metadata.black_base,
            black_channels: metadata.black_channels,
            black_repeat_width: metadata.black_repeat_width,
            black_repeat_height: metadata.black_repeat_height,
            black_repeat: &metadata.black_repeat,
            sensor_white: metadata.sensor_white,
        };
        let (corrected_x, corrected_y) = (crop.x + sx, crop.y + sy);
        if self.dng_correction.is_some() {
            neutral_gains(
                &mosaic,
                corrected_x,
                corrected_y,
                &|x, y, channel| {
                    self.corrected_sensor_sample_location(x, y, channel)
                        .map_err(|error| unusable(format!("neutral picker warp point: {error}")))
                },
                &|x, y, channel| {
                    self.gain_at_corrected_sensor(x, y, channel)
                        .map_err(|error| unusable(format!("neutral picker gain point: {error}")))
                },
            )
        } else {
            neutral_gains(
                &mosaic,
                corrected_x,
                corrected_y,
                &|x, y, _| Ok((f64::from(x), f64::from(y))),
                &|_, _, _| Ok(1.0),
            )
        }
    }
}

fn black_at(source: &Mosaic<'_>, x: u32, y: u32, black_channel: usize) -> f64 {
    let mut black = f64::from(source.black_base) + f64::from(source.black_channels[black_channel]);
    if source.black_repeat_width != 0 {
        let repeat_x = (x % u32::from(source.black_repeat_width)) as usize;
        let repeat_y = (y % u32::from(source.black_repeat_height)) as usize;
        let index = repeat_y * usize::from(source.black_repeat_width) + repeat_x;
        black += f64::from(source.black_repeat[index]);
    }
    black
}

/// Sample one 13×13 patch around a sensor point in corrected coordinates. Each site is mapped for
/// its own colour channel, then read from the nearest sensor site with that CFA colour, so a
/// chromatic warp is not treated as a single centre translation. Every site must be finite, above
/// [`DARK_THRESHOLD`] and below [`CLIPPED_THRESHOLD`] after its own black subtraction and white
/// normalisation. The mapper and gain lookup each run at most 169 times.
fn neutral_gains(
    source: &Mosaic<'_>,
    center_x: u32,
    center_y: u32,
    map_at: &dyn Fn(u32, u32, usize) -> Result<(f64, f64), RawError>,
    gain_at: &dyn Fn(f64, f64, usize) -> Result<f64, RawError>,
) -> Result<[f32; 3], RawError> {
    let start_x = center_x
        .checked_sub(PATCH_RADIUS)
        .ok_or_else(|| unusable("neutral picker patch is outside the sensor"))?;
    let start_y = center_y
        .checked_sub(PATCH_RADIUS)
        .ok_or_else(|| unusable("neutral picker patch is outside the sensor"))?;
    let end_x = start_x
        .checked_add(PATCH_SIDE - 1)
        .ok_or_else(|| unusable("neutral picker patch bounds overflow"))?;
    let end_y = start_y
        .checked_add(PATCH_SIDE - 1)
        .ok_or_else(|| unusable("neutral picker patch bounds overflow"))?;
    if end_x >= source.width || end_y >= source.height {
        return Err(unusable("neutral picker patch is outside the sensor"));
    }

    let cfa_width = u32::from(source.cfa_width);
    let cfa_height = u32::from(source.cfa_height);
    let mut sums = [0.0_f64; 3];
    let mut counts = [0_u32; 3];
    for y in start_y..=end_y {
        for x in start_x..=end_x {
            let cfa_index = ((y % cfa_height) * cfa_width + (x % cfa_width)) as usize;
            let channel = usize::from(source.cfa[cfa_index]);
            let (mapped_x, mapped_y) = map_at(x, y, channel)?;
            let (sample_x, sample_y) = nearest_site(source, mapped_x, mapped_y, channel)?;
            let sample_cfa_index =
                ((sample_y % cfa_height) * cfa_width + (sample_x % cfa_width)) as usize;
            let black_channel = usize::from(source.black_cfa[sample_cfa_index]);
            let black = black_at(source, sample_x, sample_y, black_channel);
            let sample_index = (sample_y as usize) * (source.width as usize) + sample_x as usize;
            let sample = f64::from(
                match source
                    .corrections
                    .binary_search_by_key(&(sample_index as u32), |p| p.index)
                {
                    Ok(index) => source.corrections[index].value,
                    Err(_) => source.samples[sample_index],
                },
            );
            let denominator = f64::from(source.sensor_white) - black;
            let normalized = (sample - black) / denominator;
            if !black.is_finite()
                || !denominator.is_finite()
                || denominator <= 0.0
                || !normalized.is_finite()
                || normalized <= DARK_THRESHOLD
                || normalized >= CLIPPED_THRESHOLD
            {
                return Err(unusable(
                    "neutral picker patch is dark, clipped or non-finite",
                ));
            }
            let gain = gain_at(x as f64, y as f64, channel)?;
            if !gain.is_finite() || gain <= 0.0 {
                return Err(unusable("neutral picker site gain is invalid"));
            }
            let corrected = normalized * gain;
            if !corrected.is_finite() || corrected <= 0.0 {
                return Err(unusable("neutral picker corrected sample is invalid"));
            }
            sums[channel] += corrected;
            counts[channel] += 1;
        }
    }

    let means: [f64; 3] = std::array::from_fn(|channel| sums[channel] / f64::from(counts[channel]));
    if !means.iter().all(|value| value.is_finite() && *value > 0.0) {
        return Err(unusable("neutral picker patch has an unusable channel"));
    }
    let max_gain = f64::from(MAX_GAIN);
    let gains = [means[1] / means[0], 1.0, means[1] / means[2]];
    if !gains
        .iter()
        .all(|value| value.is_finite() && *value > 0.0 && *value <= max_gain)
    {
        return Err(unusable(
            "neutral picker gains are outside the finite positive <=32 range",
        ));
    }
    let gains = gains.map(|value| value as f32);
    if !gains
        .iter()
        .all(|value| value.is_finite() && *value > 0.0 && f64::from(*value) <= max_gain)
    {
        return Err(unusable(
            "neutral picker gains are not representable as f32",
        ));
    }
    Ok(gains)
}

fn nearest_site(
    source: &Mosaic<'_>,
    x: f64,
    y: f64,
    channel: usize,
) -> Result<(u32, u32), RawError> {
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= f64::from(source.width)
        || y >= f64::from(source.height)
    {
        return Err(unusable(
            "neutral picker mapped point is outside the sensor",
        ));
    }
    if x.fract() == 0.0 && y.fract() == 0.0 {
        let (site_x, site_y) = (x as u32, y as u32);
        let site = ((site_y % u32::from(source.cfa_height)) * u32::from(source.cfa_width)
            + site_x % u32::from(source.cfa_width)) as usize;
        if usize::from(source.cfa[site]) == channel {
            return Ok((site_x, site_y));
        }
    }
    let center_x = x.round() as i64;
    let center_y = y.round() as i64;
    let radius = i64::from(source.cfa_width.max(source.cfa_height));
    let mut nearest: Option<(f64, u32, u32)> = None;
    for sy in center_y - radius..=center_y + radius {
        for sx in center_x - radius..=center_x + radius {
            if sx < 0 || sy < 0 || sx >= i64::from(source.width) || sy >= i64::from(source.height) {
                continue;
            }
            let site = ((sy as u32 % u32::from(source.cfa_height)) * u32::from(source.cfa_width)
                + sx as u32 % u32::from(source.cfa_width)) as usize;
            if usize::from(source.cfa[site]) != channel {
                continue;
            }
            let distance = (sx as f64 - x).powi(2) + (sy as f64 - y).powi(2);
            if nearest.is_none_or(|(best, _, _)| distance < best) {
                nearest = Some((distance, sx as u32, sy as u32));
            }
        }
    }
    nearest
        .map(|(_, x, y)| (x, y))
        .ok_or_else(|| unusable("neutral picker mapped point has no matching CFA site"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAYER: [u8; 4] = [0, 1, 1, 2];
    const BAYER_BLACK: [u8; 4] = [0, 1, 3, 2];
    const XTRANS: [u8; 36] = [
        1, 1, 0, 1, 1, 2, 1, 1, 2, 1, 1, 0, 2, 0, 1, 0, 2, 1, 1, 1, 2, 1, 1, 0, 1, 1, 0, 1, 1, 2,
        0, 2, 1, 2, 0, 1,
    ];

    fn identity(x: u32, y: u32, _: usize) -> Result<(f64, f64), RawError> {
        Ok((f64::from(x), f64::from(y)))
    }

    fn unit(_: f64, _: f64, _: usize) -> Result<f64, RawError> {
        Ok(1.0)
    }

    fn unmapped(source: &Mosaic<'_>, x: u32, y: u32) -> Result<[f32; 3], RawError> {
        neutral_gains(source, x, y, &identity, &unit)
    }

    #[allow(clippy::too_many_arguments)]
    fn source<'a>(
        samples: &'a [u16],
        width: u32,
        height: u32,
        cfa_width: u8,
        cfa_height: u8,
        cfa: &'a [u8],
        black_repeat: &'a [f32],
        sensor_white: f32,
    ) -> Mosaic<'a> {
        Mosaic {
            corrections: &[],
            samples,
            width,
            height,
            cfa_width,
            cfa_height,
            cfa,
            black_cfa: if cfa_width == 2 { &BAYER_BLACK } else { cfa },
            black_base: 10.0,
            black_channels: [2.0, 3.0, 4.0, 5.0],
            black_repeat_width: 2,
            black_repeat_height: 2,
            black_repeat,
            sensor_white,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fixture(
        width: u32,
        height: u32,
        cfa_width: u8,
        cfa_height: u8,
        cfa: &[u8],
        values: [f64; 3],
        black_repeat: &[f32],
        sensor_white: f32,
    ) -> Vec<u16> {
        let mut samples = vec![0_u16; (width * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let channel = usize::from(
                    cfa[((y % u32::from(cfa_height)) * u32::from(cfa_width)
                        + (x % u32::from(cfa_width))) as usize],
                );
                let black_channel = if cfa_width == 2 {
                    usize::from(BAYER_BLACK[((y % 2) * 2 + (x % 2)) as usize])
                } else {
                    channel
                };
                let black = 10.0
                    + [2.0, 3.0, 4.0, 5.0][black_channel]
                    + f64::from(black_repeat[((y % 2) * 2 + (x % 2)) as usize]);
                samples[(y * width + x) as usize] =
                    (black + values[channel] * (f64::from(sensor_white) - black)).round() as u16;
            }
        }
        samples
    }

    fn close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.002, "{actual} vs {expected}");
    }

    #[test]
    fn bayer_phase_and_per_site_black_reference_are_independent() {
        let black_repeat = [1.0, 2.0, 3.0, 4.0];
        let pixels = fixture(
            24,
            22,
            2,
            2,
            &BAYER,
            [0.25, 0.5, 0.125],
            &black_repeat,
            65_535.0,
        );
        let view = source(&pixels, 24, 22, 2, 2, &BAYER, &black_repeat, 65_535.0);
        let gains = unmapped(&view, 9, 10).unwrap();
        close(gains[0], 2.0);
        close(gains[1], 1.0);
        close(gains[2], 4.0);
    }

    #[test]
    fn sparse_sensor_repairs_are_used_without_mutating_the_mosaic() {
        let black_repeat = [1.0, 2.0, 3.0, 4.0];
        let mut pixels = fixture(
            24,
            22,
            2,
            2,
            &BAYER,
            [0.25, 0.5, 0.125],
            &black_repeat,
            65_535.0,
        );
        let expected = unmapped(
            &source(&pixels, 24, 22, 2, 2, &BAYER, &black_repeat, 65_535.0),
            9,
            10,
        )
        .unwrap();
        let index = 10 * 24 + 10;
        let repair = MosaicCorrection {
            index: index as u32,
            value: pixels[index],
        };
        pixels[index] = 0;
        let before = pixels.clone();
        let mut view = source(&pixels, 24, 22, 2, 2, &BAYER, &black_repeat, 65_535.0);
        assert!(unmapped(&view, 9, 10).is_err());
        view.corrections = std::slice::from_ref(&repair);
        assert_eq!(unmapped(&view, 9, 10).unwrap(), expected);
        assert_eq!(pixels, before);
    }

    #[test]
    fn spatial_site_gain_changes_neutral_ratios_in_bounded_patch() {
        let black_repeat = [1.0, 2.0, 3.0, 4.0];
        let pixels = fixture(
            24,
            22,
            2,
            2,
            &BAYER,
            [0.25, 0.5, 0.125],
            &black_repeat,
            65_535.0,
        );
        let view = source(&pixels, 24, 22, 2, 2, &BAYER, &black_repeat, 65_535.0);
        let calls = std::cell::Cell::new(0);
        let gains = neutral_gains(&view, 9, 10, &identity, &|_, _, channel| {
            calls.set(calls.get() + 1);
            Ok([2.0, 1.0, 0.5][channel])
        })
        .unwrap();
        assert_eq!(calls.get(), PATCH_SIDE * PATCH_SIDE);
        close(gains[0], 1.0);
        close(gains[1], 1.0);
        close(gains[2], 8.0);
        let highlight = neutral_gains(&view, 9, 10, &identity, &|_, _, channel| {
            Ok([8.0, 1.0, 1.0][channel])
        })
        .unwrap();
        close(highlight[0], 0.25);
        assert!(neutral_gains(&view, 9, 10, &identity, &|_, _, _| Ok(f64::NAN)).is_err());
    }

    #[test]
    fn chromatic_warp_maps_each_corrected_site_to_its_own_sensor_color() {
        let mut pixels = vec![0_u16; 64 * 48];
        for y in 0..48_u32 {
            for x in 0..64_u32 {
                let channel = usize::from(BAYER[((y % 2) * 2 + x % 2) as usize]);
                let normalized: f64 = match channel {
                    0 if x >= 40 => 0.5,
                    0 => 0.25,
                    1 => 0.5,
                    2 if x <= 24 => 0.25,
                    _ => 0.125,
                };
                pixels[(y * 64 + x) as usize] = (normalized * 65_535.0).round() as u16;
            }
        }
        let view = Mosaic {
            corrections: &[],
            samples: &pixels,
            width: 64,
            height: 48,
            cfa_width: 2,
            cfa_height: 2,
            cfa: &BAYER,
            black_cfa: &BAYER,
            black_base: 0.0,
            black_channels: [0.0; 4],
            black_repeat_width: 0,
            black_repeat_height: 0,
            black_repeat: &[],
            sensor_white: 65_535.0,
        };
        let calls = std::cell::Cell::new(0);
        let gains = neutral_gains(
            &view,
            32,
            24,
            &|x, y, channel| {
                calls.set(calls.get() + 1);
                Ok((f64::from(x) + [15.0, 0.0, -15.0][channel], f64::from(y)))
            },
            &unit,
        )
        .unwrap();
        assert_eq!(calls.get(), PATCH_SIDE * PATCH_SIDE);
        close(gains[0], 1.0);
        close(gains[2], 2.0);
        assert!(neutral_gains(&view, 32, 24, &|_, _, _| Ok((f64::NAN, 0.0)), &unit).is_err());
    }

    #[test]
    fn xtrans_phase_uses_the_full_six_by_six_pattern() {
        let black_repeat = [1.0, 2.0, 3.0, 4.0];
        let pixels = fixture(
            24,
            24,
            6,
            6,
            &XTRANS,
            [0.25, 0.5, 0.125],
            &black_repeat,
            65_535.0,
        );
        let view = source(&pixels, 24, 24, 6, 6, &XTRANS, &black_repeat, 65_535.0);
        let gains = unmapped(&view, 11, 11).unwrap();
        close(gains[0], 2.0);
        close(gains[1], 1.0);
        close(gains[2], 4.0);
    }

    #[test]
    fn fixed_patch_rejects_bounds_dark_clipped_and_unusable_inputs() {
        let black_repeat = [1.0, 2.0, 3.0, 4.0];
        let valid = fixture(
            20,
            20,
            2,
            2,
            &BAYER,
            [0.25, 0.5, 0.125],
            &black_repeat,
            65_535.0,
        );
        let view = source(&valid, 20, 20, 2, 2, &BAYER, &black_repeat, 65_535.0);
        assert!(unmapped(&view, 0, 0).is_err());
        assert!(unmapped(&view, u32::MAX, u32::MAX).is_err());

        for values in [[0.005, 0.5, 0.125], [0.25, 0.5, 0.999], [0.015, 0.5, 0.015]] {
            let pixels = fixture(20, 20, 2, 2, &BAYER, values, &black_repeat, 65_535.0);
            let view = source(&pixels, 20, 20, 2, 2, &BAYER, &black_repeat, 65_535.0);
            assert!(unmapped(&view, 9, 9).is_err());
        }

        let mut malformed = view;
        malformed.sensor_white = f32::NAN;
        assert!(unmapped(&malformed, 9, 9).is_err());
        malformed = view;
        malformed.black_repeat = &[f32::NAN, 2.0, 3.0, 4.0];
        assert!(unmapped(&malformed, 9, 9).is_err());
    }
}
