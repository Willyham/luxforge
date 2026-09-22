//! Build a bounded editor manifest from the downloaded modern-camera corpus.
//!
//! This scans only 13x13 neutral-picker patches. It deliberately does not
//! develop a full frame.

use lightwell_core::{SensorMosaic, sensor_neutral_gains_mapped};
use lightwell_raw::RawSource;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{File, OpenOptions},
    io::Read,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

const IDS: [&str; 10] = [
    "2418", "980", "1625", "6585", "6122", "7790", "7796", "1141", "3115", "1052",
];
const MAX_BYTES: u64 = lightwell_raw::MAX_SOURCE_BYTES as u64;
const MAX_CORPUS_BYTES: u64 = 1 << 20;
const MAX_ROWS: usize = 128;

#[derive(Deserialize)]
struct CorpusRow {
    id: String,
    path: PathBuf,
    sha256: String,
}

#[derive(Serialize)]
struct Manifest {
    format: u32,
    sources: Vec<Source>,
}

#[derive(Serialize)]
struct Source {
    id: String,
    path: PathBuf,
    sha256: String,
    mode: String,
    make: String,
    model: String,
    sensor_dimensions: Option<[u32; 2]>,
    active_area: Option<[u32; 4]>,
    default_crop: Option<[u32; 4]>,
    source_dimensions: [u32; 2],
    orientation: u8,
    neutral_point: [u32; 2],
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let corpus = PathBuf::from(
        args.next()
            .ok_or("usage: modern_raw_manifest CORPUS_RESULTS OUTPUT [SAMPLE_ID ...]")?,
    );
    let output = PathBuf::from(
        args.next()
            .ok_or("usage: modern_raw_manifest CORPUS_RESULTS OUTPUT [SAMPLE_ID ...]")?,
    );
    let supplied_ids: Vec<String> = args.map(|id| id.to_string_lossy().into_owned()).collect();
    let ids: Vec<&str> = if supplied_ids.is_empty() {
        IDS.to_vec()
    } else {
        supplied_ids.iter().map(String::as_str).collect()
    };
    if ids.len() > MAX_ROWS
        || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
    {
        return Err("select at most 128 unique sample IDs".into());
    }
    let corpus_file = File::open(corpus)?;
    if corpus_file.metadata()?.len() > MAX_CORPUS_BYTES {
        return Err("corpus results exceed 1 MiB".into());
    }
    let rows: Vec<CorpusRow> = serde_json::from_reader(corpus_file)?;
    if rows.is_empty() || rows.len() > MAX_ROWS {
        return Err("corpus row count is out of bounds".into());
    }
    let mut output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    let mut sources = Vec::new();
    for id in ids {
        let row = rows
            .iter()
            .find(|row| row.id == id)
            .ok_or_else(|| format!("missing corpus id {id}"))?;
        let file = File::open(&row.path)?;
        if !file.metadata()?.file_type().is_file() {
            return Err(format!("{id}: source is not a regular file").into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(format!("{id}: source exceeds 512 MiB").into());
        }
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        if !sha256.eq_ignore_ascii_case(&row.sha256) {
            return Err(format!("{id}: source SHA-256 mismatch").into());
        }
        let cancel = AtomicBool::new(false);
        let raw = RawSource::decode(Arc::from(bytes), &cancel)?;
        let metadata = raw.metadata();
        let crop = metadata.default_crop;
        let swap = (5..=8).contains(&metadata.exif_orientation);
        let source_dimensions = if swap {
            [crop.height, crop.width]
        } else {
            [crop.width, crop.height]
        };
        let neutral_point = find_neutral(&raw, crop, source_dimensions, metadata.exif_orientation)?;
        sources.push(Source {
            id: format!("modern-{}", id),
            path: row.path.clone(),
            sha256,
            mode: serde_json::to_value(metadata.mode)?
                .as_str()
                .unwrap()
                .to_owned(),
            make: metadata.make.clone(),
            model: metadata.model.clone(),
            sensor_dimensions: Some([metadata.sensor_width, metadata.sensor_height]),
            active_area: Some([
                metadata.active_area.x,
                metadata.active_area.y,
                metadata.active_area.width,
                metadata.active_area.height,
            ]),
            default_crop: Some([crop.x, crop.y, crop.width, crop.height]),
            source_dimensions,
            orientation: metadata.exif_orientation,
            neutral_point,
        });
    }
    serde_json::to_writer_pretty(&mut output_file, &Manifest { format: 1, sources })?;
    Ok(())
}

fn find_neutral(
    raw: &RawSource,
    crop: lightwell_raw::RawRect,
    dims: [u32; 2],
    orientation: u8,
) -> Result<[u32; 2], Box<dyn std::error::Error>> {
    let metadata = raw.metadata();
    let source = SensorMosaic {
        samples: raw.mosaic(),
        corrections: raw.mosaic_corrections(),
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
    let step = (dims[0].max(dims[1]) / 64).max(32);
    let mut points = Vec::new();
    let mut y = 6;
    while y + 6 < dims[1] {
        let mut x = 6;
        while x + 6 < dims[0] {
            let dx = i64::from(x) - i64::from(dims[0] / 2);
            let dy = i64::from(y) - i64::from(dims[1] / 2);
            points.push((dx * dx + dy * dy, x, y));
            x = x.saturating_add(step);
        }
        y = y.saturating_add(step);
    }
    points.sort_unstable_by_key(|point| point.0);
    for (_, x, y) in points {
        let (sx, sy) = match orientation {
            1 => (x, y),
            2 => (crop.width - 1 - x, y),
            3 => (crop.width - 1 - x, crop.height - 1 - y),
            4 => (x, crop.height - 1 - y),
            5 => (y, x),
            6 => (y, crop.height - 1 - x),
            7 => (crop.width - 1 - y, crop.height - 1 - x),
            8 => (crop.width - 1 - y, x),
            _ => return Err("invalid EXIF orientation".into()),
        };
        let center_x = crop.x.checked_add(sx).ok_or("neutral x overflow")?;
        let center_y = crop.y.checked_add(sy).ok_or("neutral y overflow")?;
        let mapped = |px: u32, py: u32, channel: usize| {
            raw.corrected_sensor_sample_location(px, py, channel)
                .map_err(|e| {
                    lightwell_core::Error::new(lightwell_core::ErrorKind::Validation, e.to_string())
                })
        };
        let gain = |px: f64, py: f64, channel: usize| {
            raw.gain_at_corrected_sensor(px, py, channel).map_err(|e| {
                lightwell_core::Error::new(lightwell_core::ErrorKind::Validation, e.to_string())
            })
        };
        let result = sensor_neutral_gains_mapped(&source, center_x, center_y, &mapped, &gain);
        if result.is_ok() {
            return Ok([x, y]);
        }
    }
    Err(format!(
        "{} {}: no valid neutral patch in bounded grid",
        metadata.make, metadata.model
    )
    .into())
}
