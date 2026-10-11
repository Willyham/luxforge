//! Managed resources: pinned files a module declares and the host installs, verifies and removes.
//! A resource version lives in `<resources>/<module_id>/<resource_id>/<version>/`, which holds its
//! own file, named after the resource identity (a validated plain name, where a URL's file name is
//! not), each further file under its declared name, and `installed.json`, written last. Only a
//! directory whose `installed.json` parses and matches the declaration, beside every file at its
//! declared length, is installed, so an interrupted, short, corrupt, oversized, redirected or
//! disk-full transfer never looks installed, and a resource of several files is installed whole or
//! not at all.
//!
//! The owner answers from stats and small marker files; every byte of a resource is downloaded,
//! hashed and checked on the transfer lane. A download streams into
//! `<resources>/.staging/<module_id>/<resource_id>/<version>/` beside a `.partial.json` naming the
//! pins its files were fetched under, and is moved into place only once every file's length and
//! SHA-256, the module's format check and the storage quota pass. A transfer that is interrupted
//! (a network failure, an error status or a cancel) keeps what it staged, and the next install of
//! the same pins resumes each file from its length with an HTTP `Range` request; the whole file is
//! still hashed at the end, so a resumed file that does not match its pin is refused and its
//! staging discarded. A verification failure, a refusal, a full disk or a revoked grant discards
//! the staging; staging no current declaration names is removed by the next install, and a
//! removal discards a resource's partial transfer with it. Downloaded bytes are never executed or
//! deserialized by the host; the hash proves they are the pinned bytes, not that they are safe or
//! licensed for reuse. See `docs/design/module-capabilities.md#lifecycle-jobs-and-resources`.
use super::{
    descriptor::{PinnedFile, ResourceDescriptor},
    document::JsonDocument,
    endpoint::{EndpointClass, parse_endpoint},
    transport::{MAX_REDIRECTS, Method, RedirectPolicy, SendOptions, Transport, TransportRequest},
};
use crate::{
    Error, ErrorKind, JobId, ModuleRegistry, atomic_file,
    editor::now_ms,
    jobs::{JobControl, PERMISSION_REVOKED},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// The only `installed.json` and `.partial.json` format this build reads or writes.
pub(crate) const INSTALLED_FORMAT: u32 = 1;
pub(crate) const INSTALLED_FILE: &str = "installed.json";
/// The record beside a staged transfer of the pins its files were fetched under. Its name starts
/// with a dot, which no declared file name does.
pub(crate) const PARTIAL_FILE: &str = ".partial.json";
/// Where transfers are staged, under the resource root. Its name starts with a dot, which no module
/// identity does, so it never collides with a module's directory.
pub(crate) const STAGING_DIR: &str = ".staging";
/// The storage all modules' installed resources may take together, unless configured otherwise.
pub(crate) const DEFAULT_RESOURCE_QUOTA_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// The largest `installed.json` or `.partial.json` read.
const MAX_MARKER_BYTES: u64 = 64 * 1024;
/// The staged file's write buffer, and the piece a resumed file's staged bytes are hashed in.
const STAGE_BUFFER: usize = 64 * 1024;

/// The resource methods.
pub(crate) const RESOURCE_LIST: &str = "module.resource.list";
pub const INSTALL: &str = "module.resource.install";
pub const REMOVE: &str = "module.resource.remove";

/// A download's connection and idle limits, and the slowest average rate its whole-request deadline
/// allows on top of [`DOWNLOAD_BASE`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_BASE: Duration = Duration::from_secs(120);
const DOWNLOAD_MIN_RATE: u64 = 64 * 1024;

/// One file as an install record names it: where it came from, its hash and its length.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FilePin {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

impl FilePin {
    fn of(file: PinnedFile<'_>) -> Self {
        Self {
            name: file.name.to_owned(),
            url: file.url.to_owned(),
            sha256: file.sha256.to_owned(),
            bytes: file.bytes,
        }
    }
}

/// `installed.json`: what was installed, from where, by whom and when, in a [`JsonDocument`] with
/// its own `format: 1` marker. `url`, `sha256` and `bytes` are the resource's own file's; `files`
/// lists its further files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledMarker {
    pub module_id: String,
    pub resource_id: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FilePin>,
    pub license: String,
    pub provenance: String,
    pub actor: String,
    pub installed_ms: i64,
}

impl InstalledMarker {
    /// The marker document of one version directory.
    fn document(version_dir: &Path) -> JsonDocument<Self> {
        JsonDocument::new(
            version_dir,
            INSTALLED_FILE,
            MAX_MARKER_BYTES,
            INSTALLED_FORMAT,
        )
    }

    /// The marker a version directory holds, when it holds one this build reads.
    fn read(version_dir: &Path) -> Option<Self> {
        Self::document(version_dir).read().ok().flatten()
    }

    fn matches(&self, module_id: &str, resource: &ResourceDescriptor) -> bool {
        self.module_id == module_id
            && self.resource_id == resource.id
            && self.version == resource.version
            && self.sha256 == resource.sha256
            && self.bytes == resource.bytes
            && self.files.len() == resource.files.len()
            && self
                .files
                .iter()
                .zip(resource.pinned_files().skip(1))
                .all(|(held, declared)| *held == FilePin::of(declared))
    }

    /// The bytes every file the marker records takes.
    fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(self.bytes, |total, file| total.saturating_add(file.bytes))
    }
}

/// `.partial.json`: the resource a staging directory holds a transfer of, and the pin of every
/// file it fetches. A staged file resumes only under the pin it was fetched under.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialMarker {
    module_id: String,
    resource_id: String,
    version: String,
    files: Vec<FilePin>,
}

impl PartialMarker {
    fn document(staging: &Path) -> JsonDocument<Self> {
        JsonDocument::new(staging, PARTIAL_FILE, MAX_MARKER_BYTES, INSTALLED_FORMAT)
    }
}

/// `source` of `module.resource.install`: the declared URL, under a `download-artifact` grant, is
/// the one source. No client names a path the host reads.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum InstallSource {
    /// A struct variant, so any other field, a path included, is refused rather than ignored.
    Download {},
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceState {
    NotInstalled,
    Installing,
    Installed,
    /// The last install failed; nothing is installed.
    Failed,
}

impl ResourceState {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::NotInstalled => "not-installed",
            Self::Installing => "installing",
            Self::Installed => "installed",
            Self::Failed => "failed",
        }
    }
}

/// One file of a resource as `module.resource.list` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceFileRow {
    pub name: String,
    pub bytes: u64,
}

/// What a model needs of the machine that runs it: the backends, precision, kind and resident
/// size of the `local-runtime` capability that holds it, and the resource's machine minimum. Each
/// part is present when it is declared.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirements {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<super::descriptor::ModelKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backends: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<crate::inference::Precision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_memory_bytes: Option<u64>,
}

impl Requirements {
    /// What `resource` of `module` needs: `None` when it declares no minimum and no
    /// `local-runtime` capability holds it.
    pub(crate) fn of(
        module: &crate::ModuleDescriptor,
        resource: &ResourceDescriptor,
    ) -> Option<Self> {
        let runtime = module
            .capabilities
            .iter()
            .find_map(|capability| match &capability.kind {
                super::descriptor::CapabilityKind::LocalRuntime {
                    resource: held,
                    model,
                    resident_bytes,
                    backends,
                    precision,
                } if *held == resource.id => Some((*model, *resident_bytes, backends, *precision)),
                _ => None,
            });
        if runtime.is_none() && resource.min_memory_bytes.is_none() {
            return None;
        }
        Some(Self {
            model: runtime.map(|(model, ..)| model),
            backends: runtime
                .map(|(_, _, backends, _)| backends.clone())
                .unwrap_or_default(),
            precision: runtime.map(|(.., precision)| precision),
            resident_bytes: runtime.map(|(_, resident, ..)| resident),
            min_memory_bytes: resource.min_memory_bytes,
        })
    }
}

/// One declared resource as `module.resource.list` and `module.status` report it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceRow {
    pub id: String,
    pub title: String,
    pub version: String,
    /// Every file's bytes together.
    pub bytes: u64,
    pub sha256: String,
    pub license: String,
    pub provenance: String,
    pub url: String,
    /// Every file, the resource's own first.
    #[serde(default)]
    pub files: Vec<ResourceFileRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tradeoff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_restriction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<Requirements>,
    /// `false` when the machine has less memory than the resource's minimum, which it then cannot
    /// install; absent when there is no minimum or the host cannot tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fits: Option<bool>,
    pub state: ResourceState,
    /// The bytes an interrupted transfer has staged, which the next install resumes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::jobs::JobError>,
}

/// The resource root and the paths under it. Holds no state: every question is answered from the
/// disk.
#[derive(Clone, Debug)]
pub(crate) struct ResourceStore {
    root: PathBuf,
}

impl ResourceStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<module_id>/<resource_id>/<version>`.
    pub(crate) fn version_dir(&self, module_id: &str, resource: &ResourceDescriptor) -> PathBuf {
        self.root
            .join(module_id)
            .join(&resource.id)
            .join(&resource.version)
    }

    /// The installed own file: named after the resource identity inside its version directory.
    /// Its further files are beside it, under their declared names.
    pub(crate) fn file_path(&self, module_id: &str, resource: &ResourceDescriptor) -> PathBuf {
        self.version_dir(module_id, resource).join(&resource.id)
    }

    fn staging(&self) -> PathBuf {
        self.root.join(STAGING_DIR)
    }

    /// `<root>/.staging/<module_id>/<resource_id>/<version>`: where a transfer of the resource is
    /// staged, and stays while it is interrupted.
    pub(crate) fn staging_dir(&self, module_id: &str, resource: &ResourceDescriptor) -> PathBuf {
        self.staging()
            .join(module_id)
            .join(&resource.id)
            .join(&resource.version)
    }

    /// The marker of an installed resource, when the version directory holds one that matches the
    /// declaration and every file at its declared length. One small read and a stat a file; no
    /// hashing.
    pub(crate) fn installed(
        &self,
        module_id: &str,
        resource: &ResourceDescriptor,
    ) -> Option<InstalledMarker> {
        let dir = self.version_dir(module_id, resource);
        let marker = InstalledMarker::read(&dir)?;
        let whole = resource.pinned_files().all(|file| {
            fs::metadata(dir.join(file.name))
                .is_ok_and(|meta| meta.is_file() && meta.len() == file.bytes)
        });
        (marker.matches(module_id, resource) && whole).then_some(marker)
    }

    /// The bytes an interrupted transfer of the resource's current pins has staged, when it has
    /// staged any: a small read and a stat a file.
    pub(crate) fn staged_bytes(
        &self,
        module_id: &str,
        resource: &ResourceDescriptor,
    ) -> Option<u64> {
        let dir = self.staging_dir(module_id, resource);
        let partial = PartialMarker::document(&dir).read().ok().flatten()?;
        let staged = resource
            .pinned_files()
            .filter(|file| partial.files.contains(&FilePin::of(*file)))
            .map(|file| {
                fs::metadata(dir.join(file.name)).map_or(0, |meta| meta.len().min(file.bytes))
            })
            .sum::<u64>();
        (staged > 0).then_some(staged)
    }

    /// The bytes every installed resource under the root takes, by their markers: a walk of three
    /// directory levels and one small read per installed version. Staging is not counted.
    pub(crate) fn used_bytes(&self) -> u64 {
        let mut used = 0u64;
        for module in dirs(&self.root) {
            if module
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            for resource in dirs(&module) {
                for version in dirs(&resource) {
                    if let Some(marker) = InstalledMarker::read(&version) {
                        used = used.saturating_add(marker.total_bytes());
                    }
                }
            }
        }
        used
    }
}

/// The directories directly under `dir`.
fn dirs(dir: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
}

/// Refuse an install that would take the stored resources past the quota.
pub(crate) fn check_quota(
    store: &ResourceStore,
    resource: &ResourceDescriptor,
    quota: u64,
) -> Result<(), Error> {
    let used = store.used_bytes();
    let bytes = resource.total_bytes();
    if used.saturating_add(bytes) > quota {
        return Err(Error::resource_limit(format!(
            "installing {} ({bytes} bytes) would exceed the resource quota: {used} of {quota} bytes are in use",
            resource.id
        )));
    }
    Ok(())
}

/// Everything an install job needs on the transfer lane.
pub(crate) struct InstallJob {
    pub store: ResourceStore,
    pub module_id: String,
    pub resource: ResourceDescriptor,
    /// The host's transport, which the declared URLs are downloaded through.
    pub transport: Arc<dyn Transport>,
    /// Asked to check the staged files' format, and which staging is still declared.
    pub registry: Arc<ModuleRegistry>,
    pub quota: u64,
    pub actor: String,
    pub control: Arc<JobControl>,
}

/// A staged file that hashes and counts what it is given and refuses to grow past the declared
/// length. A resumed file is opened at its staged length with those bytes already hashed. A write
/// error is kept, so the install reports it by its cause (a full disk is `resource-limit`) rather
/// than as the transport's generic storage failure.
struct Staged {
    file: BufWriter<File>,
    hasher: Sha256,
    written: u64,
    limit: u64,
    failure: Option<io::ErrorKind>,
    #[cfg(test)]
    fault: Option<(u64, io::ErrorKind)>,
}

impl Staged {
    /// Open `path`, keeping its first `offset` bytes and hashing them, checking `control` between
    /// pieces; a file is created when there is none.
    fn open(path: &Path, offset: u64, limit: u64, control: &JobControl) -> Result<Self, Error> {
        let failed = |error: io::Error| write_failure(path, error.kind());
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(failed)?;
        file.set_len(offset).map_err(failed)?;
        let mut hasher = Sha256::new();
        let mut piece = vec![0; STAGE_BUFFER];
        let mut hashed = 0;
        while hashed < offset {
            control.checkpoint()?;
            let read = file.read(&mut piece).map_err(failed)?;
            if read == 0 {
                break;
            }
            hasher.update(&piece[..read]);
            hashed += read as u64;
        }
        file.seek(SeekFrom::Start(hashed)).map_err(failed)?;
        Ok(Self {
            file: BufWriter::with_capacity(STAGE_BUFFER, file),
            hasher,
            written: hashed,
            limit,
            failure: None,
            #[cfg(test)]
            fault: None,
        })
    }

    /// Start the file again from nothing: the server sent the whole file where a range was asked
    /// for.
    fn restart(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.get_mut().set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.hasher = Sha256::new();
        self.written = 0;
        Ok(())
    }

    fn remember(&mut self, error: io::Error) -> io::Error {
        self.failure.get_or_insert(error.kind());
        error
    }

    /// Flush and sync the staged bytes, returning the length and the hash of the whole file.
    fn finish(mut self) -> Result<(u64, String), io::ErrorKind> {
        if let Some(kind) = self.failure {
            return Err(kind);
        }
        self.file.flush().map_err(|error| error.kind())?;
        atomic_file::flush(self.file.get_ref()).map_err(|error| error.kind())?;
        Ok((self.written, format!("{:x}", self.hasher.finalize())))
    }
}

impl Write for Staged {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let after = self.written.saturating_add(buf.len() as u64);
        if after > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "longer than the pinned length",
            ));
        }
        #[cfg(test)]
        if let Some((at, kind)) = self.fault
            && after > at
        {
            let error = io::Error::from(kind);
            return Err(self.remember(error));
        }
        match self.file.write_all(buf) {
            Ok(()) => {
                self.hasher.update(buf);
                self.written = after;
                Ok(buf.len())
            }
            Err(error) => Err(self.remember(error)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush().map_err(|error| self.remember(error))
    }
}

/// How the answer to one file's request is taken, as its head decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Take {
    /// Not decided yet: no head has arrived.
    Undecided,
    /// Append to what is staged: a fresh file, or the range asked for.
    Append,
    /// The whole file arrived where a range was asked for: start the staged file again.
    Restart,
}

/// The sink one file's request writes into: the staged file, started again first when the head
/// said so, reporting the resource's progress after each piece.
struct Sink<'a> {
    staged: &'a mut Staged,
    take: &'a Cell<Take>,
    control: &'a JobControl,
    /// The bytes of the resource's earlier files, and of all of them.
    before: u64,
    total: u64,
}

impl Write for Sink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.take.get() == Take::Restart {
            self.staged.restart()?;
            self.take.set(Take::Append);
        }
        let written = self.staged.write(buf)?;
        self.control.set_progress(
            Some((self.before + self.staged.written) as f64 / self.total as f64),
            "downloading",
        );
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.staged.flush()
    }
}

/// A write failure by its cause: a full disk or quota is `resource-limit: disk full`, anything else
/// a `read-error` naming the path.
fn write_failure(path: &Path, kind: io::ErrorKind) -> Error {
    atomic_file::file_error(format_args!("cannot write {}", path.display()), kind)
}

/// Remove `path`, a directory or a file; a missing one is already removed.
fn remove_path(path: &Path) -> io::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Remove every staging entry no registered module's current declaration names: anything but
/// `<module>/<resource>/<version>` directories of declared resources at their declared version.
/// The transfer lane runs one job at a time, so none of it belongs to a running job; what stays is
/// an interrupted transfer a later install resumes.
fn clear_stale_staging(store: &ResourceStore, registry: &ModuleRegistry) -> Result<(), Error> {
    let staging = store.staging();
    let entries = |dir: &Path| -> Result<Vec<(PathBuf, String, bool)>, Error> {
        match fs::read_dir(dir) {
            Ok(entries) => Ok(entries
                .flatten()
                .map(|entry| {
                    let directory = entry.file_type().is_ok_and(|kind| kind.is_dir());
                    let name = entry.file_name().to_string_lossy().into_owned();
                    (entry.path(), name, directory)
                })
                .collect()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(atomic_file::file_error(dir.display(), error.kind())),
        }
    };
    let discard = |path: &Path| {
        remove_path(path).map_err(|error| atomic_file::file_error(path.display(), error.kind()))
    };
    for (module_dir, module_id, directory) in entries(&staging)? {
        let module = registry
            .module(&module_id)
            .map(|provider| provider.descriptor())
            .filter(|_| directory);
        let Some(module) = module else {
            discard(&module_dir)?;
            continue;
        };
        for (resource_dir, resource_id, directory) in entries(&module_dir)? {
            let resource = module.resource(&resource_id).filter(|_| directory);
            let Some(resource) = resource else {
                discard(&resource_dir)?;
                continue;
            };
            for (version_dir, version, directory) in entries(&resource_dir)? {
                if !directory || version != resource.version {
                    discard(&version_dir)?;
                }
            }
        }
    }
    Ok(())
}

/// Remove `dir`, when it is empty, and each parent up to the resource root that it leaves empty. A
/// directory already gone, such as a staging directory moved into place, is passed over.
fn prune_empty(store: &ResourceStore, dir: &Path) {
    for parent in dir.ancestors() {
        if parent == store.root() || !parent.starts_with(store.root()) {
            break;
        }
        match fs::remove_dir(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => break,
        }
    }
}

/// Whether a failed install keeps what it staged for the next one to resume: an interruption (a
/// network failure, an error status or a cancel), never a verification failure, a refusal, a full
/// disk or a revoked grant.
fn resumable(error: &Error) -> bool {
    match error.kind {
        ErrorKind::FileAccess => true,
        ErrorKind::Cancelled => error.detail != PERMISSION_REVOKED,
        _ => false,
    }
}

/// Install one resource on the transfer lane: clear staging nothing declares, stream each file into
/// the resource's staging directory (resuming what an interrupted transfer of the same pins
/// staged), verify each, ask the module to check them, check the quota, move the directory into
/// place and write `installed.json` last. A failure keeps an interrupted transfer's staging and
/// removes any other.
pub(crate) fn install(job: InstallJob) -> Result<Value, Error> {
    clear_stale_staging(&job.store, &job.registry)?;
    let staging = job.store.staging_dir(&job.module_id, &job.resource);
    fs::create_dir_all(&staging).map_err(|error| write_failure(&staging, error.kind()))?;
    let result = stage_and_publish(&job, &staging);
    if let Err(error) = &result {
        let keep = resumable(error)
            && job
                .store
                .staged_bytes(&job.module_id, &job.resource)
                .is_some();
        if !keep {
            let _ = fs::remove_dir_all(&staging);
        }
    }
    prune_empty(&job.store, &staging);
    result
}

fn stage_and_publish(job: &InstallJob, staging: &Path) -> Result<Value, Error> {
    let resource = &job.resource;
    let pins: Vec<FilePin> = resource.pinned_files().map(FilePin::of).collect();
    // What is staged resumes only under the pins it was fetched under: a file whose pin changed, or
    // anything the resource does not declare, starts again.
    let partial = PartialMarker::document(staging);
    let held = partial.read().ok().flatten().unwrap_or_default();
    for entry in fs::read_dir(staging).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kept = name == PARTIAL_FILE
            || (held.module_id == job.module_id
                && held.resource_id == resource.id
                && held.version == resource.version
                && pins
                    .iter()
                    .any(|pin| pin.name == name && held.files.contains(pin)));
        if !kept {
            remove_path(&entry.path())
                .map_err(|error| write_failure(&entry.path(), error.kind()))?;
        }
    }
    partial.write(&PartialMarker {
        module_id: job.module_id.clone(),
        resource_id: resource.id.clone(),
        version: resource.version.clone(),
        files: pins,
    })?;
    let total = resource.total_bytes();
    job.control.set_progress(Some(0.0), "downloading");
    let mut before = 0;
    let mut hashes = Vec::new();
    for file in resource.pinned_files() {
        let path = staging.join(file.name);
        let sha256 = fetch(job, file, &path, before, total)?;
        hashes.push(sha256);
        before += file.bytes;
    }
    job.control.checkpoint()?;
    job.control.set_progress(Some(1.0), "checking the format");
    let module = job
        .registry
        .capabilities(&job.module_id)
        .ok_or_else(|| Error::internal(format!("module {} is not registered", job.module_id)))?;
    for file in resource.pinned_files() {
        module.validate_resource(&resource.id, &staging.join(file.name))?;
    }
    job.control.checkpoint()?;
    check_quota(&job.store, resource, job.quota)?;
    let partial = staging.join(PARTIAL_FILE);
    fs::remove_file(&partial).map_err(|error| write_failure(&partial, error.kind()))?;
    publish(job, staging)?;
    let mut result = json!({
        "resource_id": resource.id,
        "version": resource.version,
        "path": job.store.file_path(&job.module_id, resource),
        "bytes": total,
        "sha256": hashes.first(),
    });
    if !resource.files.is_empty() {
        result["files"] = resource
            .pinned_files()
            .zip(&hashes)
            .map(|(file, sha256)| json!({"name": file.name, "bytes": file.bytes, "sha256": sha256}))
            .collect();
    }
    Ok(result)
}

/// Fetch one file into `path`, resuming from what is staged there, and check its length and
/// SHA-256 over the whole file. Answers the hash.
fn fetch(
    job: &InstallJob,
    file: PinnedFile<'_>,
    path: &Path,
    before: u64,
    total: u64,
) -> Result<String, Error> {
    let staged_length = fs::metadata(path)
        .ok()
        .filter(fs::Metadata::is_file)
        .map_or(0, |meta| meta.len());
    // A staged file longer than its pin is not a prefix of it.
    let offset = if staged_length > file.bytes {
        0
    } else {
        staged_length
    };
    let mut staged = Staged::open(path, offset, file.bytes, &job.control)?;
    #[cfg(test)]
    {
        staged.fault = faults::find(job.store.root());
    }
    job.control
        .set_progress(Some((before + offset) as f64 / total as f64), "downloading");
    if offset < file.bytes {
        download(job, file, &mut staged, path, offset, before, total)?;
    }
    let (length, sha256) = staged.finish().map_err(|kind| write_failure(path, kind))?;
    job.control.checkpoint()?;
    if length != file.bytes {
        return Err(Error::validation(format!(
            "{} is {length} bytes; it is pinned at {} bytes",
            file.name, file.bytes
        )));
    }
    if sha256 != file.sha256 {
        return Err(Error::validation(if offset > 0 {
            format!(
                "{} does not match its pinned hash after resuming at byte {offset}; its partial transfer is discarded",
                file.name
            )
        } else {
            format!("{} does not match its pinned hash", file.name)
        }));
    }
    Ok(sha256)
}

/// Stream the rest of one file from its declared URL through the host transport, under the
/// resource's redirect origins and the job's cancel flag: from `offset` with a `Range` request
/// when some of it is staged. The head decides how the body is taken before any of it is written:
/// the range asked for is appended, a whole file starts the staged one again, and anything else
/// stops the request.
#[allow(clippy::too_many_arguments)]
fn download(
    job: &InstallJob,
    file: PinnedFile<'_>,
    staged: &mut Staged,
    path: &Path,
    offset: u64,
    before: u64,
    total: u64,
) -> Result<(), Error> {
    let resource = &job.resource;
    let endpoint = parse_endpoint(file.url, &[EndpointClass::Remote, EndpointClass::Loopback])?;
    let origin = endpoint.origin();
    let mut headers = Vec::new();
    if offset > 0 {
        headers.push(("Range".to_owned(), format!("bytes={offset}-")));
    }
    let request = TransportRequest {
        method: Method::Get,
        endpoint,
        headers,
        body: Vec::new(),
    };
    let take = Cell::new(Take::Undecided);
    let name = file.name;
    let pinned = file.bytes;
    let mut head = |status: u16, fields: &[(String, String)]| -> Result<(), Error> {
        let field = |wanted: &str| {
            fields
                .iter()
                .find(|(field, _)| field.eq_ignore_ascii_case(wanted))
                .map(|(_, value)| value.trim())
        };
        match status {
            200 if offset > 0 => take.set(Take::Restart),
            200 => take.set(Take::Append),
            206 if offset > 0 => {
                let expected = format!("bytes {offset}-{}/{pinned}", pinned - 1);
                let range = field("content-range");
                if range != Some(expected.as_str()) {
                    return Err(Error::validation(format!(
                        "{origin} answered the range {} for {name}; resuming at byte {offset} needs {expected}",
                        range.unwrap_or("none")
                    )));
                }
                take.set(Take::Append);
            }
            206 => {
                return Err(Error::validation(format!(
                    "{origin} answered a range for {name} where the whole file was asked for"
                )));
            }
            416 => {
                return Err(Error::validation(format!(
                    "{origin} cannot resume {name} at byte {offset}; its partial transfer is discarded"
                )));
            }
            status => {
                return Err(Error::file_access(format!(
                    "{origin} answered HTTP {status} for {name}"
                )));
            }
        }
        Ok(())
    };
    let mut sink = Sink {
        staged,
        take: &take,
        control: &job.control,
        before,
        total,
    };
    let sent = job.transport.send(
        &request,
        SendOptions {
            max_request_bytes: 0,
            max_response_bytes: pinned,
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: IDLE_TIMEOUT,
            total_timeout: DOWNLOAD_BASE
                .saturating_add(Duration::from_secs(pinned / DOWNLOAD_MIN_RATE)),
            redirects: RedirectPolicy {
                max: MAX_REDIRECTS,
                origins: &resource.redirect_origins,
            },
            control: &job.control,
            progress: &mut |_, _| {},
            head: Some(&mut head),
        },
        &mut sink,
    );
    match sent {
        Ok(_) => Ok(()),
        Err(error) => Err(match sink.staged.failure {
            Some(kind) => write_failure(path, kind),
            None => error,
        }),
    }
}

/// Move the verified staging directory into place and write `installed.json` last. A version
/// directory without a matching marker, left by a crash or an older interrupted install, is
/// replaced. A failure after the move removes the version directory again.
fn publish(job: &InstallJob, staging: &Path) -> Result<(), Error> {
    let resource = &job.resource;
    let target = job.store.version_dir(&job.module_id, resource);
    let parent = target
        .parent()
        .expect("a version directory has a parent")
        .to_path_buf();
    let moved = (|| {
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::create_dir_all(&parent)?;
        fs::rename(staging, &target)
    })();
    moved.map_err(|error| write_failure(&target, error.kind()))?;
    let marker = InstalledMarker {
        module_id: job.module_id.clone(),
        resource_id: resource.id.clone(),
        version: resource.version.clone(),
        url: resource.url.clone(),
        sha256: resource.sha256.clone(),
        bytes: resource.bytes,
        files: resource.pinned_files().skip(1).map(FilePin::of).collect(),
        license: resource.license.clone(),
        provenance: resource.provenance.clone(),
        actor: job.actor.clone(),
        installed_ms: now_ms(),
    };
    // The transfer lane runs one job at a time, so the install is the marker's one writer.
    let written = atomic_file::sync_dir(&parent)
        .map_err(|error| write_failure(&parent, error.kind()))
        .and_then(|()| InstalledMarker::document(&target).write(&marker));
    if written.is_err() {
        let _ = fs::remove_dir_all(&target);
    }
    written
}

/// Remove one installed resource version on the transfer lane, with every file of it: its marker
/// first, so an interruption leaves a directory that is no longer installed, then the directory,
/// then any parent it leaves empty; and any interrupted transfer of it. It never touches a catalog,
/// a recipe or an artifact.
pub(crate) fn remove(
    store: &ResourceStore,
    module_id: &str,
    resource: &ResourceDescriptor,
    control: &JobControl,
) -> Result<Value, Error> {
    control.checkpoint()?;
    let target = store.version_dir(module_id, resource);
    let marker = target.join(INSTALLED_FILE);
    let existed = target.exists();
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(atomic_file::file_error(marker.display(), error.kind())),
    }
    match fs::remove_dir_all(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(atomic_file::file_error(target.display(), error.kind())),
    }
    // Emptied parents go too; a parent that still holds another version stays.
    for parent in target.ancestors().skip(1).take(2) {
        if fs::remove_dir(parent).is_err() {
            break;
        }
    }
    let staging = store.staging_dir(module_id, resource);
    let staged = staging.exists();
    remove_path(&staging)
        .map_err(|error| atomic_file::file_error(staging.display(), error.kind()))?;
    prune_empty(store, &staging);
    Ok(json!({
        "resource_id": resource.id,
        "version": resource.version,
        "removed": existed,
        "discarded_partial": staged,
    }))
}

/// Injected write failures for tests, keyed by resource root so parallel tests never see each
/// other's.
#[cfg(test)]
pub(crate) mod faults {
    use std::{
        io,
        path::{Path, PathBuf},
        sync::Mutex,
    };

    static FAULTS: Mutex<Vec<(PathBuf, u64, io::ErrorKind)>> = Mutex::new(Vec::new());

    /// Fail every staged write under `root` that would pass `after` bytes with `kind`.
    pub(crate) fn inject(root: &Path, after: u64, kind: io::ErrorKind) {
        FAULTS
            .lock()
            .unwrap()
            .push((root.to_path_buf(), after, kind));
    }

    pub(crate) fn clear(root: &Path) {
        FAULTS.lock().unwrap().retain(|(held, ..)| held != root);
    }

    pub(super) fn find(root: &Path) -> Option<(u64, io::ErrorKind)> {
        FAULTS
            .lock()
            .unwrap()
            .iter()
            .find(|(held, ..)| held == root)
            .map(|(_, after, kind)| (*after, *kind))
    }
}
