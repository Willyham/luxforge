//! A prepared original: byte-exact JPEG or an immutable RAW mosaic with one WB development.
use crate::{Error, ErrorKind, LinearImage, SourceImage};
use lightwell_raw::{RawError, RawMetadata, RawSource};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug)]
pub(crate) enum PreparedSource {
    Jpeg(SourceImage),
    Raw(RawPrepared),
}

#[derive(Clone, Debug)]
pub(crate) struct RawPrepared {
    pub(crate) sensor: Arc<RawSource>,
    pub(crate) linear: Option<LinearImage>,
    pub(crate) gains: [f32; 3],
}

/// A known recipe's source-only target, resolved without reading pixels on the catalog owner.
/// The freshly unpacked interpretation must match before its gains can develop that mosaic.
#[derive(Clone, Debug)]
pub(crate) struct RawPreparation {
    pub(crate) metadata: RawMetadata,
    pub(crate) gains: [f32; 3],
}

impl RawPreparation {
    pub(crate) fn validate(&self, metadata: &RawMetadata) -> Result<(), Error> {
        // Both sides are typed first, so catalog JSON's shortest f32 decimals compare at the
        // native precision, exactly as they do when the owner adopts the completed source.
        let value = |metadata: &RawMetadata| {
            serde_json::to_value(metadata)
                .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
        };
        if value(&self.metadata)? != value(metadata)? {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "original source interpretation changed",
            ));
        }
        Ok(())
    }
}

impl PreparedSource {
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Jpeg(image) => (image.width, image.height),
            Self::Raw(raw) => {
                let metadata = raw.sensor.metadata();
                let crop = metadata.default_crop;
                if (5..=8).contains(&metadata.exif_orientation) {
                    (crop.height, crop.width)
                } else {
                    (crop.width, crop.height)
                }
            }
        }
    }
    pub(crate) fn metadata(&self) -> Option<&RawMetadata> {
        match self {
            Self::Jpeg(_) => None,
            Self::Raw(raw) => Some(raw.sensor.metadata()),
        }
    }
}

pub(crate) fn raw_error(error: RawError) -> Error {
    let kind = match error {
        RawError::ResourceLimit(_) => ErrorKind::ResourceLimit,
        RawError::Cancelled => ErrorKind::Conflict,
        RawError::UnsupportedMode(_)
        | RawError::UnsupportedRequiredOpcodes(_)
        | RawError::UnsupportedCfa => ErrorKind::UnsupportedInput,
        RawError::MissingCalibration(_) => ErrorKind::UnsupportedColor,
        RawError::InvalidInput(_) | RawError::Native(_) => ErrorKind::Decode,
    };
    Error::new(kind, error.to_string())
}

impl RawPrepared {
    pub(crate) fn decode(
        bytes: Vec<u8>,
        fingerprint: String,
        target: Option<&RawPreparation>,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let sensor = Arc::new(RawSource::decode(Arc::from(bytes), cancel).map_err(raw_error)?);
        let gains = match target {
            Some(target) => {
                target.validate(sensor.metadata())?;
                target.gains
            }
            None => sensor.metadata().as_shot_gains,
        };
        Self::develop(sensor, fingerprint, gains, cancel)
    }

    pub(crate) fn develop(
        sensor: Arc<RawSource>,
        fingerprint: String,
        gains: [f32; 3],
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let mut rgb = sensor.develop(gains, cancel).map_err(raw_error)?;
        let matrix = &sensor.metadata().rgb_cam;
        let n = rgb.plane_len();
        for i in 0..n {
            if i & 0xffff == 0 && cancel.load(Ordering::Relaxed) {
                return Err(Error::new(ErrorKind::Conflict, "RAW development cancelled"));
            }
            let camera = [rgb.data[i], rgb.data[n + i], rgb.data[2 * n + i]];
            for (channel, row) in matrix.iter().enumerate() {
                let value = row[0] * camera[0] + row[1] * camera[1] + row[2] * camera[2];
                if !value.is_finite() {
                    return Err(Error::new(
                        ErrorKind::UnsupportedColor,
                        "RAW color conversion produced a non-finite value",
                    ));
                }
                rgb.data[channel * n + i] = value;
            }
        }
        let metadata = sensor.metadata();
        let crop = metadata.default_crop;
        let linear = LinearImage::with_fingerprint(rgb.width, rgb.height, rgb.data, fingerprint)?
            .with_view(
            [crop.x, crop.y, crop.width, crop.height],
            metadata.exif_orientation,
        )?;
        Ok(Self {
            sensor,
            linear: Some(linear),
            gains,
        })
    }
}

/// Map an upright content point through the default crop and orientation. Corrected DNGs then
/// map each CFA site through the channel's optical warp before reading one fixed pre-WB patch.
/// Neither float development nor full-frame work runs on the owner.
pub(crate) fn neutral_at(raw: &RawPrepared, x: u32, y: u32) -> Result<[f32; 3], Error> {
    let metadata = raw.sensor.metadata();
    let crop = metadata.default_crop;
    let (out_w, out_h) = if (5..=8).contains(&metadata.exif_orientation) {
        (crop.height, crop.width)
    } else {
        (crop.width, crop.height)
    };
    if x >= out_w || y >= out_h {
        return Err(Error::new(
            ErrorKind::Validation,
            "neutral picker point outside upright RAW image",
        ));
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
        _ => {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "invalid persisted RAW orientation",
            ));
        }
    };
    let source = crate::SensorMosaic {
        samples: raw.sensor.mosaic(),
        corrections: raw.sensor.mosaic_corrections(),
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
    let corrected_x = crop.x + sx;
    let corrected_y = crop.y + sy;
    if metadata.dng_corrections.is_some() {
        crate::modules::sensor_neutral_gains_mapped(
            &source,
            corrected_x,
            corrected_y,
            &|x, y, channel| {
                raw.sensor
                    .corrected_sensor_sample_location(x, y, channel)
                    .map_err(|error| {
                        Error::new(
                            ErrorKind::Validation,
                            format!("neutral picker warp point: {error}"),
                        )
                    })
            },
            &|x, y, channel| {
                raw.sensor
                    .gain_at_corrected_sensor(x, y, channel)
                    .map_err(|error| {
                        Error::new(
                            ErrorKind::Validation,
                            format!("neutral picker gain point: {error}"),
                        )
                    })
            },
        )
    } else {
        crate::sensor_neutral_gains(&source, corrected_x, corrected_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LinearSettings, ModuleRegistry, PreviewSource, ProxyBounds, Recipe, SnapshotId,
        WhiteBalanceApproximation, gains_from_temperature_tint, render_linear,
    };
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    /// How far one 8-bit rendition is from another, and where the differences sit.
    fn compare(approximate: &crate::Raster, exact: &crate::Raster) -> (Value, Vec<u8>) {
        assert_eq!(
            (approximate.width, approximate.height),
            (exact.width, exact.height)
        );
        let (width, height) = (exact.width as usize, exact.height as usize);
        let pixels = width * height;
        let luma = |rgba: &[u8], index: usize| -> f64 {
            let pixel = &rgba[index * 4..index * 4 + 3];
            0.2126 * f64::from(pixel[0])
                + 0.7152 * f64::from(pixel[1])
                + 0.0722 * f64::from(pixel[2])
        };
        let mut sums = [0.0_f64; 3];
        let mut worst = vec![0_u8; pixels];
        let (mut over, mut edge_over, mut edges) = (0_usize, 0_usize, 0_usize);
        let (mut highlight_over, mut highlights) = (0_usize, 0_usize);
        let (mut saturated_over, mut saturated) = (0_usize, 0_usize);
        let (mut dark_over, mut dark) = (0_usize, 0_usize);
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                let a = &approximate.rgba[index * 4..index * 4 + 3];
                let e = &exact.rgba[index * 4..index * 4 + 3];
                let mut largest = 0_u8;
                for channel in 0..3 {
                    let difference = a[channel].abs_diff(e[channel]);
                    sums[channel] += f64::from(difference);
                    largest = largest.max(difference);
                }
                worst[index] = largest;
                // A local luma gradient of the exact frame over 24 codes is an edge; a channel at
                // 250 or above in either frame is a highlight; a channel spread of 128 or more is a
                // saturated colour; a luma under 16 is a shadow.
                let gradient = if x > 0 && y > 0 && x + 1 < width && y + 1 < height {
                    (luma(&exact.rgba, index + 1) - luma(&exact.rgba, index - 1)).abs()
                        + (luma(&exact.rgba, index + width) - luma(&exact.rgba, index - width))
                            .abs()
                } else {
                    0.0
                };
                let is_edge = gradient > 24.0;
                let is_highlight = e.iter().chain(a.iter()).any(|value| *value >= 250);
                let spread = e.iter().max().unwrap() - e.iter().min().unwrap();
                let is_saturated = spread >= 128;
                let is_dark = luma(&exact.rgba, index) < 16.0;
                edges += usize::from(is_edge);
                highlights += usize::from(is_highlight);
                saturated += usize::from(is_saturated);
                dark += usize::from(is_dark);
                if largest > 2 {
                    over += 1;
                    edge_over += usize::from(is_edge);
                    highlight_over += usize::from(is_highlight);
                    saturated_over += usize::from(is_saturated);
                    dark_over += usize::from(is_dark);
                }
            }
        }
        let mut ranked = worst.clone();
        ranked.sort_unstable();
        let share = |count: usize, of: usize| {
            if of == 0 {
                0.0
            } else {
                count as f64 / of as f64
            }
        };
        let summary = json!({
            "dimensions": [width, height],
            "mean_abs_codes": sums.map(|sum| sum / pixels as f64),
            "p99_max_channel_codes": ranked[(pixels * 99 / 100).min(pixels - 1)],
            "p999_max_channel_codes": ranked[(pixels * 999 / 1000).min(pixels - 1)],
            "max_codes": ranked[pixels - 1],
            "share_over_2_codes": share(over, pixels),
            "over_2_by_region": {
                "edges": {"share_of_over": share(edge_over, over), "share_of_frame": share(edges, pixels)},
                "highlights": {"share_of_over": share(highlight_over, over), "share_of_frame": share(highlights, pixels)},
                "saturated": {"share_of_over": share(saturated_over, over), "share_of_frame": share(saturated, pixels)},
                "shadows": {"share_of_over": share(dark_over, over), "share_of_frame": share(dark, pixels)},
            },
        });
        (summary, worst)
    }

    fn save_gray(path: &std::path::Path, width: u32, height: u32, worst: &[u8]) {
        // Eight times the difference, so a 2-code difference is a visible 16 and 32 codes is white.
        let pixels: Vec<u8> = worst.iter().map(|value| value.saturating_mul(8)).collect();
        image::save_buffer(path, &pixels, width, height, image::ColorType::L8).unwrap();
    }

    fn save_rgba(path: &std::path::Path, raster: &crate::Raster) {
        image::save_buffer(
            path,
            &raster.rgba,
            raster.width,
            raster.height,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    /// The accuracy of the drafted white-balance approximation on a real RAW file: the planes
    /// developed at the camera's as-shot gains, rendered through `W` for a custom temperature and
    /// tint, against an exact redevelopment of the mosaic at the same gains, both rendered to 8-bit
    /// sRGB at a display proxy size and at full size. It prints one JSON document and, with
    /// `LIGHTWELL_WB_OUT` set to a directory, writes each difference image (eight times the largest
    /// channel difference) and both proxy renditions there. It is a measurement, not a gate:
    ///
    /// ```text
    /// LIGHTWELL_RAW_FIXTURE=/path/to/file.NEF LIGHTWELL_WB_OUT=/tmp/wb \
    ///   cargo test --release -p lightwell-core --lib source::tests -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires a photo-sized RAW fixture and is a measurement, not a gate"]
    fn measure_the_white_balance_approximation_against_redevelopment() {
        let path =
            std::path::PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
        let out = std::env::var("LIGHTWELL_WB_OUT")
            .ok()
            .map(std::path::PathBuf::from);
        if let Some(out) = &out {
            std::fs::create_dir_all(out).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(&bytes));
        let cancel = AtomicBool::new(false);
        let as_shot = RawPrepared::decode(bytes, fingerprint.clone(), None, &cancel).unwrap();
        let metadata = as_shot.sensor.metadata().clone();
        let camera = metadata
            .rgb_cam
            .map(|row| [row[0], row[1], row[2]].map(f64::from));
        let developed = as_shot.linear.clone().unwrap();
        let registry = ModuleRegistry::builtin();
        let recipe = Recipe::default();
        let bounds = ProxyBounds {
            width: 2400,
            height: 1600,
        };
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let mut cases = Vec::new();
        for (kelvin, tint) in [
            (3200.0, 0.0),
            (5000.0, 20.0),
            (8000.0, -20.0),
            (6504.0, 0.0),
        ] {
            let target = gains_from_temperature_tint(kelvin, tint, metadata.cam_xyz).unwrap();
            let balance =
                WhiteBalanceApproximation::between(camera, as_shot.gains, target).unwrap();
            let started = std::time::Instant::now();
            let exact =
                RawPrepared::develop(as_shot.sensor.clone(), fingerprint.clone(), target, &cancel)
                    .unwrap();
            let redevelop_ms = started.elapsed().as_secs_f64() * 1000.0;
            let approximate = PreviewSource::Raw {
                image: developed.clone(),
                settings: LinearSettings {
                    exposure_ev: 0.0,
                    white_balance: Some(balance),
                },
            };
            let reference = PreviewSource::Raw {
                image: exact.linear.clone().unwrap(),
                settings: LinearSettings::default(),
            };
            let snapshot = SnapshotId::new();
            let full_approximate = approximate
                .render(&registry, snapshot.clone(), &recipe)
                .unwrap();
            let full_exact = reference
                .render(&registry, snapshot.clone(), &recipe)
                .unwrap();
            let (full, full_worst) = compare(&full_approximate, &full_exact);
            let plan = approximate
                .proxy_plan(&registry, &recipe, bounds)
                .unwrap()
                .expect("a photo is larger than the display");
            let proxy_approximate = approximate
                .proxy(plan)
                .unwrap()
                .render(&registry, snapshot.clone(), &recipe)
                .unwrap();
            let proxy_exact = reference
                .proxy(plan)
                .unwrap()
                .render(&registry, snapshot, &recipe)
                .unwrap();
            let (proxy, proxy_worst) = compare(&proxy_approximate, &proxy_exact);
            // How far the as-shot frame itself is from the target, for scale: what a drag shows
            // when nothing is previewed at all.
            let unchanged = render_linear(
                &registry,
                &developed,
                SnapshotId::new(),
                &recipe,
                LinearSettings::default(),
            )
            .unwrap();
            let (as_shot_against_target, _) = compare(&unchanged, &full_exact);
            if let Some(out) = &out {
                let name = format!("{stem}-{kelvin:.0}K-{tint:+.0}");
                save_gray(
                    &out.join(format!("{name}-proxy-diff.png")),
                    proxy_approximate.width,
                    proxy_approximate.height,
                    &proxy_worst,
                );
                save_gray(
                    &out.join(format!("{name}-full-diff.png")),
                    full_approximate.width,
                    full_approximate.height,
                    &full_worst,
                );
                save_rgba(
                    &out.join(format!("{name}-proxy-approximate.png")),
                    &proxy_approximate,
                );
                save_rgba(&out.join(format!("{name}-proxy-exact.png")), &proxy_exact);
            }
            cases.push(json!({
                "kelvin": kelvin,
                "tint": tint,
                "as_shot_gains": as_shot.gains,
                "target_gains": target,
                "matrix": balance.matrix(),
                "redevelop_ms": redevelop_ms,
                "proxy": proxy,
                "full": full,
                "as_shot_frame_against_target_full": {
                    "mean_abs_codes": as_shot_against_target["mean_abs_codes"],
                    "share_over_2_codes": as_shot_against_target["share_over_2_codes"],
                },
            }));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source": path,
                "model": metadata.model,
                "mode": metadata.mode,
                "cases": cases,
            }))
            .unwrap()
        );
    }
}
