//! The same strict catalog parser is compiled into the build script and library.
//! Camera policy is data; these enums name implemented format/processing capabilities.
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    pub version: u32,
    pub cameras: Vec<Camera>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Camera {
    pub make: String,
    pub model: String,
    pub sensor_size: [u32; 2],
    pub cfa_size: [u32; 2],
    pub crop: Crop,
    pub dng: Option<Dng>,
    pub calibration: Option<Calibration>,
    pub modes: Vec<Mode>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Calibration {
    pub xyz_to_camera: [[f64; 3]; 3],
    pub source: String,
    pub license: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Mode {
    pub id: String,
    pub bits: u32,
    pub raw_count: u32,
    pub decoder: String,
    pub dng_version: Option<u32>,
    pub validation: ModeValidation,
    pub compression: Option<Compression>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModeValidation {
    ContainerCompression,
    DecoderMetadata,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Compression {
    pub probe: CompressionProbe,
    pub value: u32,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompressionProbe {
    NefMakerNote,
    RafHeader,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Crop {
    LibrawInset,
    RafTags,
    ActiveArea,
    DngTags,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DngContainer {
    UncompressedU16SingleStrip,
    IntegerCfaSingleSegment,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DngCalibration {
    RootFixedMatrix,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DngCorrections {
    Stage3GainMapThenWarp,
    StageOrdered,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Dng {
    pub container: DngContainer,
    pub calibration: DngCalibration,
    pub illuminants: [u16; 2],
    pub selected_matrix: u8,
    pub calibration_identity: String,
    pub corrections: DngCorrections,
    pub interpretation: String,
    pub required_opcodes: Vec<Opcode>,
    #[serde(default)]
    pub decoder_active_bottom_trim: u32,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Opcode {
    pub id: u32,
    pub list: u16,
    pub version: u32,
    pub flags: u32,
}

impl Catalog {
    pub fn parse(json: &str) -> Result<Self, String> {
        if json.len() > 1024 * 1024 {
            return Err("camera catalog exceeds 1 MiB".into());
        }
        let catalog: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        catalog.validate()?;
        Ok(catalog)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.cameras.is_empty() || self.cameras.len() > 256 {
            return Err("unsupported camera catalog version or camera count".into());
        }
        let mut identities = HashSet::new();
        let mut ids = HashSet::new();
        for camera in &self.cameras {
            let fail = |message| Err(format!("{} {}: {message}", camera.make, camera.model));
            // Strings are also emitted as C++ literals; forbid truncation, controls,
            // quotes and escape characters instead of trusting code generation.
            let name = |s: &str, max| {
                !s.is_empty()
                    && s.len() < max
                    && s.bytes()
                        .all(|b| (b' '..=b'~').contains(&b) && b != b'"' && b != b'\\')
            };
            if !name(&camera.make, 64)
                || !name(&camera.model, 64)
                || !identities.insert((&camera.make, &camera.model))
            {
                return fail("invalid or duplicate camera identity");
            }
            let [w, h] = camera.sensor_size;
            if w == 0
                || h == 0
                || w > crate::limits::MAX_SIDE
                || h > crate::limits::MAX_SIDE
                || u64::from(w) * u64::from(h) > crate::limits::MAX_PIXELS as u64
                || u64::from(w) * u64::from(h) * 12 > crate::limits::MAX_RGB_BYTES as u64
            {
                return fail("sensor size exceeds decode/development limits");
            }
            if !matches!(camera.cfa_size, [2, 2] | [6, 6]) {
                return fail("unsupported CFA dimensions");
            }
            if (camera.crop == Crop::DngTags) != camera.dng.is_some() {
                return fail("DNG crop and processing settings must be enabled together");
            }
            if camera.dng.is_some() && camera.calibration.is_some() {
                return fail("DNG calibration must remain authoritative");
            }
            if let Some(calibration) = &camera.calibration {
                let valid_source_host = calibration
                    .source
                    .strip_prefix("https://")
                    .and_then(|rest| rest.split('/').next())
                    .is_some_and(|host| {
                        !host.is_empty()
                            && host
                                .bytes()
                                .all(|byte| byte > b' ' && !b"?#".contains(&byte))
                    });
                if calibration.source.len() > 1024
                    || !valid_source_host
                    || calibration.license.is_empty()
                    || calibration.license.len() > 80
                    || calibration.source.chars().any(char::is_control)
                    || calibration.license.chars().any(char::is_control)
                    || calibration
                        .xyz_to_camera
                        .iter()
                        .flatten()
                        .any(|value| !value.is_finite() || value.abs() > 16.0)
                {
                    return fail(
                        "invalid configured XYZ-to-camera calibration provenance or coefficient",
                    );
                }
                let m = &calibration.xyz_to_camera;
                let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                    - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                    + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
                if !det.is_finite() || det.abs() < 1e-8 {
                    return fail("configured XYZ-to-camera calibration is singular");
                }
            }
            if let Some(dng) = &camera.dng {
                if !matches!(
                    dng.container,
                    DngContainer::UncompressedU16SingleStrip
                        | DngContainer::IntegerCfaSingleSegment
                ) || dng.calibration != DngCalibration::RootFixedMatrix
                    || !matches!(
                        dng.corrections,
                        DngCorrections::Stage3GainMapThenWarp | DngCorrections::StageOrdered
                    )
                    || camera.cfa_size != [2, 2]
                    || !matches!(dng.selected_matrix, 1 | 2)
                    || dng.illuminants.contains(&0)
                    || !name(&dng.calibration_identity, 128)
                    || !name(&dng.interpretation, 128)
                {
                    return fail("unsupported DNG processing settings");
                }
                // This strategy implements exactly this operation order/domain.
                // These are algorithm capabilities, not camera-specific policy.
                if dng.required_opcodes.len() > 8
                    || dng
                        .required_opcodes
                        .iter()
                        .filter(|op| op.list == 51008)
                        .count()
                        > 1
                    || dng.required_opcodes.iter().any(|op| {
                        !matches!((op.list, op.id), (51022, 1 | 3 | 9) | (51008, 4 | 5))
                            || op.flags != 0
                            || op.version != 0x0103_0000
                    })
                    || dng
                        .required_opcodes
                        .windows(2)
                        .any(|ops| ops[0].list > ops[1].list)
                {
                    return fail("unsupported DNG opcode recipe");
                }
                if dng.decoder_active_bottom_trim > 63 {
                    return fail("DNG decoder active-bottom trim exceeds bound");
                }
                if dng.corrections == DngCorrections::Stage3GainMapThenWarp
                    && (dng.required_opcodes.len() != 2
                        || dng
                            .required_opcodes
                            .iter()
                            .zip([9, 1])
                            .any(|(op, id)| op.list != 51022 || op.id != id))
                {
                    return fail("unsupported DNG opcode recipe");
                }
            }
            if camera.modes.is_empty() || camera.modes.len() > 32 {
                return fail("invalid mode count");
            }
            for (i, mode) in camera.modes.iter().enumerate() {
                if mode.id == "Self"
                    || mode.id.len() > 80
                    || !mode
                        .id
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_uppercase)
                    || !mode.id.bytes().all(|b| b.is_ascii_alphanumeric())
                    || !ids.insert(&mode.id)
                    || !name(&mode.decoder, 80)
                    || !(1..=16).contains(&mode.bits)
                    || !matches!(mode.raw_count, 1 | 2)
                {
                    return fail("invalid or duplicate mode identifier, decoder or bit depth");
                }
                if let Some(dng) = &camera.dng {
                    if (dng.container == DngContainer::UncompressedU16SingleStrip
                        && mode.bits != 16)
                        || mode.dng_version.is_none_or(|v| v == 0)
                        || mode.validation != ModeValidation::DecoderMetadata
                        || mode.compression.is_some()
                    {
                        return fail(
                            "DNG strategy requires 16-bit storage and explicit DNG version",
                        );
                    }
                } else if mode.dng_version.is_some() {
                    return fail("non-DNG mode cannot declare a DNG version");
                } else if mode.validation == ModeValidation::ContainerCompression
                    && mode.compression.is_none()
                {
                    return fail("container-compression mode requires a compression probe");
                } else if mode.validation == ModeValidation::DecoderMetadata
                    && mode.compression.is_some()
                {
                    return fail("decoder-metadata mode cannot declare a compression probe");
                }
                if let Some(compression) = &mode.compression {
                    if compression.probe == CompressionProbe::NefMakerNote
                        && compression.value > u16::MAX as u32
                    {
                        return fail("NEF compression value exceeds maker-note field");
                    }
                    if (camera.crop == Crop::RafTags)
                        != (compression.probe == CompressionProbe::RafHeader)
                    {
                        return fail("crop and container probe disagree");
                    }
                }
                if camera.modes[..i].iter().any(|other| {
                    mode.bits == other.bits
                        && mode.decoder == other.decoder
                        && mode.dng_version == other.dng_version
                        && mode.compression == other.compression
                }) {
                    return fail("ambiguous recording mode selectors");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn catalog() -> Value {
        serde_json::from_str(include_str!("../data/cameras.json")).unwrap()
    }
    fn parse(value: &Value) -> Result<Catalog, String> {
        Catalog::parse(&value.to_string())
    }

    #[test]
    fn rejects_invalid_catalogs_without_partial_acceptance() {
        let mut excessive_trim = catalog();
        excessive_trim["cameras"][2]["dng"]["decoder_active_bottom_trim"] = json!(64);
        assert!(parse(&excessive_trim).is_err());
        let mutations: Vec<(&str, Value)> = vec![
            ("/version", json!(2)),
            ("/cameras", json!([])),
            ("/cameras/0/sensor_size", json!([0, 4040])),
            ("/cameras/0/sensor_size", json!([16000, 16000])),
            ("/cameras/0/cfa_size", json!([3, 3])),
            ("/cameras/0/make", json!("Nikon\u{0}")),
            ("/cameras/0/model", json!("bad\"literal")),
            ("/cameras/0/modes", json!([])),
            ("/cameras/0/modes/0/bits", json!(17)),
            ("/cameras/0/modes/0/id", json!("invalid-name")),
            ("/cameras/0/modes/0/compression/probe", json!("unknown")),
            ("/cameras/0/modes/0/compression/value", json!(65536)),
            ("/cameras/0/modes/0/compression", Value::Null),
            ("/cameras/0/modes/0/validation", json!("decoder_metadata")),
            ("/cameras/0/modes/0/validation", Value::Null),
            ("/cameras/0/crop", json!("dng_tags")),
            ("/cameras/1/crop", json!("libraw_inset")),
            ("/cameras/2/dng", Value::Null),
            ("/cameras/2/dng/selected_matrix", json!(3)),
            ("/cameras/2/dng/illuminants", json!([0, 21])),
            ("/cameras/2/dng/corrections", json!("skip_unknown")),
            ("/cameras/2/dng/required_opcodes/0/id", json!(1)),
            ("/cameras/2/dng/required_opcodes/0/version", json!(0)),
            ("/cameras/2/dng/required_opcodes/0/flags", json!(1)),
            ("/cameras/2/modes/0/dng_version", Value::Null),
            ("/cameras/2/modes/0/bits", json!(14)),
        ];
        for (pointer, replacement) in mutations {
            let mut data = catalog();
            *data.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse(&data).is_err(), "accepted invalid {pointer}");
        }
        for (pointer, replacement) in [
            (
                "/cameras/0/calibration/source",
                json!("http://example.test/matrix"),
            ),
            ("/cameras/0/calibration/license", json!("x".repeat(81))),
            (
                "/cameras/0/calibration/xyz_to_camera",
                json!([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 17.0]]),
            ),
        ] {
            let mut data = catalog();
            data["cameras"][0]["calibration"] = json!({
                "xyz_to_camera": [[1.0,0.0,0.0],[0.0,1.0,0.0],[0.0,0.0,1.0]],
                "source": "https://example.test/matrix", "license": "CC0"
            });
            *data.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse(&data).is_err(), "accepted invalid {pointer}");
        }
        let mut missing_raw_count = catalog();
        missing_raw_count["cameras"][0]["modes"][0]
            .as_object_mut()
            .unwrap()
            .remove("raw_count");
        assert!(
            parse(&missing_raw_count).is_err(),
            "accepted missing raw_count"
        );
        let mut multiple_repairs = catalog();
        multiple_repairs["cameras"][2]["dng"]["corrections"] = json!("stage_ordered");
        multiple_repairs["cameras"][2]["dng"]["required_opcodes"] = json!([
            {"list":51008,"id":4,"version":0x0103_0000,"flags":0},
            {"list":51008,"id":5,"version":0x0103_0000,"flags":0}
        ]);
        assert!(
            parse(&multiple_repairs).is_err(),
            "accepted unsupported sequential sensor repairs"
        );
        for pointer in [
            "",
            "/cameras/0",
            "/cameras/0/modes/0",
            "/cameras/0/modes/0/compression",
            "/cameras/2/dng",
            "/cameras/2/dng/required_opcodes/0",
        ] {
            let mut data = catalog();
            data.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("typo".into(), json!(true));
            assert!(parse(&data).is_err(), "accepted unknown field at {pointer}");
        }
        let mut decoder_with_probe = catalog();
        decoder_with_probe["cameras"][2]["modes"][0]["validation"] = json!("decoder_metadata");
        decoder_with_probe["cameras"][2]["modes"][0]["compression"] = json!({
            "probe": "nef_maker_note",
            "value": 3
        });
        assert!(parse(&decoder_with_probe).is_err());
        let mut container_without_probe = catalog();
        container_without_probe["cameras"][0]["modes"][0]["compression"] = Value::Null;
        assert!(parse(&container_without_probe).is_err());
        let mut dng_calibration = catalog();
        dng_calibration["cameras"][2]["calibration"] = json!({
            "xyz_to_camera": [[1.0,0.0,0.0],[0.0,1.0,0.0],[0.0,0.0,1.0]],
            "source": "https://example.test/matrix", "license": "CC0"
        });
        assert!(parse(&dng_calibration).is_err());
        let mut duplicate = catalog();
        let camera = duplicate["cameras"][0].clone();
        duplicate["cameras"].as_array_mut().unwrap().push(camera);
        assert!(parse(&duplicate).unwrap_err().contains("duplicate camera"));
        let mut duplicate = catalog();
        duplicate["cameras"][1]["modes"][0]["id"] =
            duplicate["cameras"][0]["modes"][0]["id"].clone();
        assert!(parse(&duplicate).unwrap_err().contains("duplicate mode"));
        let mut ambiguous = catalog();
        let mut mode = ambiguous["cameras"][0]["modes"][0].clone();
        mode["id"] = json!("AmbiguousMode");
        ambiguous["cameras"][0]["modes"]
            .as_array_mut()
            .unwrap()
            .push(mode);
        assert!(parse(&ambiguous).unwrap_err().contains("ambiguous"));
        assert!(Catalog::parse(&" ".repeat(1024 * 1024 + 1)).is_err());
        assert!(Catalog::parse("{broken").is_err());
    }

    #[test]
    fn camera_and_mode_names_are_labels_not_processing_switches() {
        let mut data = catalog();
        data["cameras"][1]["make"] = json!("Example");
        data["cameras"][1]["model"] = json!("Renamed sensor");
        data["cameras"][1]["sensor_size"] = json!([600, 400]);
        data["cameras"][1]["modes"][0]["id"] = json!("ExampleUncompressed");
        data["cameras"][1]["modes"][0]["compression"]["value"] = json!(7);
        let profiles = parse(&data).unwrap();
        let mut native = crate::RawSource::blank_native();
        native.width = 600;
        native.height = 400;
        native.cfa_width = 6;
        native.cfa_height = 6;
        native.raw_count = 1;
        native.raw_bps = 14;
        let mut bytes = vec![0; 0x70];
        bytes[..8].copy_from_slice(b"FUJIFILM");
        bytes[0x6c..0x70].copy_from_slice(&7_u32.to_be_bytes());
        let classify = |n: &crate::NativeMetadata, b: &[u8], make: &str| {
            crate::format::classify_mode(
                &profiles,
                n,
                make,
                "Renamed sensor",
                "unpacked_load_raw()",
                b,
            )
        };
        let (camera, mode) = classify(&native, &bytes, "Example").unwrap();
        assert_eq!(mode.id, "ExampleUncompressed");
        assert_eq!(camera.crop, Crop::RafTags);
        assert!(camera.dng.is_none());
        assert!(classify(&native, &bytes, "Fujifilm").is_err());
        assert!(classify(&native, &[], "Example").is_err());
        bytes[0x6c..0x70].copy_from_slice(&0_u32.to_be_bytes());
        assert!(classify(&native, &bytes, "Example").is_err());
        bytes[0x6c..0x70].copy_from_slice(&7_u32.to_be_bytes());
        native.width += 1;
        assert!(classify(&native, &bytes, "Example").is_err());
        native.width -= 1;
        native.raw_bps = 12;
        assert!(classify(&native, &bytes, "Example").is_err());
        native.raw_bps = 14;
        native.raw_count = 2;
        assert!(classify(&native, &bytes, "Example").is_err());
    }

    #[test]
    fn raw_count_two_is_an_explicit_primary_frame_mode_selector() {
        let mut data = catalog();
        data["cameras"][1]["modes"][0]["raw_count"] = json!(2);
        let profiles = parse(&data).unwrap();
        let mut native = crate::RawSource::blank_native();
        native.width = 7872;
        native.height = 5196;
        native.cfa_width = 6;
        native.cfa_height = 6;
        native.raw_count = 2;
        native.raw_bps = 14;
        let mut raf = vec![0_u8; 0x70];
        raf[..8].copy_from_slice(b"FUJIFILM");
        raf[0x6c..].copy_from_slice(&0_u32.to_be_bytes());
        let (_, mode) = crate::format::classify_mode(
            &profiles,
            &native,
            "Fujifilm",
            "X100VI",
            "unpacked_load_raw()",
            &raf,
        )
        .unwrap();
        assert_eq!(mode.raw_count, 2);
        native.raw_count = 1;
        assert!(
            crate::format::classify_mode(
                &profiles,
                &native,
                "Fujifilm",
                "X100VI",
                "unpacked_load_raw()",
                &raf,
            )
            .is_err()
        );
    }

    #[test]
    fn qualified_recording_modes_keep_their_exact_selectors() {
        // Minimal NEF TIFF -> EXIF -> Nikon maker-note TIFF, with compression 3.
        // The marker is independent of the profile data being verified.
        let mut nef = vec![0_u8; 80];
        nef[..8].copy_from_slice(b"II*\0\x08\0\0\0");
        for (offset, tag, kind, count, value) in [
            (8, 0x8769_u16, 4_u16, 1_u32, 26_u32),
            (26, 0x927c, 7, 36, 44),
            (62, 0x93, 3, 1, 3),
        ] {
            nef[offset..offset + 2].copy_from_slice(&1_u16.to_le_bytes());
            nef[offset + 2..offset + 4].copy_from_slice(&tag.to_le_bytes());
            nef[offset + 4..offset + 6].copy_from_slice(&kind.to_le_bytes());
            nef[offset + 6..offset + 10].copy_from_slice(&count.to_le_bytes());
            nef[offset + 10..offset + 14].copy_from_slice(&value.to_le_bytes());
        }
        nef[44..50].copy_from_slice(b"Nikon\0");
        nef[54..62].copy_from_slice(b"II*\0\x08\0\0\0");
        let mut raf = vec![0_u8; 0x70];
        raf[..8].copy_from_slice(b"FUJIFILM");
        let mut compressed_raf = raf.clone();
        compressed_raf[0x6c..].copy_from_slice(&2_u32.to_be_bytes());
        let cases = [
            (
                "Nikon",
                "Z 6",
                6064,
                4040,
                2,
                12,
                0,
                "nikon_load_raw()",
                nef.as_slice(),
                "NikonZ6Lossless12",
            ),
            (
                "Nikon",
                "Z 6",
                6064,
                4040,
                2,
                14,
                0,
                "nikon_load_raw()",
                nef.as_slice(),
                "NikonZ6Lossless14",
            ),
            (
                "Fujifilm",
                "X100VI",
                7872,
                5196,
                6,
                14,
                0,
                "unpacked_load_raw()",
                raf.as_slice(),
                "FujifilmX100ViUncompressed14",
            ),
            (
                "Fujifilm",
                "X100VI",
                7872,
                5196,
                6,
                14,
                0,
                "fuji_compressed_load_raw()",
                compressed_raf.as_slice(),
                "FujifilmX100ViLossless14",
            ),
            (
                "DJI",
                "FC3411",
                5568,
                3648,
                2,
                16,
                0x0104_0000,
                "packed_dng_load_raw()",
                &[][..],
                "DjiAir2sDng16",
            ),
        ];
        for (make, model, width, height, cfa, bits, version, decoder, bytes, id) in cases {
            let mut n = crate::RawSource::blank_native();
            n.width = width;
            n.height = height;
            n.cfa_width = cfa;
            n.cfa_height = cfa;
            n.raw_bps = bits;
            n.raw_count = 1;
            n.dng_version = version;
            let classify = |n: &crate::NativeMetadata, decoder: &str| {
                crate::format::classify_mode(
                    crate::camera_catalog(),
                    n,
                    make,
                    model,
                    decoder,
                    bytes,
                )
            };
            assert_eq!(classify(&n, decoder).unwrap().1.id, id);
            assert!(classify(&n, "wrong_decoder()").is_err());
            n.cfa_height += 1;
            assert!(classify(&n, decoder).is_err());
            n.cfa_height -= 1;
            n.width += 1;
            assert!(classify(&n, decoder).is_err());
            n.width -= 1;
            n.raw_bps = 8;
            assert!(classify(&n, decoder).is_err());
            if version != 0 {
                n.raw_bps = bits;
                n.dng_version += 1;
                assert!(classify(&n, decoder).is_err());
            }
        }
    }

    #[test]
    fn generated_modes_and_capabilities_share_the_catalog() {
        for camera in &crate::camera_catalog().cameras {
            for mode in &camera.modes {
                let public: crate::RawMode = serde_json::from_value(json!(mode.id)).unwrap();
                assert_eq!(public.id(), mode.id);
                assert_eq!(public.requires_dng_corrections(), camera.dng.is_some());
                assert_eq!(serde_json::to_value(public).unwrap(), json!(mode.id));
            }
        }
        assert!(serde_json::from_value::<crate::RawMode>(json!("UnknownCameraMode")).is_err());
    }
}
