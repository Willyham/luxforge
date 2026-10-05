//! A disk image for the library's tests of offline volumes (macOS): a 16 MB HFS+ image in a scratch
//! directory, attached at a folder beside it.
//!
//! It is attached as a volume a person browses (no `-nobrowse`, which marks a mount one nobody
//! browses and leaves it identified only by its device number), so it is identified by its volume
//! UUID, which stays the same however often it is attached and whatever else is attached
//! meanwhile — by other tests or other processes on the host. `-noautoopen` keeps the Finder from
//! opening a window on it. Attaching and detaching each wait, bounded, until the kernel shows the
//! volume mounted or gone at its mount point, and one image is attached at a time in this test
//! binary (a [`DiskImage`] holds a process-wide lock until it is dropped).
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

    /// Attach the image at its mount point, and wait until the volume is mounted there.
    pub(crate) fn attach(&mut self) {
        let status = Command::new("hdiutil")
            .args(["attach", "-quiet", "-noautoopen", "-mountpoint"])
            .arg(&self.mount)
            .arg(&self.image)
            .status()
            .unwrap();
        assert!(status.success(), "hdiutil attach: {status}");
        self.attached = true;
        let mount = self.mount.clone();
        luxforge_testbase::wait_until("the disk image to be mounted", || mounted(&mount));
    }

    /// Detach the image, and wait until its volume is gone from the mount point.
    pub(crate) fn detach(&mut self) {
        let status = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mount)
            .status()
            .unwrap();
        assert!(status.success(), "hdiutil detach: {status}");
        self.attached = false;
        let mount = self.mount.clone();
        luxforge_testbase::wait_until("the disk image to be detached", || !mounted(&mount));
    }
}

/// Whether a volume is mounted at `mount`: the folder is on another device than its parent.
fn mounted(mount: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let device = |path: &Path| path.metadata().ok().map(|metadata| metadata.dev());
    match (device(mount), mount.parent().and_then(device)) {
        (Some(mounted), Some(parent)) => mounted != parent,
        _ => false,
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
