//! Versioned build candidates. Native qualification and distribution approval are separate gates.
use crate::*;
use xtask_cli::release_tag::ReleaseTag;

pub use xtask_cli::release_artifacts::{
    TARGETS, archive_name, archive_record, platform, verify_record,
};
pub const EXECUTABLES: &[&str] = &["luxforge", "luxforge-json", "luxforge-ctl"];
/// `LSMinimumSystemVersion`, which `target.json` also records for the update feed.
pub const MACOS_MINIMUM_SYSTEM_VERSION: &str = "14.0";

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

fn build_number(value: &str) -> Result {
    ensure(
        !value.is_empty()
            && value.len() <= 18
            && value.bytes().all(|b| b.is_ascii_digit())
            && !value.starts_with('0'),
        "Build number must be a positive decimal integer, at most 18 digits",
    )
}

pub fn build(root: &Path, out: &Path, tag: &str, number: &str, allow_dirty: bool) -> Result {
    let tag = ReleaseTag::parse(tag)?;
    build_number(number)?;
    let target = host(root)?;
    platform(&target)?;
    ensure(!out.exists(), "Release build output must be new")?;
    let revision = output(root, "git", &["rev-parse", "HEAD"])?
        .trim()
        .to_owned();
    let dirty = !output(root, "git", &["status", "--porcelain"])?
        .trim()
        .is_empty();
    ensure(
        allow_dirty || !dirty,
        "Release builds require a clean checkout; --allow-dirty is a local rehearsal only",
    )?;
    if !allow_dirty {
        let tagged = output(
            root,
            "git",
            &["rev-parse", &format!("refs/tags/{}^{{commit}}", tag.tag)],
        )?;
        ensure(
            tagged.trim() == revision,
            "Release tag does not name the checked-out commit",
        )?;
    }
    let lock = hash(&root.join("Cargo.lock"))?;
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
    fs::create_dir_all(out)?;
    let resource_root = if target == TARGETS[0].0 {
        let contents = out.join("Luxforge.app/Contents");
        fs::create_dir_all(contents.join("MacOS"))?;
        fs::create_dir_all(contents.join("Helpers"))?;
        fs::write(
            contents.join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.luxforge.app</string><key>CFBundleName</key><string>Luxforge</string><key>CFBundleExecutable</key><string>luxforge</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>{}</string><key>CFBundleVersion</key><string>{number}</string><key>LSMinimumSystemVersion</key><string>{MACOS_MINIMUM_SYSTEM_VERSION}</string><key>NSHighResolutionCapable</key><true/></dict></plist>"#,
                tag.bundle_version
            ),
        )?;
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
            "Luxforge {} build candidate ({}, {}).\nThis is not a qualified public release. Source/licence/advisory review and native GPU/corpus/installation qualification remain open. macOS signing/notarization status is recorded beside the archive in target.json.\n\nmacOS: copy Luxforge.app to Applications; command lines are in Contents/Helpers. Linux: extract Luxforge and run ./Luxforge/luxforge; command lines are adjacent. Linux requires compatible Vulkan drivers, libxkbcommon/X11/Wayland libraries and a working XDG file-chooser portal/backend.\n\nUpdates replace the application, keeping originals, catalogs, artifacts and settings. Back up catalogs with associated artifacts while the owner is closed. Unsupported catalog/recipe formats are refused, with no migrations promised.\n",
            tag.version, tag.tag, target
        ),
    )?;
    let metadata = json!({"schema_version":1,"tag":tag.tag,"version":tag.version,"build_number":number,"catalog_format":luxforge_core::CATALOG_FORMAT,"revision":revision,"target":target,"profile":"release","working_tree_dirty":dirty,"usage_release":luxforge_core::usage::BuildIdentity::new(&tag.version,embedded).release(),"rehearsal":allow_dirty,"lock_sha256":lock,"rustc":output(root,"rustc",&["--version"])?.trim(),"executable_hash_kind":"before-signing","executables":executable_hashes,"release_ready":false});
    write_json(&resource_root.join("build.json"), &metadata)?;
    let archive = archive_payload(out, &tag, &target)?;
    ensure(
        hash(&root.join("Cargo.lock"))? == lock
            && output(root, "git", &["rev-parse", "HEAD"])?.trim() == revision,
        "Source identity changed during packaging",
    )?;
    let dirty_after = !output(root, "git", &["status", "--porcelain"])?
        .trim()
        .is_empty();
    ensure(
        allow_dirty || !dirty_after,
        "Source files changed during packaging",
    )?;
    let mut record = metadata;
    record["working_tree_dirty"] = json!(dirty || dirty_after);
    record["signing"] =
        json!({"status":if target == TARGETS[0].0 {"unsigned"} else {"not-applicable"}});
    if target == TARGETS[0].0 {
        record["minimum_system_version"] = json!(MACOS_MINIMUM_SYSTEM_VERSION);
    }
    record["archive"] = archive_record(&archive)?;
    write_json(&out.join("target.json"), &record)?;
    println!("Build candidate: {}", archive.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
