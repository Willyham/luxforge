//! A disk image for the library's tests of offline volumes (macOS): a 16 MB HFS+ image in a scratch
//! directory, attached at a folder beside it with `-nobrowse`, so the Finder never shows it.
//!
//! A `-nobrowse` volume is not one a person browses, so its identity is its device number
//! (`index::volume_of`), which the kernel may hand to another image attached in the meantime. So
//! one image is attached at a time in this test binary: a [`DiskImage`] holds a process-wide lock
//! from its creation until it is dropped, and a test that detaches and attaches its image again
//! finds it as the same volume.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, MutexGuard},
};

/// Held by the one disk image attached at a time.
static ATTACHED: Mutex<()> = Mutex::new(());

/// A scratch disk image, attached while `attached`.
pub(crate) struct DiskImage {
    image: PathBuf,
    pub mount: PathBuf,
    attached: bool,
    _one_at_a_time: MutexGuard<'static, ()>,
}

impl DiskImage {
    /// Create an image labelled `label` in `dir` and attach it at `dir/label`.
    pub(crate) fn create(dir: &Path, label: &str) -> Self {
        let lock = ATTACHED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let image = dir.join(format!("{label}.dmg"));
        let status = Command::new("hdiutil")
            .args([
                "create", "-quiet", "-size", "16m", "-fs", "HFS+", "-volname", label,
            ])
            .arg(&image)
            .status()
            .expect("hdiutil runs; this test needs it");
        assert!(status.success(), "hdiutil create: {status}");
        let mount = dir.join(label);
        fs::create_dir_all(&mount).unwrap();
        let mut disk = Self {
            image,
            mount,
            attached: false,
            _one_at_a_time: lock,
        };
        disk.attach();
        disk
    }

    pub(crate) fn attach(&mut self) {
        let status = Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&self.mount)
            .arg(&self.image)
            .status()
            .unwrap();
        assert!(status.success(), "hdiutil attach: {status}");
        self.attached = true;
    }

    pub(crate) fn detach(&mut self) {
        let status = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mount)
            .status()
            .unwrap();
        assert!(status.success(), "hdiutil detach: {status}");
        self.attached = false;
    }
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        if self.attached {
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.mount)
                .status();
        }
    }
}
