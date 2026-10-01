//! Prepared capture identity and a derived optical-correction ledger. EXIF is read once with
//! the verified source; questions borrow those fields and never reopen the original.

use crate::export::CaptureMetadata;
use luxforge_raw::{OpticalEntry, OpticalLedger, OpticalStatus};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpticalIdentity {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_mm: Option<f64>,
    pub focal_35mm: Option<u16>,
}

impl From<&CaptureMetadata> for OpticalIdentity {
    fn from(capture: &CaptureMetadata) -> Self {
        Self {
            make: capture.make().map(str::to_owned),
            model: capture.model().map(str::to_owned),
            lens_make: capture.lens_make().map(str::to_owned),
            lens_model: capture.lens_model().map(str::to_owned),
            focal_mm: capture.focal_length_mm(),
            focal_35mm: capture.focal_length_35mm(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOptics {
    pub identity: OpticalIdentity,
    pub ledger: OpticalLedger,
}

impl SourceOptics {
    pub(crate) fn jpeg_ledger() -> OpticalLedger {
        let unknown = || OpticalEntry {
            status: OpticalStatus::Unknown,
            provenance: "jpeg-exif-none".into(),
        };
        OpticalLedger {
            distortion: unknown(),
            lateral_ca: unknown(),
            shading: unknown(),
            interpretation: "jpeg".into(),
        }
    }

    pub(crate) fn jpeg(capture: &CaptureMetadata) -> Self {
        Self {
            identity: OpticalIdentity::from(capture),
            ledger: Self::jpeg_ledger(),
        }
    }
}

impl super::PreparedSource {
    pub(crate) fn optics(&self) -> SourceOptics {
        match self {
            Self::Jpeg(source) => SourceOptics::jpeg(&source.capture),
            Self::Raw(source) => SourceOptics {
                identity: OpticalIdentity::from(source.capture.as_ref()),
                ledger: luxforge_raw::optical_ledger(source.sensor.metadata()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ErrorKind,
        modules::{FixedStage, Stage},
    };
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg")
    }

    #[test]
    fn capture_metadata_exposes_optical_identity() {
        let source = crate::open_source(&fixture_path()).unwrap();
        assert_eq!((source.width, source.height), (600, 400));
        let capture = source.capture.as_ref();
        assert_eq!(capture.make(), Some("NIKON CORPORATION"));
        assert_eq!(capture.model(), Some("NIKON Z 6"));
        assert_eq!(capture.lens_make(), None);
        assert_eq!(capture.lens_model(), Some("NIKKOR Z 24-70mm f/4 S"));
        assert_eq!(capture.focal_length_mm(), Some(35.0));
        assert_eq!(capture.focal_length_35mm(), Some(35));
        let identity = OpticalIdentity::from(capture);
        assert_eq!(identity.focal_mm, Some(35.0));
        assert_eq!(identity.model.as_deref(), capture.model());
    }

    #[test]
    fn jpeg_optics_report_unknown_distortion() {
        let source = crate::open_source(&fixture_path()).unwrap();
        let optics = super::super::PreparedSource::Jpeg(source).optics();
        for entry in [
            &optics.ledger.distortion,
            &optics.ledger.lateral_ca,
            &optics.ledger.shading,
        ] {
            assert_eq!(entry.status, OpticalStatus::Unknown);
            assert_eq!(entry.provenance, "jpeg-exif-none");
        }
        assert_eq!(optics.ledger.interpretation, "jpeg");
    }

    #[test]
    fn optics_require_a_prepared_source() {
        use crate::modules::StageQuestions;
        let fixed = FixedStage::new(Stage {
            width: 600,
            height: 400,
        });
        assert_eq!(
            fixed.optics().unwrap_err().kind,
            ErrorKind::PreparationRequired
        );
    }

    /// A hand-built little-endian EXIF APP1: IFD0's Make/Model and its Exif pointer,
    /// then FocalLength, FocalLengthIn35mmFilm and LensModel. No maker notes or real photo data.
    fn exif_app1() -> Vec<u8> {
        let make = b"NIKON CORPORATION\0";
        let model = b"NIKON Z 6\0";
        let lens = b"NIKKOR Z 24-70mm f/4 S\0";
        let mut tiff = b"II\x2a\0\x08\0\0\0".to_vec();
        let exif_offset = 50_u32;
        let data_offset = 92_u32;
        tiff.extend_from_slice(&3_u16.to_le_bytes());
        let write_entry = |tiff: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&kind.to_le_bytes());
            tiff.extend_from_slice(&count.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        };
        write_entry(&mut tiff, 0x010f, 2, make.len() as u32, data_offset);
        write_entry(
            &mut tiff,
            0x0110,
            2,
            model.len() as u32,
            data_offset + make.len() as u32,
        );
        write_entry(&mut tiff, 0x8769, 4, 1, exif_offset);
        tiff.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(tiff.len(), exif_offset as usize);
        tiff.extend_from_slice(&3_u16.to_le_bytes());
        let focal_offset = data_offset + (make.len() + model.len()) as u32;
        write_entry(&mut tiff, 0x920a, 5, 1, focal_offset);
        write_entry(&mut tiff, 0xa405, 3, 1, 35);
        write_entry(&mut tiff, 0xa434, 2, lens.len() as u32, focal_offset + 8);
        tiff.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(tiff.len(), data_offset as usize);
        tiff.extend_from_slice(make);
        tiff.extend_from_slice(model);
        tiff.extend_from_slice(&35_u32.to_le_bytes());
        tiff.extend_from_slice(&1_u32.to_le_bytes());
        tiff.extend_from_slice(lens);
        let payload = [b"Exif\0\0".as_slice(), &tiff].concat();
        let mut app1 = vec![0xff, 0xe1];
        app1.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
        app1.extend_from_slice(&payload);
        app1
    }

    #[test]
    #[ignore = "rewrites the committed synthetic lens grid"]
    fn regenerate_lens_grid_fixture() {
        let grid = image::RgbImage::from_fn(600, 400, |x, y| {
            let line = x % 40 < 2 || y % 40 < 2;
            image::Rgb([if line { 0 } else { 255 }; 3])
        });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&grid)
            .unwrap();
        let mut tagged = vec![0xff, 0xd8];
        tagged.extend_from_slice(&exif_app1());
        tagged.extend_from_slice(&jpeg[2..]);
        let path = fixture_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, tagged).unwrap();
    }
}
