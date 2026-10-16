//! The Linux payload updater's engine: the relocatable `Luxforge/` directory a release unpacks to,
//! replaced in place by a newer one (`docs/design/auto-update.md#linux-the-payload-updater`).
//!
//! - [`Payload::locate`] finds the payload the running executable lives in, by its `build.json`.
//! - [`Payload::mode`] decides whether it can replace itself ([`Mode::Payload`]) or only offer the
//!   release page ([`Mode::Notify`]).
//! - [`Payload::check`] reads the target's feed through a [`Fetch`] and chooses the newest item on a
//!   followed channel with a higher build.
//! - [`Payload::prepare`] streams that item's archive into a staging directory beside the payload,
//!   refusing more bytes than it states, verifies its Ed25519 signature through a memory map,
//!   extracts it into `Luxforge.new/` with the refusals in [`extract`] and checks its `build.json`.
//! - [`Payload::install`] swaps the payloads at the end of the quit path, keeping the old one as
//!   `<name>.previous`; [`relaunch`] then execs the new executable.
//! - [`Payload::clean_after_launch`] removes the previous payload and any staging once a launch has
//!   succeeded.
//!
//! Every step runs on the caller's thread; the desktop's port runs them on its own workers. Any
//! failure before the swap removes its staging directory and leaves the installed payload as it
//! was. The network is reached only through [`Fetch`], so the crate stays a leaf and its tests run
//! on any unix.
mod desktop_entry;
mod extract;
mod fetch;
mod install;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;

pub use desktop_entry::{DESKTOP_ENTRY_FILE, refresh_desktop_entry, user_desktop_entry};
pub use fetch::{Cancel, Fetch, FetchKind, FetchRequest};
pub use install::{Installed, SwapMethod, relaunch};

use crate::{
    Appcast, AppcastItem, Channel, SkippedItem, Target,
    error::{Error, Result},
    key::PublicKey,
    newest_eligible,
};
use fetch::Bounded;
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
};

/// The payload's description of its build, written by the release build.
pub const BUILD_FILE: &str = "build.json";
/// A file in the payload that stands the updater down: a distribution package that installs the
/// payload puts it there, and the installation is then offered as its release page only.
pub const PACKAGE_MARKER: &str = ".package-managed";
/// OmaStore's apps directory, `$XDG_DATA_HOME/omastore/apps`, whose last two components mark a
/// payload OmaStore installed and updates: `omastore/apps/<owner>__<repo>/<version>/Luxforge`.
const OMASTORE_APPS: [&str; 2] = ["omastore", "apps"];
/// Staging directories beside the payload start with this: `.luxforge-update-<build>`.
pub const STAGING_PREFIX: &str = ".luxforge-update-";
/// The extracted payload inside a staging directory.
pub const NEW_PAYLOAD: &str = "Luxforge.new";
/// The archive's single top-level directory.
pub const ARCHIVE_ROOT: &str = "Luxforge";
/// The previous payload is kept beside the installed one as `<name>.previous`.
pub const PREVIOUS_SUFFIX: &str = ".previous";
/// The downloaded archive's name inside its staging directory.
const ARCHIVE_FILE: &str = "update.tar.gz";
/// The longest archive downloaded, whatever its feed item states.
pub const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// The largest `build.json` read.
const MAX_BUILD_FILE_BYTES: u64 = 1024 * 1024;
/// The download's file buffer: the archive streams to disk through it.
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// What a payload's `build.json` says of its build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    /// `build_number`, which orders releases.
    pub build: u64,
    pub target: Target,
}

impl BuildInfo {
    /// Reads `<payload>/build.json`.
    pub fn read(payload: &Path) -> Result<BuildInfo> {
        let path = payload.join(BUILD_FILE);
        let file = fs::File::open(&path)
            .map_err(|error| Error::NotPayload(format!("{}: {error}", path.display())))?;
        if file.metadata()?.len() > MAX_BUILD_FILE_BYTES {
            return Err(Error::NotPayload(format!(
                "{} is larger than {MAX_BUILD_FILE_BYTES} bytes",
                path.display()
            )));
        }
        BuildInfo::parse(&fs::read_to_string(&path)?)
    }

    /// Reads a `build.json`: its `version`, its `build_number` (written as a decimal string, and
    /// accepted as a number) and its `target` (the Rust target triple, or a feed target's name).
    /// Other fields are the release build's and are not read.
    pub fn parse(json: &str) -> Result<BuildInfo> {
        let bad = |why: &str| Error::NotPayload(format!("build.json: {why}"));
        let value: Value = serde_json::from_str(json).map_err(|error| bad(&error.to_string()))?;
        let version = value["version"]
            .as_str()
            .filter(|version| !version.is_empty())
            .ok_or_else(|| bad("no version"))?
            .to_owned();
        let build = match &value["build_number"] {
            Value::String(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
                text.parse().ok()
            }
            Value::Number(number) => number.as_u64(),
            _ => None,
        }
        .ok_or_else(|| bad("no numeric build_number"))?;
        let target = value["target"]
            .as_str()
            .and_then(|target| Target::from_rust_target(target).or_else(|| Target::parse(target)))
            .ok_or_else(|| bad("no known target"))?;
        Ok(BuildInfo {
            version,
            build,
            target,
        })
    }
}

/// Whether a payload can replace itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// It can: [`Payload::prepare`] and [`Payload::install`] apply.
    Payload,
    /// It cannot; an update is offered as its release page.
    Notify(NotifyReason),
}

/// Why a payload can only offer the release page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyReason {
    /// The payload holds [`PACKAGE_MARKER`].
    PackageManaged,
    /// OmaStore installed the payload in its apps directory and updates it.
    OmaStoreManaged,
    /// The person cannot write the payload.
    PayloadNotWritable,
    /// The person cannot write the directory holding it.
    ParentNotWritable,
    /// A staging directory cannot be created beside it.
    StagingUnavailable,
    /// Staging beside it is on another filesystem than the payload, so it cannot be renamed into
    /// place.
    CrossFilesystem,
}

impl NotifyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PackageManaged => "package-managed",
            Self::OmaStoreManaged => "omastore-managed",
            Self::PayloadNotWritable => "payload-not-writable",
            Self::ParentNotWritable => "parent-not-writable",
            Self::StagingUnavailable => "staging-unavailable",
            Self::CrossFilesystem => "cross-filesystem",
        }
    }
}

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The newest item on a followed channel with a higher build, if any.
    pub item: Option<AppcastItem>,
    /// The items the feed skipped, and those without `luxforge:catalogFormat`, which the payload
    /// updater never offers because it could not tell whether they change the catalog format.
    pub skipped: Vec<SkippedItem>,
}

/// A downloaded, verified and extracted update, ready to install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    staging: PathBuf,
    version: String,
    build: u64,
}

impl Staged {
    /// The staging directory, `<parent>/.luxforge-update-<build>`.
    pub fn staging(&self) -> &Path {
        &self.staging
    }

    /// The extracted payload, `<staging>/Luxforge.new`.
    pub fn new_payload(&self) -> PathBuf {
        self.staging.join(NEW_PAYLOAD)
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn build(&self) -> u64 {
        self.build
    }
}

/// What [`Payload::clean_after_launch`] removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub previous: bool,
    pub staging: Vec<PathBuf>,
}

/// The payload the running executable lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    /// The payload directory, canonical.
    dir: PathBuf,
    /// The executable's file name in it.
    executable: OsString,
    build: BuildInfo,
}

impl Payload {
    /// The payload holding `executable`: its directory, which must hold a `build.json`.
    pub fn locate(executable: &Path) -> Result<Payload> {
        let executable = executable
            .canonicalize()
            .map_err(|error| Error::NotPayload(format!("{}: {error}", executable.display())))?;
        let (Some(dir), Some(name)) = (executable.parent(), executable.file_name()) else {
            return Err(Error::NotPayload(format!(
                "{} is not in a directory",
                executable.display()
            )));
        };
        if dir.parent().is_none() {
            return Err(Error::NotPayload("a payload cannot be the root".into()));
        }
        Ok(Payload {
            build: BuildInfo::read(dir)?,
            dir: dir.to_owned(),
            executable: name.to_owned(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The directory holding the payload, where staging and the previous payload go.
    pub fn parent(&self) -> &Path {
        self.dir.parent().expect("a located payload has a parent")
    }

    pub fn build(&self) -> &BuildInfo {
        &self.build
    }

    /// The executable, at the path it has in the installed payload, before and after a swap.
    pub fn executable(&self) -> PathBuf {
        self.dir.join(&self.executable)
    }

    /// The staging directory for `build`.
    pub fn staging(&self, build: u64) -> PathBuf {
        self.parent().join(format!("{STAGING_PREFIX}{build}"))
    }

    /// Where the previous payload is kept: `<name>.previous` beside it.
    pub fn previous(&self) -> PathBuf {
        let mut name = self.dir.file_name().unwrap_or_default().to_owned();
        name.push(PREVIOUS_SUFFIX);
        self.parent().join(name)
    }

    /// Whether this payload can replace itself: it carries no [`PACKAGE_MARKER`], OmaStore did not
    /// install it, the person can write it and its parent, and a staging directory can be created
    /// beside it on the same filesystem. The probe creates and removes
    /// `.luxforge-update-probe-<pid>`.
    pub fn mode(&self) -> Mode {
        if self.dir.join(PACKAGE_MARKER).symlink_metadata().is_ok() {
            return Mode::Notify(NotifyReason::PackageManaged);
        }
        if self.omastore_managed() {
            return Mode::Notify(NotifyReason::OmaStoreManaged);
        }
        if !writable(&self.dir) {
            return Mode::Notify(NotifyReason::PayloadNotWritable);
        }
        if !writable(self.parent()) {
            return Mode::Notify(NotifyReason::ParentNotWritable);
        }
        let probe = self
            .parent()
            .join(format!("{STAGING_PREFIX}probe-{}", std::process::id()));
        if fs::create_dir(&probe).is_err() && !probe.is_dir() {
            return Mode::Notify(NotifyReason::StagingUnavailable);
        }
        let same = same_filesystem(&probe, &self.dir);
        let _ = fs::remove_dir(&probe);
        match same {
            Some(true) => Mode::Payload,
            Some(false) => Mode::Notify(NotifyReason::CrossFilesystem),
            None => Mode::Notify(NotifyReason::StagingUnavailable),
        }
    }

    /// Whether the payload lies at `omastore/apps/<app>/<version>/<payload>`, where OmaStore installs
    /// a release. OmaStore keeps each version in its own directory and moves between them itself,
    /// so a swap in place would leave its record naming a version the directory no longer holds.
    fn omastore_managed(&self) -> bool {
        let mut apps = self.dir.ancestors().skip(3);
        OMASTORE_APPS.iter().rev().all(|name| {
            apps.next()
                .and_then(Path::file_name)
                .is_some_and(|found| found == *name)
        })
    }

    /// Reads `<feed_base_url><target>/appcast.xml` and chooses the newest item with a higher build
    /// than this payload's on one of `followed`. An item without `luxforge:catalogFormat` is never
    /// chosen.
    pub fn check(
        &self,
        fetch: &dyn Fetch,
        feed_base_url: &str,
        followed: &[Channel],
        cancel: &Cancel,
    ) -> Result<Checked> {
        let url = format!("{feed_base_url}{}", self.build.target.feed_path());
        let mut ignore = |_| {};
        let mut body = Bounded::new(
            Vec::new(),
            crate::MAX_APPCAST_BYTES as u64,
            cancel,
            &mut ignore,
        );
        let fetched = fetch.fetch(
            FetchRequest {
                url: &url,
                kind: FetchKind::Feed,
                max_bytes: crate::MAX_APPCAST_BYTES as u64,
                cancel,
            },
            &mut body,
        );
        let body = body.finish(fetched)?;
        let text = std::str::from_utf8(&body)
            .map_err(|_| Error::MalformedAppcast("the feed is not UTF-8".into()))?;
        let parsed = Appcast::parse(text)?;
        // Parsing removes unusable items; diagnostics still refer to the RSS document.
        let mut skipped = parsed.skipped;
        let parser_skip_count = skipped.len();
        let mut parser_skip = 0;
        let mut original_index = 0;
        let mut items = Vec::with_capacity(parsed.appcast.items.len());
        for item in parsed.appcast.items {
            while parser_skip < parser_skip_count && skipped[parser_skip].index == original_index {
                parser_skip += 1;
                original_index += 1;
            }
            if item.catalog_format.is_some() {
                items.push(item);
            } else {
                skipped.push(SkippedItem {
                    index: original_index,
                    reason: "no luxforge:catalogFormat".into(),
                });
            }
            original_index += 1;
        }
        skipped.sort_by_key(|item| item.index);
        Ok(Checked {
            item: newest_eligible(&items, self.build.build, followed).cloned(),
            skipped,
        })
    }

    /// Downloads `item`'s archive into `.luxforge-update-<build>` beside the payload, verifies it
    /// against `key`, extracts it into `Luxforge.new` there and checks the extracted `build.json`.
    /// `progress` is told the bytes received and the archive's stated length after each write.
    /// Any failure, cancellation included, removes the staging directory.
    pub fn prepare(
        &self,
        fetch: &dyn Fetch,
        item: &AppcastItem,
        key: &PublicKey,
        cancel: &Cancel,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Staged> {
        let length = item.enclosure.length;
        if length > MAX_ARCHIVE_BYTES {
            return Err(Error::Oversize {
                limit: MAX_ARCHIVE_BYTES,
            });
        }
        let staging = self.staging(item.build);
        if staging.symlink_metadata().is_ok() {
            remove_staging(&staging)?;
        }
        fs::create_dir(&staging)?;
        let prepared = self.prepare_in(&staging, fetch, item, key, cancel, progress);
        match prepared {
            Ok(()) => Ok(Staged {
                staging,
                version: item.version.clone(),
                build: item.build,
            }),
            Err(error) => {
                let _ = remove_staging(&staging);
                Err(if cancel.is_cancelled() {
                    Error::Cancelled
                } else {
                    error
                })
            }
        }
    }

    fn prepare_in(
        &self,
        staging: &Path,
        fetch: &dyn Fetch,
        item: &AppcastItem,
        key: &PublicKey,
        cancel: &Cancel,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<()> {
        let length = item.enclosure.length;
        let archive = staging.join(ARCHIVE_FILE);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive)?;
        let mut report = |received| progress(received, length);
        let mut sink = Bounded::new(
            BufWriter::with_capacity(WRITE_BUFFER_BYTES, file),
            length,
            cancel,
            &mut report,
        );
        let fetched = fetch.fetch(
            FetchRequest {
                url: &item.enclosure.url,
                kind: FetchKind::Archive,
                max_bytes: length,
                cancel,
            },
            &mut sink,
        );
        let received = sink.received();
        let mut writer = sink.finish(fetched)?;
        writer.flush()?;
        drop(writer);
        if received != length {
            return Err(Error::LengthMismatch {
                expected: length,
                actual: received,
            });
        }
        cancel.check()?;
        key.verify_file(&archive, length, &item.enclosure.ed_signature)?;
        cancel.check()?;
        let new_payload = staging.join(NEW_PAYLOAD);
        extract::extract(&archive, &new_payload, cancel, extract::Limits::DEFAULT)?;
        fs::remove_file(&archive)?;
        let built = BuildInfo::read(&new_payload)
            .map_err(|error| Error::BuildMismatch(format!("the update's {error}")))?;
        if built.version != item.version
            || built.build != item.build
            || built.target != self.build.target
        {
            return Err(Error::BuildMismatch(format!(
                "the update's build.json names {} build {} for {}, but the feed offered {} build \
                 {} for {}",
                built.version,
                built.build,
                built.target,
                item.version,
                item.build,
                self.build.target
            )));
        }
        let executable = new_payload.join(&self.executable);
        if !is_executable_file(&executable) {
            return Err(Error::BuildMismatch(format!(
                "the update has no executable {}",
                Path::new(&self.executable).display()
            )));
        }
        sync_filesystem(&new_payload)?;
        Ok(())
    }

    /// Removes a staged update that will not be installed.
    pub fn discard(&self, staged: &Staged) -> Result<()> {
        remove_staging(staged.staging())
    }

    /// After a successful launch: removes the previous payload, kept until now as a manual way
    /// back, and every staging directory beside the payload, including any an interrupted
    /// download or install left.
    pub fn clean_after_launch(&self) -> Result<Cleaned> {
        let mut cleaned = Cleaned::default();
        let previous = self.previous();
        if previous.symlink_metadata().is_ok() {
            remove_tree(&previous)?;
            cleaned.previous = true;
        }
        for entry in fs::read_dir(self.parent())? {
            let entry = entry?;
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| name.starts_with(STAGING_PREFIX))
            {
                let path = entry.path();
                remove_staging(&path)?;
                cleaned.staging.push(path);
            }
        }
        cleaned.staging.sort();
        Ok(cleaned)
    }
}

/// Removes a staging directory, or a stray file with a staging name.
fn remove_staging(path: &Path) -> Result<()> {
    remove_tree(path)
}

/// Removes `path` and everything under it, never following a link out of it, making read-only
/// directories writable first so an extracted tree is always removable.
fn remove_tree(path: &Path) -> Result<()> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() {
        return Ok(fs::remove_file(path)?);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o700 != 0o700 {
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o700);
            fs::set_permissions(path, permissions)?;
        }
    }
    for entry in fs::read_dir(path)? {
        remove_tree(&entry?.path())?;
    }
    Ok(fs::remove_dir(path)?)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.is_file() && metadata.permissions().mode() & 0o100 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

#[cfg(unix)]
fn writable(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok()
}

#[cfg(not(unix))]
fn writable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| !metadata.permissions().readonly())
}

/// Whether `a` and `b` are on one filesystem, when both can be read.
#[cfg(unix)]
fn same_filesystem(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt as _;
    Some(a.metadata().ok()?.dev() == b.metadata().ok()?.dev())
}

#[cfg(not(unix))]
fn same_filesystem(_: &Path, _: &Path) -> Option<bool> {
    None
}

/// Flushes the filesystem holding `path` before the swap, so a crash after it cannot leave a
/// renamed payload whose files never reached the disk. Linux only; elsewhere the swap relies on
/// the filesystem's own ordering.
fn sync_filesystem(path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        #[cfg(test)]
        if let Some(kind) = SYNC_FAILURE.with(|failure| failure.take()) {
            return Err(std::io::Error::from(kind).into());
        }
        let directory = fs::File::open(path)?;
        rustix::fs::syncfs(&directory).map_err(std::io::Error::from)?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = path;
    Ok(())
}

// One-shot, thread-local fault so a failed-sync test cannot affect another preparation.
#[cfg(all(test, target_os = "linux"))]
std::thread_local! {
    static SYNC_FAILURE: std::cell::Cell<Option<std::io::ErrorKind>> = const { std::cell::Cell::new(None) };
}
