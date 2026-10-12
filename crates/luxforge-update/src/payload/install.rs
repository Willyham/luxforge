//! Swapping in a staged payload at the end of the quit path, and starting the new executable.
use super::{BuildInfo, Payload, Staged, desktop_entry::refresh_desktop_entry, remove_tree};
use crate::error::{Error, Result};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// How [`Payload::install_with`] swaps the payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapMethod {
    /// On Linux, one atomic exchange (`renameat2` with `RENAME_EXCHANGE`) where the filesystem
    /// supports it; two renames otherwise, and on every other platform.
    Auto,
    /// Two renames: the payload to `<name>.previous`, then the new payload into its place, renaming
    /// the previous one back if the second fails.
    Renames,
}

/// What an install did.
#[derive(Debug)]
pub struct Installed {
    /// The new executable, at the path the old one had.
    pub executable: PathBuf,
    /// Where the previous payload is kept, until the next successful launch removes it. `None`
    /// when it could not be moved there after an exchange; it is then removed with the staging.
    pub previous: Option<PathBuf>,
    /// The payloads were exchanged atomically.
    pub exchanged: bool,
    /// Whether a desktop entry that pointed at the payload was refreshed, or why refreshing it
    /// failed; a failure there does not undo the install.
    pub desktop_entry: std::result::Result<bool, String>,
}

impl Payload {
    /// Install `staged`: [`Self::install_with`] by [`SwapMethod::Auto`].
    pub fn install(&self, staged: &Staged, desktop_entry: Option<&Path>) -> Result<Installed> {
        self.install_with(staged, desktop_entry, SwapMethod::Auto)
    }

    /// Install `staged` in place of this payload. Call it only at the end of the quit path, after
    /// the catalog has closed. The staged payload's `build.json` is checked again first; the old
    /// payload is kept as `<name>.previous`, replacing any earlier one; the staging directory is
    /// removed; and `desktop_entry`, when given, is refreshed if it points at this payload. A
    /// failure before the swap leaves the installed payload as it was.
    pub fn install_with(
        &self,
        staged: &Staged,
        desktop_entry: Option<&Path>,
        method: SwapMethod,
    ) -> Result<Installed> {
        let failed = |what: &str, error: &dyn std::fmt::Display| {
            Error::InstallFailed(format!("{what}: {error}"))
        };
        let new = staged.new_payload();
        let built = BuildInfo::read(&new)
            .map_err(|error| failed("the staged update cannot be read", &error))?;
        if built.version != staged.version
            || built.build != staged.build
            || built.target != self.build.target
        {
            return Err(Error::InstallFailed(format!(
                "the staged update is {} build {} for {}, not the {} build {} that was verified",
                built.version, built.build, built.target, staged.version, staged.build
            )));
        }
        if !self.dir.is_dir() {
            return Err(Error::InstallFailed(format!(
                "the payload {} is gone",
                self.dir.display()
            )));
        }
        let previous = self.previous();
        remove_tree(&previous)
            .map_err(|error| failed("the earlier previous payload cannot be removed", &error))?;
        let exchanged = match method {
            SwapMethod::Auto => exchange(&self.dir, &new)?,
            SwapMethod::Renames => false,
        };
        let kept = if exchanged {
            // The staging's `Luxforge.new` now holds the old payload.
            fs::rename(&new, &previous).is_ok()
        } else {
            fs::rename(&self.dir, &previous)
                .map_err(|error| failed("the payload cannot be moved aside", &error))?;
            if let Err(error) = fs::rename(&new, &self.dir) {
                let restored = fs::rename(&previous, &self.dir);
                return Err(Error::InstallFailed(format!(
                    "the update cannot be moved into place: {error}; {}",
                    match restored {
                        Ok(()) => "the installed payload is restored".to_owned(),
                        Err(restore) => format!(
                            "the installed payload is at {} and could not be restored: {restore}",
                            previous.display()
                        ),
                    }
                )));
            }
            true
        };
        let _ = remove_tree(staged.staging());
        let desktop_entry = match desktop_entry {
            Some(entry) => refresh_desktop_entry(entry, &self.dir, &self.executable(), &built)
                .map_err(|error| error.to_string()),
            None => Ok(false),
        };
        Ok(Installed {
            executable: self.executable(),
            previous: kept.then_some(previous),
            exchanged,
            desktop_entry,
        })
    }
}

/// Exchange `a` and `b` atomically: `Ok(false)` when the filesystem or kernel cannot, so the caller
/// renames instead.
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> Result<bool> {
    use rustix::{
        fs::{CWD, RenameFlags, renameat_with},
        io::Errno,
    };
    match renameat_with(CWD, a, CWD, b, RenameFlags::EXCHANGE) {
        Ok(()) => Ok(true),
        Err(errno) if [Errno::INVAL, Errno::NOSYS, Errno::OPNOTSUPP].contains(&errno) => Ok(false),
        Err(errno) => Err(Error::InstallFailed(format!(
            "the payloads cannot be exchanged: {errno}"
        ))),
    }
}

#[cfg(not(target_os = "linux"))]
fn exchange(_: &Path, _: &Path) -> Result<bool> {
    Ok(false)
}

/// Replace this process with `executable` run with `arguments`. Returns only on failure.
#[cfg(unix)]
pub fn relaunch(executable: &Path, arguments: &[OsString]) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    std::process::Command::new(executable)
        .args(arguments)
        .exec()
}

#[cfg(not(unix))]
pub fn relaunch(_: &Path, _: &[OsString]) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "relaunching is implemented for unix only",
    )
}
