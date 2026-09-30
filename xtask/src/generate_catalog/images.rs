//! `--images N`: real JPEG files under `images/`, laid out like the sources a photographer
//! browses, each with a full EXIF segment and an embedded thumbnail, and `images/manifest.json`,
//! their ground truth.
//!
//! The layout, from the September trips of the [plan](super::plan):
//!
//! - `card/DCIM/100NZ8_1/DSC_0001.JPG` …: a camera card read in place, holding the first Nikon
//!   Z 8's folder and the second Z 8's `100NZ8_2`, whose files have the same names;
//! - `Card dumps/2026-09-12/L1003201.JPG` …: the Leica's and the drone's cards copied into dump
//!   folders named after each trip's first day;
//! - `2026-09-14 Lake/DSCF0001.JPG` …: a user-named folder holding the trip with no GPS;
//! - `iPhone export/IMG_4201.JPG` …: the phone's files;
//! - `From Anna/DSCF….JPG`: the undated files.
//!
//! Every file is a 640 × 427 baseline JPEG with a JFIF APP0 and then one EXIF APP1 (see
//! [metadata](super::metadata)) whose IFD1 holds a 160 × 107 JPEG thumbnail of the same picture.
//!
//! # The manifest
//!
//! ```text
//! {
//!   "generator": "cargo xtask generate-catalog",
//!   "seed": 1,
//!   "counts": {"images": 120},
//!   "image": {"width": 640, "height": 427},
//!   "thumbnail": {"width": 160, "height": 107},
//!   "events": [                          // the ground-truth events, in time order
//!     {
//!       "label": "konstanz",             // unique; "undated" for the undated files
//!       "place": "Konstanz",             // null when no frame of it carries a position
//!       "position": {"latitude": 47.66, "longitude": 9.175},  // the place's, or null likewise
//!       "folder": null,                  // the user-named folder its files are in, when any
//!       "days": ["2026-09-12", "2026-09-13"],  // camera-local dates; [] when undated
//!       "files": 38
//!     }
//!   ],
//!   "files": [                           // one per file, by event, then moment, then frame
//!     {
//!       "path": "card/DCIM/100NZ8_1/DSC_0001.JPG",  // relative to images/, with `/`
//!       "event": "konstanz",
//!       "day": "2026-09-12",             // camera-local date; null when undated
//!       "body": "NIKON CORPORATION|NIKON Z 8|3012845",  // make|model|serial ("" when none)
//!       "moment": "konstanz/0003",       // unique across the run
//!       "kind": "burst",                 // "single", "burst" or "bracket"
//!       "bracket_evidence": null,        // "metadata" or "previews" for a bracket, else null
//!       "step_ev": null,                 // the frame's step within its bracket, else null
//!       "frame": 0,                      // index within the moment
//!       "capture": "2026-09-12T10:15:02.130+02:00",  // DateTimeOriginal and SubSecTimeOriginal,
//!                                        // with OffsetTimeOriginal when written; null undated
//!       "utc_ms": 1789200902130,         // the capture's UTC milliseconds; null when undated
//!       "gps": {"latitude": 47.66123, "longitude": 9.17601, "altitude_m": 412.3},  // or null
//!       "exposure": {
//!         "exposure_time": "1/250",      // as written, reduced
//!         "exposure_time_s": 0.004,
//!         "f_number": 4.0,
//!         "iso": 100,
//!         "bias_ev": 0.0,                // ExposureBiasValue as written
//!         "focal_length_mm": 50.0,
//!         "focal_length_35mm": 50
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! Latitudes and longitudes are exactly what the written degrees, minutes and seconds come to.
//! Same seed and count, same bytes: of every image and of the manifest.
use super::metadata;
use super::plan::{Event, Frame, Plan, Storage};
use super::scene::{self, Encoder};
use crate::{Result, write_json};
use image::ImageEncoder;
use image::codecs::jpeg::JpegEncoder;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const QUALITY: u8 = 90;
const THUMBNAIL_QUALITY: u8 = 80;

/// What `write` made.
pub struct Written {
    pub files: usize,
    pub events: usize,
}

/// Writes the images of `plan`, made from `seed`, and the manifest into `dir`, which must not
/// exist.
pub fn write(dir: &Path, seed: u64, plan: Plan) -> Result<Written> {
    fs::create_dir(dir)?;
    let encoder = Encoder::new();
    let mut events = Vec::new();
    let mut files = Vec::new();
    for event in plan {
        for frame in &event.frames {
            let path = path(&event, frame);
            let target = path.split('/').fold(dir.to_path_buf(), |p, c| p.join(c));
            fs::create_dir_all(target.parent().expect("a folder"))?;
            fs::write(&target, jpeg(frame, &encoder)?)?;
            files.push(entry(&event, frame, &path));
        }
        events.push(event_entry(&event));
    }
    let written = Written {
        files: files.len(),
        events: events.len(),
    };
    write_json(
        &dir.join("manifest.json"),
        &json!({
            "generator": "cargo xtask generate-catalog",
            "seed": seed,
            "counts": {"images": files.len()},
            "image": {"width": scene::WIDTH, "height": scene::HEIGHT},
            "thumbnail": {"width": scene::THUMBNAIL_WIDTH, "height": scene::THUMBNAIL_HEIGHT},
            "events": events,
            "files": files,
        }),
    )?;
    Ok(written)
}

/// The file's path under `images/`, with `/` separators.
pub fn path(event: &Event, frame: &Frame) -> String {
    let body = frame.body();
    let (folder_number, stem) = body.file_stem(frame.count);
    let folder = match body.storage {
        Storage::Card { suffix } => format!("card/DCIM/{folder_number}{suffix}"),
        Storage::Dump => format!("Card dumps/{}", event.start),
        Storage::Named => event.folder.clone(),
        Storage::Phone => "iPhone export".into(),
    };
    format!("{folder}/{stem}.JPG")
}

/// The file's bytes: the rendered frame with its EXIF and thumbnail.
fn jpeg(frame: &Frame, encoder: &Encoder) -> Result<Vec<u8>> {
    let image = scene::render(frame.scene, frame.step.unwrap_or(0), encoder);
    let mut thumbnail = Vec::new();
    JpegEncoder::new_with_quality(&mut thumbnail, THUMBNAIL_QUALITY)
        .encode_image(&scene::thumbnail(&image))?;
    let tiff = metadata::tiff(frame, image.width(), image.height(), &thumbnail)?;
    let mut bytes = Vec::new();
    let mut jpeg = JpegEncoder::new_with_quality(&mut bytes, QUALITY);
    jpeg.set_exif_metadata(tiff)?;
    jpeg.encode_image(&image)?;
    Ok(bytes)
}

fn degrees(micro: i32) -> f64 {
    f64::from(micro) / 1e6
}

fn event_entry(event: &Event) -> Value {
    let place = event.place.filter(|_| event.positioned());
    let named = event
        .frames
        .iter()
        .any(|f| f.body().storage == Storage::Named);
    json!({
        "label": event.label,
        "place": place.map(|p| p.name),
        "position": place.map(|p| json!({
            "latitude": degrees(p.latitude),
            "longitude": degrees(p.longitude),
        })),
        "folder": named.then_some(&event.folder),
        "days": event.days().iter().map(ToString::to_string).collect::<Vec<_>>(),
        "files": event.frames.len(),
    })
}

fn entry(event: &Event, frame: &Frame, path: &str) -> Value {
    let body = frame.body();
    let exposure = frame.exposure;
    let bracket = match frame.kind {
        super::plan::MomentKind::Bracket(kind) => Some(kind.evidence()),
        _ => None,
    };
    let bias = metadata::bias(exposure.bias);
    json!({
        "path": path,
        "event": event.label,
        "day": frame.time.map(|t| t.date().to_string()),
        "body": body.key(),
        "moment": event.moment_label(frame.moment),
        "kind": frame.kind.name(),
        "bracket_evidence": bracket,
        "step_ev": frame.step,
        "frame": frame.index,
        "capture": frame.time.map(|t| t.iso(frame.written_offset())),
        "utc_ms": frame.utc(),
        "gps": frame.gps.map(|gps| json!({
            "latitude": degrees(gps.latitude),
            "longitude": degrees(gps.longitude),
            "altitude_m": f64::from(gps.altitude) / 10.0,
        })),
        "exposure": {
            "exposure_time": format!("{}/{}", exposure.time.0, exposure.time.1),
            "exposure_time_s": f64::from(exposure.time.0) / f64::from(exposure.time.1),
            "f_number": f64::from(exposure.f_number) / 100.0,
            "iso": exposure.iso,
            "bias_ev": f64::from(bias.num) / f64::from(bias.denom),
            "focal_length_mm": f64::from(exposure.focal) / 1000.0,
            "focal_length_35mm": exposure.focal_35,
        },
    })
}
