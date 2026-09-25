//! Bounded, developer-only functional qualification for authentic RAW files.
//!
//! This intentionally does not perform controlled colour qualification.  It
//! checks that the adapter can preserve the source, decode the selected mode,
//! and develop two finite RGB results from capture metadata.

use luxforge_raw::{RawMetadata, RawSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

const MAX_MANIFEST_BYTES: u64 = 1 << 20;
const MAX_SAMPLES: usize = 128;
const MAX_SOURCE_BYTES: u64 = luxforge_raw::MAX_SOURCE_BYTES as u64;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    samples: Vec<ManifestSample>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestSample {
    id: String,
    path: PathBuf,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct Report {
    manifest: String,
    scope: &'static str,
    results: Vec<SampleResult>,
    failed: usize,
}

#[derive(Debug, Serialize)]
struct SampleResult {
    id: String,
    path: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_sha256_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<RawMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mosaic_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    as_shot: Option<PlaneStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    perturbed_wb: Option<PlaneStats>,
}

#[derive(Debug, Serialize)]
struct PlaneStats {
    width: u32,
    height: u32,
    finite: bool,
    min: f32,
    max: f32,
    sha256: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("qualify_profiles: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args_os().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        println!("usage: qualify_profiles MANIFEST_JSON OUTPUT_NEW_JSON");
        return Ok(());
    }
    if args.len() != 3 {
        return Err("usage: qualify_profiles MANIFEST_JSON OUTPUT_NEW_JSON".into());
    }
    let manifest_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let manifest_bytes =
        read_bounded(&manifest_path, MAX_MANIFEST_BYTES).map_err(|e| format!("manifest: {e}"))?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| format!("manifest JSON: {e}"))?;
    validate_manifest(&manifest)?;

    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|e| {
            format!(
                "refusing existing/unwritable output {}: {e}",
                output_path.display()
            )
        })?;
    let mut results = Vec::with_capacity(manifest.samples.len());
    for (index, sample) in manifest.samples.iter().enumerate() {
        eprintln!(
            "qualify_profiles: [{}/{}] {}",
            index + 1,
            manifest.samples.len(),
            sample.id
        );
        results.push(qualify_sample(sample));
    }
    let failed = results.iter().filter(|r| r.status == "failed").count();
    let report = Report {
        manifest: manifest_path.display().to_string(),
        scope: "adapter functional/source preservation; not controlled colour qualification",
        results,
        failed,
    };
    let encoded = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    output
        .write_all(&encoded)
        .and_then(|_| output.write_all(b"\n"))
        .map_err(|e| format!("write output: {e}"))?;
    if failed != 0 {
        return Err(format!("{failed} sample(s) failed; report written"));
    }
    Ok(())
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if manifest.samples.is_empty() || manifest.samples.len() > MAX_SAMPLES {
        return Err(format!("samples must contain 1..={MAX_SAMPLES} entries"));
    }
    let mut ids = std::collections::HashSet::with_capacity(manifest.samples.len());
    for sample in &manifest.samples {
        if sample.id.is_empty() {
            return Err("sample id must not be empty".into());
        }
        if !sample.path.is_absolute() {
            return Err(format!("{}: path must be absolute", sample.id));
        }
        if !ids.insert(&sample.id) {
            return Err(format!("duplicate sample id: {}", sample.id));
        }
        if sample.sha256.len() != 64 || !sample.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "{}: sha256 must be 64 hexadecimal characters",
                sample.id
            ));
        }
    }
    Ok(())
}

fn qualify_sample(sample: &ManifestSample) -> SampleResult {
    let path = sample.path.display().to_string();
    let failed = |error: String, source_sha256: Option<String>| SampleResult {
        id: sample.id.clone(),
        path: path.clone(),
        status: "failed",
        error: Some(error),
        source_sha256,
        source_sha256_after: None,
        metadata: None,
        mosaic_sha256: None,
        as_shot: None,
        perturbed_wb: None,
    };
    let bytes = match read_bounded(&sample.path, MAX_SOURCE_BYTES) {
        Ok(bytes) => bytes,
        Err(e) => return failed(format!("read: {e}"), None),
    };
    let before = hex_hash(&bytes);
    if !before.eq_ignore_ascii_case(&sample.sha256) {
        return failed(
            format!("sha256 mismatch: expected {}, got {before}", sample.sha256),
            Some(before),
        );
    }
    let cancel = AtomicBool::new(false);
    let raw = match RawSource::decode(Arc::from(bytes), &cancel) {
        Ok(raw) => raw,
        Err(e) => return failed_after(sample, format!("decode: {e}"), Some(before)),
    };
    let metadata = raw.metadata().clone();
    let mosaic_sha256 = hash_u16(raw.mosaic());
    let as_shot = match raw.develop(metadata.as_shot_gains, &cancel) {
        Ok(image) => match plane_stats(image) {
            Ok(stats) => stats,
            Err(e) => return failed_after(sample, format!("as-shot develop: {e}"), Some(before)),
        },
        Err(e) => return failed_after(sample, format!("as-shot develop: {e}"), Some(before)),
    };
    let perturbed = [
        (metadata.as_shot_gains[0] * 1.05).min(32.0),
        1.0,
        (metadata.as_shot_gains[2] * 0.95).max(f32::MIN_POSITIVE),
    ];
    let perturbed_wb = match raw.develop(perturbed, &cancel) {
        Ok(image) => match plane_stats(image) {
            Ok(stats) => stats,
            Err(e) => {
                return failed_after(sample, format!("perturbed-WB develop: {e}"), Some(before));
            }
        },
        Err(e) => return failed_after(sample, format!("perturbed-WB develop: {e}"), Some(before)),
    };
    let after = match read_bounded(&sample.path, MAX_SOURCE_BYTES) {
        Ok(bytes) => hex_hash(&bytes),
        Err(e) => return failed(format!("post-read: {e}"), Some(before)),
    };
    if after != before {
        return failed(
            format!("source changed during qualification: {before} -> {after}"),
            Some(before),
        );
    }
    SampleResult {
        id: sample.id.clone(),
        path,
        status: "passed",
        error: None,
        source_sha256: Some(before),
        source_sha256_after: Some(after),
        metadata: Some(metadata),
        mosaic_sha256: Some(mosaic_sha256),
        as_shot: Some(as_shot),
        perturbed_wb: Some(perturbed_wb),
    }
}

fn failed_after(
    sample: &ManifestSample,
    error: String,
    source_sha256: Option<String>,
) -> SampleResult {
    let source_sha256_after = read_bounded(&sample.path, MAX_SOURCE_BYTES)
        .ok()
        .map(|bytes| hex_hash(&bytes));
    SampleResult {
        id: sample.id.clone(),
        path: sample.path.display().to_string(),
        status: "failed",
        error: Some(error),
        source_sha256,
        source_sha256_after,
        metadata: None,
        mosaic_sha256: None,
        as_shot: None,
        perturbed_wb: None,
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    if !file
        .metadata()
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err("source is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} bytes exceeds {limit}-byte limit", bytes.len()));
    }
    Ok(bytes)
}

fn hex_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_u16(values: &[u16]) -> String {
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 16384];
    for chunk in values.chunks(8192) {
        for (index, value) in chunk.iter().enumerate() {
            bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        hash.update(&bytes[..chunk.len() * 2]);
    }
    format!("{:x}", hash.finalize())
}

fn plane_stats(image: luxforge_raw::PlanarRgb) -> Result<PlaneStats, String> {
    if image.width == 0
        || image.height == 0
        || image.data.len() != image.width as usize * image.height as usize * 3
    {
        return Err("invalid developed dimensions or plane length".into());
    }
    let mut hash = Sha256::new();
    let mut finite = true;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut bytes = [0_u8; 16384];
    for chunk in image.data.chunks(4096) {
        for (index, value) in chunk.iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        hash.update(&bytes[..chunk.len() * 4]);
        for value in chunk {
            finite &= value.is_finite();
            min = min.min(*value);
            max = max.max(*value);
        }
    }
    if !finite {
        return Err("developed planes contain non-finite values".into());
    }
    Ok(PlaneStats {
        width: image.width,
        height: image.height,
        finite,
        min,
        max,
        sha256: format!("{:x}", hash.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_requires_absolute_paths_and_hex_hashes() {
        let manifest = Manifest {
            samples: vec![ManifestSample {
                id: "x".into(),
                path: "relative.RAF".into(),
                sha256: "0".repeat(64),
            }],
        };
        assert!(validate_manifest(&manifest).is_err());
        let manifest = Manifest {
            samples: vec![ManifestSample {
                id: "x".into(),
                path: "/tmp/x.RAF".into(),
                sha256: "z".repeat(64),
            }],
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn hashes_are_stable_little_endian() {
        assert_eq!(
            hash_u16(&[0x0102, 0x0304]),
            "d46e520a777bdd374ce8f8e6d650f1270169dd7eb97361846aac9e20c850d724"
        );
    }
}
