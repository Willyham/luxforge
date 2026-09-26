//! Local authentic-mode qualification. Set LUXFORGE_RAW_OWNER_DIR and
//! LUXFORGE_RAW_PUBLIC_DIR, then run with --ignored; no fixture is committed.
use luxforge_raw::{RawError, RawMode, RawSource, required_dng_opcodes};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

fn read_with_hash(path: &Path) -> (Arc<[u8]>, String) {
    let bytes = fs::read(path).expect("real fixture exists");
    let hash = format!("{:x}", Sha256::digest(&bytes));
    (Arc::from(bytes), hash)
}
fn unchanged(path: &Path, expected: &str) {
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(path).unwrap())),
        expected
    );
}
fn mosaic_hash(samples: &[u16]) -> String {
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 16384];
    for chunk in samples.chunks(8192) {
        for (i, &sample) in chunk.iter().enumerate() {
            bytes[2 * i..2 * i + 2].copy_from_slice(&sample.to_le_bytes());
        }
        hash.update(&bytes[..chunk.len() * 2]);
    }
    format!("{:x}", hash.finalize())
}

#[test]
#[ignore = "requires explicit local authentic owner RAW fixture paths"]
fn authentic_owner_modes_preserve_sources_and_develop_float() {
    let owner = env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner fixture directory");
    verify_authentic_modes(&[
        (
            owner.as_str(),
            "nikon_z6.NEF",
            RawMode::NikonZ6Lossless14,
            6064,
            4040,
            "86c76c382dd4273e619a2dcc177a27b15c9e9b2d1187639c54d4d4c1336e8bfc",
        ),
        (
            owner.as_str(),
            "fujifilm_x100vi.RAF",
            RawMode::FujifilmX100ViUncompressed14,
            7872,
            5196,
            "268eb98243c9a5b58fd57ed3ac79f0cc95101c20df26e0ac7ab241e594db1dce",
        ),
    ]);
}

#[test]
#[ignore = "requires explicit local authentic public RAW fixture paths"]
fn authentic_public_modes_preserve_sources_and_develop_float() {
    let public = env::var("LUXFORGE_RAW_PUBLIC_DIR").expect("public fixture directory");
    verify_authentic_modes(&[
        (
            public.as_str(),
            "z6-12-lossless.NEF",
            RawMode::NikonZ6Lossless12,
            6064,
            4040,
            "f25f0aafd76a99f5f20cbe2a001f4620397be43de829010d5dbaf9255ce64424",
        ),
        (
            public.as_str(),
            "z6-14-lossless.NEF",
            RawMode::NikonZ6Lossless14,
            6064,
            4040,
            "9896187fd3e3e29922b5b051a62f24afedbbbb75ddf63b5879a2b28de896116c",
        ),
        (
            public.as_str(),
            "x100vi-uncompressed.RAF",
            RawMode::FujifilmX100ViUncompressed14,
            7872,
            5196,
            "2348feb3f5d01634e4e843671d20468337c760c6799ca83b619a81eb64945b03",
        ),
        (
            public.as_str(),
            "x100vi-lossless.RAF",
            RawMode::FujifilmX100ViLossless14,
            7872,
            5196,
            "f10be69db8c3731fdcacf3741fd188fcef2557efd5de79f84a22a34adf443283",
        ),
    ]);
}

fn verify_authentic_modes(cases: &[(&str, &str, RawMode, u32, u32, &str)]) {
    let cancel = AtomicBool::new(false);
    for &(dir, file, mode, w, h, expected_mosaic) in cases {
        let path = Path::new(dir).join(file);
        let (bytes, hash) = read_with_hash(&path);
        let raw = RawSource::decode(bytes, &cancel).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(raw.metadata().mode, mode);
        assert_eq!(
            (raw.metadata().sensor_width, raw.metadata().sensor_height),
            (w, h)
        );
        assert_eq!(raw.mosaic().len(), w as usize * h as usize);
        assert_eq!(
            mosaic_hash(raw.mosaic()),
            expected_mosaic,
            "{file} full sensor pixels"
        );
        assert_eq!(raw.metadata().cfa.len(), if w == 6064 { 4 } else { 36 });
        if w == 6064 {
            assert_eq!(
                raw.metadata().default_crop,
                luxforge_raw::RawRect {
                    x: 8,
                    y: 8,
                    width: 6048,
                    height: 4024
                }
            );
            assert_eq!(
                raw.metadata().sensor_white,
                if mode == RawMode::NikonZ6Lossless12 {
                    4095.0
                } else {
                    16383.0
                }
            );
        } else {
            assert_eq!(
                raw.metadata().default_crop,
                luxforge_raw::RawRect {
                    x: 12,
                    y: 21,
                    width: 7728,
                    height: 5152
                }
            );
            assert_eq!(raw.metadata().black_repeat.len(), 36);
        }
        if file == "nikon_z6.NEF" {
            assert_eq!(raw.metadata().exif_orientation, 8);
        }
        assert!(raw.metadata().sensor_white > raw.metadata().black_base);
        let wb = raw.metadata().as_shot_gains;
        assert!(matches!(
            raw.develop([0.0, 1.0, 1.0], &cancel),
            Err(RawError::InvalidInput(_))
        ));
        let pre_cancel = AtomicBool::new(true);
        assert!(matches!(
            raw.develop(wb, &pre_cancel),
            Err(RawError::Cancelled)
        ));
        let rendered = raw
            .develop(wb, &cancel)
            .unwrap_or_else(|e| panic!("{file} develop: {e}"));
        assert_eq!(rendered.data.len(), 3 * w as usize * h as usize);
        assert!(rendered.data.iter().all(|v| v.is_finite()));
        let max = rendered
            .data
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let min = rendered.data.iter().copied().fold(f32::INFINITY, f32::min);
        println!("{file}: normalized camera float min={min} max={max}");
        let pointer = rendered.data.as_ptr();
        let shared = Arc::new(rendered.data);
        assert_eq!(
            pointer,
            shared.as_ptr(),
            "Vec→Arc<Vec> retains pixel allocation"
        );
        unchanged(&path, &hash);
    }
}

#[test]
#[ignore = "requires explicit local authentic DJI DNG"]
fn required_dji_opcodes_are_applied_to_fc3411() {
    let owner = env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner fixture directory");
    let path = Path::new(&owner).join("mavic_air_2s.DNG");
    let (bytes, hash) = read_with_hash(&path);
    assert_eq!(
        hash,
        "aab79ce1795a7dd5f1c2e52ec7bd07345cb9bda0262d1d5aa3701db212b09e1d"
    );
    assert_eq!(required_dng_opcodes(&bytes).unwrap(), vec![1, 9]);
    let raw = RawSource::decode(bytes, &AtomicBool::new(false)).expect("qualified FC3411 decode");
    assert_eq!(raw.metadata().mode, RawMode::DjiAir2sDng16);
    // This DNG's one uncompressed 16-bit strip independently hashes to this
    // value when read directly from TIFF bytes, before LibRaw touches it.
    assert_eq!(
        mosaic_hash(raw.mosaic()),
        "b681fbbb7c5f06c64525e675535119335b43c11dcb24493c5ace8e023d7fb888"
    );
    assert_eq!(
        (raw.metadata().sensor_width, raw.metadata().sensor_height),
        (5568, 3648)
    );
    assert_eq!(
        raw.metadata().active_area,
        luxforge_raw::RawRect {
            x: 96,
            y: 0,
            width: 5472,
            height: 3648
        }
    );
    assert_eq!(
        raw.metadata().default_crop,
        luxforge_raw::RawRect {
            x: 100,
            y: 4,
            width: 5464,
            height: 3640
        }
    );
    let corrections = raw
        .metadata()
        .dng_corrections
        .as_ref()
        .expect("opcode provenance");
    assert_eq!(
        corrections
            .applied
            .iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        vec![9, 1]
    );
    assert_eq!(
        corrections.applied[0].payload_sha256,
        "0650bbddc637dfd1c3515a5d95553edcc3502261ed3f69bf557a76f44b7b3d8d"
    );
    assert_eq!(
        corrections.applied[1].payload_sha256,
        "6c3218f3ac995b3725890f4dfc0a3244e0cba1ab12a0160014e1281cc7be18ae"
    );
    assert_eq!(corrections.calibration.illuminants, [17, 21]);
    assert_eq!(
        corrections.calibration.color_matrix1_sha256,
        "3191d0b7e46ff7ff05c10465b21da5b60c6a6edb69095cb8505e0b49cb5766a6"
    );
    assert_eq!(
        corrections.calibration.color_matrix2_sha256,
        "8d364580b59c2a1d509670ac43cceb18ed797e4c821874c9197012c2b54ef8ae"
    );
    assert!((raw.metadata().cam_xyz[0][0] - 0.8531).abs() < 1e-6);
    assert!((raw.metadata().cam_xyz[1][0] + 0.4071).abs() < 1e-6);
    assert!((raw.metadata().cam_xyz[2][2] - 0.5114).abs() < 1e-6);
    // Independently invert row-normalized ColorMatrix2 × LibRaw's pinned
    // linear-sRGB→XYZ constants (the documented cam_xyz_coeff calculation).
    // This relates our fixed D65 WB calibration to the rendered camera matrix.
    let expected_rgb_cam: [[f64; 3]; 3] = [
        [1.45702024, -0.30071009, -0.15631016],
        [-0.19137614, 1.39450546, -0.20312932],
        [-0.00003927, -0.24614702, 1.24618629],
    ];
    for (actual, expected) in raw.metadata().rgb_cam.iter().zip(expected_rgb_cam) {
        for (actual, expected) in actual[..3].iter().zip(expected) {
            assert!(
                (*actual as f64 - expected).abs() < 1e-4,
                "rgb_cam {actual} vs {expected}"
            );
        }
    }
    assert!(corrections.skipped_optional.is_empty());
    assert!(raw.gain_at_corrected_sensor(2840.0, 1800.0, 1).unwrap() > 0.0);
    let corrected = raw.corrected_sensor_sample_location(100, 4, 0).unwrap();
    assert!(corrected.0.is_finite() && corrected.1.is_finite());
    let rgb = raw
        .develop(raw.metadata().as_shot_gains, &AtomicBool::new(false))
        .expect("required stage-three corrections");
    assert_eq!(rgb.data.len(), 3 * 5568 * 3648);
    assert!(rgb.data.iter().all(|v| v.is_finite()));
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../../../probes/raw/dng_reference.json")).unwrap();
    let samples = reference["sparse_reference"]["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 18);
    let plane_len = rgb.width as usize * rgb.height as usize;
    for sample in samples {
        let x = sample["out_raw_xy"][0].as_u64().unwrap() as usize;
        let y = sample["out_raw_xy"][1].as_u64().unwrap() as usize;
        let channel = sample["plane"].as_u64().unwrap() as usize;
        let expected = sample["reference_float_headroom"].as_f64().unwrap();
        let actual = rgb.data[channel * plane_len + y * rgb.width as usize + x] as f64;
        assert!(
            (actual - expected).abs() < 1e-7,
            "corrected ({x},{y}) channel {channel}: {actual} vs {expected}"
        );
    }
    // The fixed-D65 solver's advertised 2000 K, tint +100 endpoint has a
    // blue gain above the old 16x bound. The full corrected frame must remain
    // finite at the shared 32x RAW gain limit.
    let endpoint = raw
        .develop([1.008_977, 1.0, 28.309_303], &AtomicBool::new(false))
        .expect("2000 K / +100 tint endpoint");
    assert!(endpoint.data.iter().all(|v| v.is_finite()));
    assert!(matches!(
        raw.develop([1.0, 1.0, 32.000_1], &AtomicBool::new(false)),
        Err(RawError::InvalidInput(_))
    ));
    unchanged(&path, &hash);
}

#[test]
#[ignore = "requires explicit local authentic DJI DNG"]
fn malformed_or_unknown_dji_opcode_fails_explicitly() {
    let owner = env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner fixture directory");
    let path = Path::new(&owner).join("mavic_air_2s.DNG");
    let (original, hash) = read_with_hash(&path);
    assert_eq!(
        hash,
        "aab79ce1795a7dd5f1c2e52ec7bd07345cb9bda0262d1d5aa3701db212b09e1d"
    );
    let list = 37518usize; // qualified raw SubIFD's OpcodeList3 offset
    let cancel = AtomicBool::new(false);

    let mut unknown = original.to_vec();
    unknown[list + 4..list + 8].copy_from_slice(&99_u32.to_be_bytes());
    assert!(matches!(RawSource::decode(Arc::from(unknown), &cancel),
        Err(RawError::UnsupportedRequiredOpcodes(ids)) if ids == vec![99]));

    let mut future = original.to_vec();
    future[list + 8..list + 12].copy_from_slice(&0x0200_0000_u32.to_be_bytes());
    assert!(matches!(
        RawSource::decode(Arc::from(future), &cancel),
        Err(RawError::UnsupportedMode(_))
    ));

    let mut invalid_gain = original.to_vec();
    let first_gain = list + 4 + 16 + 76;
    invalid_gain[first_gain..first_gain + 4].copy_from_slice(&f32::NAN.to_bits().to_be_bytes());
    assert!(matches!(
        RawSource::decode(Arc::from(invalid_gain), &cancel),
        Err(RawError::InvalidInput(_))
    ));

    let mut folded = original.to_vec();
    let warp_first_radial = list + 4 + 16 + 12364 + 16 + 4;
    folded[warp_first_radial..warp_first_radial + 8]
        .copy_from_slice(&0.0_f64.to_bits().to_be_bytes());
    assert!(matches!(
        RawSource::decode(Arc::from(folded), &cancel),
        Err(RawError::InvalidInput(_))
    ));
    let mut invalid_calibration = original.to_vec();
    // ColorMatrix2 is 3×3 SRATIONAL in the authoritative root IFD.
    invalid_calibration[9798 + 4..9798 + 8].fill(0);
    assert!(matches!(
        RawSource::decode(Arc::from(invalid_calibration), &cancel),
        Err(RawError::MissingCalibration(_))
    ));
    let mut competing_preview_calibration = original.to_vec();
    // The second SubIFD at 542 is the thumbnail. Its last tag's two-byte ID
    // can carry a competing ColorMatrix2 without changing sensor bytes.
    competing_preview_calibration[712..714].copy_from_slice(&50722_u16.to_le_bytes());
    assert!(matches!(
        RawSource::decode(Arc::from(competing_preview_calibration), &cancel),
        Err(RawError::InvalidInput("DNG calibration outside root IFD"))
    ));
    let mut conflicting_white_xy = original.to_vec();
    // Re-label the root XMP entry as AsShotWhiteXY; even an ill-typed
    // competing calibration must fail rather than be omitted from provenance.
    conflicting_white_xy[190..192].copy_from_slice(&50729_u16.to_le_bytes());
    assert!(matches!(
        RawSource::decode(Arc::from(conflicting_white_xy), &cancel),
        Err(RawError::UnsupportedMode(_))
    ));
    unchanged(&path, &hash);
}
