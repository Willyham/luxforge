//! Local authentic-mode qualification. Set LIGHTWELL_RAW_OWNER_DIR and
//! LIGHTWELL_RAW_PUBLIC_DIR, then run with --ignored; no fixture is committed.
use lightwell_raw::{RawError, RawMode, RawSource, required_dng_opcodes};
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
#[ignore = "requires explicit local authentic RAW fixture paths"]
fn authentic_modes_preserve_sources_and_develop_float() {
    let owner = env::var("LIGHTWELL_RAW_OWNER_DIR").expect("owner fixture directory");
    let public = env::var("LIGHTWELL_RAW_PUBLIC_DIR").expect("public fixture directory");
    let cases = [
        (
            owner.as_str(),
            "nikon_z6.NEF",
            RawMode::NikonZ6Lossless14,
            6064,
            4040,
            "86c76c382dd4273e619a2dcc177a27b15c9e9b2d1187639c54d4d4c1336e8bfc",
        ),
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
            owner.as_str(),
            "fujifilm_x100vi.RAF",
            RawMode::FujifilmX100ViUncompressed14,
            7872,
            5196,
            "268eb98243c9a5b58fd57ed3ac79f0cc95101c20df26e0ac7ab241e594db1dce",
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
    ];
    let cancel = AtomicBool::new(false);
    for (dir, file, mode, w, h, expected_mosaic) in cases {
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
                lightwell_raw::RawRect {
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
                lightwell_raw::RawRect {
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
fn required_dji_opcodes_are_reported_before_unpack() {
    let owner = env::var("LIGHTWELL_RAW_OWNER_DIR").expect("owner fixture directory");
    let path = Path::new(&owner).join("mavic_air_2s.DNG");
    let (bytes, hash) = read_with_hash(&path);
    assert_eq!(required_dng_opcodes(&bytes).unwrap(), vec![1, 9]);
    let result = RawSource::decode(bytes, &AtomicBool::new(false));
    assert!(matches!(result,Err(RawError::UnsupportedRequiredOpcodes(ids)) if ids==vec![1,9]));
    unchanged(&path, &hash);
}
