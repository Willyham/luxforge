//! Bounded neutral-patch sampling directly from an unpacked sensor mosaic.
//!
//! The host maps an edited upright point to sensor coordinates and passes that point here. This
//! module deliberately knows nothing about crop or orientation, and it never demosaics or builds a
//! frame. It reads one fixed 13x13 neighborhood, applies the decoder's CFA and per-site black
//! calibration, and returns green-normalized sensor gains.

use crate::{Error, ErrorKind};

/// The radius of the fixed sensor-space neutral-patch neighborhood.
pub const PATCH_RADIUS: u32 = 6;
/// The side length of the fixed neutral-patch neighborhood.
pub const PATCH_SIDE: u32 = PATCH_RADIUS * 2 + 1;
/// Samples at or below this normalized value are considered too dark.
///
/// These are the independent fixture thresholds from `xtask raw-reference`; keeping them here
/// makes the numerical policy explicit. The UI may choose when to offer the picker, but it must
/// not weaken this source-stage rejection once sampling is requested.
pub const DARK_THRESHOLD: f64 = 0.01;
/// Samples at or above this normalized value are considered clipped or too close to clipping.
pub const CLIPPED_THRESHOLD: f64 = 0.995;

const MAX_SENSOR_SIDE: u32 = 16_384;
const MAX_SENSOR_PIXELS: usize = lightwell_raw::MAX_PIXELS;
const MAX_BLACK_REPEAT_PIXELS: usize = 4_096;

/// Immutable view of the retained integer mosaic and the calibration needed to interpret one
/// sensor sample. CFA and black-repeat coordinates are anchored at sensor `(0, 0)`; the caller is
/// responsible for mapping from active/content coordinates before invoking the sampler.
#[derive(Clone, Copy, Debug)]
pub struct SensorMosaic<'a> {
    pub samples: &'a [u16],
    pub corrections: &'a [lightwell_raw::MosaicCorrection],
    pub width: u32,
    pub height: u32,
    pub cfa_width: u8,
    pub cfa_height: u8,
    /// Red, green, and blue are encoded as 0, 1, and 2 respectively.
    pub cfa: &'a [u8],
    /// Native CFA site IDs used to select per-site black calibration. Bayer
    /// green sites remain distinct (usually IDs 1 and 3).
    pub black_cfa: &'a [u8],
    pub black_base: f32,
    pub black_channels: [f32; 4],
    pub black_repeat_width: u8,
    pub black_repeat_height: u8,
    pub black_repeat: &'a [f32],
    pub sensor_white: f32,
}

fn validation(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, message)
}

fn checked_source_len(source: &SensorMosaic<'_>) -> Result<usize, Error> {
    if source.width == 0
        || source.height == 0
        || source.width > MAX_SENSOR_SIDE
        || source.height > MAX_SENSOR_SIDE
    {
        return Err(validation(
            "neutral picker sensor dimensions are out of bounds",
        ));
    }
    let length = (source.width as usize)
        .checked_mul(source.height as usize)
        .ok_or_else(|| validation("neutral picker sensor dimensions overflow"))?;
    if length > MAX_SENSOR_PIXELS || source.samples.len() != length {
        return Err(validation("neutral picker mosaic length is invalid"));
    }
    if source.corrections.len() > 65_536
        || source
            .corrections
            .iter()
            .any(|p| p.index as usize >= length)
        || source
            .corrections
            .windows(2)
            .any(|p| p[0].index >= p[1].index)
    {
        return Err(validation("neutral picker sparse corrections are invalid"));
    }
    Ok(length)
}

fn validate_cfa(source: &SensorMosaic<'_>) -> Result<(), Error> {
    let dimensions = (source.cfa_width as usize, source.cfa_height as usize);
    if !matches!(dimensions, (2, 2) | (6, 6)) {
        return Err(validation("neutral picker supports only 2x2 or 6x6 CFA"));
    }
    let cfa_len = dimensions
        .0
        .checked_mul(dimensions.1)
        .ok_or_else(|| validation("neutral picker CFA dimensions overflow"))?;
    if source.cfa.len() != cfa_len
        || source.cfa.iter().any(|channel| *channel > 2)
        || source.black_cfa.len() != cfa_len
        || source
            .black_cfa
            .iter()
            .zip(source.cfa)
            .any(|(&site, &channel)| site > 3 || (if site == 3 { 1 } else { site }) != channel)
    {
        return Err(validation("neutral picker CFA is malformed"));
    }
    let counts =
        [0_u8, 1, 2].map(|channel| source.cfa.iter().filter(|value| **value == channel).count());
    let expected = if dimensions == (2, 2) {
        [1, 2, 1]
    } else {
        [8, 20, 8]
    };
    if counts != expected {
        return Err(validation("neutral picker CFA channel counts are invalid"));
    }
    Ok(())
}

fn validate_calibration(source: &SensorMosaic<'_>) -> Result<(), Error> {
    if !source.sensor_white.is_finite() || source.sensor_white <= 0.0 {
        return Err(validation("neutral picker sensor white is invalid"));
    }
    if !source.black_base.is_finite() || source.black_base < 0.0 {
        return Err(validation("neutral picker black base is invalid"));
    }
    if source
        .black_channels
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(validation(
            "neutral picker black channel levels are invalid",
        ));
    }
    let repeat_dimensions = (
        source.black_repeat_width as usize,
        source.black_repeat_height as usize,
    );
    if (repeat_dimensions.0 == 0) != (repeat_dimensions.1 == 0) {
        return Err(validation(
            "neutral picker black repeat dimensions are incomplete",
        ));
    }
    let repeat_len = repeat_dimensions
        .0
        .checked_mul(repeat_dimensions.1)
        .ok_or_else(|| validation("neutral picker black repeat dimensions overflow"))?;
    if repeat_len > MAX_BLACK_REPEAT_PIXELS || source.black_repeat.len() != repeat_len {
        return Err(validation("neutral picker black repeat is malformed"));
    }
    if source
        .black_repeat
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(validation(
            "neutral picker black repeat contains an invalid value",
        ));
    }
    let max_repeat = source.black_repeat.iter().copied().fold(0.0_f32, f32::max);
    if source.black_channels.iter().any(|channel| {
        let denominator = source.sensor_white - source.black_base - *channel - max_repeat;
        !denominator.is_finite() || denominator <= 0.0
    }) {
        return Err(validation(
            "neutral picker black/white denominator is invalid",
        ));
    }
    Ok(())
}

fn black_at(source: &SensorMosaic<'_>, x: u32, y: u32, black_channel: usize) -> f64 {
    let mut black = f64::from(source.black_base) + f64::from(source.black_channels[black_channel]);
    if source.black_repeat_width != 0 {
        let repeat_x = (x % u32::from(source.black_repeat_width)) as usize;
        let repeat_y = (y % u32::from(source.black_repeat_height)) as usize;
        let index = repeat_y * usize::from(source.black_repeat_width) + repeat_x;
        black += f64::from(source.black_repeat[index]);
    }
    black
}

/// Read the fixed 13x13 pre-WB patch around a mapped sensor point and resolve green-normalized
/// gains. The result is `[green/red, 1, green/blue]`, bounded to finite positive values <= 32.
/// Every site must be finite, above [`DARK_THRESHOLD`], and below [`CLIPPED_THRESHOLD`] after
/// its own black subtraction and white normalization. No intermediate frame is allocated.
pub fn sensor_neutral_gains(
    source: &SensorMosaic<'_>,
    center_x: u32,
    center_y: u32,
) -> Result<[f32; 3], Error> {
    sensor_neutral_gains_mapped(
        source,
        center_x,
        center_y,
        &|x, y, _| Ok((f64::from(x), f64::from(y))),
        &|_, _, _| Ok(1.0),
    )
}

/// Sample one 13x13 patch in corrected coordinates. Each site is mapped for its own color
/// channel, then read from the nearest sensor site with that CFA color. This avoids treating a
/// chromatic warp as a single center translation. The mapper and gain lookup each run at most
/// 169 times; no demosaic or frame allocation occurs on the catalog owner.
pub fn sensor_neutral_gains_mapped(
    source: &SensorMosaic<'_>,
    center_x: u32,
    center_y: u32,
    map_at: &dyn Fn(u32, u32, usize) -> Result<(f64, f64), Error>,
    gain_at: &dyn Fn(f64, f64, usize) -> Result<f64, Error>,
) -> Result<[f32; 3], Error> {
    checked_source_len(source)?;
    validate_cfa(source)?;
    validate_calibration(source)?;

    let start_x = center_x
        .checked_sub(PATCH_RADIUS)
        .ok_or_else(|| validation("neutral picker patch is outside the sensor"))?;
    let start_y = center_y
        .checked_sub(PATCH_RADIUS)
        .ok_or_else(|| validation("neutral picker patch is outside the sensor"))?;
    let end_x = start_x
        .checked_add(PATCH_SIDE - 1)
        .ok_or_else(|| validation("neutral picker patch bounds overflow"))?;
    let end_y = start_y
        .checked_add(PATCH_SIDE - 1)
        .ok_or_else(|| validation("neutral picker patch bounds overflow"))?;
    if end_x >= source.width || end_y >= source.height {
        return Err(validation("neutral picker patch is outside the sensor"));
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
                return Err(validation(
                    "neutral picker patch is dark, clipped or non-finite",
                ));
            }
            let gain = gain_at(x as f64, y as f64, channel)?;
            if !gain.is_finite() || gain <= 0.0 {
                return Err(validation("neutral picker site gain is invalid"));
            }
            let corrected = normalized * gain;
            if !corrected.is_finite() || corrected <= 0.0 {
                return Err(validation("neutral picker corrected sample is invalid"));
            }
            sums[channel] += corrected;
            counts[channel] += 1;
        }
    }

    let means: [f64; 3] = std::array::from_fn(|channel| sums[channel] / f64::from(counts[channel]));
    if !means.iter().all(|value| value.is_finite() && *value > 0.0) {
        return Err(validation("neutral picker patch has an unusable channel"));
    }
    let gains = [means[1] / means[0], 1.0, means[1] / means[2]];
    if !gains
        .iter()
        .all(|value| value.is_finite() && *value > 0.0 && *value <= super::MAX_RAW_GAIN)
    {
        return Err(validation(
            "neutral picker gains are outside the finite positive <=32 range",
        ));
    }
    let gains = gains.map(|value| value as f32);
    if !gains
        .iter()
        .all(|value| value.is_finite() && *value > 0.0 && f64::from(*value) <= super::MAX_RAW_GAIN)
    {
        return Err(validation(
            "neutral picker gains are not representable as f32",
        ));
    }
    Ok(gains)
}

fn nearest_site(
    source: &SensorMosaic<'_>,
    x: f64,
    y: f64,
    channel: usize,
) -> Result<(u32, u32), Error> {
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= f64::from(source.width)
        || y >= f64::from(source.height)
    {
        return Err(validation(
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
        .ok_or_else(|| validation("neutral picker mapped point has no matching CFA site"))
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
    ) -> SensorMosaic<'a> {
        SensorMosaic {
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
        let gains = sensor_neutral_gains(&view, 9, 10).unwrap();
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
        let expected = sensor_neutral_gains(
            &source(&pixels, 24, 22, 2, 2, &BAYER, &black_repeat, 65_535.0),
            9,
            10,
        )
        .unwrap();
        let index = 10 * 24 + 10;
        let repair = lightwell_raw::MosaicCorrection {
            index: index as u32,
            value: pixels[index],
        };
        pixels[index] = 0;
        let before = pixels.clone();
        let mut view = source(&pixels, 24, 22, 2, 2, &BAYER, &black_repeat, 65_535.0);
        assert!(sensor_neutral_gains(&view, 9, 10).is_err());
        view.corrections = std::slice::from_ref(&repair);
        assert_eq!(sensor_neutral_gains(&view, 9, 10).unwrap(), expected);
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
        let gains = sensor_neutral_gains_mapped(
            &view,
            9,
            10,
            &|x, y, _| Ok((f64::from(x), f64::from(y))),
            &|_, _, channel| {
                calls.set(calls.get() + 1);
                Ok([2.0, 1.0, 0.5][channel])
            },
        )
        .unwrap();
        assert_eq!(calls.get(), PATCH_SIDE * PATCH_SIDE);
        close(gains[0], 1.0);
        close(gains[1], 1.0);
        close(gains[2], 8.0);
        let highlight = sensor_neutral_gains_mapped(
            &view,
            9,
            10,
            &|x, y, _| Ok((f64::from(x), f64::from(y))),
            &|_, _, channel| Ok([8.0, 1.0, 1.0][channel]),
        )
        .unwrap();
        close(highlight[0], 0.25);
        assert!(
            sensor_neutral_gains_mapped(
                &view,
                9,
                10,
                &|x, y, _| Ok((f64::from(x), f64::from(y))),
                &|_, _, _| Ok(f64::NAN),
            )
            .is_err()
        );
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
        let view = SensorMosaic {
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
        let gains = sensor_neutral_gains_mapped(
            &view,
            32,
            24,
            &|x, y, channel| {
                calls.set(calls.get() + 1);
                Ok((f64::from(x) + [15.0, 0.0, -15.0][channel], f64::from(y)))
            },
            &|_, _, _| Ok(1.0),
        )
        .unwrap();
        assert_eq!(calls.get(), PATCH_SIDE * PATCH_SIDE);
        close(gains[0], 1.0);
        close(gains[2], 2.0);
        assert!(
            sensor_neutral_gains_mapped(
                &view,
                32,
                24,
                &|_, _, _| Ok((f64::NAN, 0.0)),
                &|_, _, _| Ok(1.0),
            )
            .is_err()
        );
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
        let gains = sensor_neutral_gains(&view, 11, 11).unwrap();
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
        assert!(sensor_neutral_gains(&view, 0, 0).is_err());
        assert!(sensor_neutral_gains(&view, u32::MAX, u32::MAX).is_err());

        for values in [[0.005, 0.5, 0.125], [0.25, 0.5, 0.999], [0.015, 0.5, 0.015]] {
            let pixels = fixture(20, 20, 2, 2, &BAYER, values, &black_repeat, 65_535.0);
            let view = source(&pixels, 20, 20, 2, 2, &BAYER, &black_repeat, 65_535.0);
            assert!(sensor_neutral_gains(&view, 9, 9).is_err());
        }

        let mut malformed = view;
        malformed.sensor_white = f32::NAN;
        assert!(sensor_neutral_gains(&malformed, 9, 9).is_err());
        malformed = view;
        malformed.black_repeat = &[f32::NAN, 2.0, 3.0, 4.0];
        assert!(sensor_neutral_gains(&malformed, 9, 9).is_err());
    }

    #[test]
    fn malformed_cfa_and_mosaic_are_rejected_without_allocation() {
        let black_repeat = [1.0, 2.0, 3.0, 4.0];
        let pixels = fixture(
            20,
            20,
            2,
            2,
            &BAYER,
            [0.25, 0.5, 0.125],
            &black_repeat,
            65_535.0,
        );
        let mut view = source(&pixels, 20, 20, 2, 2, &BAYER, &black_repeat, 65_535.0);
        view.samples = &pixels[..pixels.len() - 1];
        assert!(sensor_neutral_gains(&view, 9, 9).is_err());
        view.samples = &pixels;
        view.cfa = &[0, 0, 1, 2];
        assert!(sensor_neutral_gains(&view, 9, 9).is_err());
    }
}
