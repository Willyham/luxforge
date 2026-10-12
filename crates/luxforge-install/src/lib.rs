//! Where the running Luxforge is installed, and moving it into Applications on macOS
//! ([design](../../../docs/design/auto-update.md#moving-into-applications)).
//!
//! An app can update itself only where it can replace its own bundle: not from a disk image, a
//! read-only volume or the read-only mirror Gatekeeper translocates a quarantined app to. A release
//! build launched by a person therefore asks to move itself into Applications, and the update
//! port's `update.move-to-applications` makes the same move:
//!
//! - [`classify`] says where a bundle is ([`Location`]), as LetsMove does: translocated, on a disk
//!   image or another read-only volume, in `/Applications` or `~/Applications`, or elsewhere.
//! - [`move_to_applications`] copies the bundle beside its destination with `ditto`, checks the
//!   copy's signature with `codesign --verify --deep --strict`, clears its quarantine, moves an
//!   existing Luxforge in the way to the Trash only when the caller confirms it, and puts the copy
//!   in place. Nothing is changed before the copy verifies, and the original is never touched.
//! - [`Relaunch`] starts the moved app once the running one has quit, then moves the original to
//!   the Trash or ejects its disk image ([`Cleanup`]). The desktop runs it at the end of its
//!   ordinary quit path, after the catalog has closed.
//!
//! Every path a move writes is passed in ([`Roots`]) and every effect outside the destination
//! goes through [`MoveEnv`], so tests move into temporary folders and never touch the real
//! Applications folders or the Trash. The types build on every platform, so the desktop names them
//! without conditions; off macOS there is no bundle ([`running_bundle`] answers `None`), so nothing
//! is ever classified or moved, and the system calls answer that they are unsupported.
#[cfg(target_os = "macos")]
mod native;
#[cfg(not(target_os = "macos"))]
#[path = "unsupported.rs"]
mod native;
#[cfg(all(test, target_os = "macos"))]
mod tests;

pub use native::{Volume, quarantined};
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Where a bundle runs from, as the launch question and `update.status` see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    /// In `/Applications` or a folder under it.
    Applications,
    /// In `~/Applications` or a folder under it.
    UserApplications,
    /// Translocated: Gatekeeper runs a quarantined app the person did not move from a read-only
    /// mirror of where they put it.
    Translocated,
    /// On an attached disk image, which is read-only.
    DiskImage,
    /// On another read-only volume.
    ReadOnlyVolume,
    /// Anywhere else, such as Downloads or the desktop.
    Elsewhere,
}

impl Location {
    /// Whether an app here can replace itself, so nothing asks it to move.
    pub fn in_applications(self) -> bool {
        matches!(self, Self::Applications | Self::UserApplications)
    }

    /// The location's name, as the desktop's log and captured state record it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applications => "applications",
            Self::UserApplications => "user-applications",
            Self::Translocated => "translocated",
            Self::DiskImage => "disk-image",
            Self::ReadOnlyVolume => "read-only-volume",
            Self::Elsewhere => "elsewhere",
        }
    }
}

/// What happens to the original once the moved app has been started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cleanup {
    /// Move the original bundle to the Trash.
    Trash(PathBuf),
    /// Eject the disk image the original ran from, by the device it is attached as.
    Eject(String),
    /// Leave it: a read-only volume that is not a disk image, or a translocated app whose original
    /// the system did not name.
    Leave,
}

/// Where a bundle is, and what a move does with the original.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The bundle as it runs: the translocated mirror when it is translocated.
    pub bundle: PathBuf,
    /// Where the person put it: the same as `bundle` unless it is translocated.
    pub original: PathBuf,
    pub location: Location,
    pub cleanup: Cleanup,
}

/// The two Applications folders a move chooses between: `/Applications`, and `~/Applications`
/// when that is not writable. Tests pass temporary ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    pub system: PathBuf,
    pub user: PathBuf,
}

impl Roots {
    /// `/Applications` and the person's `~/Applications`.
    pub fn standard() -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        Self {
            system: PathBuf::from("/Applications"),
            user: home.join("Applications"),
        }
    }
}

/// What classifying a bundle asks the system. [`System`] asks it; tests answer for it.
pub trait Probes {
    /// `Some` when `bundle` runs translocated, with where the person put it when the system says.
    fn translocated(&self, bundle: &Path) -> Option<Option<PathBuf>>;
    /// The volume `path` is on.
    fn volume(&self, path: &Path) -> io::Result<Volume>;
}

/// What a move does outside its destination folder. [`System`] does it; tests record it.
pub trait MoveEnv {
    /// Whether this process may write into `folder`.
    fn writable(&self, folder: &Path) -> bool;
    /// Move `path` to the person's Trash.
    fn trash(&self, path: &Path) -> io::Result<()>;
}

/// The real system: Security's translocation functions, `statfs` and Disk Arbitration, `access`,
/// and Foundation's Trash.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl Probes for System {
    fn translocated(&self, bundle: &Path) -> Option<Option<PathBuf>> {
        native::translocated(bundle)
    }

    fn volume(&self, path: &Path) -> io::Result<Volume> {
        native::volume(path)
    }
}

impl MoveEnv for System {
    fn writable(&self, folder: &Path) -> bool {
        native::writable(folder)
    }

    fn trash(&self, path: &Path) -> io::Result<()> {
        native::trash(path)
    }
}

/// The `.app` bundle an executable at `Contents/MacOS/<name>` belongs to, or `None` for one
/// outside a bundle, as a development build's is.
pub fn bundle_of(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

/// The bundle this process runs from, resolved through symbolic links, or `None` outside one.
pub fn running_bundle() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    bundle_of(&fs::canonicalize(&executable).unwrap_or(executable))
}

/// Where `bundle` is, asking the real system.
pub fn classify(bundle: &Path) -> Placement {
    classify_with(bundle, &Roots::standard(), &System)
}

/// Where `bundle` is, as LetsMove decides it: translocation first, since a translocated app runs
/// from a read-only mirror; then a read-only volume, a disk image when Disk Arbitration says so;
/// then the two Applications folders; otherwise elsewhere. A volume that cannot be read counts as
/// a writable one.
pub fn classify_with(bundle: &Path, roots: &Roots, probes: &dyn Probes) -> Placement {
    let removal = |path: &Path| match probes.volume(path) {
        Ok(volume) if volume.read_only && volume.disk_image => Cleanup::Eject(volume.device),
        Ok(volume) if volume.read_only => Cleanup::Leave,
        _ => Cleanup::Trash(path.to_path_buf()),
    };
    if let Some(original) = probes.translocated(bundle) {
        let cleanup = original.as_deref().map_or(Cleanup::Leave, removal);
        return Placement {
            bundle: bundle.to_path_buf(),
            original: original.unwrap_or_else(|| bundle.to_path_buf()),
            location: Location::Translocated,
            cleanup,
        };
    }
    let volume = probes.volume(bundle).ok();
    let (location, cleanup) = match &volume {
        Some(volume) if volume.read_only && volume.disk_image => {
            (Location::DiskImage, Cleanup::Eject(volume.device.clone()))
        }
        Some(volume) if volume.read_only => (Location::ReadOnlyVolume, Cleanup::Leave),
        _ if bundle.starts_with(&roots.system) => (Location::Applications, Cleanup::Leave),
        _ if bundle.starts_with(&roots.user) => (Location::UserApplications, Cleanup::Leave),
        _ => (Location::Elsewhere, Cleanup::Trash(bundle.to_path_buf())),
    };
    Placement {
        bundle: bundle.to_path_buf(),
        original: bundle.to_path_buf(),
        location,
        cleanup,
    }
}

/// Where a move puts the bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    /// The bundle's path there, under the bundle's own name.
    pub app: PathBuf,
    /// Something is already at `app`, which goes to the Trash only when the move is confirmed.
    pub existing: bool,
    /// `/Applications` is not writable, so it is `~/Applications`, created if it is missing.
    pub user: bool,
}

/// Why a move did not happen. Nothing was changed but what the variant says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveError {
    /// The bundle is already in an Applications folder.
    InApplications,
    /// The bundle is not an `.app` bundle with a name.
    NotABundle(PathBuf),
    /// An app is already at the destination and the move was not confirmed to replace it.
    Existing(PathBuf),
    /// `~/Applications` could not be created.
    Folder(String),
    /// `ditto` could not copy the bundle; nothing is left behind.
    Copy(String),
    /// The copy's signature did not verify; the copy is removed and nothing else changed.
    Verify(String),
    /// The copy's quarantine could not be cleared; the copy is removed and nothing else changed.
    Quarantine(String),
    /// The app in the way could not be moved to the Trash; the copy is removed.
    Replace(String),
    /// The verified copy could not be put in place; it is removed.
    Place(String),
}

impl MoveError {
    /// The refusal's `data.reason`, as the update port answers it.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::InApplications => "in-applications",
            Self::NotABundle(_) => "not-a-bundle",
            Self::Existing(_) => "existing-app",
            Self::Folder(_) => "no-folder",
            Self::Copy(_) => "copy-failed",
            Self::Verify(_) => "verify-failed",
            Self::Quarantine(_) => "quarantine-failed",
            Self::Replace(_) => "replace-failed",
            Self::Place(_) => "place-failed",
        }
    }
}

impl fmt::Display for MoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InApplications => write!(f, "Luxforge is already in an Applications folder"),
            Self::NotABundle(path) => write!(f, "{} is not an app bundle", path.display()),
            Self::Existing(path) => write!(f, "{} is already there", path.display()),
            Self::Folder(reason) => write!(f, "could not create Applications: {reason}"),
            Self::Copy(reason) => write!(f, "could not copy Luxforge: {reason}"),
            Self::Verify(reason) => write!(f, "the copy's signature did not verify: {reason}"),
            Self::Quarantine(reason) => {
                write!(f, "could not clear the copy's quarantine: {reason}")
            }
            Self::Replace(reason) => {
                write!(
                    f,
                    "could not move the Luxforge in the way to the Trash: {reason}"
                )
            }
            Self::Place(reason) => write!(f, "could not put the copy in place: {reason}"),
        }
    }
}

impl std::error::Error for MoveError {}

/// Where `placement`'s bundle would go: `/Applications` when this process may write there,
/// otherwise `~/Applications`. Reads only: nothing is created.
pub fn destination(
    placement: &Placement,
    roots: &Roots,
    env: &dyn MoveEnv,
) -> Result<Destination, MoveError> {
    let name = placement
        .original
        .file_name()
        .filter(|_| {
            placement
                .original
                .extension()
                .is_some_and(|ext| ext == "app")
        })
        .ok_or_else(|| MoveError::NotABundle(placement.original.clone()))?;
    let user = !env.writable(&roots.system);
    let folder = if user { &roots.user } else { &roots.system };
    let app = folder.join(name);
    Ok(Destination {
        existing: app.symlink_metadata().is_ok(),
        app,
        user,
    })
}

/// A finished move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The moved bundle, which the relaunch starts.
    pub app: PathBuf,
    /// The app that was in the way, now in the Trash.
    pub replaced: Option<PathBuf>,
    /// `~/Applications` was used because `/Applications` is not writable.
    pub user: bool,
    /// What happens to the original once the moved app has started.
    pub cleanup: Cleanup,
    /// How many files and folders of the copy had their quarantine cleared.
    pub quarantine_cleared: usize,
}

/// Move `placement`'s bundle into Applications, blocking: run it off the update loop and the owner
/// thread. The bundle is copied with `ditto` beside the destination, under a hidden name, which
/// preserves its signature, attributes and links; the copy is verified with `codesign --verify
/// --deep --strict` and its quarantine cleared before anything else changes. Only then does an app
/// in the way go to the Trash, which needs `replace`, and the copy take its place with one rename.
/// A failure removes the copy, and the original is never touched: [`Relaunch`] deals with it after
/// the quit.
pub fn move_to_applications(
    placement: &Placement,
    roots: &Roots,
    replace: bool,
    env: &dyn MoveEnv,
) -> Result<Moved, MoveError> {
    if placement.location.in_applications() {
        return Err(MoveError::InApplications);
    }
    let destination = destination(placement, roots, env)?;
    if destination.existing && !replace {
        return Err(MoveError::Existing(destination.app));
    }
    let folder = destination
        .app
        .parent()
        .expect("a destination is inside its folder");
    if destination.user {
        fs::create_dir_all(folder).map_err(|error| MoveError::Folder(error.to_string()))?;
    }
    let name = destination
        .app
        .file_name()
        .expect("a destination has the bundle's name")
        .to_string_lossy();
    let staging = Staging(folder.join(format!(".{name}.moving-{}", std::process::id())));
    staging.clear();
    run(Command::new("/usr/bin/ditto")
        .arg(&placement.bundle)
        .arg(&staging.0))
    .map_err(MoveError::Copy)?;
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&staging.0))
    .map_err(MoveError::Verify)?;
    let quarantine_cleared =
        clear_quarantine(&staging.0).map_err(|error| MoveError::Quarantine(error.to_string()))?;
    let replaced = if destination.existing {
        env.trash(&destination.app)
            .map_err(|error| MoveError::Replace(error.to_string()))?;
        Some(destination.app.clone())
    } else {
        None
    };
    fs::rename(&staging.0, &destination.app).map_err(|error| {
        MoveError::Place(match &replaced {
            Some(path) => format!(
                "{error}; the app that was there is in the Trash: {}",
                path.display()
            ),
            None => error.to_string(),
        })
    })?;
    staging.placed();
    Ok(Moved {
        app: destination.app,
        replaced,
        user: destination.user,
        cleanup: placement.cleanup.clone(),
        quarantine_cleared,
    })
}

/// The hidden copy a move verifies before it is placed, removed unless it was placed.
struct Staging(PathBuf);

impl Staging {
    fn clear(&self) {
        let _ = fs::remove_dir_all(&self.0);
    }

    fn placed(mut self) {
        self.0 = PathBuf::new();
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            self.clear();
        }
    }
}

/// The longest a tool's own words are kept in an error.
const MAX_TOOL_OUTPUT: usize = 600;

/// Run a tool to completion; its failure, with what it said on stderr.
fn run(command: &mut Command) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said: String = said.trim().chars().take(MAX_TOOL_OUTPUT).collect();
    Err(format!("{} ({})", said, output.status))
}

/// Remove the quarantine attribute from everything under `root`, `root` included, as LetsMove's
/// `xattr -d -r` does, without following symbolic links. How many items had it.
pub fn clear_quarantine(root: &Path) -> io::Result<usize> {
    let mut cleared = usize::from(native::clear_quarantine(root)?);
    if root.symlink_metadata()?.is_dir() {
        for entry in fs::read_dir(root)? {
            cleared += clear_quarantine(&entry?.path())?;
        }
    }
    Ok(cleared)
}

/// The tools a relaunch runs; tests pass stand-ins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    pub open: PathBuf,
    pub hdiutil: PathBuf,
}

impl Tools {
    pub fn system() -> Self {
        Self {
            open: PathBuf::from("/usr/bin/open"),
            hdiutil: PathBuf::from("/usr/bin/hdiutil"),
        }
    }
}

/// How long the waiter waits for the running process to exit before it opens the app anyway: the
/// catalog has already closed when it starts, so a slow exit only delays the relaunch.
pub const RELAUNCH_WAIT_S: u32 = 30;

/// The waiter: wait (in tenths of a second, at most `$6` of them) for process `$1` to exit, open
/// `$2` as a new instance with `$4`, then, when `$3` names a device, detach it with `$5`, once more
/// after a pause should the first attempt find it busy. Every value is an argument, never part of
/// the script's text.
const WAITER: &str = r#"pid="$1"; app="$2"; device="$3"; open="$4"; hdiutil="$5"; limit="$6"
n=0
while /bin/kill -0 "$pid" 2>/dev/null && [ "$n" -lt "$limit" ]; do /bin/sleep 0.1; n=$((n+1)); done
"$open" -n -a "$app"
if [ -n "$device" ]; then
  "$hdiutil" detach "$device" -quiet || { /bin/sleep 2; "$hdiutil" detach "$device" -quiet; }
fi
"#;

/// Start an app once the running process has quit: the hook every relaunch takes after the
/// desktop's ordinary quit path has closed the catalog. A move's relaunch also deals with the
/// original ([`Cleanup`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relaunch {
    pub app: PathBuf,
    pub cleanup: Cleanup,
}

/// What [`Relaunch::start`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    /// The original was moved to the Trash.
    pub trashed: Option<PathBuf>,
    /// Why the original could not be moved to the Trash; the relaunch goes ahead.
    pub trash_failed: Option<String>,
    /// The disk image device the waiter detaches after opening the app.
    pub eject: Option<String>,
}

impl Relaunch {
    /// Relaunch `app` and leave everything else.
    pub fn new(app: PathBuf) -> Self {
        Self {
            app,
            cleanup: Cleanup::Leave,
        }
    }

    /// The relaunch a finished move asks for.
    pub fn moved(moved: &Moved) -> Self {
        Self {
            app: moved.app.clone(),
            cleanup: moved.cleanup.clone(),
        }
    }

    /// Run once the catalog has closed, on the way out: move the original to the Trash, then start
    /// a detached waiter that opens the app as a new instance once this process has exited and
    /// ejects the original's disk image after it. A Trash failure is reported, not fatal.
    pub fn start(&self, env: &dyn MoveEnv) -> io::Result<Started> {
        self.start_with(
            env,
            &Tools::system(),
            std::process::id(),
            RELAUNCH_WAIT_S * 10,
        )
    }

    /// [`Self::start`] with the tools, the process waited for and the wait's limit in tenths of a
    /// second named, for tests.
    pub fn start_with(
        &self,
        env: &dyn MoveEnv,
        tools: &Tools,
        pid: u32,
        limit: u32,
    ) -> io::Result<Started> {
        let mut started = Started {
            trashed: None,
            trash_failed: None,
            eject: None,
        };
        match &self.cleanup {
            Cleanup::Trash(original) => match env.trash(original) {
                Ok(()) => started.trashed = Some(original.clone()),
                Err(error) => started.trash_failed = Some(error.to_string()),
            },
            Cleanup::Eject(device) => started.eject = Some(device.clone()),
            Cleanup::Leave => {}
        }
        Command::new("/bin/sh")
            .arg("-c")
            .arg(WAITER)
            .arg("luxforge-relaunch")
            .arg(pid.to_string())
            .arg(&self.app)
            .arg(started.eject.as_deref().unwrap_or(""))
            .arg(&tools.open)
            .arg(&tools.hdiutil)
            .arg(limit.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(started)
    }
}
