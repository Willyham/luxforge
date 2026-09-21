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
const MAX_SENSOR_PIXELS: usize = 64_000_000;
const MAX_BLACK_REPEAT_PIXELS: usize = 4_096;

/// Immutable view of the retained integer mosaic and the calibration needed to interpret one
/// sensor sample. CFA and black-repeat coordinates are anchored at sensor `(0, 0)`; the caller is
/// responsible for mapping from active/content coordinates before invoking the sampler.
#[derive(Clone, Copy, Debug)]
pub struct SensorMosaic<'a> {
    pub samples: &'a [u16],
    pub width: u32,
    pub height: u32,
    pub cfa_width: u8,
    pub cfa_height: u8,
    /// Red, green, and blue are encoded as 0, 1, and 2 respectively.
    pub cfa: &'a [u8],
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
    if source.cfa.len() != cfa_len || source.cfa.iter().any(|channel| *channel > 2) {
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
    Ok(())
}

fn black_at(source: &SensorMosaic<'_>, x: u32, y: u32, channel: usize) -> f64 {
    let mut black = f64::from(source.black_base) + f64::from(source.black_channels[channel]);
    if source.black_repeat_width != 0 {
        let repeat_x = (x % u32::from(source.black_repeat_width)) as usize;
        let repeat_y = (y % u32::from(source.black_repeat_height)) as usize;
        let index = repeat_y * usize::from(source.black_repeat_width) + repeat_x;
        black += f64::from(source.black_repeat[index]);
    }
    black
}

/// Read the fixed 13x13 pre-WB patch around a mapped sensor point and resolve green-normalized
/// gains. The result is `[green/red, 1, green/blue]`, bounded to finite positive values <= 16.
/// Every site must be finite, above [`DARK_THRESHOLD`], and below [`CLIPPED_THRESHOLD`] after
/// its own black subtraction and white normalization. No intermediate frame is allocated.
pub fn sensor_neutral_gains(
    source: &SensorMosaic<'_>,
    center_x: u32,
    center_y: u32,
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
            let black = black_at(source, x, y, channel);
            let sample =
                f64::from(source.samples[(y as usize) * (source.width as usize) + x as usize]);
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
            sums[channel] += normalized;
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
        .all(|value| value.is_finite() && *value > 0.0 && *value <= 16.0)
    {
        return Err(validation(
            "neutral picker gains are outside the finite positive <=16 range",
        ));
    }
    let gains = gains.map(|value| value as f32);
    if !gains
        .iter()
        .all(|value| value.is_finite() && *value > 0.0 && *value <= 16.0)
    {
        return Err(validation(
            "neutral picker gains are not representable as f32",
        ));
    }
    Ok(gains)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAYER: [u8; 4] = [0, 1, 1, 2];
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
            samples,
            width,
            height,
            cfa_width,
            cfa_height,
            cfa,
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
                let black = 10.0
                    + [2.0, 3.0, 4.0, 5.0][channel]
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

        for values in [[0.005, 0.5, 0.125], [0.25, 0.5, 0.999], [0.02, 0.5, 0.02]] {
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
