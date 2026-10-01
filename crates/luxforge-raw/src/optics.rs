//! Source correction status derived from the existing RAW interpretation and static catalog.
//! No ledger field is persisted: catalog interpretations retain their current exact shape.

use crate::{DngOpticalRole, RawMetadata, opcodes::Opcode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpticalStatus {
    Applied,
    KnownUnapplied,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpticalEntry {
    pub status: OpticalStatus,
    pub provenance: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpticalLedger {
    pub distortion: OpticalEntry,
    pub lateral_ca: OpticalEntry,
    pub shading: OpticalEntry,
    pub interpretation: String,
}

impl OpticalEntry {
    fn mosaic() -> Self {
        Self {
            status: OpticalStatus::KnownUnapplied,
            provenance: "raw-mosaic".into(),
        }
    }

    fn applied(&mut self, provenance: String) {
        if self.status == OpticalStatus::Applied {
            self.provenance.push(';');
            self.provenance.push_str(&provenance);
        } else {
            self.status = OpticalStatus::Applied;
            self.provenance = provenance;
        }
    }
}

/// Derive optical status without opening the source or preparing any pixels. An undeclared DNG
/// warp is conservatively a distortion correction. Declared roles were checked against the
/// typed, per-plane warp when the source was prepared.
pub fn optical_ledger(metadata: &RawMetadata) -> OpticalLedger {
    let mut ledger = OpticalLedger {
        distortion: OpticalEntry::mosaic(),
        lateral_ca: OpticalEntry::mosaic(),
        shading: OpticalEntry::mosaic(),
        interpretation: format!("raw-mosaic:{}", metadata.mode.id()),
    };
    let Some(correction) = &metadata.dng_corrections else {
        return ledger;
    };
    ledger.interpretation.clone_from(&correction.interpretation);
    let declared = metadata.mode.dng_optics();
    for opcode in &correction.applied {
        // The implemented optical operations all run in list 3. Report its ordinal, not its
        // numeric TIFF tag.
        let provenance = |name| format!("dng-opcode:list3:{name}");
        match Opcode::implemented(opcode.list, opcode.id) {
            Some(Opcode::WarpRectilinear) => {
                if declared.and_then(|optics| optics.warp_rectilinear)
                    == Some(DngOpticalRole::LateralCa)
                {
                    ledger
                        .lateral_ca
                        .applied(format!("{}:planes=R,B", provenance("WarpRectilinear")));
                } else {
                    ledger.distortion.applied(provenance("WarpRectilinear"));
                }
            }
            Some(Opcode::GainMap) => ledger.shading.applied(provenance("GainMap")),
            Some(Opcode::FixVignetteRadial) => {
                ledger.shading.applied(provenance("FixVignetteRadial"))
            }
            _ => {}
        }
    }
    ledger
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DngCalibrationMetadata, DngCorrectionMetadata, DngOpcodeProvenance, RawMode, RawRect,
    };

    fn metadata(mode: &str, ids: &[u32]) -> RawMetadata {
        let rect = RawRect {
            x: 0,
            y: 0,
            width: 32,
            height: 32,
        };
        let dng_corrections = (!ids.is_empty()).then(|| DngCorrectionMetadata {
            interpretation: "test-dng".into(),
            applied: ids
                .iter()
                .map(|id| DngOpcodeProvenance {
                    list: crate::opcodes::OPCODE_LIST3,
                    id: *id,
                    version: crate::opcodes::VERSION,
                    flags: 0,
                    payload_sha256: "0".repeat(64),
                })
                .collect(),
            skipped_optional: vec![],
            calibration: DngCalibrationMetadata {
                illuminants: [17, 21],
                color_matrix1_sha256: "0".repeat(64),
                color_matrix2_sha256: "1".repeat(64),
                selected: "ColorMatrix2".into(),
            },
        });
        RawMetadata {
            make: "Test".into(),
            model: "Camera".into(),
            mode: RawMode::from_id(mode).unwrap(),
            sensor_width: 32,
            sensor_height: 32,
            active_area: rect,
            default_crop: rect,
            cfa_width: 2,
            cfa_height: 2,
            cfa: vec![0, 1, 1, 2],
            black_cfa: vec![0, 1, 3, 2],
            black_base: 0.0,
            black_channels: [0.0; 4],
            black_repeat_width: 1,
            black_repeat_height: 1,
            black_repeat: vec![0.0],
            sensor_white: 16383.0,
            as_shot_gains: [1.0; 3],
            libraw_flip: 0,
            rgb_cam: [[0.0; 4]; 3],
            cam_xyz: [[0.0; 3]; 4],
            backend: "test".into(),
            exif_orientation: 1,
            libraw_inset: None,
            format_identity: "test".into(),
            warnings: vec![],
            dng_corrections,
        }
    }

    #[test]
    fn optical_ledger_of_mosaic_raw_is_known_unapplied() {
        for mode in ["NikonZ6Lossless14", "FujifilmX100ViLossless14"] {
            let ledger = optical_ledger(&metadata(mode, &[]));
            assert_eq!(ledger.distortion.status, OpticalStatus::KnownUnapplied);
            assert_eq!(ledger.distortion.provenance, "raw-mosaic");
            assert_eq!(ledger.interpretation, format!("raw-mosaic:{mode}"));
        }
    }

    #[test]
    fn fc3411_ledger_reports_lateral_ca_applied_and_distortion_known_unapplied() {
        let ledger = optical_ledger(&metadata("DjiAir2sDng16", &[9, 1]));
        assert_eq!(ledger.distortion.status, OpticalStatus::KnownUnapplied);
        assert_eq!(ledger.distortion.provenance, "raw-mosaic");
        assert_eq!(ledger.lateral_ca.status, OpticalStatus::Applied);
        assert_eq!(
            ledger.lateral_ca.provenance,
            "dng-opcode:list3:WarpRectilinear:planes=R,B"
        );
        assert_eq!(ledger.shading.status, OpticalStatus::Applied);
        assert_eq!(ledger.shading.provenance, "dng-opcode:list3:GainMap");
        assert_eq!(ledger.interpretation, "test-dng");
    }

    #[test]
    fn undeclared_dng_warp_is_conservatively_applied_distortion() {
        let ledger = optical_ledger(&metadata("DJIFC220Dng16", &[1]));
        assert_eq!(ledger.distortion.status, OpticalStatus::Applied);
        assert_eq!(
            ledger.distortion.provenance,
            "dng-opcode:list3:WarpRectilinear"
        );
    }

    #[test]
    fn gain_map_and_vignette_opcodes_record_shading() {
        let ledger = optical_ledger(&metadata("DjiAir2sDng16", &[9, 3]));
        assert_eq!(ledger.shading.status, OpticalStatus::Applied);
        assert_eq!(
            ledger.shading.provenance,
            "dng-opcode:list3:GainMap;dng-opcode:list3:FixVignetteRadial"
        );
        let mut raw = metadata("DjiAir2sDng16", &[9]);
        let correction = raw.dng_corrections.as_mut().unwrap();
        correction.skipped_optional = std::mem::take(&mut correction.applied);
        assert_eq!(
            optical_ledger(&raw).shading.status,
            OpticalStatus::KnownUnapplied
        );
    }

    #[test]
    fn ledger_is_not_persisted_in_raw_metadata() {
        let raw = metadata("DjiAir2sDng16", &[9, 1]);
        let before = serde_json::to_value(&raw).unwrap();
        let _ = optical_ledger(&raw);
        let after = serde_json::to_value(&raw).unwrap();
        assert_eq!(before, after);
        let keys = |value: &serde_json::Value| {
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(&after),
            [
                "active_area",
                "as_shot_gains",
                "backend",
                "black_base",
                "black_cfa",
                "black_channels",
                "black_repeat",
                "black_repeat_height",
                "black_repeat_width",
                "cam_xyz",
                "cfa",
                "cfa_height",
                "cfa_width",
                "default_crop",
                "dng_corrections",
                "exif_orientation",
                "format_identity",
                "libraw_flip",
                "libraw_inset",
                "make",
                "mode",
                "model",
                "rgb_cam",
                "sensor_height",
                "sensor_white",
                "sensor_width",
                "warnings",
            ]
        );
        assert_eq!(
            keys(&after["dng_corrections"]),
            [
                "applied",
                "calibration",
                "interpretation",
                "skipped_optional"
            ]
        );
    }
}
