//! A synthetic camera for the merge lane, for a test outside the core that runs real merges over
//! files no decoder reads (the desktop's own, through the `test-holds` feature, which only
//! `[dev-dependencies]` turn on). Each input file is real and read once, its SHA-256 is its
//! identity, and its frame is the test's: everything after the development — spooling, analysis,
//! alignment, fusion, durable storage and publication — is the production path.
use super::{
    super::{OwnerHandle, OwnerMessage, catalog::CatalogMessage, library::LibraryMessage},
    MergeLane,
};
use crate::{
    Error, LinearImage,
    catalog_types::{CameraBody, Dimensions, Exposure, HeaderMetadata},
    editor::now_ms,
    jobs::JobControl,
    merge::{
        hdr::{FrameMetadata, Recording, SensorClipModel, SensorLayout},
        job::{self, Developed, Developer, Development},
    },
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, sync::Arc};

/// One developed frame of a synthetic Bayer camera: its camera-linear RGB, plane after plane,
/// and the exposure it was recorded at.
pub struct SyntheticFrame {
    pub width: u32,
    pub height: u32,
    /// Red, green and blue planes of `width × height` camera-linear values each.
    pub planes: Vec<f32>,
    pub shutter_s: f32,
    pub f_number: f32,
    pub iso: u32,
    /// The camera's matrix from its RGB to linear sRGB.
    pub camera_to_srgb: [[f32; 3]; 3],
    pub gains: [f32; 3],
}

/// What the test's camera develops each path to; `None` is a file it cannot read.
type Frames = dyn Fn(&Path) -> Option<SyntheticFrame> + Send + Sync;

struct Synthetic {
    frames: Arc<Frames>,
    reading: Option<Arc<luxforge_testbase::Gate>>,
}

impl Developer for Synthetic {
    fn develop(
        &self,
        path: &Path,
        _gains: Option<[f32; 3]>,
        control: &JobControl,
    ) -> Result<Developed, Error> {
        if let Some(reading) = &self.reading
            && !reading.pass_unless(|| control.is_cancelled())
        {
            return Err(control.cancelled_error());
        }
        control.checkpoint()?;
        let bytes = fs::read(path)
            .map_err(|error| crate::atomic_file::file_error(path.display(), error.kind()))?;
        let fingerprint = format!("{:x}", Sha256::digest(&bytes));
        let frame = (self.frames)(path).ok_or_else(|| {
            Error::unsupported_input(format!("{} could not be developed", path.display()))
        })?;
        let header = HeaderMetadata {
            camera: Some(CameraBody {
                make: "NIKON CORPORATION".into(),
                model: "NIKON Z 6".into(),
                serial: None,
            }),
            exposure: Exposure {
                time_s: Some(frame.shutter_s),
                f_number: Some(frame.f_number),
                iso: Some(frame.iso),
                bias_ev: None,
                focal_mm: Some(24.0),
                focal_35mm_mm: None,
            },
            dimensions: Some(Dimensions {
                width: frame.width,
                height: frame.height,
            }),
            ..HeaderMetadata::default()
        };
        let image = LinearImage::new(frame.width, frame.height, frame.planes)?;
        Ok(Developed {
            path: path.to_path_buf(),
            frame: FrameMetadata {
                identity: fingerprint.clone(),
                make: Some("NIKON CORPORATION".into()),
                model: Some("NIKON Z 6".into()),
                recording: Recording::Raw {
                    mode: "nikon-z6-14bit-lossless".into(),
                    layout: SensorLayout::Bayer,
                },
                width: frame.width,
                height: frame.height,
                orientation: 1,
                exposure: job::settings(&header),
                captured_at_ms: None,
            },
            fingerprint,
            header,
            place: None,
            volume: crate::index::volume_of(path, now_ms())?,
            development: Some(Development {
                gains: frame.gains,
                cam_xyz: [
                    [0.92, -0.31, -0.05],
                    [-0.45, 1.25, 0.2],
                    [-0.05, 0.15, 0.6],
                    [0.0, 0.0, 0.0],
                ],
                image,
                clip: SensorClipModel::new(frame.camera_to_srgb, frame.gains, SensorLayout::Bayer)?,
            }),
        })
    }
}

impl OwnerHandle {
    /// Develop every merge's inputs from now on with `frames`, holding each read at `reading`
    /// while it is shut (a cancel of the job releases it). For a test, through the `test-holds`
    /// feature, which only `[dev-dependencies]` turn on.
    pub fn merge_synthetic_frames(
        &self,
        frames: Arc<Frames>,
        reading: Option<Arc<luxforge_testbase::Gate>>,
    ) {
        let developer: Arc<dyn Developer> = Arc::new(Synthetic { frames, reading });
        self.sender
            .send(OwnerMessage::Catalog(CatalogMessage::Merge(
                LibraryMessage::Run(Box::new(move |owner| {
                    let lane: &mut MergeLane = &mut owner.catalog.merge;
                    lane.developer = developer;
                })),
            )))
            .expect("the owner is running");
    }
}
