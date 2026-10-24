use super::*;
use crate::release_artifacts::archive_record;
use crate::release_candidate::fixture_record;
use luxforge_update::{Appcast, PublicKey, Signature};
use serde_json::json;

/// The throwaway Sparkle 2.10.0 test key of `luxforge-update`'s tests, as `generate_keys -x`
/// exported it (no newline), and its `SUPublicEDKey`. It signs nothing real.
const SPARKLE_TEST_KEY: &str = "xk6SHO8suB1F5pPkxgHWAKFKbzr5g2q4LrsYNlmnA6M=";
const SPARKLE_TEST_PUBLIC_KEY: &str = "rg7plRI+lkAvslNaVkA284ovU8kr7pm2xW1/AQL86Cs=";
/// A fixture archive and the signature Sparkle 2.10.0's `sign_update` printed for it.
const SMALL: &[u8] = b"Luxforge update fixture archive\n";
const SMALL_SIGNATURE: &str =
    "RIXc47mfBITFuflYFYzYdTBtdQOwNnYdEGzD3NFlQop1BTowmvGrM8ZaijSqrNOdYPnMMX/REkfcRMBaGAJ3DQ==";

fn config(public_key: Option<&str>) -> UpdateConfig {
    UpdateConfig {
        feed_base_url: "https://api.luxforge.app/v1/updates/".into(),
        public_key: public_key.map(|k| PublicKey::from_base64(k).unwrap()),
    }
}

fn key_file(dir: &Path, text: &str) -> std::path::PathBuf {
    let path = dir.join("update-key");
    fs::write(&path, text).unwrap();
    path
}

/// A `release-assemble` input directory for `tag`: each platform's archive and `target.json`.
fn builds(dir: &Path, tag: &str, build: &str, mac_status: &str) -> std::path::PathBuf {
    let tag = ReleaseTag::parse(tag).unwrap();
    let input = dir.join(format!("builds-{}", tag.version));
    for &(rust_target, platform) in TARGETS {
        let directory = input.join(platform);
        // A complete record, with the disk image download beside the macOS update archive; the
        // update record never signs the image.
        let mut record = fixture_record(&directory, &tag, rust_target, mac_status);
        let archive = directory.join(archive_name(&tag, rust_target).unwrap());
        let mac = rust_target == TARGETS[0].0;
        fs::write(
            &archive,
            if mac {
                SMALL.to_vec()
            } else {
                format!("linux {}", tag.version).into_bytes()
            },
        )
        .unwrap();
        record["archive"] = archive_record(&archive).unwrap();
        record["build_number"] = json!(build);
        fs::write(
            directory.join("target.json"),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
    }
    input
}

fn edit_target(input: &Path, platform: &str, change: impl FnOnce(&mut Value)) {
    let path = input.join(platform).join("target.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    change(&mut value);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn update_record_signs_the_final_archives_as_sparkle_does() {
    let dir = tempfile::tempdir().unwrap();
    let key = read_key(&key_file(dir.path(), SPARKLE_TEST_KEY)).unwrap();
    let input = builds(dir.path(), "v0.2-rc.1", "41", "notarized");
    let output = dir.path().join("luxforge-0.2.0-rc.1-updates.json");
    let config = config(Some(SPARKLE_TEST_PUBLIC_KEY));
    let Recorded {
        record: signed,
        notes,
    } = record(&config, &input, &output, "v0.2-rc.1", &key).unwrap();
    assert!(notes.is_empty());
    assert_eq!(signed.tag, "v0.2-rc.1");
    assert_eq!(signed.version, "0.2.0-rc.1");
    assert_eq!(signed.channel, Channel::Prerelease);
    assert_eq!(signed.public_key.to_base64(), SPARKLE_TEST_PUBLIC_KEY);
    let mac = &signed.archives[&Target::MacosArm64];
    assert_eq!(mac.file, "luxforge-0.2.0-rc.1-macos-arm64.zip");
    assert_eq!(mac.ed_signature.to_base64(), SMALL_SIGNATURE);
    assert_eq!(
        (mac.length, mac.build, mac.catalog_format),
        (SMALL.len() as u64, 41, 1)
    );
    assert_eq!(mac.minimum_system_version.as_deref(), Some("14.0"));
    assert_eq!(mac.signing, SigningStatus::Notarized);
    let linux = &signed.archives[&Target::LinuxX86_64];
    assert_eq!(linux.file, "luxforge-0.2.0-rc.1-linux-x86_64.tar.gz");
    assert_eq!(linux.minimum_system_version, None);
    assert_eq!(linux.signing, SigningStatus::NotApplicable);
    let written = fs::read(&output).unwrap();
    assert_eq!(UpdateRecord::parse(&written).unwrap(), signed);
    assert!(!String::from_utf8_lossy(&written).contains(SPARKLE_TEST_KEY));
    // The disk image is a download, not an update archive.
    assert!(!String::from_utf8_lossy(&written).contains(".dmg"));
    // The same builds and key write the same record.
    let again = dir.path().join("again");
    fs::create_dir(&again).unwrap();
    let again = again.join("luxforge-0.2.0-rc.1-updates.json");
    record(&config, &input, &again, "v0.2-rc.1", &key).unwrap();
    assert_eq!(fs::read(&again).unwrap(), written);
    // With no committed public key, any key signs.
    let unkeyed = dir.path().join("unkeyed");
    fs::create_dir(&unkeyed).unwrap();
    record(
        &self::config(None),
        &input,
        &unkeyed.join("luxforge-0.2.0-rc.1-updates.json"),
        "v0.2-rc.1",
        &key,
    )
    .unwrap();
}

#[test]
fn update_record_refuses_mixed_source_identity_even_when_macos_is_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let key = read_key(&key_file(dir.path(), SPARKLE_TEST_KEY)).unwrap();
    for status in ["notarized", "unsigned"] {
        for (field, value) in [
            ("revision", json!("c".repeat(40))),
            ("lock_sha256", json!("d".repeat(64))),
            ("build_number", json!("2")),
        ] {
            let scenario = dir.path().join(format!("{status}-{field}"));
            fs::create_dir(&scenario).unwrap();
            let input = builds(&scenario, "v0.1", "1", status);
            edit_target(&input, "macos-arm64", |record| record[field] = value);
            let output = scenario.join("luxforge-0.1.0-updates.json");
            let error = record(&config(None), &input, &output, "v0.1", &key)
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains(field), "{error}");
            assert!(
                !output.exists(),
                "mixed candidates produced a signed record"
            );
        }
    }
}

#[test]
fn update_record_leaves_out_an_unnotarized_macos_archive() {
    let dir = tempfile::tempdir().unwrap();
    let key = read_key(&key_file(dir.path(), SPARKLE_TEST_KEY)).unwrap();
    for status in ["unsigned", "pending", "failed"] {
        let input = builds(dir.path(), "v0.1", "3", status);
        let output = dir.path().join(status);
        fs::create_dir(&output).unwrap();
        let output = output.join("luxforge-0.1.0-updates.json");
        let Recorded { record, notes } =
            record(&config(None), &input, &output, "v0.1", &key).unwrap();
        // No macOS entry: the update key signs nothing a feed may not list.
        assert_eq!(
            record.archives.keys().copied().collect::<Vec<_>>(),
            [Target::LinuxX86_64, Target::LinuxArm64]
        );
        assert_eq!(
            notes,
            [format!(
                "macos-arm64: the build is {status}, not notarized; \
                 luxforge-0.1.0-macos-arm64.zip is not signed or recorded"
            )]
        );
        assert_eq!(record.channel, Channel::Default);
        let written = fs::read_to_string(&output).unwrap();
        assert!(!written.contains("macos"), "{written}");
        assert_eq!(UpdateRecord::parse(written.as_bytes()).unwrap(), record);
        fs::remove_dir_all(&input).unwrap();
    }
    // The macOS build is still checked as release-assemble checks it.
    let input = builds(dir.path(), "v0.1", "3", "unsigned");
    fs::write(
        input.join("macos-arm64/luxforge-0.1.0-macos-arm64.zip"),
        "changed",
    )
    .unwrap();
    let output = dir.path().join("luxforge-0.1.0-updates.json");
    assert!(
        record(&config(None), &input, &output, "v0.1", &key)
            .err()
            .unwrap()
            .to_string()
            .contains("checksum")
    );
}

#[test]
fn update_record_refuses_wrong_keys_outputs_and_builds() {
    let dir = tempfile::tempdir().unwrap();
    let key = read_key(&key_file(dir.path(), &format!("{SPARKLE_TEST_KEY}\n"))).unwrap();
    let input = builds(dir.path(), "v0.3.0", "50", "notarized");
    let output = dir.path().join("luxforge-0.3.0-updates.json");
    let refused = |config: &UpdateConfig, input: &Path, output: &Path, tag: &str| {
        record(config, input, output, tag, &key)
            .err()
            .unwrap()
            .to_string()
    };
    let other = SigningKey::from_seed(&[3; 32])
        .unwrap()
        .public_key()
        .to_base64();
    assert!(refused(&config(Some(&other)), &input, &output, "v0.3.0").starts_with("key-mismatch"));
    assert!(
        refused(
            &config(None),
            &input,
            &dir.path().join("updates.json"),
            "v0.3.0"
        )
        .contains("must be named luxforge-0.3.0-updates.json")
    );
    let exists = dir.path().join("exists");
    fs::create_dir(&exists).unwrap();
    fs::write(exists.join("luxforge-0.3.0-updates.json"), "").unwrap();
    assert!(
        refused(
            &config(None),
            &input,
            &exists.join("luxforge-0.3.0-updates.json"),
            "v0.3.0"
        )
        .contains("must be new")
    );
    assert!(
        refused(
            &config(None),
            &input,
            &dir.path().join("luxforge-0.4.0-updates.json"),
            "v0.4.0"
        )
        .contains("identity mismatch")
    );
    type Change = fn(&mut Value);
    for (platform, change, expected) in [
        (
            "linux-x86_64",
            (|v| v["build_number"] = json!("51")) as Change,
            "build_number differs",
        ),
        (
            "linux-x86_64",
            |v| v["build_number"] = json!("051"),
            "no build number",
        ),
        (
            "macos-arm64",
            |v| v["catalog_format"] = Value::Null,
            "no catalog format",
        ),
        (
            "macos-arm64",
            |v| v["catalog_format"] = json!(0),
            "no catalog format",
        ),
        (
            "macos-arm64",
            |v| v["minimum_system_version"] = Value::Null,
            "no minimum system version",
        ),
        (
            "macos-arm64",
            |v| v["signing"] = json!({"status": "maybe", "disk_image": {"status": "maybe"}}),
            "no known signing status",
        ),
        (
            "linux-x86_64",
            |v| v["working_tree_dirty"] = json!(true),
            "rehearsal",
        ),
        ("macos-arm64", |v| v["rehearsal"] = json!(true), "rehearsal"),
    ] {
        let copy = dir.path().join(format!("copy-{platform}-{expected}"));
        copy_dir(&input, &copy);
        edit_target(&copy, platform, change);
        let error = refused(&config(None), &copy, &output, "v0.3.0");
        assert!(error.contains(expected), "{expected}: {error}");
    }
    let tampered = dir.path().join("tampered");
    copy_dir(&input, &tampered);
    fs::write(
        tampered.join("linux-x86_64/luxforge-0.3.0-linux-x86_64.tar.gz"),
        "linux 0.3.1",
    )
    .unwrap();
    assert!(refused(&config(None), &tampered, &output, "v0.3.0").contains("checksum"));
}

fn copy_dir(from: &Path, to: &Path) {
    for path in crate::files(from).unwrap() {
        let destination = to.join(path.strip_prefix(from).unwrap());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(&path, destination).unwrap();
    }
}

#[test]
fn key_files_take_sparkle_2_exports_only_and_never_show_them() {
    let dir = tempfile::tempdir().unwrap();
    // Base64 of 96 bytes of 5: Sparkle's older private key length.
    let legacy = "BQUF".repeat(32);
    for (text, expected) in [
        (legacy.as_str(), "legacy-key"),
        ("c2VjcmV0", "malformed-key"),
        ("", "malformed-key"),
    ] {
        let error = read_key(&key_file(dir.path(), text))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with(expected), "{error}");
        assert!(text.is_empty() || !error.contains(text));
    }
    fs::write(
        dir.path().join("long"),
        "A".repeat(MAX_PRIVATE_KEY_TEXT + 1),
    )
    .unwrap();
    assert!(read_key(&dir.path().join("long")).is_err());
    fs::write(dir.path().join("binary"), [0xff, 0xfe, 0x00]).unwrap();
    assert!(read_key(&dir.path().join("binary")).is_err());
    assert!(read_key(&dir.path().join("missing")).is_err());
}

/// GitHub's release list as `gh api --paginate --slurp` writes it: an array of pages.
fn published_json(releases: &[(&str, &[&Path])]) -> Value {
    let page: Vec<Value> = releases
        .iter()
        .enumerate()
        .map(|(i, (tag, archives))| {
            json!({"tag_name": tag, "draft": false, "prerelease": tag.contains('-'), "published_at": format!("2026-10-{:02}T08:00:00Z", 10 + i), "assets": archives.iter().map(|path| json!({"name": path.file_name().unwrap().to_str().unwrap(), "size": fs::metadata(path).unwrap().len(), "state": "uploaded", "browser_download_url": "ignored"})).collect::<Vec<_>>()})
        })
        .collect();
    json!([page, [{"tag_name": "v9.9.9", "draft": true, "published_at": null, "assets": []}]])
}

#[test]
fn update_feed_writes_each_targets_appcast_from_published_records() {
    let dir = tempfile::tempdir().unwrap();
    let key = read_key(&key_file(dir.path(), SPARKLE_TEST_KEY)).unwrap();
    let records = dir.path().join("records");
    fs::create_dir(&records).unwrap();
    let config = config(Some(SPARKLE_TEST_PUBLIC_KEY));
    let mut releases = Vec::new();
    for (tag, build, status) in [
        ("v0.1.0", "3", "unsigned"),
        ("v0.2.0-rc.1", "5", "notarized"),
    ] {
        let input = builds(dir.path(), tag, build, status);
        let version = ReleaseTag::parse(tag).unwrap().version;
        record(
            &config,
            &input,
            &records.join(UpdateRecord::file_name(&version)),
            tag,
            &key,
        )
        .unwrap();
        releases.push((tag, input));
    }
    let archives: Vec<Vec<std::path::PathBuf>> = releases
        .iter()
        .map(|(_, input)| {
            crate::files(input)
                .unwrap()
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e != "json"))
                .collect()
        })
        .collect();
    let refs: Vec<Vec<&Path>> = archives
        .iter()
        .map(|a| a.iter().map(|p| p.as_path()).collect())
        .collect();
    let published = dir.path().join("published.json");
    fs::write(
        &published,
        serde_json::to_vec(&published_json(&[
            (releases[0].0, &refs[0]),
            (releases[1].0, &refs[1]),
        ]))
        .unwrap(),
    )
    .unwrap();
    let withdrawn = dir.path().join("withdrawn.json");
    fs::write(&withdrawn, "[]").unwrap();
    let output = dir.path().join("feeds");
    let generated = feeds(
        &config,
        &records,
        &published,
        &withdrawn,
        feed::DEFAULT_REPOSITORY,
        &output,
    )
    .unwrap();
    // The unsigned macOS build of v0.1.0 has no record entry, so no feed lists it.
    assert!(generated.notes.is_empty(), "{:?}", generated.notes);
    let mac = fs::read_to_string(output.join("macos-arm64/appcast.xml")).unwrap();
    let linux = fs::read_to_string(output.join("linux-x86_64/appcast.xml")).unwrap();
    let mac_items = Appcast::parse(&mac).unwrap().appcast.items;
    let linux_items = Appcast::parse(&linux).unwrap().appcast.items;
    let arm = fs::read_to_string(output.join("linux-arm64/appcast.xml")).unwrap();
    let arm_items = Appcast::parse(&arm).unwrap().appcast.items;
    assert_eq!(mac_items.iter().map(|i| i.build).collect::<Vec<_>>(), [5]);
    assert_eq!(
        arm_items.iter().map(|i| i.build).collect::<Vec<_>>(),
        [5, 3]
    );
    assert_eq!(
        linux_items.iter().map(|i| i.build).collect::<Vec<_>>(),
        [5, 3]
    );
    assert_eq!(mac_items[0].channel.as_deref(), Some("prerelease"));
    assert_eq!(
        mac_items[0].enclosure.ed_signature,
        Signature::from_base64(SMALL_SIGNATURE).unwrap()
    );
    assert_eq!(
        mac_items[0].enclosure.url,
        "https://github.com/Willyham/luxforge/releases/download/v0.2.0-rc.1/luxforge-0.2.0-rc.1-macos-arm64.zip"
    );
    assert_eq!(
        linux_items[1].pub_date.as_deref(),
        Some("Sat, 10 Oct 2026 08:00:00 +0000")
    );
    // Each listed archive verifies against the committed key from the feed alone.
    let public = config.public_key.unwrap();
    // Each build also published its macOS disk image, which no feed lists.
    let asset = |suffix: &str| {
        let found: Vec<_> = archives[1]
            .iter()
            .filter(|p| p.to_string_lossy().ends_with(suffix))
            .collect();
        assert_eq!(found.len(), 1, "{suffix}");
        found[0]
    };
    assert!(
        mac_items
            .iter()
            .chain(&linux_items)
            .all(|item| !item.enclosure.url.ends_with(".dmg"))
    );
    asset(".dmg");
    for (item, path) in [
        (&mac_items[0], asset(".zip")),
        (&linux_items[0], asset("-linux-x86_64.tar.gz")),
        (&arm_items[0], asset("-linux-arm64.tar.gz")),
    ] {
        assert!(
            item.enclosure
                .url
                .ends_with(path.file_name().unwrap().to_str().unwrap())
        );
        public
            .verify_file(path, item.enclosure.length, &item.enclosure.ed_signature)
            .unwrap();
    }
    // Deterministic: the same inputs write the same bytes.
    let again = dir.path().join("again");
    feeds(
        &config,
        &records,
        &published,
        &withdrawn,
        feed::DEFAULT_REPOSITORY,
        &again,
    )
    .unwrap();
    for target in ["macos-arm64", "linux-x86_64", "linux-arm64"] {
        assert_eq!(
            fs::read(output.join(target).join("appcast.xml")).unwrap(),
            fs::read(again.join(target).join("appcast.xml")).unwrap()
        );
    }
    assert!(
        feeds(
            &config,
            &records,
            &published,
            &withdrawn,
            feed::DEFAULT_REPOSITORY,
            &again
        )
        .unwrap_err()
        .to_string()
        .contains("must be new")
    );

    // The deploy archive holds exactly each target's feed, as the store installs it.
    let archive = dir.path().join("luxforge-update-feeds.tar");
    deploy(&generated, &archive).unwrap();
    let entries = deploy_entries(&archive);
    assert_eq!(
        entries.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
        [
            "macos-arm64/appcast.xml",
            "linux-x86_64/appcast.xml",
            "linux-arm64/appcast.xml"
        ]
    );
    for (name, mode, bytes) in &entries {
        assert_eq!(*mode, 0o644);
        assert_eq!(bytes, &fs::read(output.join(name)).unwrap(), "{name}");
    }
    let again_archive = dir.path().join("again.tar");
    deploy(&generated, &again_archive).unwrap();
    assert_eq!(
        fs::read(&archive).unwrap(),
        fs::read(&again_archive).unwrap()
    );
    assert!(
        deploy(&generated, &archive)
            .unwrap_err()
            .to_string()
            .contains("must be new")
    );
    assert!(
        deploy(&generated, &dir.path().join("feeds.tgz"))
            .unwrap_err()
            .to_string()
            .contains("*.tar")
    );

    // A withdrawn release is left out; an unpublished asset and a stray file are refused.
    fs::write(&withdrawn, r#"[{"tag":"v0.2.0-rc.1","reason":"test"}]"#).unwrap();
    let feeds_withdrawn = feeds(
        &config,
        &records,
        &published,
        &withdrawn,
        feed::DEFAULT_REPOSITORY,
        &dir.path().join("withdrawn-feeds"),
    )
    .unwrap();
    assert!(
        feeds_withdrawn.appcasts[&Target::MacosArm64]
            .items
            .is_empty()
    );
    assert_eq!(
        feeds_withdrawn.appcasts[&Target::LinuxX86_64].items.len(),
        1
    );
    fs::write(&withdrawn, "[]").unwrap();
    let partial = dir.path().join("partial.json");
    fs::write(
        &partial,
        serde_json::to_vec(&published_json(&[(releases[0].0, &refs[0])])).unwrap(),
    )
    .unwrap();
    let error = feeds(
        &config,
        &records,
        &partial,
        &withdrawn,
        feed::DEFAULT_REPOSITORY,
        &dir.path().join("partial-feeds"),
    )
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("unpublished-asset"), "{error}");
    assert!(!dir.path().join("partial-feeds").exists());
    let other_key = config_with_other_key();
    assert!(
        feeds(
            &other_key,
            &records,
            &published,
            &withdrawn,
            feed::DEFAULT_REPOSITORY,
            &dir.path().join("other-key"),
        )
        .unwrap_err()
        .to_string()
        .starts_with("record-key-mismatch")
    );
    fs::write(records.join("notes.txt"), "").unwrap();
    assert!(
        feeds(
            &config,
            &records,
            &published,
            &withdrawn,
            feed::DEFAULT_REPOSITORY,
            &dir.path().join("stray")
        )
        .unwrap_err()
        .to_string()
        .contains("not an update record")
    );
    fs::remove_file(records.join("notes.txt")).unwrap();
    fs::rename(
        records.join("luxforge-0.1.0-updates.json"),
        records.join("luxforge-0.1.1-updates.json"),
    )
    .unwrap();
    assert!(
        feeds(
            &config,
            &records,
            &published,
            &withdrawn,
            feed::DEFAULT_REPOSITORY,
            &dir.path().join("renamed")
        )
        .unwrap_err()
        .to_string()
        .contains("holds the record of v0.1.0")
    );
}

/// Each entry of a deploy archive: its path, mode and bytes. Every entry must be a regular file.
fn deploy_entries(path: &Path) -> Vec<(String, u32, Vec<u8>)> {
    let mut archive = tar::Archive::new(fs::File::open(path).unwrap());
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            assert_eq!(entry.header().entry_type(), tar::EntryType::Regular);
            assert_eq!(entry.header().mtime().unwrap(), 0);
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mode = entry.header().mode().unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            (name, mode, bytes)
        })
        .collect()
}

fn config_with_other_key() -> UpdateConfig {
    let other = SigningKey::from_seed(&[4; 32])
        .unwrap()
        .public_key()
        .to_base64();
    config(Some(&other))
}

#[test]
fn published_lists_take_pages_or_one_page_and_skip_drafts_and_partial_uploads() {
    let release = |tag: &str, draft: bool, published_at: Value, state: &str| json!({"tag_name": tag, "draft": draft, "published_at": published_at, "assets": [{"name": "a.zip", "size": 3, "state": state}, {"name": "b.zip", "size": 4}]});
    let page = json!([
        release("v0.1.0", false, json!("2026-10-10T08:00:00Z"), "uploaded"),
        release("v0.2.0", true, json!("2026-10-11T08:00:00Z"), "uploaded"),
        release("v0.3.0", false, Value::Null, "uploaded"),
        release("v0.4.0", false, json!("2026-10-12T08:00:00Z"), "starter"),
    ]);
    for input in [page.clone(), json!([page.clone()])] {
        let published = parse_published(&serde_json::to_vec(&input).unwrap()).unwrap();
        let summary: Vec<_> = published
            .iter()
            .map(|r| (r.tag.as_str(), r.assets.keys().cloned().collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            summary,
            [
                ("v0.1.0", vec!["a.zip".to_owned(), "b.zip".to_owned()]),
                ("v0.4.0", vec!["b.zip".to_owned()]),
            ]
        );
        assert_eq!(published[0].assets["a.zip"], 3);
    }
    for bad in [
        json!({}),
        json!([{"draft": false, "published_at": null, "assets": []}]),
        json!([{"tag_name": "v1.0.0", "published_at": "2026-10-10T08:00:00Z", "assets": []}]),
        json!([{"tag_name": "v1.0.0", "draft": false, "published_at": 5, "assets": []}]),
        json!([{"tag_name": "v1.0.0", "draft": false, "published_at": "2026-10-10T08:00:00Z"}]),
        json!([{"tag_name": "v1.0.0", "draft": false, "published_at": "2026-10-10T08:00:00Z", "assets": [{"name": "a"}]}]),
        json!([{"tag_name": "v1.0.0", "draft": false, "published_at": "2026-10-10T08:00:00Z", "assets": [{"name": "a", "size": 1}, {"name": "a", "size": 1}]}]),
        json!([{"tag_name": "v1.0.0", "draft": false, "published_at": "2026-10-10T08:00:00Z", "assets": [{"name": "a", "size": 1, "state": 1}]}]),
    ] {
        assert!(
            parse_published(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "{bad}"
        );
    }
    assert!(parse_published(b"not json").is_err());
}
