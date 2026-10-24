//! Admission of the expanded app that signing will mutate against its verified unsigned ZIP.
use crate::*;
use std::collections::BTreeMap;

const MAX_ENTRIES: usize = 100_000;
const MAX_BYTES: u64 = 4 << 30;
const MAX_DEPTH: usize = 128;

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory(u32),
    File { mode: u32, size: u64, hash: String },
    Link(PathBuf),
}

/// A bounded snapshot of paths, types, permissions, link targets and streamed file contents.
/// Links are recorded without following them, including framework links to versioned code.
fn contents(root: &Path) -> Result<BTreeMap<PathBuf, Entry>> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    let mut bytes = 0u64;
    while let Some(relative) = pending.pop() {
        ensure(
            entries.len() < MAX_ENTRIES && relative.components().count() <= MAX_DEPTH,
            "Signing bundle exceeds the admission entry/depth bound",
        )?;
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path)?;
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o7777
        };
        #[cfg(not(unix))]
        let mode = u32::from(metadata.permissions().readonly());
        let entry = if metadata.file_type().is_symlink() {
            ensure(
                !relative.as_os_str().is_empty(),
                "Signing bundle root is a link",
            )?;
            Entry::Link(fs::read_link(&path)?)
        } else if metadata.is_dir() {
            for child in fs::read_dir(&path)? {
                pending.push(relative.join(child?.file_name()));
                ensure(
                    pending.len() + entries.len() <= MAX_ENTRIES,
                    "Signing bundle exceeds the admission entry bound",
                )?;
            }
            Entry::Directory(mode)
        } else if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or("Bundle size overflow")?;
            ensure(
                bytes <= MAX_BYTES,
                "Signing bundle exceeds the admission byte bound",
            )?;
            Entry::File {
                mode,
                size: metadata.len(),
                hash: hash(&path)?,
            }
        } else {
            return Err(format!("Unsupported signing bundle entry: {}", relative.display()).into());
        };
        entries.insert(relative, entry);
    }
    Ok(entries)
}

fn compare(expected: &Path, actual: &Path) -> Result {
    let expected = contents(expected)?;
    let actual = contents(actual)?;
    for path in expected.keys().chain(actual.keys()) {
        ensure(
            expected.get(path) == actual.get(path),
            format!(
                "Expanded signing bundle differs from the verified archive at {}; rebuild the candidate",
                path.display()
            ),
        )?;
    }
    Ok(())
}

fn check_executables(extracted: &Path, record: &Value) -> Result {
    for name in release_build::EXECUTABLES {
        let path = extracted
            .join("Contents")
            .join(if *name == "luxforge" {
                "MacOS"
            } else {
                "Helpers"
            })
            .join(name);
        ensure(
            record["executables"]["before_signing"][name] == hash(&path)?,
            format!("Verified signing archive's {name} differs from its before-signing identity"),
        )?;
    }
    Ok(())
}

/// Bound and validate the archive before ditto extracts it. AppleDouble metadata lives beside
/// Luxforge.app under __MACOSX and is restored by ditto rather than compared as another app.
fn check_zip(archive: &Path) -> Result {
    let mut zip = zip::ZipArchive::new(fs::File::open(archive)?)?;
    ensure(
        zip.len() <= MAX_ENTRIES,
        "Signing ZIP exceeds the admission entry bound",
    )?;
    let mut bytes = 0u64;
    let mut paths = std::collections::BTreeSet::new();
    let mut links = std::collections::BTreeSet::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let path = entry
            .enclosed_name()
            .ok_or("Signing ZIP contains an unsafe path")?;
        ensure(
            path.starts_with("Luxforge.app") || path.starts_with("__MACOSX"),
            "Signing ZIP contains an unexpected payload",
        )?;
        ensure(
            path.components().count() <= MAX_DEPTH,
            "Signing ZIP exceeds the admission depth bound",
        )?;
        bytes = bytes.checked_add(entry.size()).ok_or("ZIP size overflow")?;
        ensure(
            bytes <= MAX_BYTES,
            "Signing ZIP exceeds the admission byte bound",
        )?;
        ensure(
            paths.insert(path.clone()),
            "Signing ZIP contains duplicate paths",
        )?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            ensure(entry.size() <= 4096, "Signing ZIP link target is too long")?;
            let mut target = String::new();
            std::io::Read::read_to_string(&mut std::io::Read::take(&mut entry, 4097), &mut target)?;
            ensure(target.len() <= 4096, "Signing ZIP link target is too long")?;
            let mut resolved = path
                .parent()
                .ok_or("Signing ZIP link has no parent")?
                .to_path_buf();
            for component in Path::new(&target).components() {
                match component {
                    std::path::Component::Normal(name) => resolved.push(name),
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        ensure(resolved.pop(), "Signing ZIP link escapes its payload")?;
                    }
                    _ => return Err("Signing ZIP link escapes its payload".into()),
                }
            }
            ensure(
                resolved.starts_with("Luxforge.app"),
                "Signing ZIP link escapes its app",
            )?;
            links.insert(path);
        }
    }
    ensure(
        paths.iter().all(|path| {
            path.ancestors()
                .skip(1)
                .all(|parent| !links.contains(parent))
        }),
        "Signing ZIP places an entry below a symbolic link",
    )?;
    Ok(())
}

/// Extract into fresh staging, retaining macOS metadata, permissions and symbolic links. Never
/// replaces or edits the existing app; a mismatch fails before signing begins.
pub(super) fn verify(archive: &Path, app: &Path, record: &Value) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Signing bundle admission requires macOS ditto",
    )?;
    check_zip(archive)?;
    let staging = tempfile::tempdir()?;
    let result = Command::new("ditto")
        .args(["-x", "-k"])
        .arg(archive)
        .arg(staging.path())
        .output()?;
    ensure(
        result.status.success(),
        "Extracting the verified signing ZIP failed",
    )?;
    let roots = fs::read_dir(staging.path())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure(
        roots == [OsString::from("Luxforge.app")],
        "Signing ZIP must extract exactly Luxforge.app",
    )?;
    let extracted = staging.path().join("Luxforge.app");
    check_executables(&extracted, record)?;
    compare(&extracted, app)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(app: &Path) -> Value {
        let mut record = json!({"executables": {"before_signing": {}}});
        for name in release_build::EXECUTABLES {
            let path = app
                .join("Contents")
                .join(if *name == "luxforge" {
                    "MacOS"
                } else {
                    "Helpers"
                })
                .join(name);
            record["executables"]["before_signing"][name] = json!(hash(&path).unwrap());
        }
        record
    }

    fn bundle(root: &Path) {
        for (name, bytes) in [
            ("Contents/MacOS/luxforge", b"editor".as_slice()),
            ("Contents/Helpers/luxforge-json", b"helper".as_slice()),
            ("Contents/Helpers/luxforge-ctl", b"live helper".as_slice()),
            ("Contents/Helpers/luxforge-mcp", b"mcp helper".as_slice()),
            ("Contents/Info.plist", b"identity".as_slice()),
            ("Contents/Resources/lensfun/data", b"resource".as_slice()),
            (
                "Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle",
                b"library".as_slice(),
            ),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }

    #[test]
    fn signing_archive_must_match_its_before_signing_executable_identity() {
        let temporary = tempfile::tempdir().unwrap();
        bundle(temporary.path());
        let mut record = record(temporary.path());
        check_executables(temporary.path(), &record).unwrap();
        record["executables"]["before_signing"]["luxforge"] = json!("f".repeat(64));
        assert!(check_executables(temporary.path(), &record).is_err());
    }

    #[test]
    fn signing_zip_refuses_foreign_paths_and_writes_through_links() {
        let temporary = tempfile::tempdir().unwrap();
        for (name, target, child) in [
            ("foreign/link", "inside", false),
            ("Luxforge.app/link", "../../outside", false),
            ("Luxforge.app/link", "Contents", true),
        ] {
            let archive = temporary.path().join("input.zip");
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.add_symlink(name, target, options).unwrap();
            if child {
                zip.start_file("Luxforge.app/link/file", options).unwrap();
                std::io::Write::write_all(&mut zip, b"file").unwrap();
            }
            zip.finish().unwrap();
            assert!(check_zip(&archive).is_err(), "{name}: {target}");
        }
    }

    #[test]
    fn signing_admission_refuses_modified_missing_and_extra_bundle_files() {
        let temporary = tempfile::tempdir().unwrap();
        let expected = temporary.path().join("expected");
        let actual = temporary.path().join("actual");
        bundle(&expected);
        bundle(&actual);
        compare(&expected, &actual).unwrap();
        for file in [
            "Contents/MacOS/luxforge",
            "Contents/Helpers/luxforge-json",
            "Contents/Helpers/luxforge-mcp",
            "Contents/Info.plist",
            "Contents/Resources/lensfun/data",
            "Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle",
        ] {
            let path = actual.join(file);
            let original = fs::read(&path).unwrap();
            fs::write(&path, b"changed").unwrap();
            assert!(compare(&expected, &actual).is_err(), "{file}");
            fs::remove_file(&path).unwrap();
            assert!(compare(&expected, &actual).is_err(), "missing {file}");
            fs::write(path, original).unwrap();
            compare(&expected, &actual).unwrap();
        }
        fs::write(actual.join("Contents/extra"), b"extra").unwrap();
        assert!(compare(&expected, &actual).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn signing_admission_preserves_framework_links_and_executable_modes() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let temporary = tempfile::tempdir().unwrap();
        let expected = temporary.path().join("expected");
        let actual = temporary.path().join("actual");
        for root in [&expected, &actual] {
            bundle(root);
            symlink(
                "B",
                root.join("Contents/Frameworks/Sparkle.framework/Versions/Current"),
            )
            .unwrap();
            fs::set_permissions(
                root.join("Contents/MacOS/luxforge"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        compare(&expected, &actual).unwrap();
        let executable = actual.join("Contents/MacOS/luxforge");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(compare(&expected, &actual).is_err());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let link = actual.join("Contents/Frameworks/Sparkle.framework/Versions/Current");
        fs::remove_file(&link).unwrap();
        symlink("elsewhere", &link).unwrap();
        assert!(compare(&expected, &actual).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[luxforge_test_macros::slow]
    fn signing_admission_checks_the_actual_metadata_preserving_zip() {
        let temporary = tempfile::tempdir().unwrap();
        let app = temporary.path().join("Luxforge.app");
        bundle(&app);
        std::os::unix::fs::symlink(
            "B",
            app.join("Contents/Frameworks/Sparkle.framework/Versions/Current"),
        )
        .unwrap();
        let record = record(&app);
        let archive = release_build::archive_payload(
            temporary.path(),
            &xtask_cli::release_tag::ReleaseTag::parse("v0.1").unwrap(),
            release_build::TARGETS[0].0,
        )
        .unwrap();
        verify(&archive, &app, &record).unwrap();
        fs::write(app.join("Contents/Resources/lensfun/data"), b"modified").unwrap();
        assert!(verify(&archive, &app, &record).is_err());
    }
}
