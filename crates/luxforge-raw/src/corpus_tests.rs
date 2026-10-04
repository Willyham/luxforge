//! Opt-in discovery of authentic corpus files before production admission.
//!
//! This uses the existing test-only native open and never admits an unknown
//! camera into `RawSource::decode`. Its LibRaw-only unpack hashes are the
//! independent references used when reviewing new catalog entries.
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    File::open(path)
        .unwrap()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= limit, "bounded discovery input");
    bytes
}

fn identity(identity: &NativeIdentity) -> Value {
    json!({
        "make": c_text(&identity.make), "model": c_text(&identity.model),
        "decoder": c_text(&identity.decoder), "width": identity.width,
        "height": identity.height, "bits": identity.raw_bps,
        "raw_count": identity.raw_count, "channels": identity.channels, "dng_version": identity.dng_version,
        "decoder_flags": identity.decoder_flags,
        "cfa_size": [identity.cfa_width, identity.cfa_height],
        "cfa": &identity.cfa[..(identity.cfa_width * identity.cfa_height) as usize],
    })
}

fn native(native: &NativeMetadata) -> Value {
    json!({
        "make": c_text(&native.make), "model": c_text(&native.model),
        "decoder": c_text(&native.decoder), "width": native.width,
        "height": native.height, "bits": native.raw_bps,
        "raw_count": native.raw_count, "channels": native.channels, "dng_version": native.dng_version,
        "decoder_flags": native.decoder_flags,
        "cfa_size": [native.cfa_width, native.cfa_height],
        "cfa": &native.cfa[..(native.cfa_width * native.cfa_height) as usize],
        "active_area": {"x": native.active_x, "y": native.active_y,
                        "width": native.active_width, "height": native.active_height},
        "inset": {"x": native.inset_x, "y": native.inset_y,
                  "width": native.inset_width, "height": native.inset_height},
        "black_base": native.black_base, "black_channels": native.black_channels,
        "black_repeat_size": [native.black_repeat_width, native.black_repeat_height],
        "black_repeat": &native.black_repeat[..(native.black_repeat_width * native.black_repeat_height) as usize],
        "white": native.white, "as_shot": native.as_shot,
        "rgb_cam": native.rgb_cam, "cam_xyz": native.cam_xyz,
        "flip": native.flip,
    })
}

#[test]
#[ignore = "requires explicit verified-source manifest and new discovery output"]
fn discover_profiles() {
    let manifest_path = std::env::var("LUXFORGE_RAW_DISCOVERY_MANIFEST").unwrap();
    let output_path = std::env::var("LUXFORGE_RAW_DISCOVERY_OUTPUT").unwrap();
    let manifest: Value =
        serde_json::from_slice(&read(Path::new(&manifest_path), 1024 * 1024)).unwrap();
    let samples = manifest["samples"].as_array().unwrap();
    assert!((1..=128).contains(&samples.len()));
    let mut output = File::create_new(output_path).unwrap();
    let mut results = Vec::new();
    for sample in samples {
        let path = Path::new(sample["path"].as_str().unwrap());
        assert!(path.is_absolute());
        let bytes = read(path, MAX_SOURCE_BYTES);
        let before = hash(&bytes);
        assert_eq!(before, sample["sha256"].as_str().unwrap());
        let cancel = AtomicBool::new(false);
        let mut result = json!({"id": sample["id"], "source_sha256": before, "status": "failed"});
        let discovered = (|| -> Result<(), RawError> {
            format::reject_nikon_high_efficiency(&bytes)?;
            let (mut handle, identified) = NativeHandle::open_uncatalogued(&bytes, &cancel)?;
            result["identity"] = identity(&identified);
            result["nef_compression"] = json!(format::nef_compression(&bytes));
            result["raf_compression"] = json!(format::raf_compression(&bytes));
            result["raf_crop"] = json!(format::raf_default_crop(&bytes));
            if identified.dng_version != 0 {
                result["dng_opcodes"] = json!(
                    format::dng_opcodes(&bytes)?
                        .iter()
                        .map(|op| {
                            json!({"id": op.id, "list": op.list, "version": op.version,
                           "flags": op.flags, "ifd": op.ifd, "bytes": op.data.len()})
                        })
                        .collect::<Vec<_>>()
                );
            }
            let unpacker = if c_text(&identified.decoder) == "jxl_dng_load_raw_placeholder()" {
                NativeUnpacker::JxlOxide
            } else {
                NativeUnpacker::Libraw
            };
            let unpacked = handle.unpack(unpacker, &cancel)?;
            result["native"] = native(&unpacked);
            result["identity_unchanged"] = json!(identified.unchanged_in(&unpacked).is_ok());
            let n = RawSource::checked_len(unpacked.width, unpacked.height)?
                * unpacked.channels as usize;
            let mut mosaic = vec![0_u16; n];
            if unpacker == NativeUnpacker::JxlOxide {
                jxl::decode_into(&bytes, &unpacked, &mut mosaic, &cancel)?;
            } else {
                handle.copy(&mut mosaic)?;
            }
            let mut digest = Sha256::new();
            for sample in mosaic {
                digest.update(sample.to_le_bytes());
            }
            result["mosaic_sha256"] = json!(format!("{:x}", digest.finalize()));
            result["status"] = json!("passed");
            Ok(())
        })();
        if let Err(error) = discovered {
            result["error"] = json!(error.to_string());
        }
        result["source_sha256_after"] = json!(hash(&read(path, MAX_SOURCE_BYTES)));
        assert_eq!(result["source_sha256"], result["source_sha256_after"]);
        results.push(result);
    }
    serde_json::to_writer_pretty(
        &mut output,
        &json!({
            "scope": "test-only LibRaw identity/unpack discovery; not production support",
            "results": results,
        }),
    )
    .unwrap();
    output.write_all(b"\n").unwrap();
}
