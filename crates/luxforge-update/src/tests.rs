use super::*;
use feed::{FeedInputs, PublishedRelease, Withdrawal};

/// A throwaway key made for these tests by Sparkle 2.10.0's `generate_keys` under a test account
/// and exported with `-x`; its keychain item was deleted. It signs nothing real.
const SPARKLE_TEST_KEY: &str = "xk6SHO8suB1F5pPkxgHWAKFKbzr5g2q4LrsYNlmnA6M=";
/// The `SUPublicEDKey` `generate_keys` printed for it.
const SPARKLE_TEST_PUBLIC_KEY: &str = "rg7plRI+lkAvslNaVkA284ovU8kr7pm2xW1/AQL86Cs=";
/// What Sparkle 2.10.0's `sign_update --ed-key-file` printed for each fixture with that key.
const SMALL: &[u8] = b"Luxforge update fixture archive\n";
const SMALL_SIGNATURE: &str =
    "RIXc47mfBITFuflYFYzYdTBtdQOwNnYdEGzD3NFlQop1BTowmvGrM8ZaijSqrNOdYPnMMX/REkfcRMBaGAJ3DQ==";
const PATTERN_SIGNATURE: &str =
    "d3u3MAHgillE60cNddjgwxVhilnf3uyOkCM/m03uFtb4s2JBfYj2a3fmuMGRgOV1djQRppns8gVEsgpESmMHDA==";
const EMPTY_SIGNATURE: &str =
    "uN1yr4ZyH3YZXHU1bUA8EtYB5H0erCD1XMbcN2nzzZf9YcFgmAvYVbbvvGp9G3Ai67ZWGntysFv8Z4Ei/nykDQ==";

/// The 3 MiB + 7 byte fixture: byte `i` is `(31 i + 7) mod 251`.
fn pattern() -> Vec<u8> {
    (0..3 * 1024 * 1024 + 7)
        .map(|i: usize| ((i * 31 + 7) % 251) as u8)
        .collect()
}

fn test_key() -> SigningKey {
    SigningKey::from_sparkle_export(SPARKLE_TEST_KEY).unwrap()
}

#[test]
fn a_sparkle_exported_key_signs_exactly_as_sign_update() {
    let key = test_key();
    assert_eq!(key.public_key().to_base64(), SPARKLE_TEST_PUBLIC_KEY);
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes, expected) in [
        ("small.zip", SMALL.to_vec(), SMALL_SIGNATURE),
        ("pattern.tar.gz", pattern(), PATTERN_SIGNATURE),
        ("empty", Vec::new(), EMPTY_SIGNATURE),
    ] {
        assert_eq!(key.sign(&bytes).to_base64(), expected, "{name} in memory");
        let path = dir.path().join(name);
        std::fs::write(&path, &bytes).unwrap();
        let (signature, length) = key.sign_file(&path).unwrap();
        assert_eq!(signature.to_base64(), expected, "{name} mapped");
        assert_eq!(length, bytes.len() as u64);
        let public = PublicKey::from_base64(SPARKLE_TEST_PUBLIC_KEY).unwrap();
        public
            .verify_file(&path, length, &Signature::from_base64(expected).unwrap())
            .unwrap();
    }
}

#[test]
fn verification_refuses_another_signature_length_key_or_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.zip");
    std::fs::write(&path, SMALL).unwrap();
    let public = test_key().public_key();
    let signature = Signature::from_base64(SMALL_SIGNATURE).unwrap();
    public
        .verify_file(&path, SMALL.len() as u64, &signature)
        .unwrap();
    let other = Signature::from_base64(PATTERN_SIGNATURE).unwrap();
    assert_eq!(
        public
            .verify_file(&path, SMALL.len() as u64, &other)
            .unwrap_err()
            .code(),
        "bad-signature"
    );
    for expected in [SMALL.len() as u64 - 1, SMALL.len() as u64 + 1] {
        assert!(matches!(
            public.verify_file(&path, expected, &signature),
            Err(Error::LengthMismatch { actual, .. }) if actual == SMALL.len() as u64
        ));
    }
    let another_key = SigningKey::from_seed(&[7; 32]).unwrap().public_key();
    assert_eq!(
        another_key
            .verify_file(&path, SMALL.len() as u64, &signature)
            .unwrap_err()
            .code(),
        "bad-signature"
    );
    let mut tampered = SMALL.to_vec();
    tampered[0] ^= 1;
    std::fs::write(&path, &tampered).unwrap();
    assert_eq!(
        public
            .verify_file(&path, SMALL.len() as u64, &signature)
            .unwrap_err()
            .code(),
        "bad-signature"
    );
    assert_eq!(
        public
            .verify_file(&dir.path().join("missing"), 1, &signature)
            .unwrap_err()
            .code(),
        "io"
    );
}

#[test]
fn only_sparkle_2_seed_keys_are_accepted_and_none_is_ever_shown() {
    use base64::Engine as _;
    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    // `generate_keys -x` writes no newline; a secret pasted into a file may have one.
    for text in [
        format!("{SPARKLE_TEST_KEY}\n"),
        format!("  {SPARKLE_TEST_KEY}\r\n"),
    ] {
        assert_eq!(
            SigningKey::from_sparkle_export(&text)
                .unwrap()
                .public_key()
                .to_base64(),
            SPARKLE_TEST_PUBLIC_KEY
        );
    }
    assert_eq!(
        SigningKey::from_sparkle_export(&encode(&[1; 96]))
            .unwrap_err()
            .code(),
        "legacy-key"
    );
    for text in [
        String::new(),
        encode(&[1; 31]),
        encode(&[1; 33]),
        encode(&[1; 64]),
        "not base64 at all".to_owned(),
        SPARKLE_TEST_KEY.trim_end_matches('=').to_owned(),
        "A".repeat(MAX_PRIVATE_KEY_TEXT + 4),
    ] {
        let error = SigningKey::from_sparkle_export(&text).unwrap_err();
        assert_eq!(error.code(), "malformed-key", "{text:?}");
        assert!(!error.to_string().contains(SPARKLE_TEST_KEY));
    }
    let shown = format!("{:?}", test_key());
    assert!(!shown.contains(SPARKLE_TEST_KEY) && shown.contains(SPARKLE_TEST_PUBLIC_KEY));
    for text in ["", "AAAA", &encode(&[1; 33]), &SMALL_SIGNATURE[..40]] {
        assert_eq!(
            PublicKey::from_base64(text).unwrap_err().code(),
            "malformed-public-key"
        );
        assert_eq!(
            Signature::from_base64(text).unwrap_err().code(),
            "malformed-signature"
        );
    }
}

fn archive(target: Target, version: &str, build: u64, length: u64) -> ArchiveRecord {
    let key = test_key();
    ArchiveRecord {
        file: format!(
            "luxforge-{version}-{target}.{}",
            if target.is_macos() { "zip" } else { "tar.gz" }
        ),
        length,
        build,
        ed_signature: key.sign(format!("{version} {target}").as_bytes()),
        minimum_system_version: target.is_macos().then(|| "14.0".to_owned()),
        catalog_format: 1,
        signing: if target.is_macos() {
            SigningStatus::Notarized
        } else {
            SigningStatus::NotApplicable
        },
    }
}

fn record(tag: &str, build: u64) -> UpdateRecord {
    let version = tag_version(tag).unwrap();
    let channel = Channel::of(&semver::Version::parse(&version).unwrap());
    UpdateRecord {
        schema_version: RECORD_SCHEMA,
        tag: tag.to_owned(),
        archives: Target::RELEASED
            .into_iter()
            .map(|t| (t, archive(t, &version, build, 1000 + build)))
            .collect(),
        version,
        channel,
        public_key: test_key().public_key(),
    }
}

fn published(records: &[UpdateRecord]) -> Vec<PublishedRelease> {
    records
        .iter()
        .enumerate()
        .map(|(day, record)| PublishedRelease {
            tag: record.tag.clone(),
            published_at: format!("2026-10-{:02}T09:30:00Z", day + 10),
            assets: record
                .archives
                .values()
                .map(|a| (a.file.clone(), a.length))
                .chain([(UpdateRecord::file_name(&record.version), 900)])
                .collect(),
        })
        .collect()
}

fn generate(
    records: &[UpdateRecord],
    published: &[PublishedRelease],
    withdrawn: &[Withdrawal],
) -> Result<feed::Feeds> {
    let key = test_key().public_key();
    feed::generate(&FeedInputs {
        records,
        published,
        withdrawn,
        repository: feed::DEFAULT_REPOSITORY,
        public_key: Some(&key),
    })
}

#[test]
fn records_round_trip_and_refuse_inconsistent_fields() {
    let original = record("v0.2-rc.1", 7);
    assert_eq!(original.version, "0.2.0-rc.1");
    assert_eq!(original.channel, Channel::Prerelease);
    let json = original.to_json();
    assert_eq!(UpdateRecord::parse(json.as_bytes()).unwrap(), original);
    assert_eq!(json, original.to_json());
    assert!(json.contains("\"channel\": \"prerelease\""));
    assert!(json.contains("\"macos-arm64\": {"));
    assert!(json.contains("\"signing\": \"notarized\""));
    assert_eq!(record("v0.2.0", 8).channel, Channel::Default);

    type Change = fn(&mut UpdateRecord);
    let refused: [(&str, Change); 12] = [
        ("schema", |r| r.schema_version = 2),
        ("tag", |r| r.tag = "v0.3.0-rc.1".into()),
        ("unnormalized", |r| r.version = "0.2.0-rc.01".into()),
        ("channel", |r| r.channel = Channel::Default),
        ("no archives", |r| r.archives.clear()),
        ("file", |r| {
            r.archives.get_mut(&Target::LinuxX86_64).unwrap().file =
                "luxforge-0.2.0-rc.1-linux-x86_64/../x.tar.gz".into()
        }),
        ("other version file", |r| {
            r.archives.get_mut(&Target::LinuxX86_64).unwrap().file =
                "luxforge-0.1.0-linux-x86_64.tar.gz".into()
        }),
        ("build", |r| {
            r.archives.get_mut(&Target::LinuxX86_64).unwrap().build = 0
        }),
        ("length", |r| {
            r.archives.get_mut(&Target::LinuxX86_64).unwrap().length = 0
        }),
        ("macOS minimum", |r| {
            r.archives
                .get_mut(&Target::MacosArm64)
                .unwrap()
                .minimum_system_version = None
        }),
        ("Linux minimum", |r| {
            r.archives
                .get_mut(&Target::LinuxX86_64)
                .unwrap()
                .minimum_system_version = Some("14.0".into())
        }),
        ("minimum form", |r| {
            r.archives
                .get_mut(&Target::MacosArm64)
                .unwrap()
                .minimum_system_version = Some("14.x".into())
        }),
    ];
    for (name, change) in refused {
        let mut bad = original.clone();
        change(&mut bad);
        let error = UpdateRecord::parse(bad.to_json().as_bytes()).unwrap_err();
        assert_eq!(error.code(), "malformed-record", "{name}: {error}");
    }
    for text in [
        json.replace("\"schema_version\"", "\"extra\": 1, \"schema_version\""),
        json.replace("\"macos-arm64\"", "\"macos-x86_64\""),
        json.replace("\"notarized\"", "\"maybe\""),
        json.replace(&original.public_key.to_base64(), "AAAA"),
        "[]".to_owned(),
    ] {
        assert_eq!(
            UpdateRecord::parse(text.as_bytes()).unwrap_err().code(),
            "malformed-record"
        );
    }
    assert_eq!(
        UpdateRecord::parse(&vec![b' '; MAX_RECORD_BYTES + 1])
            .unwrap_err()
            .code(),
        "malformed-record"
    );
}

#[test]
fn a_feed_is_the_expected_sparkle_appcast_and_reads_back() {
    let records = [record("v0.1", 3), record("v0.2.0-rc.1", 9)];
    let feeds = generate(&records, &published(&records), &[]).unwrap();
    assert!(feeds.notes.is_empty(), "{:?}", feeds.notes);
    assert_eq!(
        feeds.appcasts.keys().copied().collect::<Vec<_>>(),
        Target::RELEASED.to_vec()
    );
    let mac = feeds.appcasts[&Target::MacosArm64].to_xml();
    let rc_signature = records[1].archives[&Target::MacosArm64]
        .ed_signature
        .to_base64();
    let first_signature = records[0].archives[&Target::MacosArm64]
        .ed_signature
        .to_base64();
    let expected = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:luxforge="https://luxforge.app/xmlns/update">
  <channel>
    <title>Luxforge</title>
    <link>https://github.com/Willyham/luxforge/releases</link>
    <description>Luxforge updates for Apple silicon macOS</description>
    <language>en</language>
    <item>
      <title>Luxforge 0.2.0-rc.1</title>
      <link>https://github.com/Willyham/luxforge/releases/tag/v0.2.0-rc.1</link>
      <pubDate>Sun, 11 Oct 2026 09:30:00 +0000</pubDate>
      <sparkle:version>9</sparkle:version>
      <sparkle:shortVersionString>0.2.0-rc.1</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
      <sparkle:channel>prerelease</sparkle:channel>
      <luxforge:catalogFormat>1</luxforge:catalogFormat>
      <enclosure url="https://github.com/Willyham/luxforge/releases/download/v0.2.0-rc.1/luxforge-0.2.0-rc.1-macos-arm64.zip" length="1009" type="application/octet-stream" sparkle:edSignature="{rc_signature}"/>
    </item>
    <item>
      <title>Luxforge 0.1.0</title>
      <link>https://github.com/Willyham/luxforge/releases/tag/v0.1</link>
      <pubDate>Sat, 10 Oct 2026 09:30:00 +0000</pubDate>
      <sparkle:version>3</sparkle:version>
      <sparkle:shortVersionString>0.1.0</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
      <luxforge:catalogFormat>1</luxforge:catalogFormat>
      <enclosure url="https://github.com/Willyham/luxforge/releases/download/v0.1/luxforge-0.1.0-macos-arm64.zip" length="1003" type="application/octet-stream" sparkle:edSignature="{first_signature}"/>
    </item>
  </channel>
</rss>
"#
    );
    assert_eq!(mac, expected);
    // The Linux feed has no minimum system version and the tarball as its enclosure.
    let linux = feeds.appcasts[&Target::LinuxX86_64].to_xml();
    assert!(!linux.contains("minimumSystemVersion"));
    assert!(linux.contains("/v0.1/luxforge-0.1.0-linux-x86_64.tar.gz\" length=\"1003\""));
    // Every feed reads back as the appcast it was written from.
    for appcast in feeds.appcasts.values() {
        let parsed = Appcast::parse(&appcast.to_xml()).unwrap();
        assert!(parsed.skipped.is_empty());
        assert_eq!(&parsed.appcast, appcast);
    }
    // The same inputs give the same feeds, whatever order the records and releases come in.
    let reversed = [records[1].clone(), records[0].clone()];
    let mut releases = published(&records);
    releases.reverse();
    assert_eq!(generate(&reversed, &releases, &[]).unwrap(), feeds);
}

#[test]
fn feeds_refuse_non_rising_builds_duplicates_unpublished_assets_and_other_keys() {
    let records = [record("v0.1.0", 5), record("v0.2.0", 5)];
    assert!(matches!(
        generate(&records, &published(&records), &[]),
        Err(Error::BuildNotRising { earlier_build: 5, later_build: 5, ref later, .. }) if later == "v0.2.0"
    ));
    // Builds rise with SemVer order, not with the order releases were made: a release candidate
    // precedes its release.
    let records = [record("v0.2.0", 5), record("v0.2.0-rc.1", 6)];
    assert_eq!(
        generate(&records, &published(&records), &[])
            .unwrap_err()
            .code(),
        "build-not-rising"
    );
    // A withdrawn release still counts, since a person may have installed it.
    let records = [record("v0.1.0", 5), record("v0.2.0", 4)];
    let withdrawn = [Withdrawal {
        tag: "v0.1.0".into(),
        reason: "test".into(),
    }];
    assert_eq!(
        generate(&records, &published(&records), &withdrawn)
            .unwrap_err()
            .code(),
        "build-not-rising"
    );
    // One target out of order is enough.
    let mut records = [record("v0.1.0", 5), record("v0.2.0", 6)];
    records[1]
        .archives
        .get_mut(&Target::LinuxX86_64)
        .unwrap()
        .build = 5;
    assert!(matches!(
        generate(&records, &published(&records), &[]),
        Err(Error::BuildNotRising {
            target: Target::LinuxX86_64,
            ..
        })
    ));

    for duplicate in [
        [record("v0.1.0", 5), record("v0.1.0", 6)],
        [record("v0.1", 5), record("v0.1.0", 6)],
    ] {
        assert_eq!(
            generate(&duplicate, &published(&duplicate), &[])
                .unwrap_err()
                .code(),
            "duplicate-release"
        );
    }

    let records = [record("v0.1.0", 5), record("v0.2.0", 6)];
    let all = published(&records);
    assert!(matches!(
        generate(&records, &all[..1], &[]),
        Err(Error::UnpublishedAsset { ref tag, .. }) if tag == "v0.2.0"
    ));
    let mut missing_asset = all.clone();
    missing_asset[1]
        .assets
        .remove("luxforge-0.2.0-linux-x86_64.tar.gz");
    assert!(matches!(
        generate(&records, &missing_asset, &[]),
        Err(Error::UnpublishedAsset { ref file, .. }) if file == "luxforge-0.2.0-linux-x86_64.tar.gz"
    ));
    let mut resized = all.clone();
    *resized[0]
        .assets
        .get_mut("luxforge-0.1.0-macos-arm64.zip")
        .unwrap() += 1;
    assert_eq!(
        generate(&records, &resized, &[]).unwrap_err().code(),
        "asset-length-mismatch"
    );
    let mut twice = all.clone();
    twice.push(all[0].clone());
    assert_eq!(
        generate(&records, &twice, &[]).unwrap_err().code(),
        "malformed-input"
    );
    let mut undated = all.clone();
    undated[0].published_at = "yesterday".into();
    assert_eq!(
        generate(&records, &undated, &[]).unwrap_err().code(),
        "malformed-input"
    );

    let mut other_key = records.clone();
    other_key[0].public_key = SigningKey::from_seed(&[9; 32]).unwrap().public_key();
    assert_eq!(
        generate(&other_key, &all, &[]).unwrap_err().code(),
        "record-key-mismatch"
    );
    // A release signed by a retired key is withdrawn, and then left out rather than refused.
    let retired = [Withdrawal {
        tag: "v0.1.0".into(),
        reason: "signed by a retired key".into(),
    }];
    let feeds = generate(&other_key, &all, &retired).unwrap();
    assert_eq!(feeds.appcasts[&Target::LinuxX86_64].items.len(), 1);
    // With no committed key, records are not held to one.
    let feeds = feed::generate(&FeedInputs {
        records: &other_key,
        published: &all,
        withdrawn: &[],
        repository: feed::DEFAULT_REPOSITORY,
        public_key: None,
    })
    .unwrap();
    assert_eq!(feeds.appcasts[&Target::LinuxX86_64].items.len(), 2);

    let mut malformed = records.clone();
    malformed[0].schema_version = 0;
    assert_eq!(
        generate(&malformed, &all, &[]).unwrap_err().code(),
        "malformed-record"
    );
    for repository in ["Willyham", "a/b/c", "../x", "owner/na me", "/name"] {
        let error = feed::generate(&FeedInputs {
            records: &records,
            published: &all,
            withdrawn: &[],
            repository,
            public_key: None,
        })
        .unwrap_err();
        assert_eq!(error.code(), "malformed-input", "{repository}");
    }
}

#[test]
fn feeds_leave_out_unnotarized_macos_archives_and_withdrawn_releases_with_notes() {
    let mut records = [
        record("v0.1.0", 5),
        record("v0.2.0", 6),
        record("v0.3.0", 7),
    ];
    records[1]
        .archives
        .get_mut(&Target::MacosArm64)
        .unwrap()
        .signing = SigningStatus::Unsigned;
    let mut releases = published(&records);
    // A withdrawn release's assets need not still be published.
    releases.retain(|r| r.tag != "v0.3.0");
    let withdrawn = [
        Withdrawal {
            tag: "v0.3.0".into(),
            reason: "broken catalog".into(),
        },
        Withdrawal {
            tag: "v9.0.0".into(),
            reason: "never released".into(),
        },
    ];
    let feeds = generate(&records, &releases, &withdrawn).unwrap();
    let builds = |target| {
        feeds.appcasts[&target]
            .items
            .iter()
            .map(|i| i.build)
            .collect::<Vec<_>>()
    };
    assert_eq!(builds(Target::MacosArm64), [5]);
    assert_eq!(builds(Target::LinuxX86_64), [6, 5]);
    assert_eq!(
        feeds.notes,
        [
            "v9.0.0: withdrawn, and no record names it",
            "v0.3.0: withdrawn (broken catalog), not listed",
            "v0.2.0: macos-arm64 archive is unsigned, not notarized; not listed",
        ]
    );
    // The withdrawal list is strict.
    assert_eq!(
        feed::parse_withdrawn(r#"[{"tag":"v0.1.0","reason":"x"}]"#).unwrap()[0].tag,
        "v0.1.0"
    );
    for text in [
        r#"{}"#,
        r#"[{"tag":"v0.1.0"}]"#,
        r#"[{"tag":"v0.1.0","reason":" "}]"#,
        r#"[{"tag":"v0.1.0","reason":"x","targets":[]}]"#,
        r#"[{"tag":"v0.1.0","reason":"x"},{"tag":"v0.1.0","reason":"y"}]"#,
    ] {
        assert_eq!(
            feed::parse_withdrawn(text).unwrap_err().code(),
            "malformed-input"
        );
    }
}

#[test]
fn a_record_naming_windows_gets_a_windows_feed() {
    let mut record = record("v0.4.0", 11);
    let mut windows = archive(Target::WindowsX86_64, "0.4.0", 11, 77);
    windows.file = "luxforge-0.4.0-windows-x86_64.zip".into();
    record.archives.insert(Target::WindowsX86_64, windows);
    let records = [record];
    let feeds = generate(&records, &published(&records), &[]).unwrap();
    assert_eq!(feeds.appcasts.len(), Target::RELEASED.len() + 1);
    assert!(
        feeds.appcasts[&Target::WindowsX86_64].items[0]
            .enclosure
            .url
            .ends_with("/v0.4.0/luxforge-0.4.0-windows-x86_64.zip")
    );
}

#[test]
fn tags_with_build_metadata_are_percent_encoded_in_urls() {
    let records = [record("v0.5.0+build.2", 12)];
    let feeds = generate(&records, &published(&records), &[]).unwrap();
    let item = &feeds.appcasts[&Target::LinuxX86_64].items[0];
    assert_eq!(
        item.enclosure.url,
        "https://github.com/Willyham/luxforge/releases/download/v0.5.0%2Bbuild.2/luxforge-0.5.0%2Bbuild.2-linux-x86_64.tar.gz"
    );
    assert_eq!(item.version, "0.5.0+build.2");
}

#[test]
fn reading_tolerates_sparkle_elements_luxforge_does_not_use() {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lf="https://luxforge.app/xmlns/update">
  <channel>
    <title>Elsewhere</title>
    <item>
      <title>Version 2.0</title>
      <sparkle:version>2.0</sparkle:version>
      <enclosure url="https://example.org/a.zip" length="1" type="application/octet-stream" sparkle:edSignature="{SMALL_SIGNATURE}"/>
    </item>
    <item>
      <title><![CDATA[Luxforge & friends]]></title>
      <sparkle:releaseNotesLink>
        https://example.org/notes.html
      </sparkle:releaseNotesLink>
      <sparkle:criticalUpdate sparkle:version="3"/>
      <sparkle:phasedRolloutInterval>86400</sparkle:phasedRolloutInterval>
      <sparkle:deltas>
        <enclosure url="https://example.org/d.delta" sparkle:deltaFrom="3" length="9" sparkle:edSignature="{SMALL_SIGNATURE}"/>
      </sparkle:deltas>
      <lf:catalogFormat> 2 </lf:catalogFormat>
      <dc:creator>someone</dc:creator>
      <enclosure url="https://example.org/b.tar.gz" sparkle:version="41" sparkle:shortVersionString="0.4.1" length="32" sparkle:edSignature="{SMALL_SIGNATURE}" sparkle:installationType="package"/>
    </item>
    <item>
      <sparkle:version>50</sparkle:version>
      <enclosure url="https://example.org/c.zip" length="5"/>
    </item>
    <item>
      <sparkle:version>51</sparkle:version>
      <sparkle:informationalUpdate/>
      <link>https://example.org/</link>
    </item>
    <item>
      <sparkle:version>52</sparkle:version>
      <luxforge:catalogFormat xmlns:luxforge="https://luxforge.app/xmlns/update">one</luxforge:catalogFormat>
      <enclosure url="https://example.org/e.zip" length="5" sparkle:edSignature="{SMALL_SIGNATURE}"/>
    </item>
    <item>
      <sparkle:version>53</sparkle:version>
      <catalogFormat>7</catalogFormat>
      <sparkle:channel>nightly</sparkle:channel>
      <enclosure url="https://example.org/f.zip" length="6" sparkle:edSignature="{SMALL_SIGNATURE}"/>
    </item>
  </channel>
</rss>"#
    );
    let parsed = Appcast::parse(&xml).unwrap();
    assert_eq!(parsed.appcast.title, "Elsewhere");
    assert_eq!(
        parsed
            .skipped
            .iter()
            .map(|s| (s.index, s.reason.as_str()))
            .collect::<Vec<_>>(),
        [
            (0, "sparkle:version is not a build number"),
            (2, "the enclosure has no sparkle:edSignature"),
            (3, "an informational update has nothing to install"),
            (4, "luxforge:catalogFormat is not a number"),
        ]
    );
    let [first, second] = &parsed.appcast.items[..] else {
        panic!("two items");
    };
    assert_eq!(first.title.as_deref(), Some("Luxforge & friends"));
    assert_eq!((first.build, first.version.as_str()), (41, "0.4.1"));
    assert_eq!(first.catalog_format, Some(2));
    assert_eq!(first.enclosure.url, "https://example.org/b.tar.gz");
    assert_eq!(first.enclosure.length, 32);
    assert_eq!(first.enclosure.mime_type, None);
    assert_eq!(first.channel, None);
    // An element outside the Luxforge namespace is not its catalog format.
    assert_eq!((second.build, second.catalog_format), (53, None));
    assert_eq!(second.version, "53");
    assert_eq!(second.channel.as_deref(), Some("nightly"));

    for (xml, why) in [
        ("<rss><item/></rss>", "no channel"),
        ("<feed><channel/></feed>", "not rss"),
        (
            "<x:rss xmlns:x=\"urn:x\"><channel/></x:rss>",
            "namespaced rss",
        ),
        ("<rss><channel>", "unclosed"),
        (
            "<!DOCTYPE rss [<!ENTITY a \"b\">]><rss><channel/></rss>",
            "a DTD",
        ),
    ] {
        assert_eq!(
            Appcast::parse(xml).unwrap_err().code(),
            "malformed-appcast",
            "{why}"
        );
    }
    let huge = format!(
        "<rss><channel>{}</channel></rss>",
        " ".repeat(MAX_APPCAST_BYTES)
    );
    assert_eq!(
        Appcast::parse(&huge).unwrap_err().code(),
        "malformed-appcast"
    );
    let empty = Appcast::parse("<rss version=\"2.0\"><channel/></rss>").unwrap();
    assert!(empty.appcast.items.is_empty() && empty.skipped.is_empty());
}

#[test]
fn the_newest_item_on_a_followed_channel_above_the_current_build_is_chosen() {
    let records = [
        record("v0.1.0", 5),
        record("v0.2.0-rc.1", 6),
        record("v0.2.0", 7),
        record("v0.3.0-rc.1", 8),
    ];
    let feeds = generate(&records, &published(&records), &[]).unwrap();
    let items = &feeds.appcasts[&Target::LinuxX86_64].items;
    let chosen = |current, followed: &[Channel]| {
        newest_eligible(items, current, followed).map(|item| item.build)
    };
    let stable = [Channel::Default];
    let both = [Channel::Default, Channel::Prerelease];
    assert_eq!(chosen(1, &stable), Some(7));
    assert_eq!(chosen(1, &both), Some(8));
    assert_eq!(chosen(7, &stable), None);
    assert_eq!(chosen(7, &both), Some(8));
    assert_eq!(chosen(8, &both), None);
    assert_eq!(chosen(5, &[Channel::Prerelease]), Some(8));
    assert_eq!(chosen(1, &[]), None);
    // An unknown channel is never followed; of equal builds the first wins.
    let mut odd = items.clone();
    odd[0].channel = Some("nightly".into());
    odd.push(odd[1].clone());
    odd.last_mut().unwrap().link = Some("second".into());
    let chosen = newest_eligible(&odd, 1, &both).unwrap();
    assert_eq!(
        (chosen.build, chosen.link.as_deref()),
        (7, items[1].link.as_deref())
    );
}

#[test]
fn the_update_configuration_is_strict_and_the_checked_in_one_reads() {
    let checked_in = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = UpdateConfig::load_from_repository(&checked_in).unwrap();
    assert_eq!(config.feed_base_url, "https://api.luxforge.app/v1/updates/");
    assert_eq!(
        config.feed_url(Target::MacosArm64),
        "https://api.luxforge.app/v1/updates/macos-arm64/appcast.xml"
    );
    assert_eq!(
        config.feed_url(Target::LinuxX86_64),
        "https://api.luxforge.app/v1/updates/linux-x86_64/appcast.xml"
    );
    let withdrawn = std::fs::read_to_string(checked_in.join(WITHDRAWN_PATH)).unwrap();
    feed::parse_withdrawn(&withdrawn).unwrap();

    let keyed = UpdateConfig::parse(&format!(
        r#"{{"feed_base_url":"https://example.org/u/","public_key":"{SPARKLE_TEST_PUBLIC_KEY}"}}"#
    ))
    .unwrap();
    assert_eq!(keyed.public_key, Some(test_key().public_key()));
    for text in [
        r#"{"feed_base_url":"http://example.org/u/","public_key":null}"#,
        r#"{"feed_base_url":"https://example.org/u","public_key":null}"#,
        r#"{"feed_base_url":"https://example.org/u/?x=1/","public_key":null}"#,
        r#"{"feed_base_url":"https://example.org/u/","public_key":"AAAA"}"#,
        r#"{"feed_base_url":"https://example.org/u/","public_key":null,"extra":1}"#,
    ] {
        assert_eq!(
            UpdateConfig::parse(text).unwrap_err().code(),
            "malformed-config",
            "{text}"
        );
    }
}
