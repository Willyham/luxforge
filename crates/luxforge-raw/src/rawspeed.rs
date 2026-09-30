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
    use crate::{NativeHandle, NativeUnpacker, RawSource, native_result};
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
        let mode = match RawSource::decode(&bytes[..], &cancel) {
            Ok(libraw) => {
                let routed =
                    RawSource::decode_forcing(&bytes[..], &cancel, NativeUnpacker::Rawspeed)
                        .unwrap_or_else(|e| panic!("{path}: routed RawSource: {e}"));
                assert_eq!(routed.metadata(), libraw.metadata(), "{path}: RawMetadata");
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
}
