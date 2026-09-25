//! Managed resources: pinned files a module declares and the host installs, verifies and removes.
//! A resource version lives in `<resources>/<module_id>/<resource_id>/<version>/`, which holds the
//! file, named after the resource identity (a validated plain name, where a URL's file name is
//! not), and `installed.json`, written last. Only a directory whose `installed.json` parses and
//! matches the declaration, beside a file of the declared length, is installed, so an interrupted,
//! short, corrupt, oversized, redirected or disk-full transfer never looks installed.
//!
//! The owner answers from stats and small marker files; every byte of a resource is read, hashed,
//! copied or downloaded on the transfer lane. A transfer streams into
//! `<resources>/.staging/<job_id>/` and is moved into place only once its length, SHA-256, format
//! check and the storage quota pass. A crash leaves at worst a staging directory, which the next
//! install removes. Downloaded bytes are never executed or deserialized by the host; the hash
//! proves they are the pinned bytes, not that they are safe or licensed for reuse. See
//! `docs/design/module-capabilities.md#lifecycle-jobs-and-resources`.
use super::{
    atomic,
    descriptor::ResourceDescriptor,
    grants::now_ms,
    jobs::JobControl,
    transport::{
        EndpointClass, Method, RedirectPolicy, SendOptions, Transport, TransportConfig,
        TransportRequest, parse_endpoint,
    },
};
use crate::{Error, ErrorKind, JobId, ModuleRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// The only `installed.json` format this build reads or writes.
pub const INSTALLED_FORMAT: u32 = 1;
pub const INSTALLED_FILE: &str = "installed.json";
const INSTALLED_TEMPORARY: &str = "installed.json.tmp";
/// Where transfers are staged, under the resource root. Its name starts with a dot, which no module
/// identity does, so it never collides with a module's directory.
pub const STAGING_DIR: &str = ".staging";
/// The storage all modules' installed resources may take together, unless configured otherwise.
pub const DEFAULT_RESOURCE_QUOTA_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// The largest `installed.json` read.
const MAX_MARKER_BYTES: u64 = 64 * 1024;
/// Bytes copied from a local file at a time.
const COPY_BUFFER: usize = 64 * 1024;

/// The resource methods.
pub const RESOURCE_LIST: &str = "module.resource.list";
pub const INSTALL: &str = "module.resource.install";
pub const REMOVE: &str = "module.resource.remove";

/// A download's connection and idle limits, and the slowest average rate its whole-request deadline
/// allows on top of [`DOWNLOAD_BASE`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_BASE: Duration = Duration::from_secs(120);
const DOWNLOAD_MIN_RATE: u64 = 64 * 1024;

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// Where an installed resource came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstalledFrom {
    Download,
    File,
}

/// `installed.json`: what was installed, from where, by whom and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledMarker {
    pub format: u32,
    pub module_id: String,
    pub resource_id: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub license: String,
    pub provenance: String,
    pub source: InstalledFrom,
    pub actor: String,
    pub installed_ms: u64,
}

impl InstalledMarker {
    fn matches(&self, module_id: &str, resource: &ResourceDescriptor) -> bool {
        self.format == INSTALLED_FORMAT
            && self.module_id == module_id
            && self.resource_id == resource.id
            && self.version == resource.version
            && self.sha256 == resource.sha256
            && self.bytes == resource.bytes
    }
}

/// `source` of `module.resource.install`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum InstallSource {
    /// The declared URL, under a `download-artifact` grant.
    #[default]
    Download,
    /// A local copy; only bytes matching the pinned hash are accepted, so it needs no grant.
    File { path: PathBuf },
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
    pub fn name(self) -> &'static str {
        match self {
            Self::NotInstalled => "not-installed",
            Self::Installing => "installing",
            Self::Installed => "installed",
            Self::Failed => "failed",
        }
    }
}

/// One declared resource as `module.resource.list` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceRow {
    pub id: String,
    pub title: String,
    pub version: String,
    pub bytes: u64,
    pub sha256: String,
    pub license: String,
    pub provenance: String,
    pub url: String,
    pub state: ResourceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<super::jobs::JobError>,
}

/// The resource root and the paths under it. Holds no state: every question is answered from the
/// disk.
#[derive(Clone, Debug)]
pub struct ResourceStore {
    root: PathBuf,
}

impl ResourceStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<module_id>/<resource_id>/<version>`.
    pub fn version_dir(&self, module_id: &str, resource: &ResourceDescriptor) -> PathBuf {
        self.root
            .join(module_id)
            .join(&resource.id)
            .join(&resource.version)
    }

    /// The installed file: named after the resource identity inside its version directory.
    pub fn file_path(&self, module_id: &str, resource: &ResourceDescriptor) -> PathBuf {
        self.version_dir(module_id, resource).join(&resource.id)
    }

    fn staging(&self) -> PathBuf {
        self.root.join(STAGING_DIR)
    }

    /// The marker of an installed resource, when the version directory holds one that matches the
    /// declaration and a file of the declared length. One small read and two stats; no hashing.
    pub fn installed(
        &self,
        module_id: &str,
        resource: &ResourceDescriptor,
    ) -> Option<InstalledMarker> {
        let marker = read_marker(&self.version_dir(module_id, resource).join(INSTALLED_FILE))?;
        let length = fs::metadata(self.file_path(module_id, resource))
            .ok()
            .filter(fs::Metadata::is_file)?
            .len();
        (marker.matches(module_id, resource) && length == resource.bytes).then_some(marker)
    }

    /// The bytes every installed resource under the root takes, by their markers: a walk of three
    /// directory levels and one small read per installed version. Staging is not counted.
    pub fn used_bytes(&self) -> u64 {
        let entries = |dir: &Path| {
            fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.path())
        };
        let mut used = 0u64;
        for module in entries(&self.root) {
            if module
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            for resource in entries(&module) {
                for version in entries(&resource) {
                    if let Some(marker) = read_marker(&version.join(INSTALLED_FILE)) {
                        used = used.saturating_add(marker.bytes);
                    }
                }
            }
        }
        used
    }
}

fn read_marker(path: &Path) -> Option<InstalledMarker> {
    let bytes = atomic::read(path, MAX_MARKER_BYTES).ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// Refuse an install that would take the stored resources past the quota.
pub(crate) fn check_quota(
    store: &ResourceStore,
    resource: &ResourceDescriptor,
    quota: u64,
) -> Result<(), Error> {
    let used = store.used_bytes();
    if used.saturating_add(resource.bytes) > quota {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "installing {} ({} bytes) would exceed the resource quota: {used} of {quota} bytes are in use",
                resource.id, resource.bytes
            ),
        ));
    }
    Ok(())
}

/// The one transport the host's downloads and task requests share, built on a lane the first time
/// one needs it rather than when the owner starts.
pub(crate) struct SharedTransport {
    config: TransportConfig,
    built: Mutex<Option<Arc<Transport>>>,
}

impl SharedTransport {
    pub(crate) fn new(config: TransportConfig) -> Self {
        Self {
            config,
            built: Mutex::new(None),
        }
    }

    pub(crate) fn get(&self) -> Result<Arc<Transport>, Error> {
        let mut built = self.built.lock().expect("shared transport");
        if let Some(transport) = built.as_ref() {
            return Ok(transport.clone());
        }
        let transport = Arc::new(Transport::new(self.config.clone())?);
        *built = Some(transport.clone());
        Ok(transport)
    }
}

/// Where an install's bytes come from, on the worker.
pub(crate) enum Fetch {
    Download(Arc<SharedTransport>),
    File(PathBuf),
}

/// Everything an install job needs on the transfer lane.
pub(crate) struct InstallJob {
    pub store: ResourceStore,
    pub job_id: JobId,
    pub module_id: String,
    pub resource: ResourceDescriptor,
    pub fetch: Fetch,
    /// Asked to check the staged file's format.
    pub registry: Arc<ModuleRegistry>,
    pub quota: u64,
    pub actor: String,
    pub control: Arc<JobControl>,
}

/// A staged file that hashes and counts what it is given and refuses to grow past the declared
/// length. A write error is kept, so the install reports it by its cause (a full disk is
/// `resource-limit`) rather than as the transport's generic storage failure.
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
    fn create(path: &Path, limit: u64) -> io::Result<Self> {
        Ok(Self {
            file: BufWriter::with_capacity(COPY_BUFFER, File::create(path)?),
            hasher: Sha256::new(),
            written: 0,
            limit,
            failure: None,
            #[cfg(test)]
            fault: None,
        })
    }

    fn remember(&mut self, error: io::Error) -> io::Error {
        self.failure.get_or_insert(error.kind());
        error
    }

    /// Flush and sync the staged bytes, returning the length and the hash.
    fn finish(mut self) -> Result<(u64, String), io::ErrorKind> {
        if let Some(kind) = self.failure {
            return Err(kind);
        }
        self.file.flush().map_err(|error| error.kind())?;
        self.file
            .get_ref()
            .sync_all()
            .map_err(|error| error.kind())?;
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

/// A write failure by its cause: a full disk or quota is `resource-limit: disk full`, anything else
/// a `read-error` naming the path.
fn write_failure(path: &Path, kind: io::ErrorKind) -> Error {
    match kind {
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
            Error::new(ErrorKind::ResourceLimit, "disk full")
        }
        kind => Error::new(
            ErrorKind::FileAccess,
            format!("cannot write {}: {kind}", path.display()),
        ),
    }
}

/// Remove every staging directory: the transfer lane runs one job at a time, so none of them
/// belongs to a running job.
fn clear_staging(store: &ResourceStore) -> Result<(), Error> {
    let staging = store.staging();
    let entries = match fs::read_dir(&staging) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(atomic::file_error(&staging, error)),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.map_err(|error| atomic::file_error(&path, error))?;
    }
    Ok(())
}

/// Install one resource on the transfer lane: clear stale staging, stream the bytes into a new
/// staging directory, verify them, ask the module to check the format, check the quota, move the
/// directory into place and write `installed.json` last. Any failure removes what it staged.
pub(crate) fn install(job: InstallJob) -> Result<Value, Error> {
    clear_staging(&job.store)?;
    let staging = job.store.staging().join(job.job_id.as_str());
    fs::create_dir_all(&staging).map_err(|error| write_failure(&staging, error.kind()))?;
    let result = stage_and_publish(&job, &staging);
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    // The staging root goes too once it is empty, which it is unless removing failed.
    let _ = fs::remove_dir(job.store.staging());
    result
}

fn stage_and_publish(job: &InstallJob, staging: &Path) -> Result<Value, Error> {
    let resource = &job.resource;
    let staged_path = staging.join(&resource.id);
    let staged = Staged::create(&staged_path, resource.bytes)
        .map_err(|error| write_failure(&staged_path, error.kind()))?;
    #[cfg(test)]
    let staged = Staged {
        fault: faults::find(job.store.root()),
        ..staged
    };
    let (length, sha256, from) = match &job.fetch {
        Fetch::Download(transport) => {
            let (length, sha256) = download(job, transport, staged, &staged_path)?;
            (length, sha256, InstalledFrom::Download)
        }
        Fetch::File(path) => {
            let (length, sha256) = copy_local(job, path, staged, &staged_path)?;
            (length, sha256, InstalledFrom::File)
        }
    };
    job.control.checkpoint()?;
    if length != resource.bytes {
        return Err(validation(format!(
            "{} is {length} bytes; it is pinned at {} bytes",
            resource.id, resource.bytes
        )));
    }
    if sha256 != resource.sha256 {
        return Err(validation(format!(
            "{} does not match its pinned hash",
            resource.id
        )));
    }
    job.control.set_progress(Some(1.0), "checking the format");
    let module = job.registry.module(&job.module_id).ok_or_else(|| {
        Error::new(
            ErrorKind::Internal,
            format!("module {} is not registered", job.module_id),
        )
    })?;
    module.validate_resource(&resource.id, &staged_path)?;
    job.control.checkpoint()?;
    check_quota(&job.store, resource, job.quota)?;
    publish(job, staging, from)?;
    Ok(json!({
        "resource_id": resource.id,
        "version": resource.version,
        "path": job.store.file_path(&job.module_id, resource),
        "bytes": length,
        "sha256": sha256,
    }))
}

/// Stream the declared URL into the staged file through the host transport, under the resource's
/// redirect origins and the job's cancel flag.
fn download(
    job: &InstallJob,
    transport: &SharedTransport,
    mut staged: Staged,
    staged_path: &Path,
) -> Result<(u64, String), Error> {
    let resource = &job.resource;
    let endpoint = parse_endpoint(
        &resource.url,
        &[EndpointClass::Remote, EndpointClass::Loopback],
    )?;
    let transport = transport.get()?;
    let request = TransportRequest {
        method: Method::Get,
        endpoint,
        headers: Vec::new(),
        body: Vec::new(),
    };
    job.control.set_progress(Some(0.0), "downloading");
    let control = &job.control;
    let total = resource.bytes;
    let mut progress = |received: u64, _: Option<u64>| {
        control.set_progress(Some(received as f64 / total as f64), "downloading");
    };
    let sent = transport.send(
        &request,
        SendOptions {
            max_request_bytes: 0,
            max_response_bytes: total,
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: IDLE_TIMEOUT,
            total_timeout: DOWNLOAD_BASE
                .saturating_add(Duration::from_secs(total / DOWNLOAD_MIN_RATE)),
            redirects: RedirectPolicy {
                max: super::transport::MAX_REDIRECTS,
                origins: &resource.redirect_origins,
            },
            cancel: &|| control.is_cancelled(),
            progress: &mut progress,
        },
        &mut staged,
    );
    let response = match sent {
        Ok(response) => response,
        Err(error) => {
            return Err(match staged.failure {
                Some(kind) => write_failure(staged_path, kind),
                None => error,
            });
        }
    };
    if response.status != 200 {
        return Err(Error::new(
            ErrorKind::FileAccess,
            format!(
                "{} answered HTTP {} for {}",
                response.final_url.origin().ascii_serialization(),
                response.status,
                resource.id
            ),
        ));
    }
    staged
        .finish()
        .map_err(|kind| write_failure(staged_path, kind))
}

/// Copy a local file into the staged file through the same hashing sink, reading at most one byte
/// more than the declared length.
fn copy_local(
    job: &InstallJob,
    path: &Path,
    mut staged: Staged,
    staged_path: &Path,
) -> Result<(u64, String), Error> {
    let unreadable = |error: io::Error| {
        Error::new(
            ErrorKind::FileAccess,
            format!("cannot read {}: {}", path.display(), error.kind()),
        )
    };
    let file = File::open(path).map_err(unreadable)?;
    if !file.metadata().map_err(unreadable)?.is_file() {
        return Err(validation(format!("{} is not a file", path.display())));
    }
    let total = job.resource.bytes;
    let mut reader = file.take(total.saturating_add(1));
    let mut buffer = vec![0; COPY_BUFFER];
    let mut copied = 0u64;
    job.control.set_progress(Some(0.0), "copying");
    loop {
        job.control.checkpoint()?;
        let read = reader.read(&mut buffer).map_err(unreadable)?;
        if read == 0 {
            break;
        }
        if copied + read as u64 > total {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "{} is longer than the {total} bytes {} is pinned at",
                    path.display(),
                    job.resource.id
                ),
            ));
        }
        staged
            .write_all(&buffer[..read])
            .map_err(|error| write_failure(staged_path, error.kind()))?;
        copied += read as u64;
        job.control
            .set_progress(Some(copied as f64 / total as f64), "copying");
    }
    staged
        .finish()
        .map_err(|kind| write_failure(staged_path, kind))
}

/// Move the verified staging directory into place and write `installed.json` last. A version
/// directory without a matching marker, left by a crash or an older interrupted install, is
/// replaced. A failure after the move removes the version directory again.
fn publish(job: &InstallJob, staging: &Path, from: InstalledFrom) -> Result<(), Error> {
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
        format: INSTALLED_FORMAT,
        module_id: job.module_id.clone(),
        resource_id: resource.id.clone(),
        version: resource.version.clone(),
        url: resource.url.clone(),
        sha256: resource.sha256.clone(),
        bytes: resource.bytes,
        license: resource.license.clone(),
        provenance: resource.provenance.clone(),
        source: from,
        actor: job.actor.clone(),
        installed_ms: now_ms(),
    };
    let bytes = serde_json::to_vec_pretty(&marker)
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
    let written = atomic::sync_dir(&parent).and_then(|()| {
        atomic::try_replace(&target, INSTALLED_FILE, INSTALLED_TEMPORARY, &bytes)
            .map_err(|(path, error)| write_failure(&path, error.kind()))
    });
    if written.is_err() {
        let _ = fs::remove_dir_all(&target);
    }
    written
}

/// Remove one installed resource version on the transfer lane: its marker first, so an
/// interruption leaves a directory that is no longer installed, then the directory, then any
/// parent it leaves empty. It never touches a catalog, a recipe or an artifact.
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
        Err(error) => return Err(atomic::file_error(&marker, error)),
    }
    match fs::remove_dir_all(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(atomic::file_error(&target, error)),
    }
    // Emptied parents go too; a parent that still holds another version stays.
    for parent in target.ancestors().skip(1).take(2) {
        if fs::remove_dir(parent).is_err() {
            break;
        }
    }
    Ok(json!({
        "resource_id": resource.id,
        "version": resource.version,
        "removed": existed,
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
