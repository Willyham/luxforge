//! The crate-private boundary to the vendored RawSpeed ([RawSpeed unpacking](../../../docs/design/rawspeed-unpack.md)).
//!
//! RawSpeed decodes a borrowed encoded buffer into one uncropped, uncorrected u16 sensor mosaic
//! of a size the caller already knows. Its camera data is the pinned `cameras.xml` compiled into
//! the native library, parsed once per process on first use. Nothing here is routed yet: LibRaw
//! still fills every `RawSource` mosaic.

use crate::{RawError, c_text, native_status::NativeStatus};
use std::ffi::{c_char, c_int};

/// What RawSpeed decoded, reported whether or not it matched the expected size and type. The
/// native `LfRawSpeedImage` has the same layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RawSpeedImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) components: u32,
    /// 1 for u16 samples, 0 for float.
    pub(crate) u16: u32,
}

unsafe extern "C" {
    fn lf_rawspeed_decode(
        bytes: *const u8,
        byte_length: usize,
        dest: *mut u16,
        length: usize,
        width: u32,
        height: u32,
        image: *mut RawSpeedImage,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    #[cfg(test)]
    fn lf_rawspeed_cameras(
        cameras: *mut u32,
        parses: *mut u32,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

/// Decode `bytes` with RawSpeed into `mosaic`, which must hold exactly `width × height` samples
/// in row order: uncorrected values (no linearization curve), no crop, no bad-pixel interpolation
/// and no stage-1 DNG opcodes, with unknown cameras refused. A RawSpeed failure, or an image that
/// is not one-component u16 of exactly that size, is an error naming RawSpeed and what it
/// produced; `mosaic` is then unspecified.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "routing into LibRaw's unpack is a later task")
)]
pub(crate) fn decode_mosaic(
    bytes: &[u8],
    width: u32,
    height: u32,
    mosaic: &mut [u16],
) -> Result<(), RawError> {
    let mut image = RawSpeedImage::default();
    let mut error = [0 as c_char; 256];
    // SAFETY: `bytes` and `mosaic` are live, unaliased borrows for this synchronous call, which
    // reads at most `bytes.len()` bytes and writes at most `mosaic.len()` samples (it checks that
    // length against width × height first). `image` and `error` are writable and not retained.
    let code = unsafe {
        lf_rawspeed_decode(
            bytes.as_ptr(),
            bytes.len(),
            mosaic.as_mut_ptr(),
            mosaic.len(),
            width,
            height,
            &mut image,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if code == NativeStatus::Ok as c_int {
        return Ok(());
    }
    let decoded = if image.width == 0 {
        "nothing decoded".to_owned()
    } else {
        format!(
            "decoded {}x{} with {} component(s) of {}",
            image.width,
            image.height,
            image.components,
            if image.u16 == 1 { "u16" } else { "float" }
        )
    };
    Err(RawError::Native(format!(
        "RawSpeed: {} ({decoded}; expected {width}x{height} u16)",
        c_text(&error)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeHandle, RawSource, cancelled, lf_raw_open, native_result};
    use sha2::{Digest, Sha256};
    use std::{
        ffi::c_void,
        sync::{Arc, Barrier, atomic::AtomicBool},
    };

    unsafe extern "C" {
        fn lf_raw_curve(handle: *mut c_void, dest: *mut u16, length: usize) -> c_int;
    }

    /// The camera count and this process's parse count, initializing the camera data if needed.
    fn cameras() -> (u32, u32) {
        let (mut cameras, mut parses) = (0, 0);
        let mut error = [0 as c_char; 256];
        // SAFETY: every pointer is to a live local, written synchronously and not retained.
        let code = unsafe {
            lf_rawspeed_cameras(&mut cameras, &mut parses, error.as_mut_ptr(), error.len())
        };
        native_result(code, &error).expect("embedded camera data");
        (cameras, parses)
    }

    /// No other non-ignored test touches the camera data, so in an ordinary test run these threads
    /// are its first users.
    #[test]
    fn rawspeed_camera_data_is_parsed_once_under_concurrent_first_use() {
        const THREADS: usize = 16;
        let barrier = Arc::new(Barrier::new(THREADS));
        let counts: Vec<(u32, u32)> = (0..THREADS)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    cameras()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().expect("camera data thread"))
            .collect();
        let (camera_count, _) = counts[0];
        // The pinned cameras.xml has well over a thousand entries once aliases are added.
        assert!(camera_count > 1000, "camera entries: {camera_count}");
        for (count, parses) in counts {
            assert_eq!(count, camera_count);
            assert_eq!(parses, 1, "every caller sees the one parse");
        }
        assert_eq!(
            cameras(),
            (camera_count, 1),
            "a later call does not parse again"
        );
    }

    #[test]
    fn rawspeed_embedded_camera_data_is_the_pinned_file() {
        let xml = include_bytes!("../vendor/rawspeed-c835b05a/data/cameras.xml");
        assert_eq!(
            format!("{:x}", Sha256::digest(xml)),
            "d67d32beb3acf073a4ecc521daae29545f90bf79270749e9041031dedb89d166"
        );
    }

    #[test]
    fn rawspeed_refuses_bad_buffers_and_undecodable_input() {
        let mut mosaic = vec![0u16; 16];
        assert!(matches!(
            decode_mosaic(&[], 4, 4, &mut mosaic),
            Err(RawError::Native(text)) if text.contains("invalid or oversized")
        ));
        assert!(matches!(
            decode_mosaic(&[0; 64], 4, 5, &mut mosaic),
            Err(RawError::Native(text)) if text.contains("outside the adapter limits")
        ));
    }

    /// LibRaw's linearization table for `bytes` after unpack.
    fn libraw_curve(bytes: &[u8]) -> Vec<u16> {
        let mut native = RawSource::blank_native();
        let mut handle = std::ptr::null_mut();
        let mut error = [0 as c_char; 256];
        let cancel = AtomicBool::new(false);
        // SAFETY: bytes, metadata, error and the cancel token outlive the synchronous open; the
        // handle is closed by its guard.
        let code = unsafe {
            lf_raw_open(
                bytes.as_ptr(),
                bytes.len(),
                cancelled,
                (&cancel as *const AtomicBool).cast_mut().cast(),
                &mut handle,
                &mut native,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        native_result(code, &error).expect("LibRaw open");
        let guard = NativeHandle(handle);
        let mut curve = vec![0u16; 0x10000];
        // SAFETY: the guard holds the live handle; curve has the table's 65536 slots.
        let code = unsafe { lf_raw_curve(guard.0, curve.as_mut_ptr(), curve.len()) };
        native_result(code, &error).expect("LibRaw curve");
        curve
    }

    /// Decode `path` with LibRaw (through `RawSource::decode`) and RawSpeed, and compare every
    /// sample: `curve[rawspeed] == libraw` always, and `rawspeed == libraw` directly when
    /// `direct`. Returns (samples, samples LibRaw's curve changed).
    fn compare(path: &str, sha256: &str, direct: bool) -> (usize, usize) {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            sha256,
            "{path} hash"
        );
        let cancel = AtomicBool::new(false);
        let libraw = RawSource::decode(&bytes, &cancel).expect("LibRaw decode");
        let (width, height) = (
            libraw.metadata().sensor_width,
            libraw.metadata().sensor_height,
        );
        let curve = libraw_curve(&bytes);
        let mut rawspeed = vec![0u16; libraw.mosaic().len()];
        decode_mosaic(&bytes, width, height, &mut rawspeed).expect("RawSpeed decode");
        let mut curved = 0;
        let mut first_mismatch = None;
        let mut mismatches = 0usize;
        for (index, (&raw, &expected)) in rawspeed.iter().zip(libraw.mosaic()).enumerate() {
            let mapped = curve[usize::from(raw)];
            curved += usize::from(mapped != raw);
            if mapped != expected || (direct && raw != expected) {
                mismatches += 1;
                first_mismatch.get_or_insert((index, raw, mapped, expected));
            }
        }
        assert_eq!(
            (mismatches, first_mismatch),
            (0, None),
            "{path}: (index, RawSpeed, curve[RawSpeed], LibRaw) of the first differing sample"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            sha256,
            "{path} unchanged by both decodes"
        );
        let bent = curve
            .iter()
            .enumerate()
            .filter(|&(value, &mapped)| usize::from(mapped) != value)
            .count();
        println!(
            "{path}: {width}x{height}, {} samples equal after the curve rule, {curved} of them \
             changed by LibRaw's curve ({bent} non-identity curve entries)",
            rawspeed.len()
        );
        (rawspeed.len(), curved)
    }

    /// Run with `LUXFORGE_RAW_OWNER_DIR` (holding `nikon_z6.NEF`) and `LUXFORGE_RAW_POPULAR_DIR`
    /// (the CC0 popular-camera corpus, files named `<raw.pixls.us id>.<EXT>`, holding `4161.NEF`, a
    /// Nikon Z 6II lossy NEF, and `1625.CR2`, a Canon EOS 6D Mark II):
    ///
    /// ```sh
    /// LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_POPULAR_DIR=/path/to/popular \
    ///   cargo test --release -p luxforge-raw --locked --lib rawspeed -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "authentic RawSpeed/LibRaw mosaic comparison; needs owner and popular-camera sources"]
    fn rawspeed_uncorrected_mosaic_matches_libraw_after_its_curve() {
        let owner = std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner RAW fixture directory");
        let popular =
            std::env::var("LUXFORGE_RAW_POPULAR_DIR").expect("popular-camera RAW directory");
        // Nikon Z 6 lossless compressed 14-bit NEF: LibRaw's nikon_load_raw applies its curve.
        let (nef, _) = compare(
            &format!("{owner}/nikon_z6.NEF"),
            "e4db4e1f152110da0a3feb77a4b666c9de4e005509c4c443d15a2d8071bd49fb",
            false,
        );
        assert_eq!(nef, 6064 * 4040);
        // A size other than the decoded one fails and reports what RawSpeed produced.
        let bytes = std::fs::read(format!("{owner}/nikon_z6.NEF")).expect("read Z6");
        let mut short = vec![0u16; 6064 * 4039];
        let failure = decode_mosaic(&bytes, 6064, 4039, &mut short).expect_err("wrong size");
        assert!(
            matches!(&failure, RawError::Native(text)
                if text.contains("differs from the expected size")
                    && text.contains("decoded 6064x4040 with 1 component(s) of u16")),
            "{failure:?}"
        );
        // Bytes RawSpeed cannot parse are an explicit RawSpeed error.
        let mut mosaic = vec![0u16; 16];
        let failure = decode_mosaic(&[0; 64], 4, 4, &mut mosaic).expect_err("not a RAW file");
        assert!(
            matches!(&failure, RawError::Native(text)
                if text.starts_with("RawSpeed: ") && text.contains("nothing decoded")),
            "{failure:?}"
        );
        // Nikon Z 6II lossy (type 2) 14-bit NEF: the curve moves decoded values.
        let (lossy, lossy_curved) = compare(
            &format!("{popular}/4161.NEF"),
            "c79fa1f8986ba059771f125af283dc3b0ed8bd27c46db9e4f83d2a0e3fb34059",
            false,
        );
        assert_eq!(lossy, 6064 * 4040);
        assert!(
            lossy_curved > 0,
            "the lossy NEF curve changes decoded values"
        );
        // Canon EOS 6D Mark II lossless JPEG CR2: LibRaw's curve is the identity here.
        let (cr2, cr2_curved) = compare(
            &format!("{popular}/1625.CR2"),
            "712815b910fa473f7aaeb97c0804292d996b16dd6b974a3ab9a7195a2e41d90c",
            true,
        );
        assert!(cr2 > 0);
        assert_eq!(cr2_curved, 0);
    }
}
