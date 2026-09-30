//! The crate-private boundary to the vendored RawSpeed ([RawSpeed unpacking](../../../docs/design/rawspeed-unpack.md)).
//!
//! RawSpeed decodes a borrowed encoded buffer into one uncropped, uncorrected u16 sensor mosaic
//! of a size the caller already knows. Its camera data is the pinned `cameras.xml` compiled into
//! the native library, parsed once per process on first use. A catalog mode whose `unpacker` is
//! `rawspeed` is decoded by the native LibRaw adapter, which calls the same native decode from
//! inside LibRaw's unpack ([`crate::NativeUnpacker::Rawspeed`]); this module's direct decode
//! serves the crate's comparison tests.

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
    expect(
        dead_code,
        reason = "routed modes decode inside LibRaw's unpack; this direct decode serves tests"
    )
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

/// The native RawSpeed adapter's per-thread test observability: each decode runs synchronously
/// on its caller's thread, so a test sees only its own.
#[cfg(test)]
pub(crate) mod test_hooks {
    unsafe extern "C" {
        fn lf_rawspeed_decode_calls() -> std::ffi::c_ulonglong;
        fn lf_rawspeed_fail_next_decode();
    }

    /// How many RawSpeed `decodeRaw` calls this thread has started.
    pub(crate) fn decode_calls() -> u64 {
        // SAFETY: reads this thread's counter; no arguments.
        unsafe { lf_rawspeed_decode_calls() }
    }

    /// Make this thread's next RawSpeed decode fail, with a RawSpeed decoder exception thrown
    /// in place of `decodeRaw`.
    pub(crate) fn fail_next_decode() {
        // SAFETY: sets this thread's flag; no arguments.
        unsafe { lf_rawspeed_fail_next_decode() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeHandle, NativeUnpacker, RawMetadata, RawSource, native_result};
    use sha2::{Digest, Sha256};
    use std::{
        ffi::c_void,
        sync::{Arc, Barrier, atomic::AtomicBool},
    };

    unsafe extern "C" {
        fn lf_raw_curve(handle: *mut c_void, dest: *mut u16, length: usize) -> c_int;
        fn lf_rawspeed_log_probe() -> c_int;
    }

    /// The build leaves out upstream's `common/Common.cpp` because it defines only `writeLog`,
    /// which prints to standard output. Pinned so an update that adds anything to it is reviewed.
    #[test]
    fn excluded_rawspeed_source_defines_only_the_stdout_log() {
        let source = include_str!("../vendor/rawspeed-c835b05a/src/librawspeed/common/Common.cpp");
        assert_eq!(
            format!("{:x}", Sha256::digest(source.as_bytes())),
            "2d19717ba6f9d7b3f2c558ae77fc57989adb99237ac19100d5c99af0382964e3"
        );
        assert_eq!(
            source.matches("void writeLog(").count(),
            2,
            "one per build mode"
        );
        assert!(source.contains("fprintf(stdout"));
    }

    /// A warning from RawSpeed's own code (here `RawImageData::subFrame`) goes to standard error
    /// through the adapter's `writeLog`, and nothing reaches standard output, which the
    /// `luxforge-json` CLI owns. The probe runs in a child process of this test binary so its
    /// streams can be read whole.
    #[test]
    fn rawspeed_warnings_write_nothing_to_standard_output() {
        if std::env::var_os(CHILD).is_some() {
            // SAFETY: no arguments; the probe allocates and frees a 4 x 4 RawSpeed image.
            assert_eq!(
                unsafe { lf_rawspeed_log_probe() },
                NativeStatus::Ok as c_int
            );
            return;
        }
        let (stdout, stderr) =
            in_child("rawspeed::tests::rawspeed_warnings_write_nothing_to_standard_output");
        assert!(!stdout.contains("RawSpeed"), "stdout: {stdout}");
        assert!(
            stderr.contains(
                "RawSpeed:WARNING: RawImageData::subFrame - Attempted to create new subframe \
                 larger than original size. Crop skipped.\n"
            ),
            "stderr: {stderr}"
        );
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

    /// Set in a child process of this test binary that runs one test alone.
    const CHILD: &str = "LUXFORGE_RAWSPEED_TEST_CHILD";

    /// Run this binary's test `name` alone in a child process with [`CHILD`] set, so the test
    /// is its process's first RawSpeed user and its output streams can be read whole. Fails
    /// unless the child's test passed.
    fn in_child(name: &str) -> (String, String) {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("run the child test");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stdout.contains("1 passed"),
            "the child ran {name}: {stdout}"
        );
        (stdout, stderr)
    }

    /// Other tests decode with RawSpeed, so the concurrent first use runs in a child process,
    /// where these threads are the camera data's first users.
    #[test]
    fn rawspeed_camera_data_is_parsed_once_under_concurrent_first_use() {
        if std::env::var_os(CHILD).is_none() {
            in_child(
                "rawspeed::tests::rawspeed_camera_data_is_parsed_once_under_concurrent_first_use",
            );
            return;
        }
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
        let cancel = AtomicBool::new(false);
        let (mut handle, _) = NativeHandle::open_uncatalogued(bytes, &cancel).expect("LibRaw open");
        handle
            .unpack(NativeUnpacker::Libraw, &cancel)
            .expect("LibRaw unpack");
        let mut curve = vec![0u16; 0x10000];
        let error = [0 as c_char; 256];
        // SAFETY: the guard holds the live, unpacked handle; curve has the table's 65536 slots.
        let code = unsafe { lf_raw_curve(handle.handle, curve.as_mut_ptr(), curve.len()) };
        native_result(code, &error).expect("LibRaw curve");
        curve
    }

    /// Decode `path` with LibRaw (through `RawSource::decode_forcing`) and RawSpeed, and compare
    /// every sample: `curve[rawspeed] == libraw` always, and `rawspeed == libraw` directly when
    /// `direct`. Returns (samples, samples LibRaw's curve changed).
    fn compare(path: &str, sha256: &str, direct: bool) -> (usize, usize) {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            sha256,
            "{path} hash"
        );
        let cancel = AtomicBool::new(false);
        let libraw = RawSource::decode_forcing(&bytes[..], &cancel, NativeUnpacker::Libraw)
            .expect("LibRaw decode");
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

    /// One authentic file routed through RawSpeed and compared with LibRaw.
    struct Routed {
        name: String,
        decoder: String,
        curve: crate::unpacker::Curve,
        /// Samples whose written value differs from RawSpeed's uncorrected value: the curve
        /// rule at work.
        curved: usize,
        /// Entries of LibRaw's `curve` that are not the identity. Where the rule is `Unchanged`
        /// and this is not zero, the equal mosaic shows LibRaw's decoder does not apply it.
        bent: usize,
        mosaic_sha256: String,
        /// Where the LibRaw-only mosaic hash this equals is recorded, if anywhere.
        recorded: Option<&'static str>,
        /// The classified catalog mode, when the catalog holds it.
        mode: Option<String>,
    }

    /// Route `path` through RawSpeed and require LibRaw's result: the identical mosaic and every
    /// native metadata field (opened without the catalog check, so any camera compares), the
    /// recorded LibRaw-only hash where the evidence manifest lists the source or `pinned` gives
    /// one, and, when the catalog holds the mode, the identical `RawSource` (metadata and
    /// mosaic) with a finite development.
    fn route(path: &str, evidence: &serde_json::Value, pinned: Option<&str>) -> Routed {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let source = format!("{:x}", Sha256::digest(&bytes));
        let cancel = AtomicBool::new(false);
        let (libraw, libraw_mosaic) =
            crate::tests::native_unpack(&bytes, false, NativeUnpacker::Libraw, &cancel)
                .unwrap_or_else(|e| panic!("{path}: LibRaw: {e}"));
        let decodes = test_hooks::decode_calls();
        let (routed, routed_mosaic) =
            crate::tests::native_unpack(&bytes, false, NativeUnpacker::Rawspeed, &cancel)
                .unwrap_or_else(|e| panic!("{path}: RawSpeed: {e}"));
        assert_eq!(test_hooks::decode_calls(), decodes + 1, "{path}");
        let first = routed_mosaic
            .iter()
            .zip(&libraw_mosaic)
            .position(|(routed, libraw)| routed != libraw);
        assert_eq!(first, None, "{path}: first differing sample");
        assert_eq!(
            crate::tests::metadata_bytes(&routed),
            crate::tests::metadata_bytes(&libraw),
            "{path}: native metadata"
        );
        let decoder = c_text(&routed.decoder);
        let row = crate::unpacker::replaceable(&decoder).expect("a replaceable decoder");
        // How many samples the curve rule changed, from RawSpeed's own uncorrected values.
        let mut uncorrected = vec![0u16; routed_mosaic.len()];
        decode_mosaic(&bytes, routed.width, routed.height, &mut uncorrected)
            .unwrap_or_else(|e| panic!("{path}: direct RawSpeed: {e}"));
        let curved = uncorrected
            .iter()
            .zip(&routed_mosaic)
            .filter(|(raw, written)| raw != written)
            .count();
        if row.curve == crate::unpacker::Curve::Unchanged {
            assert_eq!(curved, 0, "{path}");
        }
        let bent = libraw_curve(&bytes)
            .iter()
            .enumerate()
            .filter(|&(value, &mapped)| usize::from(mapped) != value)
            .count();
        let mosaic_sha256 = format!("{:x}", Sha256::digest(le_bytes(&routed_mosaic)));
        let listed = evidence["entries"]
            .as_array()
            .expect("evidence entries")
            .iter()
            .find(|entry| entry["source_sha256"] == source.as_str())
            .map(|entry| {
                entry["mosaic_sha256"]
                    .as_str()
                    .expect("recorded mosaic hash")
            });
        let recorded = match (listed, pinned) {
            (Some(hash), _) => Some(("evidence manifest", hash)),
            (None, Some(hash)) => Some(("owner pin in tests/real_files.rs", hash)),
            (None, None) => None,
        };
        if let Some((place, hash)) = recorded {
            assert_eq!(mosaic_sha256, hash, "{path}: mosaic hash in the {place}");
        }
        let recorded = recorded.map(|(place, _)| place);
        let mode = match RawSource::decode_forcing(&bytes[..], &cancel, NativeUnpacker::Libraw) {
            Ok(libraw) => {
                let routed =
                    RawSource::decode_forcing(&bytes[..], &cancel, NativeUnpacker::Rawspeed)
                        .unwrap_or_else(|e| panic!("{path}: routed RawSource: {e}"));
                assert_eq!(
                    metadata_differences(libraw.metadata(), routed.metadata()),
                    Vec::<String>::new(),
                    "{path}: RawMetadata"
                );
                assert!(
                    routed.mosaic() == libraw.mosaic(),
                    "{path}: RawSource mosaic"
                );
                assert_eq!(routed.mosaic(), &routed_mosaic[..], "{path}");
                let rgb = routed
                    .develop(routed.metadata().as_shot_gains, &cancel)
                    .unwrap_or_else(|e| panic!("{path}: develop: {e}"));
                assert!(rgb.data.iter().all(|value| value.is_finite()), "{path}");
                Some(routed.metadata().mode.id().to_owned())
            }
            Err(error) => {
                println!("{path}: not a catalog mode ({error}); native comparison only");
                None
            }
        };
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            source,
            "{path} unchanged"
        );
        Routed {
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            decoder,
            curve: row.curve,
            curved,
            bent,
            mosaic_sha256,
            recorded,
            mode,
        }
    }

    /// The fields in which `routed` differs from `libraw`, other than `backend`, which names the
    /// unpacker and must: LibRaw's for `libraw`, RawSpeed's for `routed`.
    fn metadata_differences(libraw: &RawMetadata, routed: &RawMetadata) -> Vec<String> {
        assert_eq!(libraw.backend, crate::LIBRAW_PROVIDER);
        assert_eq!(routed.backend, crate::RAWSPEED_PROVIDER);
        let fields = |metadata: &RawMetadata| {
            let mut value = serde_json::to_value(metadata).expect("metadata JSON");
            value
                .as_object_mut()
                .expect("metadata object")
                .remove("backend");
            value
        };
        let (libraw, routed) = (fields(libraw), fields(routed));
        let names: std::collections::BTreeSet<&String> = libraw
            .as_object()
            .into_iter()
            .chain(routed.as_object())
            .flat_map(|object| object.keys())
            .collect();
        names
            .into_iter()
            .filter(|name| libraw.get(name.as_str()) != routed.get(name.as_str()))
            .cloned()
            .collect()
    }

    /// The little-endian bytes of a mosaic, as the evidence manifest hashes it.
    fn le_bytes(samples: &[u16]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    /// Every replaceable LibRaw decoder on authentic files, routed through RawSpeed and compared
    /// with LibRaw. Run with `LUXFORGE_RAW_OWNER_DIR` (`nikon_z6.NEF`, `mavic_air_2s.DNG`),
    /// `LUXFORGE_RAW_POPULAR_DIR` and `LUXFORGE_RAW_CORPUS_DIR` (raw.pixls.us samples named
    /// `<id>.<EXT>`):
    ///
    /// ```sh
    /// LUXFORGE_RAW_OWNER_DIR=/path/to/owner/raw LUXFORGE_RAW_POPULAR_DIR=/path/to/popular \
    ///   LUXFORGE_RAW_CORPUS_DIR=/path/to/corpus cargo test --release -p luxforge-raw --locked \
    ///   --lib rawspeed_unpacker_matches_libraw -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "authentic RawSpeed-routed unpack comparison; needs owner, popular and corpus sources"]
    fn rawspeed_unpacker_matches_libraw_on_authentic_files() {
        let dir = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
        let (owner, popular, corpus) = (
            dir("LUXFORGE_RAW_OWNER_DIR"),
            dir("LUXFORGE_RAW_POPULAR_DIR"),
            dir("LUXFORGE_RAW_CORPUS_DIR"),
        );
        let evidence: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/modern-camera-evidence.json"
            ))
            .expect("evidence manifest"),
        )
        .expect("evidence JSON");
        // The owner Z6's LibRaw mosaic hash is pinned by the owner-mode test in real_files.rs;
        // the evidence manifest records the others it lists.
        let z6 = "86c76c382dd4273e619a2dcc177a27b15c9e9b2d1187639c54d4d4c1336e8bfc";
        let files = [
            (owner.as_str(), "nikon_z6.NEF", Some(z6)),
            (owner.as_str(), "mavic_air_2s.DNG", None),
            (popular.as_str(), "4161.NEF", None),
            (popular.as_str(), "1840.NEF", None),
            (popular.as_str(), "1625.CR2", None),
            (popular.as_str(), "983.CR2", None),
            (popular.as_str(), "5283.ORF", None),
            (popular.as_str(), "3115.DNG", None),
            (popular.as_str(), "7790.RW2", None),
            (corpus.as_str(), "6122.RAF", None),
            (corpus.as_str(), "4677.PEF", None),
            (corpus.as_str(), "4096.RW2", None),
            (corpus.as_str(), "5967.RW2", None),
            (corpus.as_str(), "3204.DNG", None),
            (corpus.as_str(), "7853.DNG", None),
        ];
        let mut decoders = std::collections::BTreeSet::new();
        println!(
            "| file | decoder | curve rule | non-identity curve entries | samples the rule changed \
             | mosaic SHA-256 | recorded LibRaw hash | catalog mode |"
        );
        for (dir, name, pinned) in files {
            let routed = route(&format!("{dir}/{name}"), &evidence, pinned);
            println!(
                "| {} | {} | {:?} | {} | {} | {} | {} | {} |",
                routed.name,
                routed.decoder,
                routed.curve,
                routed.bent,
                routed.curved,
                routed.mosaic_sha256,
                routed
                    .recorded
                    .map_or("not recorded".to_owned(), |place| format!(
                        "equal ({place})"
                    )),
                routed.mode.as_deref().unwrap_or("not catalogued"),
            );
            decoders.insert(routed.decoder);
        }
        let covered: Vec<_> = crate::unpacker::REPLACEABLE
            .iter()
            .map(|row| row.decoder)
            .filter(|decoder| decoders.contains(*decoder))
            .collect();
        assert_eq!(
            covered.len(),
            crate::unpacker::REPLACEABLE.len(),
            "{covered:?}"
        );
        // A RawSpeed failure on a routed decode is an explicit error naming RawSpeed, with no
        // LibRaw fallback and no handle left open.
        let bytes = std::fs::read(format!("{owner}/nikon_z6.NEF")).expect("read Z6");
        let cancel = AtomicBool::new(false);
        test_hooks::fail_next_decode();
        let failure = RawSource::decode_forcing(&bytes[..], &cancel, NativeUnpacker::Rawspeed)
            .expect_err("forced RawSpeed failure");
        println!("forced failure: {failure}");
        assert!(
            matches!(&failure, RawError::Native(text) if text.starts_with("RawSpeed: ")
                && text.contains("injected test failure")
                && text.ends_with("(nothing decoded; expected 6064x4040 u16)")),
            "{failure:?}"
        );
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            RawSource::decode_forcing(&bytes[..], &cancelled, NativeUnpacker::Rawspeed).map(|_| ()),
            Err(RawError::Cancelled)
        );
    }

    /// The LibRaw-only mosaic hashes of the authentic samples of the catalog modes the evidence
    /// manifest has no entry for, as the authentic tests in `tests/real_files.rs` pin them:
    /// (sample, source SHA-256, mosaic SHA-256).
    const PINNED_MOSAICS: [(&str, &str, &str); 4] = [
        (
            "owner nikon_z6.NEF",
            "e4db4e1f152110da0a3feb77a4b666c9de4e005509c4c443d15a2d8071bd49fb",
            "86c76c382dd4273e619a2dcc177a27b15c9e9b2d1187639c54d4d4c1336e8bfc",
        ),
        (
            "z6-12-lossless.NEF (raw.pixls.us 3585)",
            "59615b65f8a7edc92a845b2a9c8ef313d6e7d7a5d842f94e6adf786da045a6db",
            "f25f0aafd76a99f5f20cbe2a001f4620397be43de829010d5dbaf9255ce64424",
        ),
        (
            "z6-14-lossless.NEF (raw.pixls.us 3582)",
            "c079345fc93f53a4f0d322f8ddaae505f920c36015f25b58befccaa61db1af31",
            "9896187fd3e3e29922b5b051a62f24afedbbbb75ddf63b5879a2b28de896116c",
        ),
        (
            "x100vi-lossless.RAF (raw.pixls.us 7301)",
            "e9709b98f4ff96b993dbe4ad20eff0accfa709a58559eb4219f59d1e7181934a",
            "f10be69db8c3731fdcacf3741fd188fcef2557efd5de79f84a22a34adf443283",
        ),
    ];

    /// What a sample's LibRaw-only results must equal, recorded before any mode was routed.
    struct Recorded {
        place: &'static str,
        mosaic: String,
        /// The as-shot and perturbed-white-balance development hashes, where recorded.
        developments: Option<[String; 2]>,
    }

    /// The SHA-256 of a developed image's float planes, as the qualifier and the evidence
    /// manifest hash them.
    fn planes_sha256(image: &crate::PlanarRgb) -> String {
        let mut hash = Sha256::new();
        for chunk in image.data.chunks(4096) {
            let bytes: Vec<u8> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
            hash.update(bytes);
        }
        format!("{:x}", hash.finalize())
    }

    /// The as-shot and perturbed-white-balance developments' hashes, with the qualifier's
    /// perturbation.
    fn development_hashes(raw: &RawSource) -> Result<[String; 2], String> {
        let cancel = AtomicBool::new(false);
        let gains = raw.metadata().as_shot_gains;
        let perturbed = [
            (gains[0] * 1.05).min(32.0),
            1.0,
            (gains[2] * 0.95).max(f32::MIN_POSITIVE),
        ];
        let mut hashes = [String::new(), String::new()];
        for (hash, gains) in hashes.iter_mut().zip([gains, perturbed]) {
            let image = raw
                .develop(gains, &cancel)
                .map_err(|e| format!("develop: {e}"))?;
            if !image.data.iter().all(|v| v.is_finite()) {
                return Err("development is not finite".into());
            }
            *hash = planes_sha256(&image);
        }
        Ok(hashes)
    }

    /// Route one authentic sample of a catalog mode on a replaceable decoder through RawSpeed and
    /// compare it with its LibRaw-only decode and with what was recorded for it. `Err` names the
    /// first difference.
    fn check_routed_sample(
        bytes: &[u8],
        libraw: &RawSource,
        recorded: Option<&Recorded>,
    ) -> Result<(), String> {
        let cancel = AtomicBool::new(false);
        let libraw_mosaic = format!("{:x}", Sha256::digest(le_bytes(libraw.mosaic())));
        if let Some(recorded) = recorded
            && libraw_mosaic != recorded.mosaic
        {
            return Err(format!(
                "the LibRaw-only mosaic {libraw_mosaic} is not the one recorded in the {}",
                recorded.place
            ));
        }
        let routed = RawSource::decode_forcing(bytes, &cancel, NativeUnpacker::Rawspeed)
            .map_err(|e| format!("routed decode refused: {e}"))?;
        if routed.mosaic() != libraw.mosaic() {
            let pairs = || routed.mosaic().iter().zip(libraw.mosaic());
            let differing = pairs().filter(|(a, b)| a != b).count();
            let first = pairs().position(|(a, b)| a != b).unwrap_or(0);
            let width = libraw.metadata().sensor_width as usize;
            return Err(format!(
                "mosaic differs in {differing} of {} samples; first at (x {}, y {}): RawSpeed {}, \
                 LibRaw {}",
                libraw.mosaic().len(),
                first % width,
                first / width,
                routed.mosaic()[first],
                libraw.mosaic()[first],
            ));
        }
        let fields = metadata_differences(libraw.metadata(), routed.metadata());
        if !fields.is_empty() {
            return Err(format!("metadata differs in {fields:?}"));
        }
        let libraw_developments = development_hashes(libraw)?;
        let routed_developments = development_hashes(&routed)?;
        if routed_developments != libraw_developments {
            return Err(format!(
                "developments differ: RawSpeed {routed_developments:?}, LibRaw \
                 {libraw_developments:?}"
            ));
        }
        if let Some(Recorded {
            place,
            developments: Some(developments),
            ..
        }) = recorded
            && &libraw_developments != developments
        {
            return Err(format!(
                "developments {libraw_developments:?} are not the ones recorded in the {place}"
            ));
        }
        // The catalog's own decode names the unpacker its mode routes to, and fills the same
        // mosaic.
        let catalogued =
            RawSource::decode(bytes, &cancel).map_err(|e| format!("catalog decode: {e}"))?;
        let expected = match libraw.metadata().mode.0.unpacker {
            crate::unpacker::Unpacker::Libraw => crate::LIBRAW_PROVIDER,
            crate::unpacker::Unpacker::Rawspeed => crate::RAWSPEED_PROVIDER,
        };
        if catalogued.metadata().backend != expected || catalogued.mosaic() != libraw.mosaic() {
            return Err(format!(
                "the catalog decode ({}) differs",
                catalogued.metadata().backend
            ));
        }
        Ok(())
    }

    /// Every catalog mode whose LibRaw decoder RawSpeed may replace, routed through RawSpeed on
    /// every local authentic sample of it and compared with LibRaw: the identical mosaic, every
    /// metadata field but `backend` identical, identical as-shot and perturbed-white-balance
    /// developments, and each LibRaw-only result equal to what was recorded before routing (the
    /// evidence manifest's mosaic and development hashes, or the owner and public-fixture pins).
    /// A mode the catalog routes must pass on every sample found, with every evidence sample of
    /// it found. A candidate that fails is printed with the reason and must stay on LibRaw.
    ///
    /// `LUXFORGE_RAW_SAMPLE_DIRS` is a path list of directories of authentic RAW files, each file
    /// read once whatever its name; files outside the catalog are skipped:
    ///
    /// ```sh
    /// LUXFORGE_RAW_SAMPLE_DIRS=/selection:/popular:/corpus:/owner cargo test --release \
    ///   -p luxforge-raw --locked --lib replaceable_catalog_modes -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "authentic routed-mode exactness over local sample directories"]
    fn replaceable_catalog_modes_match_libraw_on_every_local_sample() {
        let dirs = std::env::var_os("LUXFORGE_RAW_SAMPLE_DIRS").expect("LUXFORGE_RAW_SAMPLE_DIRS");
        let evidence: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/modern-camera-evidence.json"
            ))
            .expect("evidence manifest"),
        )
        .expect("evidence JSON");
        let entries = evidence["entries"].as_array().expect("evidence entries");
        let recorded_for = |source: &str| -> Option<Recorded> {
            let text = |value: &serde_json::Value| value.as_str().expect("hash").to_owned();
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry["source_sha256"] == source)
            {
                return Some(Recorded {
                    place: "evidence manifest",
                    mosaic: text(&entry["mosaic_sha256"]),
                    developments: Some([
                        text(&entry["as_shot"]["sha256"]),
                        text(&entry["perturbed_wb"]["sha256"]),
                    ]),
                });
            }
            PINNED_MOSAICS
                .iter()
                .find(|(_, pinned, _)| *pinned == source)
                .map(|(_, _, mosaic)| Recorded {
                    place: "real_files.rs pins",
                    mosaic: (*mosaic).to_owned(),
                    developments: None,
                })
        };
        let mut files = Vec::new();
        for dir in std::env::split_paths(&dirs) {
            let mut listed: Vec<_> = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .map(|entry| entry.expect("directory entry").path())
                .filter(|path| {
                    std::fs::metadata(path).is_ok_and(|m| m.is_file())
                        && path.extension().is_some_and(|ext| {
                            [
                                "nef", "cr2", "cr3", "raf", "orf", "pef", "dng", "rw2", "arw",
                            ]
                            .contains(&ext.to_string_lossy().to_ascii_lowercase().as_str())
                        })
                })
                .collect();
            listed.sort();
            files.extend(listed);
        }
        /// One mode's decoder, catalog unpacker and checked samples (name, record, outcome).
        type Checked = (
            &'static str,
            crate::unpacker::Unpacker,
            Vec<(String, Option<&'static str>, Result<(), String>)>,
        );
        let mut modes: std::collections::BTreeMap<&'static str, Checked> =
            std::collections::BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        let cancel = AtomicBool::new(false);
        for path in files {
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let source = format!("{:x}", Sha256::digest(&bytes));
            if !seen.insert(source.clone()) {
                continue;
            }
            let name = path.display().to_string();
            let libraw =
                match RawSource::decode_forcing(&bytes[..], &cancel, NativeUnpacker::Libraw) {
                    Ok(libraw) => libraw,
                    Err(error) => {
                        println!("{name}: not a catalog mode ({error})");
                        continue;
                    }
                };
            let mode = libraw.metadata().mode.0;
            if crate::unpacker::replaceable(&mode.decoder).is_none() {
                continue;
            }
            let recorded = recorded_for(&source);
            let outcome = check_routed_sample(&bytes, &libraw, recorded.as_ref());
            drop(libraw);
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(&path).expect("reread"))
                ),
                source,
                "{name} unchanged"
            );
            println!(
                "{name}: {} ({}): {}",
                mode.id,
                recorded.as_ref().map_or("no record", |r| r.place),
                match &outcome {
                    Ok(()) => "exact",
                    Err(reason) => reason,
                }
            );
            modes
                .entry(&mode.id)
                .or_insert((&mode.decoder, mode.unpacker, Vec::new()))
                .2
                .push((name, recorded.map(|r| r.place), outcome));
        }
        println!("| mode | decoder | catalog unpacker | samples | result |");
        let mut failures = Vec::new();
        for (mode, (decoder, unpacker, samples)) in &modes {
            let reasons: Vec<_> = samples
                .iter()
                .filter_map(|(name, _, outcome)| {
                    let file = name.rsplit('/').next().unwrap_or(name);
                    outcome
                        .as_ref()
                        .err()
                        .map(|reason| format!("{file}: {reason}"))
                })
                .collect();
            println!(
                "| {mode} | {decoder} | {unpacker:?} | {} | {} |",
                samples
                    .iter()
                    .map(|(name, place, _)| format!(
                        "{} ({})",
                        name.rsplit('/').next().unwrap_or(name),
                        place.unwrap_or("no record")
                    ))
                    .collect::<Vec<_>>()
                    .join(", "),
                if reasons.is_empty() {
                    "exact".to_owned()
                } else {
                    reasons.join("; ")
                }
            );
            if *unpacker == crate::unpacker::Unpacker::Rawspeed {
                if !reasons.is_empty() {
                    failures.push(format!("{mode} is routed but not exact: {reasons:?}"));
                }
                if samples.iter().all(|(_, place, _)| place.is_none()) {
                    failures.push(format!("{mode} is routed with no recorded LibRaw result"));
                }
            }
        }
        // Every routed mode was found, with every evidence sample of it.
        for mode in crate::camera_catalog()
            .cameras
            .iter()
            .flat_map(|camera| camera.modes.iter())
            .filter(|mode| mode.unpacker == crate::unpacker::Unpacker::Rawspeed)
        {
            let listed = entries
                .iter()
                .filter(|entry| entry["mode"] == mode.id.as_ref())
                .count();
            let checked = modes.get(mode.id.as_ref()).map_or(0, |(_, _, samples)| {
                samples
                    .iter()
                    .filter(|(_, place, _)| *place == Some("evidence manifest"))
                    .count()
            });
            if !modes.contains_key(mode.id.as_ref()) || checked < listed {
                failures.push(format!(
                    "{} is routed but {checked} of its {listed} evidence samples were found",
                    mode.id
                ));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The one-minute load average, as the timing records carry it; NaN where it is unknown.
    fn load_average() -> f64 {
        #[cfg(unix)]
        {
            let mut load = [0.0_f64; 3];
            // SAFETY: getloadavg writes at most the one element asked for into the array.
            if unsafe { libc::getloadavg(load.as_mut_ptr(), 1) } == 1 {
                return load[0];
            }
        }
        f64::NAN
    }

    /// One decode of `LUXFORGE_RAW_FIXTURE` in this process with the unpacker
    /// `LUXFORGE_RAW_UNPACKER` names (`libraw` or `rawspeed`), for a peak process RSS read from
    /// outside, for example with `/usr/bin/time -l`. Run it alone, one decode per process:
    ///
    /// ```sh
    /// LUXFORGE_RAW_FIXTURE=/path/to/1840.NEF LUXFORGE_RAW_UNPACKER=rawspeed /usr/bin/time -l \
    ///   target/release/deps/luxforge_raw-HASH --ignored --exact \
    ///   rawspeed::tests::one_decode_for_peak_rss --test-threads 1
    /// ```
    #[test]
    #[ignore = "a measurement, not a gate; one decode per process"]
    fn one_decode_for_peak_rss() {
        let path = std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE");
        let unpacker = match std::env::var("LUXFORGE_RAW_UNPACKER").as_deref() {
            Ok("libraw") => NativeUnpacker::Libraw,
            Ok("rawspeed") => NativeUnpacker::Rawspeed,
            other => panic!("LUXFORGE_RAW_UNPACKER must be libraw or rawspeed, not {other:?}"),
        };
        let bytes = std::fs::read(&path).expect("read sample");
        let cancel = AtomicBool::new(false);
        let source = RawSource::decode_forcing(&bytes[..], &cancel, unpacker).expect("decode");
        println!(
            "{} {}x{} {}",
            source.metadata().mode.0.id,
            source.metadata().sensor_width,
            source.metadata().sensor_height,
            source.metadata().backend
        );
    }

    /// Unpack time per RawSpeed-routed catalog mode, LibRaw forced against the mode's RawSpeed
    /// unpacker, in one release process: identify, classification, unpack, interpretation and
    /// the copy of the mosaic out of the handle (`RawSource::decode`), with the encoded bytes
    /// already in memory. Reading the file and dropping the decoded source are outside the clock;
    /// nothing is developed. Each sample is first decoded once by each unpacker, which warms both
    /// (the first routed unpack parses RawSpeed's camera data) and checks their mosaics are
    /// equal; then `LUXFORGE_RAW_SAMPLES` observations per unpacker (default 16, rounded up to
    /// even) are taken in LibRaw, RawSpeed, RawSpeed, LibRaw blocks. A measurement, not a gate.
    ///
    /// Samples are read from `LUXFORGE_RAW_SAMPLE_DIRS`, as the routing exactness test reads
    /// them. Every mode the catalog routes is timed, or with `LUXFORGE_RAW_TIMING_MODES` (a
    /// comma list of mode ids) only those modes, whatever their unpacker, provided RawSpeed may
    /// replace their decoder. `LUXFORGE_RAW_TIMING_OUTPUT` names a CSV file that receives every
    /// observation. One JSON line per mode is printed, with each sample's nearest-rank p50 and
    /// p95 in milliseconds per unpacker, their ratio (LibRaw p50 over RawSpeed p50) and the
    /// mode's lowest sample ratio:
    ///
    /// ```sh
    /// LUXFORGE_RAW_SAMPLE_DIRS=/selection:/popular:/corpus:/owner \
    ///   LUXFORGE_RAW_TIMING_OUTPUT=target/unpack.csv cargo test --release -p luxforge-raw \
    ///   --locked --lib rawspeed_unpack_timing -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a measurement, not a gate; run alone in release"]
    fn rawspeed_unpack_timing() {
        use std::io::Write;
        use std::time::Instant;
        let dirs = std::env::var_os("LUXFORGE_RAW_SAMPLE_DIRS").expect("LUXFORGE_RAW_SAMPLE_DIRS");
        let output = std::env::var("LUXFORGE_RAW_TIMING_OUTPUT").expect("observation CSV path");
        let samples: usize = std::env::var("LUXFORGE_RAW_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(16);
        let blocks = samples.div_ceil(2).max(1);
        let only: Option<Vec<String>> = std::env::var("LUXFORGE_RAW_TIMING_MODES")
            .ok()
            .map(|list| list.split(',').map(|id| id.trim().to_owned()).collect());
        let mut files = Vec::new();
        for dir in std::env::split_paths(&dirs) {
            let mut listed: Vec<_> = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .map(|entry| entry.expect("directory entry").path())
                .filter(|path| {
                    std::fs::metadata(path).is_ok_and(|m| m.is_file())
                        && path.extension().is_some_and(|ext| {
                            ["nef", "cr2", "raf", "orf", "pef", "dng", "rw2"]
                                .contains(&ext.to_string_lossy().to_ascii_lowercase().as_str())
                        })
                })
                .collect();
            listed.sort();
            files.extend(listed);
        }
        let cancel = AtomicBool::new(false);
        // Whether the catalog routes the mode, and its samples (path, bytes).
        type Timed = (bool, Vec<(String, Arc<[u8]>)>);
        let mut modes: std::collections::BTreeMap<String, Timed> = Default::default();
        let mut seen = std::collections::HashSet::new();
        for path in files {
            let bytes: Arc<[u8]> = Arc::from(std::fs::read(&path).expect("read sample"));
            if !seen.insert(format!("{:x}", Sha256::digest(&bytes))) {
                continue;
            }
            let Ok(libraw) = RawSource::decode_forcing(&bytes[..], &cancel, NativeUnpacker::Libraw)
            else {
                continue;
            };
            let mode = libraw.metadata().mode.0;
            let routed = mode.unpacker == crate::unpacker::Unpacker::Rawspeed;
            let selected = match &only {
                Some(ids) => {
                    ids.iter().any(|id| id == mode.id.as_ref())
                        && crate::unpacker::replaceable(&mode.decoder).is_some()
                }
                None => routed,
            };
            if selected {
                modes
                    .entry(mode.id.to_string())
                    .or_insert((routed, Vec::new()))
                    .1
                    .push((path.display().to_string(), bytes));
            }
        }
        let mut csv = std::fs::File::create(&output).expect("create observation CSV");
        writeln!(
            csv,
            "mode,sample,unpacker,block,position,wall_ns,load_before_block"
        )
        .unwrap();
        let decode = |bytes: &[u8], unpacker: NativeUnpacker, routed: bool| {
            let started = Instant::now();
            let source = if routed && unpacker == NativeUnpacker::Rawspeed {
                // The routed mode's own unpacker, as production decodes it.
                RawSource::decode(bytes, &cancel)
            } else {
                RawSource::decode_forcing(bytes, &cancel, unpacker)
            }
            .expect("decode");
            (started.elapsed().as_nanos(), source)
        };
        for (mode, (routed, sources)) in &modes {
            let mut lines = Vec::new();
            let mut lowest = f64::INFINITY;
            for (path, bytes) in sources {
                let file = path.rsplit('/').next().unwrap_or(path).to_owned();
                let (_, libraw) = decode(bytes, NativeUnpacker::Libraw, *routed);
                let (_, rawspeed) = decode(bytes, NativeUnpacker::Rawspeed, *routed);
                assert_eq!(libraw.metadata().backend, crate::LIBRAW_PROVIDER, "{file}");
                assert_eq!(
                    rawspeed.metadata().backend,
                    crate::RAWSPEED_PROVIDER,
                    "{file}"
                );
                assert!(
                    libraw.mosaic() == rawspeed.mosaic(),
                    "{file}: mosaics differ"
                );
                let sensor = [
                    libraw.metadata().sensor_width,
                    libraw.metadata().sensor_height,
                ];
                drop((libraw, rawspeed));
                let load_start = load_average();
                let mut times = [Vec::new(), Vec::new()];
                for block in 0..blocks {
                    let load = load_average();
                    for (position, unpacker) in [
                        NativeUnpacker::Libraw,
                        NativeUnpacker::Rawspeed,
                        NativeUnpacker::Rawspeed,
                        NativeUnpacker::Libraw,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let (ns, source) = decode(bytes, unpacker, *routed);
                        drop(source);
                        let index = usize::from(unpacker == NativeUnpacker::Rawspeed);
                        times[index].push(ns as f64 / 1e6);
                        writeln!(
                            csv,
                            "{mode},{file},{},{block},{position},{ns},{load:.2}",
                            ["libraw", "rawspeed"][index]
                        )
                        .unwrap();
                    }
                }
                csv.flush().unwrap();
                let load_end = load_average();
                let [libraw, rawspeed] = times.map(|values| {
                    luxforge_testbase::Distribution::of(values).expect("observations")
                });
                let ratio = libraw.p50 / rawspeed.p50;
                lowest = lowest.min(ratio);
                lines.push(serde_json::json!({
                    "sample": file,
                    "sensor": sensor,
                    "encoded_bytes": bytes.len(),
                    "observations_per_unpacker": libraw.count,
                    "libraw_p50_p95_ms": [libraw.p50, libraw.p95],
                    "rawspeed_p50_p95_ms": [rawspeed.p50, rawspeed.p95],
                    "libraw_min_max_ms": [libraw.min, libraw.max],
                    "rawspeed_min_max_ms": [rawspeed.min, rawspeed.max],
                    "ratio": ratio,
                    "load_start_end": [load_start, load_end],
                }));
            }
            println!(
                "{}",
                serde_json::json!({
                    "mode": mode,
                    "routed": routed,
                    "lowest_ratio": lowest,
                    "samples": lines,
                })
            );
        }
    }
}
