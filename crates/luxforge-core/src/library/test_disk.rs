//! A disk image for the library's tests of offline volumes (macOS): a 16 MB HFS+ image in a scratch
//! directory, attached at a folder beside it.
//!
//! It is attached as a volume a person browses (no `-nobrowse`, which marks a mount one nobody
//! browses and leaves it identified only by its device number), so it is identified by its volume
//! UUID, which stays the same however often it is attached and whatever else is attached
//! meanwhile — by other tests or other processes on the host. `-noautoopen` keeps the Finder from
//! opening a window on it. Attaching and detaching each wait, bounded, until the kernel shows the
//! volume mounted or gone at its mount point, and one image is attached at a time in this test
//! binary (a [`DiskImage`] holds a process-wide lock until it is dropped). Disk-image commands
//! retry within the shared test hang bound: Disk Arbitration can refuse them while busy. A
//! persistent failure reports the command's status and captured output.
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
        hdiutil(
            Command::new("hdiutil")
                .args([
                    "create", "-ov", "-size", "16m", "-fs", "HFS+", "-volname", label,
                ])
                .arg(&image),
        );
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
        hdiutil(
            Command::new("hdiutil")
                .args(["attach", "-noautoopen", "-mountpoint"])
                .arg(&self.mount)
                .arg(&self.image),
        );
        self.attached = true;
        let mount = self.mount.clone();
        luxforge_testbase::wait_until("the disk image to be mounted", || mounted(&mount));
    }

    /// Detach the image, and wait until its volume is gone from the mount point.
    pub(crate) fn detach(&mut self) {
        hdiutil(
            Command::new("hdiutil")
                .args(["detach", "-force"])
                .arg(&self.mount),
        );
        self.attached = false;
        let mount = self.mount.clone();
        luxforge_testbase::wait_until("the disk image to be detached", || !mounted(&mount));
    }
}

/// Disk Arbitration may still be busy after a previous command returned. Keep the native
/// operation as the proof, retrying only within the same bound as the mount-table waits.
fn hdiutil(command: &mut Command) {
    let mut last = None;
    let result = luxforge_testbase::try_wait_for("the disk-image command to succeed", || {
        let output = command.output().expect("hdiutil runs; this test needs it");
        let succeeded = output.status.success();
        last = Some(output);
        succeeded.then_some(())
    });
    if let Err(hung) = result {
        let output = last.expect("the command was attempted");
        panic!(
            "{hung}\n{command:?}: {}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
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
