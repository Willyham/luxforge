//! The paths tests read and write: repository fixtures and unique scratch paths. They are plain
//! paths, so the core's own unit tests use them as every other crate's tests do.
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// The workspace root.
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A file under the repository's `fixtures/`.
pub fn fixture(name: &str) -> PathBuf {
    repository().join("fixtures").join(name)
}

/// The 480x320 synthetic quadrant JPEG most journeys import.
pub fn jpeg() -> PathBuf {
    fixture("s0/orientation-1.jpg")
}

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

/// A path in the system temporary directory no other test of any process uses: the process, a
/// per-process counter and `name`, which ends in whatever extension the file needs. Whatever a
/// previous run left there is removed first.
pub fn temp_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "luxforge-{}-{}-{name}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// A new, empty scratch directory at a [`temp_path`].
pub fn temp_dir(name: &str) -> PathBuf {
    let path = temp_path(name);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}

/// A new catalog path, as [`temp_path`] makes one.
pub fn temp_catalog(label: &str) -> PathBuf {
    temp_path(&format!("{label}.sqlite"))
}
