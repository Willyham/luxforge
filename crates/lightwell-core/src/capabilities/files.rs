//! Scoped access to files a person selected: canonical paths, the declared mode and the identity a
//! later read checks against. The host reads these files and never writes them; nothing here opens
//! a file for writing.
use crate::{
    Error, ErrorKind,
    editor::{SourceSignature, source_signature},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

/// The most a read of a selected file returns unless a capability declares less.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Whether a `file` setting holds one file or a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileMode {
    File,
    Directory,
}

/// A selected path, canonicalized when it was chosen, with the identity it had then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedFile {
    pub canonical: PathBuf,
    pub mode: FileMode,
    pub(crate) signature: SourceSignature,
}

/// Canonicalize `path`, which must exist and be of the declared `mode`, and record its identity.
pub fn select(path: &Path, mode: FileMode) -> Result<SelectedFile, Error> {
    let canonical = path.canonicalize().map_err(|error| {
        Error::new(
            ErrorKind::FileAccess,
            format!("cannot resolve {}: {}", path.display(), error.kind()),
        )
    })?;
    let metadata = canonical
        .metadata()
        .map_err(|error| Error::new(ErrorKind::FileAccess, error.kind().to_string()))?;
    let matches = match mode {
        FileMode::File => metadata.is_file(),
        FileMode::Directory => metadata.is_dir(),
    };
    if !matches {
        return Err(Error::new(
            ErrorKind::Validation,
            format!(
                "{} is not a {}",
                canonical.display(),
                match mode {
                    FileMode::File => "file",
                    FileMode::Directory => "directory",
                }
            ),
        ));
    }
    let signature = source_signature(&canonical, &metadata);
    Ok(SelectedFile {
        canonical,
        mode,
        signature,
    })
}

fn refused(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn unreadable(path: &Path, error: &io::Error) -> Error {
    Error::new(
        ErrorKind::FileAccess,
        format!("cannot read {}: {}", path.display(), error.kind()),
    )
}

/// Read the selected file, at most `max_bytes` of it. The file must have the identity, length and
/// modification time it had when it was selected before it is opened, through the open handle and
/// after it is read; otherwise the read fails with `conflict` and the person selects it again.
pub fn read_bounded(selected: &SelectedFile, max_bytes: u64) -> Result<Vec<u8>, Error> {
    if selected.mode != FileMode::File {
        return Err(refused(format!(
            "{} is a directory; name a file inside it",
            selected.canonical.display()
        )));
    }
    read_unchanged(
        &selected.canonical,
        &selected.signature,
        max_bytes,
        "file changed since it was selected",
    )
}

/// Read the file at `relative` inside a selected directory, at most `max_bytes` of it. `relative`
/// may contain only normal names: no root, drive prefix, `.` or `..`. Its canonical path must stay
/// inside the directory, so a symbolic link that leads out of it is refused, and the file must not
/// change while it is read.
pub fn read_in_directory(
    selected: &SelectedFile,
    relative: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, Error> {
    if selected.mode != FileMode::Directory {
        return Err(refused(format!(
            "{} is a file, not a directory",
            selected.canonical.display()
        )));
    }
    let path = Path::new(relative);
    if relative.is_empty()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(refused(format!(
            "{relative:?} is not a relative path of plain names"
        )));
    }
    let joined = selected.canonical.join(path);
    let canonical = joined
        .canonicalize()
        .map_err(|error| unreadable(&joined, &error))?;
    if canonical == selected.canonical || !canonical.starts_with(&selected.canonical) {
        return Err(refused(format!(
            "{relative:?} leads outside the selected directory"
        )));
    }
    let metadata = canonical
        .metadata()
        .map_err(|error| unreadable(&canonical, &error))?;
    if !metadata.is_file() {
        return Err(refused(format!("{relative:?} is not a file")));
    }
    read_unchanged(
        &canonical,
        &source_signature(&canonical, &metadata),
        max_bytes,
        "file changed while it was read",
    )
}

/// Read at most `max_bytes` of `path`, which must match `expected` before opening, through the
/// open handle and after reading.
fn read_unchanged(
    path: &Path,
    expected: &SourceSignature,
    max_bytes: u64,
    changed: &str,
) -> Result<Vec<u8>, Error> {
    let conflict = || Error::new(ErrorKind::Conflict, changed);
    let too_large = || {
        Error::new(
            ErrorKind::ResourceLimit,
            format!("{} is larger than {max_bytes} bytes", path.display()),
        )
    };
    let failed = |error: io::Error| unreadable(path, &error);
    let before = fs::metadata(path).map_err(failed)?;
    if source_signature(path, &before) != *expected {
        return Err(conflict());
    }
    if before.len() > max_bytes {
        return Err(too_large());
    }
    let file = File::open(path).map_err(failed)?;
    if source_signature(path, &file.metadata().map_err(failed)?) != *expected {
        return Err(conflict());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large());
    }
    let after = fs::metadata(path).map_err(failed)?;
    if source_signature(path, &after) != *expected || bytes.len() as u64 != after.len() {
        return Err(conflict());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_is_canonical_and_checks_its_mode() {
        let root = std::env::temp_dir().join(format!("lightwell-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("inner")).unwrap();
        std::fs::write(root.join("inner/file.txt"), b"x").unwrap();
        let file = select(&root.join("inner/../inner/file.txt"), FileMode::File).unwrap();
        assert_eq!(
            file.canonical,
            root.join("inner/file.txt").canonicalize().unwrap()
        );
        assert!(select(&root.join("inner"), FileMode::File).is_err());
        assert!(select(&root.join("inner"), FileMode::Directory).is_ok());
        assert!(select(&root.join("missing"), FileMode::File).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A fresh directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("lightwell-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    fn code(result: Result<Vec<u8>, Error>) -> &'static str {
        result.unwrap_err().kind.code()
    }

    #[test]
    fn a_selected_file_is_read_within_its_bound() {
        let root = scratch("bounded");
        let path = root.join("input.bin");
        fs::write(&path, b"0123456789").unwrap();
        let file = select(&path, FileMode::File).unwrap();
        assert_eq!(
            read_bounded(&file, DEFAULT_MAX_FILE_BYTES).unwrap(),
            b"0123456789"
        );
        assert_eq!(read_bounded(&file, 10).unwrap(), b"0123456789");
        assert_eq!(code(read_bounded(&file, 9)), "resource-limit");
        let directory = select(&root, FileMode::Directory).unwrap();
        assert_eq!(code(read_bounded(&directory, 64)), "validation");
        assert_eq!(
            code(read_in_directory(&file, "input.bin", 64)),
            "validation"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_file_changed_after_selection_is_a_conflict_and_is_never_reselected() {
        let root = scratch("changed");
        let path = root.join("input.bin");

        fs::write(&path, b"original").unwrap();
        let file = select(&path, FileMode::File).unwrap();
        fs::write(&path, b"original and more").unwrap();
        let error = read_bounded(&file, 64).unwrap_err();
        assert_eq!(
            error.to_string(),
            "conflict: file changed since it was selected"
        );
        assert_eq!(
            code(read_bounded(&file, 64)),
            "conflict",
            "a retry does not adopt the new file"
        );

        fs::write(&path, b"original").unwrap();
        let file = select(&path, FileMode::File).unwrap();
        let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(earlier)
            .unwrap();
        assert_eq!(
            code(read_bounded(&file, 64)),
            "conflict",
            "modification time"
        );

        let file = select(&path, FileMode::File).unwrap();
        let replacement = root.join("replacement.bin");
        fs::write(&replacement, b"original").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(code(read_bounded(&file, 64)), "conflict", "replaced file");

        let file = select(&path, FileMode::File).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(code(read_bounded(&file, 64)), "read-error", "removed file");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_directory_read_stays_inside_the_selected_directory() {
        let root = scratch("directory");
        let selected_path = root.join("selected");
        fs::create_dir_all(selected_path.join("models/v1")).unwrap();
        fs::write(selected_path.join("models/v1/weights.bin"), b"weights").unwrap();
        fs::write(root.join("secret.txt"), b"outside").unwrap();
        let selected = select(&selected_path, FileMode::Directory).unwrap();

        assert_eq!(
            read_in_directory(&selected, "models/v1/weights.bin", 64).unwrap(),
            b"weights"
        );
        assert_eq!(
            code(read_in_directory(&selected, "models/v1/weights.bin", 3)),
            "resource-limit"
        );
        let absolute = root.join("secret.txt");
        for refused in [
            "",
            "..",
            "../secret.txt",
            "models/../../secret.txt",
            "./models/v1/weights.bin",
            absolute.to_str().unwrap(),
        ] {
            assert_eq!(
                code(read_in_directory(&selected, refused, 64)),
                "validation",
                "{refused:?}"
            );
        }
        assert_eq!(
            code(read_in_directory(&selected, "models", 64)),
            "validation"
        );
        assert_eq!(
            code(read_in_directory(&selected, "missing.bin", 64)),
            "read-error"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_the_selected_directory_is_refused() {
        let root = scratch("symlink");
        let selected_path = root.join("selected");
        fs::create_dir_all(selected_path.join("inner")).unwrap();
        fs::write(root.join("secret.txt"), b"outside").unwrap();
        fs::write(selected_path.join("inner/data.txt"), b"inside").unwrap();
        std::os::unix::fs::symlink(root.join("secret.txt"), selected_path.join("escape")).unwrap();
        std::os::unix::fs::symlink(&root, selected_path.join("parent")).unwrap();
        std::os::unix::fs::symlink(
            selected_path.join("inner/data.txt"),
            selected_path.join("alias"),
        )
        .unwrap();
        let selected = select(&selected_path, FileMode::Directory).unwrap();
        assert_eq!(
            code(read_in_directory(&selected, "escape", 64)),
            "validation"
        );
        assert_eq!(
            code(read_in_directory(&selected, "parent/secret.txt", 64)),
            "validation"
        );
        assert_eq!(
            read_in_directory(&selected, "alias", 64).unwrap(),
            b"inside",
            "a link that stays inside the directory is followed"
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
