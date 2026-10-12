//! The Sparkle 2 framework the macOS release bundle embeds (`docs/design/auto-update.md#macos-sparkle`):
//! the pinned official release archive, fetched once into `target/sparkle/` and refused unless its
//! length and SHA-256 are the pinned ones, and the framework copied into an app bundle without its
//! XPC services. No binary is committed; `crates/luxforge-sparkle` loads the framework at run time.
use crate::*;

/// The Sparkle release, its tag's commit, and its official archive.
pub const VERSION: &str = "2.10.0";
pub const REVISION: &str = "eef1a539a373c1f1a320624b1130fc5de7b2e100";
pub const URL: &str =
    "https://github.com/sparkle-project/Sparkle/releases/download/2.10.0/Sparkle-2.10.0.tar.xz";
pub const BYTES: u64 = 16_319_840;
pub const SHA256: &str = "c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c";

/// `Contents/Frameworks/Sparkle.framework` in `app`.
pub fn framework_in(app: &Path) -> PathBuf {
    app.join("Contents/Frameworks/Sparkle.framework")
}

/// The framework's code in the order it is signed, innermost first: `Autoupdate`, `Updater.app`,
/// then the framework itself, each by its versioned path (Sparkle's own Developer ID guidance).
pub fn signing_order(app: &Path) -> [PathBuf; 3] {
    let framework = framework_in(app);
    [
        framework.join("Versions/B/Autoupdate"),
        framework.join("Versions/B/Updater.app"),
        framework,
    ]
}

/// The pinned archive's `Sparkle.framework` and `LICENSE`, extracted under
/// `target/sparkle/<version>/extracted/`. The archive is downloaded when missing, and its length
/// and SHA-256 are checked before every extraction; a download that does not match is deleted.
pub fn fetch(root: &Path) -> Result<PathBuf> {
    ensure(
        cfg!(target_os = "macos"),
        "The Sparkle framework is embedded on macOS only",
    )?;
    let cache = root.join("target/sparkle").join(VERSION);
    let extracted = cache.join("extracted");
    let marker = extracted.join(".sha256");
    if fs::read_to_string(&marker).is_ok_and(|text| text == SHA256)
        && extracted.join("Sparkle.framework/Versions/B/Sparkle").is_file()
    {
        return Ok(extracted);
    }
    fs::create_dir_all(&cache)?;
    let archive = cache.join(format!("Sparkle-{VERSION}.tar.xz"));
    if verify(&archive).is_err() {
        let partial = cache.join(format!("Sparkle-{VERSION}.tar.xz.partial"));
        let _ = fs::remove_file(&partial);
        let status = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-filesize",
                &(BYTES + 1).to_string(),
                "--output",
            ])
            .arg(&partial)
            .arg(URL)
            .status()?;
        ensure(status.success(), format!("Downloading {URL} failed"))?;
        if let Err(error) = verify(&partial) {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
        fs::rename(&partial, &archive)?;
    }
    verify(&archive)?;
    let extracting = cache.join("extracting");
    for stale in [&extracting, &extracted] {
        if stale.exists() {
            fs::remove_dir_all(stale)?;
        }
    }
    fs::create_dir(&extracting)?;
    let status = Command::new("tar")
        .arg("-xJf")
        .arg(&archive)
        .arg("-C")
        .arg(&extracting)
        .args(["./Sparkle.framework", "./LICENSE"])
        .status()?;
    ensure(status.success(), "Extracting the Sparkle archive failed")?;
    fs::write(extracting.join(".sha256"), SHA256)?;
    fs::rename(&extracting, &extracted)?;
    Ok(extracted)
}

/// Whether `archive` is the pinned one: its length first, then its SHA-256.
fn verify(archive: &Path) -> Result {
    let length = fs::metadata(archive)?.len();
    ensure(
        length == BYTES,
        format!("The Sparkle archive is {length} bytes, not the pinned {BYTES}"),
    )?;
    let digest = hash(archive)?;
    ensure(
        digest == SHA256,
        format!("The Sparkle archive's SHA-256 is {digest}, not the pinned {SHA256}"),
    )
}

/// Copies `extracted`'s framework into `app` with its symbolic links, without its XPC services
/// (Luxforge is not sandboxed), and seals the changed framework ad hoc so an unsigned candidate is
/// consistent; `release-sign` signs it again with the release identity.
pub fn embed(extracted: &Path, app: &Path) -> Result {
    let destination = framework_in(app);
    ensure(!destination.exists(), "The app already embeds Sparkle")?;
    fs::create_dir_all(destination.parent().ok_or("Frameworks folder")?)?;
    let status = Command::new("ditto")
        .arg(extracted.join("Sparkle.framework"))
        .arg(&destination)
        .status()?;
    ensure(status.success(), "Copying Sparkle.framework failed")?;
    strip_xpc_services(&destination)?;
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(&destination)
        .status()?;
    ensure(status.success(), "Sealing Sparkle.framework failed")
}

/// Removes the framework's XPC services, the versioned folder and its top-level link, and checks
/// what remains: the framework binary, `Autoupdate` and `Updater.app`, reached through links.
fn strip_xpc_services(framework: &Path) -> Result {
    let link = framework.join("XPCServices");
    if fs::symlink_metadata(&link).is_ok() {
        fs::remove_file(&link)?;
    }
    let services = framework.join("Versions/B/XPCServices");
    if services.exists() {
        fs::remove_dir_all(&services)?;
    }
    for required in ["Sparkle", "Autoupdate", "Updater.app"] {
        ensure(
            fs::symlink_metadata(framework.join(required))?.is_symlink()
                && framework.join("Versions/B").join(required).exists(),
            format!("Sparkle.framework lacks {required}"),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A framework laid out as the archive's: versioned contents and top-level links.
    #[cfg(unix)]
    fn framework(dir: &Path) -> PathBuf {
        use std::os::unix::fs::symlink;
        let framework = dir.join("Sparkle.framework");
        let b = framework.join("Versions/B");
        fs::create_dir_all(b.join("Updater.app/Contents")).unwrap();
        fs::create_dir_all(b.join("XPCServices/Installer.xpc")).unwrap();
        fs::write(b.join("Sparkle"), b"binary").unwrap();
        fs::write(b.join("Autoupdate"), b"tool").unwrap();
        symlink("B", framework.join("Versions/Current")).unwrap();
        for name in ["Sparkle", "Autoupdate", "Updater.app", "XPCServices"] {
            symlink(format!("Versions/Current/{name}"), framework.join(name)).unwrap();
        }
        framework
    }

    #[test]
    #[cfg(unix)]
    fn the_xpc_services_are_removed_and_the_links_kept() {
        let dir = tempfile::tempdir().unwrap();
        let framework = framework(dir.path());
        strip_xpc_services(&framework).unwrap();
        assert!(fs::symlink_metadata(framework.join("XPCServices")).is_err());
        assert!(!framework.join("Versions/B/XPCServices").exists());
        assert!(
            fs::symlink_metadata(framework.join("Updater.app"))
                .unwrap()
                .is_symlink()
        );
        // A framework missing its installer is refused.
        fs::remove_file(framework.join("Versions/B/Autoupdate")).unwrap();
        assert!(strip_xpc_services(&framework).is_err());
    }

    #[test]
    fn nested_code_is_signed_innermost_first() {
        let order = signing_order(Path::new("out/Luxforge.app"));
        assert_eq!(
            order.map(|p| p.to_string_lossy().into_owned()),
            [
                "out/Luxforge.app/Contents/Frameworks/Sparkle.framework/Versions/B/Autoupdate",
                "out/Luxforge.app/Contents/Frameworks/Sparkle.framework/Versions/B/Updater.app",
                "out/Luxforge.app/Contents/Frameworks/Sparkle.framework",
            ]
        );
    }

    #[test]
    fn only_the_pinned_archive_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Sparkle.tar.xz");
        fs::write(&archive, b"not the archive").unwrap();
        assert!(
            verify(&archive)
                .unwrap_err()
                .to_string()
                .contains("not the pinned 16319840")
        );
        // Of the pinned length but other bytes.
        fs::write(&archive, vec![0; BYTES as usize]).unwrap();
        assert!(verify(&archive).unwrap_err().to_string().contains(SHA256));
        assert!(URL.contains(VERSION) && URL.starts_with("https://github.com/sparkle-project/"));
    }
}
