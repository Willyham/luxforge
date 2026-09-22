//! Scoped access to files a person selected: canonical paths, the declared mode and the identity a
//! later read checks against. The host reads these files and never writes them.
use crate::{Error, ErrorKind, editor::SourceSignature};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    let signature = crate::editor::source_signature(&canonical, &metadata);
    Ok(SelectedFile {
        canonical,
        mode,
        signature,
    })
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
}
