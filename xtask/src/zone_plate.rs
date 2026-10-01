//! The zone-plate judgement of a minified photograph: does the photograph drawn smaller than its
//! source show full-contrast replica rings, or only what a box filter leaves?
//!
//! A zone plate ([`fixtures::ZONE_PLATE`]) is a radial chirp whose frequency grows with the distance
//! from its centre. Drawn smaller than its source, the part of it whose frequency lies beyond the
//! display's Nyquist limit holds nothing a correct reduction can show: a linear-light box downscale
//! averages it to a flat grey (the rings a box filter's side lobes leave are faint), while a sampler
//! that skips source texels folds it into replicas of the centre's rings at full contrast, and
//! averages the encoded values, not the light, so the replicas also read darker than the true mean.
//!
//! The judgement is over exactly that part, the pixels of the drawn photograph whose source
//! frequency lies beyond the display's Nyquist limit: the mean and standard deviation of the
//! capture's luma there, beside those of an independently computed reference, the source decoded and
//! reduced to the capture's size by an area-weighted box average of its linear light, re-encoded.
//! That is the measurement the crop draft's input stage was settled with ([performance]), whose
//! recorded figures are a standard deviation of 63.6 codes and a mean of 145.1 for the exact stage
//! drawn without mips against 15.8 and 160.3 for the display proxy, the reference's own being 10.9
//! and 160.8.
//!
//! [performance]: ../../docs/specs/performance.md
use crate::scenario::pixels::luminance;
use image::RgbImage;
use luxforge_reference::srgb;

/// Linear light's sRGB code, unrounded, in `0.0..=255.0`, by the one shared test reference.
fn encode(linear: f64) -> f64 {
    srgb::encode_clamped(linear) * 255.0
}

/// The Rec. 709 luminance of `source` in linear light, row by row.
fn linear_luminance(source: &RgbImage) -> Vec<f64> {
    let table: Vec<f64> = (0..=255u8).map(srgb::decode).collect();
    source
        .pixels()
        .map(|pixel| {
            let [r, g, b] = pixel.0;
            0.2126 * table[usize::from(r)]
                + 0.7152 * table[usize::from(g)]
                + 0.0722 * table[usize::from(b)]
        })
        .collect()
}

/// `source` reduced to `width` by `height` by an area-weighted box average of its linear light,
/// as sRGB codes (unrounded) in row-major order. Each output pixel averages exactly the source
/// area it covers, fractional edge texels weighted by the part they overlap.
pub fn reference(source: &RgbImage, (width, height): (u32, u32)) -> Vec<f64> {
    let (sw, sh) = (source.width() as usize, source.height() as usize);
    let (w, h) = (width as usize, height as usize);
    let light = linear_luminance(source);
    let mut sums = vec![0.0f64; w * h];
    let mut prefix = vec![0.0f64; sw + 1];
    // The source position a boundary between output pixels falls on, and the sum of a row's
    // texels up to a fractional position.
    let column_edge = |x: usize| x as f64 * sw as f64 / w as f64;
    let row_edge = |y: usize| y as f64 * sh as f64 / h as f64;
    let up_to = |prefix: &[f64], row: &[f64], at: f64| -> f64 {
        let whole = (at.floor() as usize).min(sw);
        let fraction = at - whole as f64;
        prefix[whole]
            + if whole < sw {
                fraction * row[whole]
            } else {
                0.0
            }
    };
    for sy in 0..sh {
        let row = &light[sy * sw..(sy + 1) * sw];
        for x in 0..sw {
            prefix[x + 1] = prefix[x] + row[x];
        }
        let (top, bottom) = (sy as f64, (sy + 1) as f64);
        let first = ((top * h as f64 / sh as f64).floor() as usize).min(h - 1);
        let last = (((bottom * h as f64 / sh as f64).ceil() as usize).max(1) - 1).min(h - 1);
        for oy in first..=last {
            let overlap = bottom.min(row_edge(oy + 1)) - top.max(row_edge(oy));
            if overlap <= 0.0 {
                continue;
            }
            let output = &mut sums[oy * w..(oy + 1) * w];
            for (ox, sum) in output.iter_mut().enumerate() {
                let (x0, x1) = (column_edge(ox), column_edge(ox + 1));
                *sum += overlap * (up_to(&prefix, row, x1) - up_to(&prefix, row, x0));
            }
        }
    }
    let area = (sw as f64 / w as f64) * (sh as f64 / h as f64);
    sums.into_iter().map(|sum| encode(sum / area)).collect()
}

/// What the zone plate says of one capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Judgement {
    /// The pixels of the drawn photograph whose source frequency lies beyond the display's Nyquist
    /// limit, and their share of the photograph.
    pub pixels: u64,
    pub fraction: f64,
    /// The mean and standard deviation of the capture's luma codes over those pixels.
    pub captured_mean: f64,
    pub captured_std: f64,
    /// The same over the reference's codes.
    pub reference_mean: f64,
    pub reference_std: f64,
}

/// Judge the photograph drawn in `rect` (`[left, top, right, bottom]`, right and bottom exclusive)
/// of `capture`, a zone plate of chirp `rate` ([`crate::fixtures::zone_plate_rate`]) whose decoded
/// pixels are `source`.
pub fn judge(source: &RgbImage, capture: &RgbImage, rect: [u32; 4], rate: f64) -> Judgement {
    let [left, top, right, bottom] = rect;
    let (width, height) = (right - left, bottom - top);
    let reduced = reference(source, (width, height));
    let (sw, sh) = (f64::from(source.width()), f64::from(source.height()));
    // The display's Nyquist limit in the source's own units, cycles per source pixel: half a cycle
    // per displayed pixel, and a displayed pixel spans `sw / width` source pixels.
    let nyquist = 0.5 * f64::from(width) / sw;
    let (mut count, mut captured, mut expected) = (0u64, Vec::new(), Vec::new());
    for y in 0..height {
        for x in 0..width {
            let dx = (f64::from(x) + 0.5) * sw / f64::from(width) - sw / 2.0;
            let dy = (f64::from(y) + 0.5) * sh / f64::from(height) - sh / 2.0;
            if rate * dx.hypot(dy) <= nyquist {
                continue;
            }
            count += 1;
            captured.push(luminance(capture.get_pixel(left + x, top + y).0));
            expected.push(reduced[(y * width + x) as usize]);
        }
    }
    let moments = |values: &[f64]| -> (f64, f64) {
        if values.is_empty() {
            return (0.0, 0.0);
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
        (mean, variance.sqrt())
    };
    let ((captured_mean, captured_std), (reference_mean, reference_std)) =
        (moments(&captured), moments(&expected));
    Judgement {
        pixels: count,
        fraction: count as f64 / (f64::from(width) * f64::from(height)),
        captured_mean,
        captured_std,
        reference_mean,
        reference_std,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn plate(width: u32, height: u32) -> RgbImage {
        let rate = crate::fixtures::zone_plate_rate((width, height));
        let (cx, cy) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
        RgbImage::from_fn(width, height, |x, y| {
            let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
            let phase = std::f64::consts::PI * rate * (dx * dx + dy * dy);
            Rgb([(127.5 + 127.5 * phase.cos()).round() as u8; 3])
        })
    }

    #[test]
    fn the_reference_of_a_flat_image_is_that_image_at_any_size() {
        let flat = RgbImage::from_pixel(37, 23, Rgb([90, 90, 90]));
        for size in [(37, 23), (10, 6), (7, 5), (3, 2)] {
            let reduced = reference(&flat, size);
            assert_eq!(reduced.len(), (size.0 * size.1) as usize);
            assert!(reduced.iter().all(|code| (code - 90.0).abs() < 1e-9));
        }
    }

    #[test]
    fn the_reference_averages_light_rather_than_codes() {
        // A black and a white texel average to half the light, sRGB code 188, not to code 127.5.
        let pair = RgbImage::from_fn(2, 1, |x, _| Rgb([[0, 0, 0], [255, 255, 255]][x as usize]));
        let reduced = reference(&pair, (1, 1));
        assert!((reduced[0] - 188.0).abs() < 0.6, "{}", reduced[0]);
    }

    #[test]
    fn the_reference_weights_fractional_texels_by_overlap() {
        // Three texels reduced to two: each output covers 1.5 texels, so the middle one is shared
        // equally and the outputs are the light means (a + b/2)/1.5 and (b/2 + c)/1.5.
        let row = RgbImage::from_fn(3, 1, |x, _| {
            Rgb([[255, 255, 255], [0, 0, 0], [255, 255, 255]][x as usize])
        });
        let reduced = reference(&row, (2, 1));
        let expected = encode(1.0 / 1.5);
        assert!((reduced[0] - expected).abs() < 1e-9 && (reduced[1] - expected).abs() < 1e-9);
    }

    #[test]
    fn a_box_reduction_passes_and_a_texel_skipping_one_is_judged_aliased() {
        let source = plate(600, 400);
        let (width, height) = (150u32, 100u32);
        let mut box_filtered = RgbImage::new(width, height);
        for (index, code) in reference(&source, (width, height)).into_iter().enumerate() {
            let code = code.round() as u8;
            box_filtered.put_pixel(index as u32 % width, index as u32 / width, Rgb([code; 3]));
        }
        // Nearest-texel sampling at the same size: every fourth texel, which folds the chirp.
        let skipped = RgbImage::from_fn(width, height, |x, y| {
            *source.get_pixel(x * 4 + 2, y * 4 + 2)
        });
        let rate = crate::fixtures::zone_plate_rate((600, 400));
        let rect = [0, 0, width, height];
        let kept = judge(&source, &box_filtered, rect, rate);
        let folded = judge(&source, &skipped, rect, rate);
        assert!(kept.fraction > 0.5 && (kept.fraction - folded.fraction).abs() < 1e-12);
        assert!((kept.captured_mean - kept.reference_mean).abs() < 0.5);
        assert!(
            kept.captured_std < 2.0 * kept.reference_std + 1.0,
            "{kept:?}"
        );
        assert!(folded.captured_std > 40.0, "{folded:?}");
        assert!(
            folded.captured_mean < folded.reference_mean - 8.0,
            "{folded:?}"
        );
    }
}
