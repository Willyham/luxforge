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
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let sensor = Arc::new(RawSource::decode(Arc::from(bytes), cancel).map_err(raw_error)?);
        let gains = sensor.metadata().as_shot_gains;
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
        width: metadata.sensor_width,
        height: metadata.sensor_height,
        cfa_width: metadata.cfa_width,
        cfa_height: metadata.cfa_height,
        cfa: &metadata.cfa,
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
                raw.sensor.gain_at_sensor(x, y, channel).map_err(|error| {
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
