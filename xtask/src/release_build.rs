//! Versioned build candidates. Native qualification and distribution approval are separate gates.
use crate::*;
use xtask_cli::release_candidate;
use xtask_cli::release_tag::ReleaseTag;

pub use xtask_cli::release_artifacts::{
    TARGETS, archive_name, archive_record, disk_image_name, platform, verify_record,
};
pub use xtask_cli::release_candidate::EXECUTABLES;
/// `LSMinimumSystemVersion`, which `target.json` also records for the update feed.
pub const MACOS_MINIMUM_SYSTEM_VERSION: &str = "14.0";

/// What a macOS bundle's `Info.plist` states.
pub struct BundleInfo<'a> {
    pub identifier: &'a str,
    /// `CFBundleShortVersionString`, the tag's bundle version.
    pub version: &'a str,
    /// `CFBundleVersion`, the build number Sparkle orders updates by.
    pub build: &'a str,
    /// `SUFeedURL`: the target's appcast.
    pub feed_url: &'a str,
    /// `SUPublicEDKey`, the committed update key; `None` while none is committed, when the app
    /// reports `no-update-key`.
    pub public_key: Option<&'a str>,
    /// Further `<key>…</key><value/>` pairs, already XML: the update journey's.
    pub extra: &'a str,
}

/// The release bundle's `Info.plist`, with Sparkle's settings
/// (`docs/design/auto-update.md#macos-sparkle`): the feed and key, automatic checks once a day
/// (which also suppresses Sparkle's permission prompt), and an archive verified before it is
/// extracted. The feed itself is not required to be signed (`SURequireSignedFeed` is absent): the
/// update key signs only archives, and stays in the release workflow's one signing step.
pub fn info_plist(info: &BundleInfo) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let key = info.public_key.map_or_else(String::new, |key| {
        format!("<key>SUPublicEDKey</key><string>{}</string>", escape(key))
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{}</string><key>CFBundleName</key><string>Luxforge</string><key>CFBundleExecutable</key><string>luxforge</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>{}</string><key>CFBundleVersion</key><string>{}</string><key>LSMinimumSystemVersion</key><string>{MACOS_MINIMUM_SYSTEM_VERSION}</string><key>NSHighResolutionCapable</key><true/><key>SUFeedURL</key><string>{}</string>{key}<key>SUEnableAutomaticChecks</key><true/><key>SUScheduledCheckInterval</key><integer>86400</integer><key>SUVerifyUpdateBeforeExtraction</key><true/>{}</dict></plist>"#,
        escape(info.identifier),
        escape(info.version),
        escape(info.build),
        escape(info.feed_url),
        info.extra,
    )
}

pub fn executable_paths(out: &Path, target: &str) -> Vec<PathBuf> {
    EXECUTABLES
        .iter()
        .map(|name| {
            if target == TARGETS[0].0 {
                out.join("Luxforge.app/Contents")
                    .join(if *name == "luxforge" {
                        "MacOS"
                    } else {
                        "Helpers"
                    })
                    .join(name)
            } else {
                out.join("Luxforge").join(name)
            }
        })
        .collect()
}

pub fn archive_payload(out: &Path, tag: &ReleaseTag, target: &str) -> Result<PathBuf> {
    let archive = out.join(archive_name(tag, target)?);
    if target == TARGETS[0].0 {
        ensure(
            cfg!(target_os = "macos"),
            "macOS ZIP creation requires macOS ditto",
        )?;
        let status = Command::new("ditto")
            .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
            .arg(out.join("Luxforge.app"))
            .arg(&archive)
            .status()?;
        ensure(
            status.success(),
            "Metadata-preserving macOS ZIP creation failed",
        )?;
    } else {
        package::archive(&out.join("Luxforge"), &archive)?;
    }
    Ok(archive)
}

/// Creates the macOS download from `out/Luxforge.app`, replacing an earlier one: a compressed,
/// read-only HFS+ disk image named `Luxforge` holding a copy of the app and an `Applications`
/// link, in Finder's default window. hdiutil's occasional "Resource busy" on hosted runners is
/// retried twice.
pub fn create_disk_image(out: &Path, tag: &ReleaseTag) -> Result<PathBuf> {
    ensure(
        cfg!(target_os = "macos"),
        "macOS disk image creation requires hdiutil",
    )?;
    let name = disk_image_name(tag, TARGETS[0].0)?.ok_or("macOS has a disk image")?;
    let image = out.join(name);
    let staging = out.join("disk-image-staging");
    ensure(
        !staging.exists(),
        "A previous disk image staging folder remains",
    )?;
    fs::create_dir(&staging)?;
    let outcome = (|| -> Result {
        let status = Command::new("ditto")
            .arg(out.join("Luxforge.app"))
            .arg(staging.join("Luxforge.app"))
            .status()?;
        ensure(
            status.success(),
            "Copying the app into the disk image staging folder failed",
        )?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("/Applications", staging.join("Applications"))?;
        if image.exists() {
            fs::remove_file(&image)?;
        }
        let mut failure = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                let _ = fs::remove_file(&image);
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            let result = Command::new("hdiutil")
                .args(["create", "-quiet", "-volname", "Luxforge", "-fs", "HFS+"])
                .args(["-format", "UDZO", "-imagekey", "zlib-level=9", "-srcfolder"])
                .arg(&staging)
                .arg(&image)
                .output()?;
            if result.status.success() {
                return Ok(());
            }
            failure = String::from_utf8_lossy(&result.stderr).trim().to_owned();
        }
        Err(format!("Disk image creation failed: {failure}").into())
    })();
    let cleanup = fs::remove_dir_all(&staging);
    outcome?;
    cleanup?;
    Ok(image)
}

pub fn build(root: &Path, out: &Path, tag: &str, number: &str, allow_dirty: bool) -> Result {
    let tag = ReleaseTag::parse(tag)?;
    release_candidate::check_build_number(number)?;
    let target = host(root)?;
    let release_candidate::Source {
        revision,
        dirty,
        lock_sha256: lock,
    } = release_candidate::check_source(root, out, &tag, &target, allow_dirty)?;
    let toolchain = release_candidate::toolchain(root, allow_dirty)?;
    let environment = release_candidate::build_environment(root, &target)?;
    // A clean tag build embeds its revision, so its usage statistics release is
    // `<version>+<revision>`; a dirty rehearsal embeds none and is ineligible to report.
    let embedded = package::embedded_revision(&revision, dirty);
    let mut command = cargo_command();
    package::embed_revision(&mut command, embedded);
    let status = command
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "--release",
            "--package",
            "luxforge-app",
            "--package",
            "luxforge-cli",
        ])
        .env("LUXFORGE_RELEASE_VERSION", &tag.version)
        .status()?;
    ensure(status.success(), "Release binary build failed")?;
    ensure(
        hash(&root.join("Cargo.lock"))? == lock,
        "Lockfile changed during the build",
    )?;
    ensure(
        output(root, "git", &["rev-parse", "HEAD"])?.trim() == revision,
        "Source revision changed during the build",
    )?;
    // The macOS bundle embeds the pinned Sparkle, fetched (and verified) before anything is built.
    let sparkle = if target == TARGETS[0].0 {
        Some(sparkle::fetch(root)?)
    } else {
        None
    };
    fs::create_dir_all(out)?;
    let resource_root = if let Some(sparkle) = &sparkle {
        let contents = out.join("Luxforge.app/Contents");
        fs::create_dir_all(contents.join("MacOS"))?;
        fs::create_dir_all(contents.join("Helpers"))?;
        let config = luxforge_update::UpdateConfig::load_from_repository(root)?;
        let public_key = config.public_key.map(|key| key.to_base64());
        fs::write(
            contents.join("Info.plist"),
            info_plist(&BundleInfo {
                identifier: "org.luxforge.app",
                version: &tag.bundle_version,
                build: number,
                feed_url: &config.feed_url(luxforge_update::Target::MacosArm64),
                public_key: public_key.as_deref(),
                extra: "",
            }),
        )?;
        sparkle::embed(sparkle, &out.join("Luxforge.app"))?;
        contents.join("Resources")
    } else {
        out.join("Luxforge")
    };
    fs::create_dir_all(&resource_root)?;
    let binaries = executable_paths(out, &target);
    let mut executable_hashes = serde_json::Map::new();
    for (name, destination) in EXECUTABLES.iter().zip(&binaries) {
        fs::copy(binary(root)?.with_file_name(name), destination)?;
        let version = output(root, destination, &["--version"])?;
        ensure(
            version.trim() == format!("{name} {}", tag.version),
            format!("Packaged {name} has the wrong version"),
        )?;
        executable_hashes.insert((*name).to_owned(), json!(hash(destination)?));
    }
    package::lens_resources(root, &resource_root.join("lensfun"))?;
    fs::copy(root.join("LICENSE"), resource_root.join("LICENSE"))?;
    package::inventory(root, &resource_root.join("notices"))?;
    fs::write(
        resource_root.join("README.txt"),
        format!(
            "Luxforge {} build candidate ({}, {}).\nThis is not a qualified public release. Source/licence/advisory review and native GPU/corpus/installation qualification remain open. macOS signing/notarization status is recorded beside the downloads in target.json.\n\nmacOS: open the disk image (.dmg) and drag Luxforge to Applications, then open it from Applications; command lines are in Luxforge.app/Contents/Helpers. The macOS ZIP holds the same app and is the update archive. Linux: extract Luxforge and run ./Luxforge/luxforge; command lines are adjacent. Linux requires compatible Vulkan drivers, libxkbcommon/X11/Wayland libraries and a working XDG file-chooser portal/backend.\n\nUpdates replace the application, keeping originals, catalogs, artifacts and settings. Back up catalogs with associated artifacts while the owner is closed. Unsupported catalog/recipe formats are refused, with no migrations promised.\n",
            tag.version, tag.tag, target
        ),
    )?;
    // The build's identity, inside the payload as `build.json` and beside it as `target.json`. It
    // names executables, never the archive or bundle that holds it.
    let mut metadata = json!({"schema_version":release_candidate::SCHEMA_VERSION,"tag":tag.tag,"version":tag.version,"build_number":number,"catalog_format":luxforge_core::CATALOG_FORMAT,"revision":revision,"target":target,"platform":platform(&target)?,"profile":"release","working_tree_dirty":dirty,"usage_release":luxforge_core::usage::BuildIdentity::new(&tag.version,embedded).release(),"rehearsal":allow_dirty,"lock_sha256":lock,"toolchain":toolchain,"build_environment":environment,"executables":release_candidate::executables(&target,executable_hashes),"release_ready":false});
    if target == TARGETS[0].0 {
        metadata["minimum_system_version"] = json!(MACOS_MINIMUM_SYSTEM_VERSION);
    }
    write_json(&resource_root.join("build.json"), &metadata)?;
    let archive = archive_payload(out, &tag, &target)?;
    let disk_image = if target == TARGETS[0].0 {
        Some(create_disk_image(out, &tag)?)
    } else {
        None
    };
    ensure(
        hash(&root.join("Cargo.lock"))? == lock
            && output(root, "git", &["rev-parse", "HEAD"])?.trim() == revision,
        "Source identity changed during packaging",
    )?;
    let dirty_after = release_candidate::working_tree_dirty(root)?;
    ensure(
        allow_dirty || !dirty_after,
        "Source files changed during packaging",
    )?;
    let mut record = metadata;
    record["working_tree_dirty"] = json!(dirty || dirty_after);
    record["signing"] = json!({"status":release_candidate::NOT_APPLICABLE});
    if target == TARGETS[0].0 {
        record["signing"] = json!({"status":"unsigned","disk_image":{"status":"unsigned"}});
    }
    record["archive"] = archive_record(&archive)?;
    if let Some(image) = &disk_image {
        record["disk_image"] = archive_record(image)?;
    }
    write_json(&out.join("target.json"), &record)?;
    verify_record(out, &tag, &target)?;
    println!("Build candidate: {}", archive.display());
    if let Some(image) = disk_image {
        println!("Disk image (unsigned): {}", image.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_info_plist_names_sparkles_feed_and_only_a_committed_key() {
        let info = |public_key| {
            info_plist(&BundleInfo {
                identifier: "org.luxforge.app",
                version: "0.2.0",
                build: "42",
                feed_url: "https://api.luxforge.app/v1/updates/macos-arm64/appcast.xml?a&b",
                public_key,
                extra: "<key>LSBackgroundOnly</key><true/>",
            })
        };
        let keyed = info(Some("rg7plRI+lkAvslNaVkA284ovU8kr7pm2xW1/AQL86Cs="));
        for pair in [
            "<key>CFBundleShortVersionString</key><string>0.2.0</string>",
            "<key>CFBundleVersion</key><string>42</string>",
            "<key>SUFeedURL</key><string>https://api.luxforge.app/v1/updates/macos-arm64/appcast.xml?a&amp;b</string>",
            "<key>SUPublicEDKey</key><string>rg7plRI+lkAvslNaVkA284ovU8kr7pm2xW1/AQL86Cs=</string>",
            "<key>SUEnableAutomaticChecks</key><true/>",
            "<key>SUScheduledCheckInterval</key><integer>86400</integer>",
            "<key>SUVerifyUpdateBeforeExtraction</key><true/>",
            "<key>LSBackgroundOnly</key><true/></dict></plist>",
        ] {
            assert!(keyed.contains(pair), "{pair}");
        }
        assert!(!keyed.contains("SURequireSignedFeed"));
        let unkeyed = info(None);
        assert!(!unkeyed.contains("SUPublicEDKey"));
        assert!(unkeyed.contains("<key>SUFeedURL</key>"));
        // The committed configuration's feed is the macOS target's.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let config = luxforge_update::UpdateConfig::load_from_repository(root).unwrap();
        assert_eq!(
            config.feed_url(luxforge_update::Target::MacosArm64),
            "https://api.luxforge.app/v1/updates/macos-arm64/appcast.xml"
        );
    }

    #[test]
    #[cfg(unix)]
    fn linux_archive_preserves_all_cli_executable_modes() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let payload = tmp.path().join("Luxforge");
        fs::create_dir(&payload).unwrap();
        for name in EXECUTABLES {
            fs::write(payload.join(name), b"executable").unwrap();
            fs::set_permissions(payload.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = archive_payload(
            tmp.path(),
            &ReleaseTag::parse("v0.1").unwrap(),
            TARGETS[1].0,
        )
        .unwrap();
        let mut archive =
            tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(path).unwrap()));
        let mut found = 0;
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            if entry.header().entry_type().is_file() {
                assert_eq!(entry.header().mode().unwrap() & 0o777, 0o755);
                found += 1;
            }
        }
        assert_eq!(found, 3);
    }
}
