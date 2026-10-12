//! Assemble downloaded build records without compiling the editor or accepting credentials.
use crate::release_tag::ReleaseTag;
use crate::{Result, ensure, files, output, read_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

fn write_json(path: &Path, value: &Value) -> Result {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub const TARGETS: &[(&str, &str)] = &[
    ("aarch64-apple-darwin", "macos-arm64"),
    ("x86_64-unknown-linux-gnu", "linux-x86_64"),
];

pub fn platform(target: &str) -> Result<&'static str> {
    TARGETS
        .iter()
        .find(|(t, _)| *t == target)
        .map(|(_, p)| *p)
        .ok_or_else(|| "Release builds support Apple silicon macOS and x86-64 Linux only".into())
}

pub fn archive_name(tag: &ReleaseTag, target: &str) -> Result<String> {
    let platform = platform(target)?;
    Ok(format!(
        "luxforge-{}-{platform}.{}",
        tag.version,
        if target == TARGETS[0].0 {
            "zip"
        } else {
            "tar.gz"
        }
    ))
}

pub fn archive_record(path: &Path) -> Result<Value> {
    Ok(
        json!({"name":path.file_name().ok_or("Archive filename")?.to_str().ok_or("Archive filename encoding")?,"size":fs::metadata(path)?.len(),"sha256":hash(path)?}),
    )
}

pub fn verify_record(directory: &Path, tag: &ReleaseTag, target: &str) -> Result<Value> {
    let record = read_json(&directory.join("target.json"))?;
    ensure(
        record["schema_version"] == 1
            && record["tag"] == tag.tag
            && record["version"] == tag.version
            && record["target"] == target
            && record["profile"] == "release",
        "Build artifact identity mismatch",
    )?;
    let expected = directory.join(archive_name(tag, target)?);
    ensure(
        record["archive"] == archive_record(&expected)?,
        "Archive checksum/size/name mismatch",
    )?;
    ensure(
        record["release_ready"] == false,
        "Build artifacts must not claim release qualification",
    )?;
    Ok(record)
}

pub fn assemble(root: &Path, input: &Path, out: &Path, text: &str) -> Result {
    let tag = ReleaseTag::parse(text)?;
    ensure(!out.exists(), "Release assembly output must be new")?;
    let mut records = Vec::new();
    for &(target, platform) in TARGETS {
        records.push(verify_record(&input.join(platform), &tag, target)?);
    }
    for field in ["revision", "lock_sha256", "build_number"] {
        ensure(
            records[0][field].is_string() && records[0][field] == records[1][field],
            format!("Platform {field} mismatch"),
        )?;
    }
    ensure(
        records
            .iter()
            .all(|r| r["working_tree_dirty"] == false && r["rehearsal"] == false),
        "A dirty/local rehearsal cannot be published by the hosted workflow",
    )?;
    ensure(
        records[0]["revision"].as_str()
            == Some(output(root, "git", &["rev-parse", "HEAD"])?.trim()),
        "Artifacts are from another commit",
    )?;
    ensure(
        records[0]["lock_sha256"] == hash(&root.join("Cargo.lock"))?,
        "Artifacts use another dependency lockfile",
    )?;
    ensure(
        matches!(
            records[0]["signing"]["status"].as_str(),
            Some("unsigned" | "notarized")
        ) && records[1]["signing"]["status"] == "not-applicable",
        "Invalid platform signing state",
    )?;
    fs::create_dir_all(out)?;
    for ((target, platform), record) in TARGETS.iter().zip(&records) {
        let name = archive_name(&tag, target)?;
        fs::copy(input.join(platform).join(&name), out.join(name))?;
        write_json(&out.join(format!("{platform}.json")), record)?;
    }
    write_json(
        &out.join("release.json"),
        &json!({"schema_version":1,"tag":tag.tag,"version":tag.version,"revision":records[0]["revision"],"lock_sha256":records[0]["lock_sha256"],"status":"build-candidate","release_ready":false,"qualification":{"source_and_licences":"pending","distribution_advisories":"pending","native_gpu_corpus_installation":"not-run"},"targets":records}),
    )?;
    fs::write(
        out.join("release-notes.md"),
        format!(
            "Luxforge {} — pre-release build candidate\n\nTag: `{}`. Source commit: `{}`.\n\nApple silicon macOS ZIP and x86-64 Ubuntu 24.04-baseline Linux tarball, including both CLIs, resources and notices. macOS signing status: **{}**. See `release.json` and `SHA256SUMS` for identities and final hashes.\n\nThese are automated pre-release build candidates. Complete corresponding source/licence/advisory review and native GPU/corpus/installation qualification remain open. Published downloads do not establish release readiness; see the v0.1 distribution gates.\n",
            tag.version,
            tag.tag,
            records[0]["revision"].as_str().unwrap(),
            records[0]["signing"]["status"].as_str().unwrap()
        ),
    )?;
    let mut sums = String::new();
    for path in files(out)? {
        sums.push_str(&format!(
            "{}  {}\n",
            hash(&path)?,
            path.file_name().unwrap().to_string_lossy()
        ));
    }
    fs::write(out.join("SHA256SUMS"), sums)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_test_macros::slow;
    #[test]
    fn record_verification_refuses_tampering_or_another_version() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        let target = TARGETS[1].0;
        let archive = tmp.path().join(archive_name(&tag, target).unwrap());
        fs::write(&archive, b"final archive").unwrap();
        let record = json!({"schema_version":1,"tag":tag.tag,"version":tag.version,"target":target,"profile":"release","archive":archive_record(&archive).unwrap(),"release_ready":false});
        write_json(&tmp.path().join("target.json"), &record).unwrap();
        verify_record(tmp.path(), &tag, target).unwrap();
        assert!(verify_record(tmp.path(), &ReleaseTag::parse("v0.2").unwrap(), target).is_err());
        fs::write(&archive, b"tampered archive").unwrap();
        assert!(verify_record(tmp.path(), &tag, target).is_err());
    }

    #[test]
    #[slow]
    fn assembly_requires_both_matching_clean_targets_and_checksums_final_files() {
        let root = crate::root().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        let input = tmp.path().join("input");
        for &(target, platform) in TARGETS {
            let directory = input.join(platform);
            fs::create_dir_all(&directory).unwrap();
            let archive = directory.join(archive_name(&tag, target).unwrap());
            fs::write(&archive, b"final build archive").unwrap();
            write_json(&directory.join("target.json"), &json!({"schema_version":1,"tag":tag.tag,"version":tag.version,"target":target,"profile":"release","archive":archive_record(&archive).unwrap(),"release_ready":false,"revision":output(&root,"git",&["rev-parse","HEAD"]).unwrap().trim(),"lock_sha256":hash(&root.join("Cargo.lock")).unwrap(),"build_number":"1","working_tree_dirty":false,"rehearsal":false,"signing":{"status":if target == TARGETS[0].0 {"unsigned"} else {"not-applicable"}}})).unwrap();
        }
        let out = tmp.path().join("assembled");
        assemble(&root, &input, &out, &tag.tag).unwrap();
        for line in fs::read_to_string(out.join("SHA256SUMS")).unwrap().lines() {
            let (expected, name) = line.split_once("  ").unwrap();
            assert_eq!(expected, hash(&out.join(name)).unwrap());
        }
        let linux = input.join(TARGETS[1].1).join("target.json");
        let original = read_json(&linux).unwrap();
        for (field, bad) in [
            ("revision", json!("another commit")),
            ("lock_sha256", json!("another lock")),
            ("working_tree_dirty", json!(true)),
            ("rehearsal", json!(true)),
        ] {
            let mut record = original.clone();
            record[field] = bad;
            write_json(&linux, &record).unwrap();
            assert!(assemble(&root, &input, &tmp.path().join("rejected"), &tag.tag).is_err());
            assert!(!tmp.path().join("rejected").exists());
        }
        fs::remove_file(linux).unwrap();
        assert!(assemble(&root, &input, &tmp.path().join("missing"), &tag.tag).is_err());
    }
}
