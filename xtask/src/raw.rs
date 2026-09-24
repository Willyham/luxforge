//! RAW preparation evidence. These tools do not register or qualify an editor decoder.
use crate::*;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs::File, time::SystemTime};

const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_SOURCE: u64 = 128 * 1024 * 1024;
const MAX_SAMPLES: usize = 256;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    samples: Vec<Fixture>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    id: String,
    path: PathBuf,
    sha256: String,
    camera: String,
    mode: String,
    source_url: Option<String>,
    license: String,
    notes: String,
}

fn load_manifest(path: &Path) -> Result<Manifest> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    ensure(
        bytes.len() as u64 <= MAX_MANIFEST,
        "RAW manifest exceeds 1 MiB",
    )?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure(manifest.format == 1, "Unsupported RAW manifest format")?;
    ensure(
        !manifest.samples.is_empty() && manifest.samples.len() <= MAX_SAMPLES,
        "RAW manifest needs 1..256 samples",
    )?;
    let mut ids = HashSet::new();
    for sample in &manifest.samples {
        ensure(
            !sample.id.is_empty()
                && sample.id.len() <= 96
                && sample
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Invalid RAW fixture id",
        )?;
        ensure(ids.insert(&sample.id), "Duplicate RAW fixture id")?;
        ensure(
            sample.sha256.len() == 64
                && sample
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "RAW fixture needs a lowercase SHA-256",
        )?;
        ensure(
            !sample.path.as_os_str().is_empty(),
            "Empty RAW fixture path",
        )?;
        ensure(
            !sample.camera.is_empty() && !sample.mode.is_empty(),
            "Camera and declared mode are required",
        )?;
        ensure(
            matches!(sample.license.as_str(), "CC0-1.0" | "private-permission"),
            "Declare CC0-1.0 or private-permission for each RAW fixture",
        )?;
        if sample.license == "CC0-1.0" {
            ensure(
                sample
                    .source_url
                    .as_deref()
                    .is_some_and(|s| s.starts_with("https://")),
                "Public fixture needs an HTTPS provenance URL",
            )?;
        }
    }
    Ok(manifest)
}

#[derive(Debug, PartialEq, Eq)]
struct Signature {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity_change: (u64, u64, i64, i64),
}

fn signature(m: &fs::Metadata) -> Result<Signature> {
    ensure(m.is_file(), "RAW source must be a regular file")?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Signature {
        len: m.len(),
        modified: m.modified()?,
        #[cfg(unix)]
        identity_change: (m.dev(), m.ino(), m.ctime(), m.ctime_nsec()),
    })
}

fn verify(path: &Path, expected: &str) -> Result<Value> {
    let before = signature(&fs::metadata(path)?)?;
    ensure(before.len <= MAX_SOURCE, "RAW source exceeds 128 MiB")?;
    let file = File::open(path)?;
    ensure(
        signature(&file.metadata()?)? == before,
        "RAW source changed before reading",
    )?;
    let mut stream = file.take(MAX_SOURCE + 1);
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure(total <= MAX_SOURCE, "RAW source grew beyond 128 MiB")?;
        hash.update(&buffer[..n]);
    }
    ensure(
        total == before.len
            && signature(&stream.get_ref().metadata()?)? == before
            && signature(&fs::metadata(path)?)? == before,
        "RAW source changed while hashing",
    )?;
    let actual = format!("{:x}", hash.finalize());
    ensure(actual == expected, "RAW source SHA-256 mismatch")?;
    Ok(json!({"bytes":total,"sha256":actual,"verified_file":true,"decoder_qualified":false}))
}

pub fn corpus(manifest_path: &Path, out: &Path) -> Result {
    let manifest = load_manifest(manifest_path)?;
    // create_dir, not create_dir_all: an existing evidence directory must never be reused.
    fs::create_dir(out)?;
    let base = manifest_path.parent().ok_or("Manifest has no parent")?;
    let mut results = Vec::new();
    let mut passed = true;
    for sample in &manifest.samples {
        let path = absolute(base, &sample.path);
        let result = match verify(&path, &sample.sha256) {
            Ok(value) => json!({"id":sample.id,"verification":value}),
            Err(error) => {
                passed = false;
                json!({"id":sample.id,"verification":{"verified_file":false,"decoder_qualified":false,"error":error.to_string()}})
            }
        };
        results.push(result);
    }
    write_json(
        &out.join("result.json"),
        &json!({"format":1,"passed":passed,
        "scope":"Read-only file integrity and declared provenance; no decoder or camera-mode qualification",
        "manifest":manifest,"results":results}),
    )?;
    ensure(passed, "RAW corpus verification failed; see result.json")?;
    println!(
        "PASS RAW file integrity ({} samples); decoder support remains unqualified",
        results.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(path: &Path, digest: &str) -> Value {
        json!({"format":1,"samples":[{"id":"sample","path":path,"sha256":digest,
            "camera":"Synthetic","mode":"file-integrity only","source_url":null,
            "license":"private-permission","notes":"not a RAW decoder fixture"}]})
    }
    #[test]
    fn corpus_preserves_readonly_sources_and_refuses_reused_output() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("source.NEF");
        fs::write(&input, b"synthetic integrity fixture").unwrap();
        let expected = hash(&input).unwrap();
        let mut permissions = fs::metadata(&input).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&input, permissions).unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        write_json(
            &manifest_path,
            &manifest(Path::new("source.NEF"), &expected),
        )
        .unwrap();
        let out = tmp.path().join("result");
        corpus(&manifest_path, &out).unwrap();
        let before = fs::read(out.join("result.json")).unwrap();
        assert!(corpus(&manifest_path, &out).is_err());
        assert_eq!(before, fs::read(out.join("result.json")).unwrap());
        assert_eq!(hash(&input).unwrap(), expected);
        assert_eq!(
            read_json(&out.join("result.json")).unwrap()["results"][0]["verification"]["decoder_qualified"],
            false
        );
    }
    #[test]
    fn missing_and_changed_sources_are_failures_with_retained_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        let input = tmp.path().join("source.RAF");
        write_json(
            &manifest_path,
            &manifest(Path::new("source.RAF"), &"0".repeat(64)),
        )
        .unwrap();
        let missing = tmp.path().join("missing");
        assert!(corpus(&manifest_path, &missing).is_err());
        assert_eq!(
            read_json(&missing.join("result.json")).unwrap()["passed"],
            false
        );
        fs::write(&input, b"wrong bytes").unwrap();
        let changed = tmp.path().join("changed");
        assert!(corpus(&manifest_path, &changed).is_err());
        assert!(
            read_json(&changed.join("result.json")).unwrap()["results"][0]["verification"]["error"]
                .as_str()
                .unwrap()
                .contains("mismatch")
        );
    }
    #[test]
    fn unknown_fields_duplicate_ids_invalid_hashes_and_limits_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let original = manifest(Path::new("source.NEF"), &"a".repeat(64));
        for bad in [
            json!({"format":2,"samples":[]}),
            json!({"format":1,"samples":[]}),
        ] {
            write_json(&path, &bad).unwrap();
            assert!(load_manifest(&path).is_err());
        }
        let mut bad = original.clone();
        bad["unknown"] = json!(true);
        write_json(&path, &bad).unwrap();
        assert!(load_manifest(&path).is_err());
        let mut bad = original.clone();
        bad["samples"] = json!([bad["samples"][0], bad["samples"][0]]);
        write_json(&path, &bad).unwrap();
        assert!(load_manifest(&path).is_err());
        let mut bad = original;
        bad["samples"][0]["sha256"] = json!("invalid");
        write_json(&path, &bad).unwrap();
        assert!(load_manifest(&path).is_err());
        fs::write(&path, vec![b' '; MAX_MANIFEST as usize + 1]).unwrap();
        assert!(load_manifest(&path).is_err());
        let source = tmp.path().join("large.NEF");
        File::create(&source)
            .unwrap()
            .set_len(MAX_SOURCE + 1)
            .unwrap();
        assert!(
            verify(&source, &"0".repeat(64))
                .unwrap_err()
                .to_string()
                .contains("128 MiB")
        );
    }
}
