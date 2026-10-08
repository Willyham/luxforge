//! Whether the sensor was saturated under one developed pixel, read from the retained mosaic.
//!
//! The developed planes cannot answer this: a Bayer demosaic limits each site after its white
//! balance gain, X-Trans and the direct layouts do not, and the camera matrix mixes the channels
//! before the planes are stored. So the question goes back to the sensor, as the neutral picker's
//! does, through the same crop, orientation, optical warp, repairs and per-site black model.

use crate::{
    RawError, RawLayout, RawSource,
    neutral::{CLIPPED_THRESHOLD, Mosaic},
    normalize::BlackLevels,
};

impl RawSource {
    /// Whether a sensor sample the upright default-crop pixel `(x, y)` develops from is at or
    /// above [`CLIPPED_THRESHOLD`] of the sensor's white after its own black level: before white
    /// balance and before any optical gain, so neither pushes a sample over. On a CFA sensor the
    /// samples are every site within one site of where the pixel is read, which holds the nearest
    /// site of each Bayer colour, each read with its repair; a corrected DNG reads each channel
    /// where its warp maps the pixel. On a linear or monochrome layout they are the pixel's own
    /// samples. `false` outside the upright image. Bounded point work: at most 27 sites.
    pub fn sensor_clipped_at(&self, x: u32, y: u32) -> Result<bool, RawError> {
        let Some((cx, cy)) = self.upright_to_corrected(x, y)? else {
            return Ok(false);
        };
        let metadata = &self.metadata;
        let (width, height) = (metadata.sensor_width, metadata.sensor_height);
        let white = f64::from(metadata.sensor_white);
        let saturated = |sample: u16, black: f32| {
            let black = f64::from(black);
            (f64::from(sample) - black) / (white - black) >= CLIPPED_THRESHOLD
        };
        // Where `channel` of the pixel is read: the pixel itself, or its warped location.
        let read_at = |channel: usize| -> Result<Option<(u32, u32)>, RawError> {
            let (mx, my) = self.corrected_sensor_sample_location(cx, cy, channel)?;
            let (sx, sy) = (mx.round(), my.round());
            Ok(
                (sx >= 0.0 && sy >= 0.0 && sx < f64::from(width) && sy < f64::from(height))
                    .then_some((sx as u32, sy as u32)),
            )
        };
        if metadata.layout == RawLayout::Mosaic {
            let mosaic = Mosaic::of(self);
            let mut checked = None;
            for channel in 0..3 {
                let Some((sx, sy)) = read_at(channel)? else {
                    continue;
                };
                if checked.replace((sx, sy)) == Some((sx, sy)) {
                    continue;
                }
                for y in sy.saturating_sub(1)..=(sy + 1).min(height - 1) {
                    for x in sx.saturating_sub(1)..=(sx + 1).min(width - 1) {
                        if saturated(mosaic.sample(x, y), mosaic.black.at(x as usize, y as usize)) {
                            return Ok(true);
                        }
                    }
                }
            }
            return Ok(false);
        }
        let channels = metadata.layout.channels();
        let black = BlackLevels::of(metadata);
        for channel in 0..channels {
            let Some((sx, sy)) = read_at(channel)? else {
                continue;
            };
            let (sx, sy) = (sx as usize, sy as usize);
            let sample = self.mosaic[(sy * width as usize + sx) * channels + channel];
            if saturated(sample, black.at_channel(sx, sy, channel)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use crate::{RawLayout, RawRect, RawSource, layout_tests::source};
    use std::sync::Arc;

    /// An 8 × 6 RGGB sensor, black 128 and white 1024, its sites `samples`, with a default crop of
    /// 6 × 4 at (1, 1) seen through `orientation`.
    fn bayer(orientation: u8, samples: Vec<u16>) -> RawSource {
        let mut raw = source(RawLayout::Mosaic, samples, 8, 6);
        let m = &mut raw.metadata;
        m.cfa_width = 2;
        m.cfa_height = 2;
        m.cfa = vec![0, 1, 1, 2];
        m.black_cfa = vec![0, 1, 3, 2];
        m.default_crop = RawRect {
            x: 1,
            y: 1,
            width: 6,
            height: 4,
        };
        m.exif_orientation = orientation;
        raw
    }

    fn flagged(raw: &RawSource) -> Vec<(u32, u32)> {
        let crop = raw.metadata.default_crop;
        let (w, h) = if raw.metadata.exif_orientation >= 5 {
            (crop.height, crop.width)
        } else {
            (crop.width, crop.height)
        };
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| raw.sensor_clipped_at(x, y).unwrap())
            .collect()
    }

    /// A red site at sensor white, at (4, 2), is flagged under exactly the pixels within one site
    /// of it, corrected x 3..=5 and y 1..=3, which the crop at (1, 1) and each orientation place in
    /// the upright image by hand.
    #[test]
    fn a_saturated_site_flags_the_pixels_around_it_through_crop_and_orientation() {
        let mut samples = vec![576; 48];
        samples[2 * 8 + 4] = 1024;
        let upright = bayer(1, samples.clone());
        assert_eq!(
            flagged(&upright),
            vec![
                (2, 0),
                (3, 0),
                (4, 0),
                (2, 1),
                (3, 1),
                (4, 1),
                (2, 2),
                (3, 2),
                (4, 2)
            ]
        );
        // Orientation 6: upright (x, y) is corrected (1 + y, 1 + 3 - x).
        let turned = bayer(6, samples);
        assert_eq!(
            flagged(&turned),
            vec![
                (1, 2),
                (2, 2),
                (3, 2),
                (1, 3),
                (2, 3),
                (3, 3),
                (1, 4),
                (2, 4),
                (3, 4)
            ]
        );
        assert!(
            !turned.sensor_clipped_at(4, 0).unwrap(),
            "outside the image"
        );
        assert!(
            !turned.sensor_clipped_at(0, 6).unwrap(),
            "outside the image"
        );
    }

    /// The threshold is the neutral picker's 0.995 of the range above black, before any gain:
    /// 892 of 896 codes is clipped, 891 is not. A repair replaces the retained sample.
    #[test]
    fn sites_flag_at_the_neutral_pickers_clip_threshold_and_read_their_repairs() {
        let green = |value: u16| {
            let mut samples = vec![576; 48];
            for y in 0..6 {
                for x in 0..8 {
                    if (x + y) % 2 == 1 {
                        samples[y * 8 + x] = value;
                    }
                }
            }
            bayer(1, samples)
        };
        assert_eq!(flagged(&green(128 + 892)).len(), 24);
        assert!(flagged(&green(128 + 891)).is_empty());
        let mut repaired = bayer(1, {
            let mut samples = vec![576; 48];
            samples[2 * 8 + 4] = 1024;
            samples
        });
        repaired.mosaic_corrections = Arc::new(vec![crate::MosaicCorrection {
            index: 2 * 8 + 4,
            value: 576,
        }]);
        assert!(flagged(&repaired).is_empty());
    }

    #[test]
    fn a_linear_layout_flags_a_pixel_with_any_channel_at_white() {
        // Two pixels: the first's blue at white, the second just below in every channel.
        let raw = source(
            RawLayout::LinearRgb,
            vec![576, 576, 1024, 1019, 1019, 1019],
            2,
            1,
        );
        assert!(raw.sensor_clipped_at(0, 0).unwrap());
        assert!(!raw.sensor_clipped_at(1, 0).unwrap());
    }
}
