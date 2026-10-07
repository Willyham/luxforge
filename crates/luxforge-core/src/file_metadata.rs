//! Shared filesystem metadata predicates; traversal policy belongs to each caller.
use std::{ffi::OsStr, fs::Metadata};

/// Whether an entry is hidden: its name starts with a dot, or the platform marks it hidden (the
/// `UF_HIDDEN` flag on macOS, as `~/Library` carries; the hidden attribute on Windows).
pub(crate) fn hidden(name: &OsStr, metadata: &Metadata) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.') || platform_hidden(metadata)
}

#[cfg(target_os = "macos")]
fn platform_hidden(metadata: &Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    /// `UF_HIDDEN` (`sys/stat.h`).
    const UF_HIDDEN: u32 = 0x0000_8000;
    metadata.st_flags() & UF_HIDDEN != 0
}

#[cfg(windows)]
fn platform_hidden(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    /// `FILE_ATTRIBUTE_HIDDEN`.
    const HIDDEN: u32 = 0x2;
    metadata.file_attributes() & HIDDEN != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_hidden(_: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_names_are_hidden_and_visible_names_are_not() {
        let root = luxforge_testbase::paths::temp_dir("hidden-metadata");
        std::fs::create_dir_all(&root).unwrap();
        let metadata = std::fs::metadata(&root).unwrap();
        assert!(hidden(OsStr::new(".photo"), &metadata));
        assert!(!hidden(OsStr::new("photo.jpg"), &metadata));
        std::fs::remove_dir(&root).unwrap();
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn platform_hidden_metadata_is_shared_with_both_walkers() {
        let root = luxforge_testbase::paths::temp_dir("platform-hidden-metadata");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("photo.jpg");
        std::fs::write(&path, []).unwrap();
        #[cfg(target_os = "macos")]
        let status = std::process::Command::new("chflags")
            .arg("hidden")
            .arg(&path)
            .status()
            .unwrap();
        #[cfg(windows)]
        let status = std::process::Command::new("attrib")
            .arg("+H")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(hidden(path.file_name().unwrap(), &metadata));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
