//! The complete corresponding source archive's record (`docs/design/release-v0.1.md#candidate-records`):
//! `source.json`, which `release-source` writes beside `luxforge-VERSION-source.tar.gz`, and the
//! check `release-assemble` makes before it names the archive in `release.json`. The archive itself
//! is built by the full tooling (`xtask/src/release_source.rs`), which needs the notice collectors.
use crate::release_artifacts::archive_record;
use crate::release_candidate::{SCHEMA_VERSION, source_archive_name};
use crate::release_tag::ReleaseTag;
use crate::{Result, ensure, read_json};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The directory of `release-assemble`'s input that holds the source archive and its record, beside
/// the platform directories.
pub const DIRECTORY: &str = "source";
/// The record beside the archive. It holds the archive's name, size and SHA-256, which the archive
/// itself never contains.
pub const RECORD: &str = "source.json";

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Checks a source record's shape: the current schema, `tag` and its version, a full commit, the
/// lockfile hash, the tree's state and the archive named for the version with its size and hash.
pub fn check_record(record: &Value, tag: &ReleaseTag) -> Result {
    ensure(
        record["schema_version"] == SCHEMA_VERSION
            && record["tag"] == tag.tag
            && record["version"] == tag.version,
        "Source archive identity mismatch",
    )?;
    ensure(
        record["revision"]
            .as_str()
            .is_some_and(|r| is_hex(r, 40) || is_hex(r, 64))
            && record["lock_sha256"]
                .as_str()
                .is_some_and(|h| is_hex(h, 64)),
        "The source record names a full commit and the lockfile's SHA-256",
    )?;
    ensure(
        record["working_tree_dirty"].is_boolean() && record["rehearsal"].is_boolean(),
        "The source record states whether the tree was dirty and whether it is a rehearsal",
    )?;
    let archive = &record["archive"];
    ensure(
        archive["name"] == source_archive_name(tag)
            && archive["size"].is_u64()
            && archive["sha256"].as_str().is_some_and(|h| is_hex(h, 64)),
        "The source record names the archive for the version with its size and hash",
    )
}

/// Reads `directory/source.json`, checks it and checks its archive against the file beside it.
pub fn verify(directory: &Path, tag: &ReleaseTag) -> Result<Value> {
    let record = read_json(&directory.join(RECORD))?;
    check_record(&record, tag)?;
    let archive = directory.join(source_archive_name(tag));
    ensure(archive.is_file(), "Missing source archive")?;
    ensure(
        record["archive"] == archive_record(&archive)?,
        "Source archive checksum/size/name mismatch",
    )?;
    Ok(record)
}

/// The source archive in `input/source`, when there is one: checked against its record, the
/// targets' common revision and lockfile (`common`, a checked target record) and refused when it
/// comes from a dirty tree or a rehearsal. Answers the archive's path and its `release.json` entry.
pub fn bind(input: &Path, tag: &ReleaseTag, common: &Value) -> Result<Option<(PathBuf, Value)>> {
    let directory = input.join(DIRECTORY);
    if !directory.exists() {
        return Ok(None);
    }
    let record = verify(&directory, tag)?;
    for field in ["revision", "lock_sha256"] {
        ensure(
            record[field] == common[field],
            format!("The source archive's {field} differs from the builds'"),
        )?;
    }
    ensure(
        record["working_tree_dirty"] == false && record["rehearsal"] == false,
        "A source archive from a dirty tree or a local rehearsal cannot be published",
    )?;
    Ok(Some((
        directory.join(source_archive_name(tag)),
        record["archive"].clone(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_artifacts::{TARGETS, assemble, hash};
    use crate::{output, release_candidate};
    use luxforge_test_macros::slow;
    use serde_json::json;
    use std::fs;

    fn write_json(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    }

    /// A source archive and its record in `directory`, for `revision` and `lock`.
    fn source(directory: &Path, tag: &ReleaseTag, revision: &str, lock: &str) -> Value {
        fs::create_dir_all(directory).unwrap();
        let archive = directory.join(source_archive_name(tag));
        fs::write(&archive, b"complete source").unwrap();
        let record = json!({
            "schema_version": SCHEMA_VERSION,
            "tag": tag.tag,
            "version": tag.version,
            "revision": revision,
            "lock_sha256": lock,
            "working_tree_dirty": false,
            "rehearsal": false,
            "archive": archive_record(&archive).unwrap(),
        });
        write_json(&directory.join(RECORD), &record);
        record
    }

    #[test]
    fn source_records_bind_the_version_and_refuse_tampering() {
        let tmp = tempfile::tempdir().unwrap();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        let record = source(tmp.path(), &tag, &"a".repeat(40), &"b".repeat(64));
        assert_eq!(record["archive"]["name"], "luxforge-0.1.0-source.tar.gz");
        verify(tmp.path(), &tag).unwrap();
        assert!(verify(tmp.path(), &ReleaseTag::parse("v0.2").unwrap()).is_err());
        let changes: &[fn(&mut Value)] = &[
            |r| r["schema_version"] = json!(1),
            |r| r["revision"] = json!("abc"),
            |r| r["lock_sha256"] = json!("B".repeat(64)),
            |r| r["rehearsal"] = json!("no"),
            |r| r["archive"]["name"] = json!("source.tar.gz"),
            |r| r["archive"]["sha256"] = json!(null),
        ];
        for (index, change) in changes.iter().enumerate() {
            let mut bad = record.clone();
            change(&mut bad);
            assert!(check_record(&bad, &tag).is_err(), "change {index}");
        }
        fs::write(tmp.path().join(source_archive_name(&tag)), b"tampered").unwrap();
        assert!(verify(tmp.path(), &tag).is_err());
    }

    #[test]
    #[slow]
    fn assembly_names_and_checksums_a_matching_clean_source_archive() {
        let root = crate::root().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let tag = ReleaseTag::parse("v0.1").unwrap();
        let revision = output(&root, "git", &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        let lock = hash(&root.join("Cargo.lock")).unwrap();
        let input = tmp.path().join("input");
        for &(target, platform) in TARGETS {
            let directory = input.join(platform);
            let mut record =
                release_candidate::fixture_record(&directory, &tag, target, "unsigned");
            record["revision"] = json!(revision);
            record["lock_sha256"] = json!(lock);
            write_json(&directory.join("target.json"), &record);
        }
        // Without a source archive, release.json names none.
        assemble(&root, &input, &tmp.path().join("unsourced"), &tag.tag).unwrap();
        let unsourced = read_json(&tmp.path().join("unsourced/release.json")).unwrap();
        assert_eq!(unsourced["source_archive"], Value::Null);
        let directory = input.join(DIRECTORY);
        let record = source(&directory, &tag, &revision, &lock);
        let out = tmp.path().join("assembled");
        assemble(&root, &input, &out, &tag.tag).unwrap();
        let release = read_json(&out.join("release.json")).unwrap();
        release_candidate::check_release(&release, &tag).unwrap();
        assert_eq!(release["source_archive"], record["archive"]);
        let name = source_archive_name(&tag);
        assert_eq!(archive_record(&out.join(&name)).unwrap(), record["archive"]);
        let sums = fs::read_to_string(out.join("SHA256SUMS")).unwrap();
        assert!(
            sums.lines()
                .any(|line| line.ends_with(&format!("  {name}")))
        );
        // The packet verifier finds the archive where it expects it; without one, it is not ready.
        let source_problems = |packet: &Path| {
            let verdict = crate::release_verify::verify(packet);
            verdict
                .integrity
                .iter()
                .chain(&verdict.readiness)
                .filter(|reason| reason.contains("source archive") || reason.contains(&name))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(source_problems(&out), Vec::<String>::new());
        assert_eq!(source_problems(&tmp.path().join("unsourced")).len(), 1);
        // Another commit or lockfile, a rehearsal, a dirty tree or a tampered archive is refused.
        for (field, bad) in [
            ("revision", json!("c".repeat(40))),
            ("lock_sha256", json!("c".repeat(64))),
            ("rehearsal", json!(true)),
            ("working_tree_dirty", json!(true)),
        ] {
            let mut changed = record.clone();
            changed[field] = bad;
            write_json(&directory.join(RECORD), &changed);
            let rejected = tmp.path().join("rejected");
            assert!(
                assemble(&root, &input, &rejected, &tag.tag).is_err(),
                "{field}"
            );
            assert!(!rejected.exists());
        }
        write_json(&directory.join(RECORD), &record);
        fs::write(directory.join(&name), b"tampered").unwrap();
        assert!(assemble(&root, &input, &tmp.path().join("tampered"), &tag.tag).is_err());
    }
}
