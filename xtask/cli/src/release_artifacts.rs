//! Assemble downloaded build records without compiling the editor or accepting credentials.
use crate::release_candidate;
use crate::release_tag::ReleaseTag;
use crate::{Result, ensure, files, output, read_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

fn write_json(path: &Path, value: &Value) -> Result {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
pub(crate) fn hash(path: &Path) -> Result<String> {
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

/// The macOS download: a drag-to-Applications disk image beside the update ZIP. Other targets
/// download their archive.
pub fn disk_image_name(tag: &ReleaseTag, target: &str) -> Result<Option<String>> {
    let platform = platform(target)?;
    Ok((target == TARGETS[0].0).then(|| format!("luxforge-{}-{platform}.dmg", tag.version)))
}

/// A target's published files: its archive, then its disk image where it has one.
pub fn asset_names(tag: &ReleaseTag, target: &str) -> Result<Vec<String>> {
    let mut names = vec![archive_name(tag, target)?];
    names.extend(disk_image_name(tag, target)?);
    Ok(names)
}

pub fn archive_record(path: &Path) -> Result<Value> {
    Ok(
        json!({"name":path.file_name().ok_or("Archive filename")?.to_str().ok_or("Archive filename encoding")?,"size":fs::metadata(path)?.len(),"sha256":hash(path)?}),
    )
}

/// Reads `directory/target.json`, checks it against the candidate contract
/// ([`release_candidate::check_record`]) and its archive and any disk image against their files.
pub fn verify_record(directory: &Path, tag: &ReleaseTag, target: &str) -> Result<Value> {
    let record = read_json(&directory.join("target.json"))?;
    release_candidate::check_record(&record, tag, target)?;
    let expected = directory.join(archive_name(tag, target)?);
    ensure(
        record["archive"] == archive_record(&expected)?,
        "Archive checksum/size/name mismatch",
    )?;
    match disk_image_name(tag, target)? {
        Some(name) => {
            let path = directory.join(name);
            ensure(path.is_file(), "Missing disk image")?;
            ensure(
                record["disk_image"] == archive_record(&path)?,
                "Disk image checksum/size/name mismatch",
            )?;
            ensure(
                record["signing"]["disk_image"]["status"] == record["signing"]["status"],
                "Disk image signing state differs from the app's",
            )?;
        }
        None => ensure(
            record.get("disk_image").is_none(),
            "Only macOS records a disk image",
        )?,
    }
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
    let release = release_candidate::release_record(&tag, &records);
    release_candidate::check_release(&release, &tag)?;
    fs::create_dir_all(out)?;
    for ((target, platform), record) in TARGETS.iter().zip(&records) {
        for name in asset_names(&tag, target)? {
            fs::copy(input.join(platform).join(&name), out.join(name))?;
        }
        write_json(&out.join(format!("{platform}.json")), record)?;
    }
    write_json(&out.join("release.json"), &release)?;
    fs::write(
        out.join("release-notes.md"),
        format!(
            "Luxforge {} — pre-release build candidate\n\nTag: `{}`. Source commit: `{}`.\n\nApple silicon macOS disk image (open it and drag Luxforge to Applications) and x86-64 Ubuntu 24.04-baseline Linux tarball, including both CLIs, resources and notices. The macOS ZIP holds the same app as the update archive. macOS signing status, for the app and its disk image: **{}**. See `release.json` and `SHA256SUMS` for identities and final hashes.\n\nThese are automated pre-release build candidates. Complete corresponding source/licence/advisory review and native GPU/corpus/installation qualification remain open. Published downloads do not establish release readiness; see the v0.1 distribution gates.\n",
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

    /// Writes a build candidate's assets and `target.json` into `directory`, as `release-build`
    /// would, and returns the record.
    fn candidate(directory: &Path, tag: &ReleaseTag, target: &str, extra: Value) -> Value {
        let mut record = release_candidate::fixture_record(directory, tag, target, "unsigned");
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        write_json(&directory.join("target.json"), &record).unwrap();
        record
    }

    #[test]
    fn record_verification_refuses_tampering_or_another_version() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        for &(target, platform) in TARGETS {
            let directory = tmp.path().join(platform);
            candidate(&directory, &tag, target, json!({}));
            verify_record(&directory, &tag, target).unwrap();
            assert!(
                verify_record(&directory, &ReleaseTag::parse("v0.2").unwrap(), target).is_err()
            );
            fs::write(
                directory.join(archive_name(&tag, target).unwrap()),
                b"tampered archive",
            )
            .unwrap();
            assert!(verify_record(&directory, &tag, target).is_err());
        }
    }

    #[test]
    fn record_verification_refuses_a_missing_tampered_or_mislabelled_disk_image() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        let target = TARGETS[0].0;
        let image = tmp
            .path()
            .join(disk_image_name(&tag, target).unwrap().unwrap());
        assert_eq!(image.file_name().unwrap(), "luxforge-0.1.0-macos-arm64.dmg");
        let record = candidate(tmp.path(), &tag, target, json!({}));
        verify_record(tmp.path(), &tag, target).unwrap();
        fs::write(&image, b"tampered disk image").unwrap();
        assert!(verify_record(tmp.path(), &tag, target).is_err());
        fs::remove_file(&image).unwrap();
        assert!(verify_record(tmp.path(), &tag, target).is_err());
        fs::write(&image, b"final disk image").unwrap();
        verify_record(tmp.path(), &tag, target).unwrap();
        for change in [
            |r: &mut Value| r["signing"]["disk_image"] = json!({"status":"notarized"}),
            |r: &mut Value| r["signing"] = json!({"status":"unsigned"}),
            |r: &mut Value| {
                r.as_object_mut().unwrap().remove("disk_image");
            },
        ] {
            let mut bad = record.clone();
            change(&mut bad);
            write_json(&tmp.path().join("target.json"), &bad).unwrap();
            assert!(verify_record(tmp.path(), &tag, target).is_err());
        }
        let linux = tmp.path().join("linux");
        let mut record = candidate(&linux, &tag, TARGETS[1].0, json!({}));
        record["disk_image"] = record["archive"].clone();
        write_json(&linux.join("target.json"), &record).unwrap();
        assert!(verify_record(&linux, &tag, TARGETS[1].0).is_err());
    }

    #[test]
    #[slow]
    fn assembly_requires_both_matching_clean_targets_and_checksums_final_files() {
        let root = crate::root().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        let input = tmp.path().join("input");
        for &(target, platform) in TARGETS {
            candidate(
                &input.join(platform),
                &tag,
                target,
                json!({"revision":output(&root,"git",&["rev-parse","HEAD"]).unwrap().trim(),"lock_sha256":hash(&root.join("Cargo.lock")).unwrap(),"build_number":"1","working_tree_dirty":false,"rehearsal":false}),
            );
        }
        let out = tmp.path().join("assembled");
        assemble(&root, &input, &out, &tag.tag).unwrap();
        let sums = fs::read_to_string(out.join("SHA256SUMS")).unwrap();
        for line in sums.lines() {
            let (expected, name) = line.split_once("  ").unwrap();
            assert_eq!(expected, hash(&out.join(name)).unwrap());
        }
        let image = disk_image_name(&tag, TARGETS[0].0).unwrap().unwrap();
        for name in [
            image.as_str(),
            "luxforge-0.1.0-macos-arm64.zip",
            "luxforge-0.1.0-linux-x86_64.tar.gz",
            "release.json",
        ] {
            assert!(
                sums.lines()
                    .any(|line| line.ends_with(&format!("  {name}")))
            );
        }
        // An assembled packet is never overwritten.
        assert!(assemble(&root, &input, &out, &tag.tag).is_err());
        let release = read_json(&out.join("release.json")).unwrap();
        release_candidate::check_release(&release, &tag).unwrap();
        assert_eq!(release["release_ready"], false);
        assert_eq!(
            release["qualification"]["macos-arm64"]["clean_installation"]["status"],
            "not-run"
        );
        assert_eq!(
            release["targets"][0]["disk_image"],
            archive_record(&out.join(&image)).unwrap()
        );
        assert!(release["targets"][1].get("disk_image").is_none());
        let mac_image = input.join(TARGETS[0].1).join(&image);
        fs::write(&mac_image, b"tampered disk image").unwrap();
        assert!(assemble(&root, &input, &tmp.path().join("tampered"), &tag.tag).is_err());
        assert!(!tmp.path().join("tampered").exists());
        fs::remove_file(&mac_image).unwrap();
        assert!(assemble(&root, &input, &tmp.path().join("no-image"), &tag.tag).is_err());
        assert!(!tmp.path().join("no-image").exists());
        fs::write(&mac_image, b"final disk image").unwrap();
        let linux = input.join(TARGETS[1].1).join("target.json");
        let original = read_json(&linux).unwrap();
        for (field, bad) in [
            ("revision", json!("c".repeat(40))),
            ("lock_sha256", json!("c".repeat(64))),
            ("build_number", json!("2")),
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
