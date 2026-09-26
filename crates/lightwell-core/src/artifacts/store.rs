//! The artifact root on disk: `manifest.json` names the catalog the root belongs to,
//! `objects/<sha256>` holds each artifact's immutable bytes and `tmp/` stages writes. Publishing,
//! verified reads and collection run here, on a worker thread or in a direct service call; the
//! catalog owner only stats files and reads the small manifest.
use super::{
    ArtifactId, ArtifactMeta, ArtifactRecord, LiveArtifacts, MAX_ARTIFACT_BYTES, PreparedArtifact,
    lock,
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Error, atomic_file,
    editor::{SourceSignature, source_signature},
    modules::valid_identity,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) const OBJECTS: &str = "objects";
pub(crate) const TEMPORARY: &str = "tmp";
pub(crate) const MANIFEST: &str = "manifest.json";
const MANIFEST_FORMAT: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 4096;
/// A staged file this old belongs to a write that will never finish.
const STALE_TEMPORARY: Duration = Duration::from_secs(60 * 60);
/// The read size for comparing and hashing files, so verifying a large artifact never holds a
/// second copy of it.
const CHUNK: usize = 64 * 1024;

/// Serializes a writer's decision to keep or replace an object with a collection's decision to
/// remove one. A writer marks its artifact live before deciding and a collection checks liveness
/// before removing, both under this lock, so a collection never deletes a file a publish is about
/// to hand to the catalog. Only workers take it; the catalog owner never waits on it.
static OBJECT_DECISIONS: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    catalog_id: String,
}

fn access(context: &str, error: &io::Error) -> Error {
    Error::file_access(format!("{context}: {}", error.kind()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// Fill `buffer` from `file`, retrying interrupted reads; `0` is the end of the file.
fn read_chunk(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        match file.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::cancelled("artifact job cancelled"))
    } else {
        Ok(())
    }
}

/// Where one artifact's bytes live under a root.
pub(crate) fn object_path(root: &Path, id: &ArtifactId) -> PathBuf {
    root.join(OBJECTS).join(id.sha256())
}

/// What an artifact root is, as its manifest says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RootState {
    /// No directory: nothing was published yet, or it did not move with the catalog.
    Absent,
    /// A directory without a manifest.
    Unmarked,
    /// A directory this catalog must not use, and why.
    Foreign(String),
    /// A directory whose manifest names this catalog.
    Ready,
}

/// Classify a root by its manifest. Reads at most 4 KiB, so the catalog owner may call it.
pub(crate) fn root_state(root: &Path, catalog_id: &str) -> Result<RootState, Error> {
    match fs::metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(RootState::Absent),
        Err(error) => return Err(access("cannot read artifact directory", &error)),
        Ok(metadata) if !metadata.is_dir() => {
            return Ok(RootState::Foreign(format!(
                "artifact directory {} is not a directory",
                root.display()
            )));
        }
        Ok(_) => {}
    }
    let mut file = match File::open(root.join(MANIFEST)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(RootState::Unmarked),
        Err(error) => return Err(access("cannot read artifact manifest", &error)),
        Ok(file) => file,
    };
    let mut text = Vec::new();
    (&mut file)
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut text)
        .map_err(|error| access("cannot read artifact manifest", &error))?;
    let manifest = (text.len() as u64 <= MAX_MANIFEST_BYTES)
        .then(|| serde_json::from_slice::<Manifest>(&text).ok())
        .flatten();
    let Some(manifest) = manifest else {
        return Ok(RootState::Foreign(format!(
            "artifact directory {} has an unreadable manifest",
            root.display()
        )));
    };
    if manifest.format != MANIFEST_FORMAT {
        return Ok(RootState::Foreign(format!(
            "artifact directory {} has manifest format {}; expected {MANIFEST_FORMAT}",
            root.display(),
            manifest.format
        )));
    }
    if manifest.catalog_id != catalog_id {
        return Ok(RootState::Foreign(format!(
            "artifact directory {} belongs to catalog {}",
            root.display(),
            manifest.catalog_id
        )));
    }
    Ok(RootState::Ready)
}

/// Whether `path` already holds exactly `bytes`. Compared in chunks, so it never reads a second
/// copy into memory.
fn object_holds(path: &Path, bytes: &[u8]) -> Result<bool, Error> {
    let mut file = match File::open(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(access("cannot read artifact", &error)),
        Ok(file) => file,
    };
    let metadata = file
        .metadata()
        .map_err(|error| access("cannot read artifact", &error))?;
    if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
        return Ok(false);
    }
    let mut buffer = vec![0; CHUNK];
    let mut offset = 0;
    loop {
        let read = read_chunk(&mut file, &mut buffer)
            .map_err(|error| access("cannot read artifact", &error))?;
        if read == 0 {
            return Ok(offset == bytes.len());
        }
        if bytes.get(offset..offset + read) != Some(&buffer[..read]) {
            return Ok(false);
        }
        offset += read;
    }
}

/// Publishes artifacts into one catalog's root from any thread: a module worker writes the bytes
/// and the catalog owner records the returned [`ArtifactRecord`] with
/// [`crate::EditorService::register_artifact`] when the task completes. Obtained from
/// [`crate::EditorService::artifact_writer`]; cloning it is cheap.
#[derive(Clone, Debug)]
pub struct ArtifactWriter {
    root: PathBuf,
    catalog_id: String,
    live: LiveArtifacts,
}

impl ArtifactWriter {
    pub(crate) fn new(root: PathBuf, catalog_id: String, live: LiveArtifacts) -> Self {
        Self {
            root,
            catalog_id,
            live,
        }
    }

    /// The directory this writer publishes into.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Publish `bytes` as one immutable artifact and return its record, which the catalog does not
    /// know yet, and its verified bytes.
    ///
    /// The root, its `objects/` and `tmp/` directories and its manifest are created on first use,
    /// and a root whose manifest names another catalog is refused as `incompatible`. The bytes are
    /// staged in `tmp/`, synced and renamed to `objects/<sha256>`, so a crash leaves either no
    /// object or a whole one, and at worst an unreferenced file a later collection removes. Equal
    /// bytes yield the same identity and one file: an object that already holds them is reused, and
    /// one that holds anything else is replaced atomically. More than 256 MiB is a
    /// `resource-limit`. The artifact is live while the service that made this writer is open, so
    /// no collection removes it before the catalog has recorded it. One bounded copy of the bytes
    /// is made for the returned [`PreparedArtifact`].
    pub fn write(
        &self,
        bytes: &[u8],
        meta: ArtifactMeta,
        module_id: &str,
    ) -> Result<(ArtifactRecord, Arc<PreparedArtifact>), Error> {
        let length = bytes.len() as u64;
        if length > MAX_ARTIFACT_BYTES {
            return Err(Error::resource_limit(format!(
                "an artifact of {length} bytes exceeds the {MAX_ARTIFACT_BYTES}-byte limit"
            )));
        }
        meta.validate()?;
        if !valid_identity(module_id) {
            return Err(Error::validation(format!(
                "invalid module identity {module_id}"
            )));
        }
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let id = ArtifactId::for_hash(&sha256)?;
        self.prepare_root()?;
        // The expensive write and sync happen before the lock, so a collection waits only for the
        // decision below.
        let staged = self.stage(bytes)?;
        let object = object_path(&self.root, &id);
        let decided = {
            let _decision = lock(&OBJECT_DECISIONS);
            self.live.mark(&id);
            match object_holds(&object, bytes) {
                Ok(true) => Ok(()),
                Ok(false) => atomic_file::publish(&staged, &object)
                    .map_err(|error| access("cannot publish artifact", &error)),
                Err(error) => Err(error),
            }
        };
        // Reused, or the rename failed: the staged copy is not needed. After a successful rename
        // there is nothing left to remove.
        let _ = fs::remove_file(&staged);
        decided?;
        let prepared = Arc::new(PreparedArtifact::new(id.clone(), &meta, Arc::from(bytes)));
        Ok((
            ArtifactRecord {
                id,
                sha256,
                bytes: length,
                meta,
                module_id: module_id.to_owned(),
                created_ms: now_ms(),
            },
            prepared,
        ))
    }

    /// Make the root usable: claim it when it has no manifest yet, refuse it when its manifest
    /// names another catalog, and make sure its object and staging directories exist.
    fn prepare_root(&self) -> Result<(), Error> {
        match root_state(&self.root, &self.catalog_id)? {
            RootState::Ready => {}
            RootState::Foreign(detail) => return Err(Error::incompatible(detail)),
            RootState::Absent | RootState::Unmarked => self.claim()?,
        }
        for directory in [OBJECTS, TEMPORARY] {
            fs::create_dir_all(self.root.join(directory))
                .map_err(|error| access("cannot create artifact directory", &error))?;
        }
        Ok(())
    }

    /// Write the manifest atomically. A directory that already holds objects but no manifest is
    /// not claimed: nothing says whose objects they are.
    fn claim(&self) -> Result<(), Error> {
        let temporary = self.root.join(TEMPORARY);
        fs::create_dir_all(&temporary)
            .map_err(|error| access("cannot create artifact directory", &error))?;
        let occupied = match fs::read_dir(self.root.join(OBJECTS)) {
            Ok(mut entries) => entries.next().is_some(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(access("cannot read artifact directory", &error)),
        };
        if occupied {
            return Err(Error::incompatible(format!(
                "artifact directory {} holds objects but no manifest",
                self.root.display()
            )));
        }
        let manifest = serde_json::to_vec(&Manifest {
            format: MANIFEST_FORMAT,
            catalog_id: self.catalog_id.clone(),
        })
        .map_err(|error| Error::internal(error.to_string()))?;
        let staged = self.stage(&manifest)?;
        atomic_file::publish(&staged, &self.root.join(MANIFEST)).map_err(|error| {
            let _ = fs::remove_file(&staged);
            access("cannot write artifact manifest", &error)
        })
    }

    /// Write `bytes` to a new file in `tmp/` and sync it. A failed write removes what it staged.
    fn stage(&self, bytes: &[u8]) -> Result<PathBuf, Error> {
        let staged = self
            .root
            .join(TEMPORARY)
            .join(uuid::Uuid::new_v4().simple().to_string());
        atomic_file::stage(&staged, bytes)
            .map_err(|error| access("cannot stage artifact", &error))?;
        Ok(staged)
    }
}

/// One artifact a worker should read and verify, with the metadata its catalog row records.
#[derive(Clone, Debug)]
pub(crate) struct ArtifactRead {
    pub(crate) root: PathBuf,
    pub(crate) id: ArtifactId,
    pub(crate) bytes: u64,
    pub(crate) meta: ArtifactMeta,
}

/// Bytes whose hash a worker checked, with the file signature they were read under. The owner
/// keeps them ready keyed by that signature, so a later change to the file is a miss.
#[derive(Debug)]
pub(crate) struct VerifiedArtifact {
    pub(crate) artifact: Arc<PreparedArtifact>,
    pub(crate) signature: SourceSignature,
}

/// Read one artifact through a single bounded handle and check its length and hash: a missing
/// file is `source-unavailable: artifact <id> is missing`, any other length or hash is
/// `source-unavailable: artifact <id> is corrupt`, and a file that changes while it is read is a
/// `conflict`. Worker-side work: it reads and hashes up to 256 MiB.
pub(crate) fn read_verified(
    read: &ArtifactRead,
    cancel: &AtomicBool,
) -> Result<VerifiedArtifact, Error> {
    let id = &read.id;
    let missing = || Error::source_unavailable(format!("artifact {id} is missing"));
    let corrupt = || Error::source_unavailable(format!("artifact {id} is corrupt"));
    if read.bytes > MAX_ARTIFACT_BYTES {
        return Err(Error::resource_limit(format!(
            "artifact {id} holds more than {MAX_ARTIFACT_BYTES} bytes"
        )));
    }
    cancelled(cancel)?;
    let path = object_path(&read.root, id);
    let mut file = match File::open(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(missing()),
        Err(error) => return Err(access("cannot read artifact", &error)),
        Ok(file) => file,
    };
    let stat = |metadata: io::Result<fs::Metadata>| {
        metadata.map_err(|error| access("cannot read artifact", &error))
    };
    let handle_before = stat(file.metadata())?;
    let path_before = stat(path.metadata())?;
    let signature = source_signature(&path, &handle_before);
    if signature != source_signature(&path, &path_before) {
        return Err(Error::conflict(format!(
            "artifact {id} changed before preparation"
        )));
    }
    if !handle_before.is_file() {
        return Err(missing());
    }
    if handle_before.len() != read.bytes {
        return Err(corrupt());
    }
    let mut bytes = Vec::with_capacity(read.bytes as usize);
    (&mut file)
        .take(read.bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| access("cannot read artifact", &error))?;
    cancelled(cancel)?;
    if bytes.len() as u64 != read.bytes || format!("{:x}", Sha256::digest(&bytes)) != id.sha256() {
        return Err(corrupt());
    }
    if signature != source_signature(&path, &stat(file.metadata())?)
        || signature != source_signature(&path, &stat(path.metadata())?)
    {
        return Err(Error::conflict(format!(
            "artifact {id} changed during preparation"
        )));
    }
    Ok(VerifiedArtifact {
        artifact: Arc::new(PreparedArtifact::new(
            id.clone(),
            &read.meta,
            Arc::from(bytes),
        )),
        signature,
    })
}

/// One collection the catalog owner planned: how many rows it already removed and every artifact
/// the catalog still records. Any other object file is garbage unless it is live: published while
/// the service is open.
#[derive(Debug)]
pub(crate) struct Collection {
    pub(crate) root: PathBuf,
    pub(crate) catalog_id: String,
    pub(crate) keep: HashSet<ArtifactId>,
    pub(crate) live: LiveArtifacts,
    pub(crate) rows: usize,
}

/// What a collection removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Collected {
    pub(crate) rows: usize,
    pub(crate) objects: usize,
    pub(crate) temporary: usize,
}

/// Remove the object files the catalog no longer records and that are not live, and staged files
/// older than an hour. A root whose manifest does not name this catalog is left
/// untouched, and a name that is not an artifact hash is never removed.
pub(crate) fn collect_files(
    collection: &Collection,
    cancel: &AtomicBool,
) -> Result<Collected, Error> {
    let root = &collection.root;
    let mut collected = Collected {
        rows: collection.rows,
        objects: 0,
        temporary: 0,
    };
    match root_state(root, &collection.catalog_id)? {
        RootState::Absent => return Ok(collected),
        RootState::Ready => {}
        RootState::Unmarked | RootState::Foreign(_) => {
            return Err(Error::incompatible(format!(
                "artifact directory {} is not this catalog's; nothing was removed",
                root.display()
            )));
        }
    }
    let listing = |directory: &Path| match fs::read_dir(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(access("cannot read artifact directory", &error)),
        Ok(entries) => Ok(Some(entries)),
    };
    for entry in listing(&root.join(OBJECTS))?.into_iter().flatten() {
        cancelled(cancel)?;
        let entry = entry.map_err(|error| access("cannot read artifact directory", &error))?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| ArtifactId::for_hash(name).ok())
        else {
            continue;
        };
        if collection.keep.contains(&id) {
            continue;
        }
        let _decision = lock(&OBJECT_DECISIONS);
        if collection.live.contains(&id) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => collected.objects += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(access("cannot remove artifact", &error)),
        }
    }
    let now = SystemTime::now();
    for entry in listing(&root.join(TEMPORARY))?.into_iter().flatten() {
        cancelled(cancel)?;
        let entry = entry.map_err(|error| access("cannot read artifact directory", &error))?;
        let metadata = entry
            .metadata()
            .map_err(|error| access("cannot read staged artifact", &error))?;
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_TEMPORARY);
        if metadata.is_file() && stale {
            match fs::remove_file(entry.path()) {
                Ok(()) => collected.temporary += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(access("cannot remove staged artifact", &error)),
            }
        }
    }
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn temp(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "lightwell-artifact-store-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        directory.canonicalize().unwrap()
    }
    fn meta() -> ArtifactMeta {
        ArtifactMeta {
            kind: "tint".into(),
            width: None,
            height: None,
            colour: Some("linear-srgb".into()),
        }
    }
    fn writer(root: &Path, catalog_id: &str) -> ArtifactWriter {
        ArtifactWriter::new(
            root.to_path_buf(),
            catalog_id.into(),
            LiveArtifacts::default(),
        )
    }

    #[test]
    fn the_writer_creates_its_root_publishes_once_and_refuses_a_foreign_one() {
        let directory = temp("writer");
        let root = directory.join("catalog.artifacts");
        let writer = writer(&root, "catalog-a");
        let bytes = b"the writer publishes these twelve".as_slice();
        let (record, prepared_bytes) = writer.write(bytes, meta(), "test.writer").unwrap();
        assert_eq!(record.sha256, format!("{:x}", Sha256::digest(bytes)));
        assert_eq!(record.id.sha256(), record.sha256);
        assert_eq!(record.bytes, bytes.len() as u64);
        assert_eq!(&*prepared_bytes.bytes, bytes);
        assert_eq!(prepared_bytes.colour.as_deref(), Some("linear-srgb"));
        assert_eq!(prepared_bytes.id, record.id);
        assert_eq!(
            root_state(&root, "catalog-a").unwrap(),
            RootState::Ready,
            "the manifest names the catalog"
        );
        assert_eq!(fs::read(object_path(&root, &record.id)).unwrap(), bytes);
        assert!(
            writer.live.contains(&record.id),
            "live until the service closes"
        );
        // The same bytes again: the same identity, one object and nothing left staged.
        let (again, _) = writer.write(bytes, meta(), "test.writer").unwrap();
        assert_eq!(again.id, record.id);
        assert_eq!(fs::read_dir(root.join(OBJECTS)).unwrap().count(), 1);
        assert_eq!(fs::read_dir(root.join(TEMPORARY)).unwrap().count(), 0);
        // Another catalog's writer never publishes into this root.
        let foreign = self::writer(&root, "catalog-b")
            .write(b"foreign", meta(), "test.writer")
            .unwrap_err();
        assert_eq!(foreign.kind, ErrorKind::Incompatible);
        assert!(
            foreign.detail.contains("belongs to catalog catalog-a"),
            "{foreign}"
        );
        assert_eq!(fs::read_dir(root.join(OBJECTS)).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_writer_refuses_oversized_bytes_bad_metadata_and_an_unclaimable_root() {
        let directory = temp("refusals");
        let root = directory.join("catalog.artifacts");
        let writer = writer(&root, "catalog-a");
        let oversized = vec![0_u8; MAX_ARTIFACT_BYTES as usize + 1];
        let error = writer.write(&oversized, meta(), "test.writer").unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        drop(oversized);
        assert!(!root.exists(), "nothing is created for a refused write");
        for (bad, fragment) in [
            (
                ArtifactMeta {
                    kind: "Tint".into(),
                    ..meta()
                },
                "artifact kind",
            ),
            (
                ArtifactMeta {
                    width: Some(0),
                    ..meta()
                },
                "dimensions",
            ),
            (
                ArtifactMeta {
                    colour: Some("linear srgb".into()),
                    ..meta()
                },
                "colour",
            ),
        ] {
            let error = writer.write(b"bytes", bad, "test.writer").unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation);
            assert!(error.detail.contains(fragment), "{error}");
        }
        assert_eq!(
            writer
                .write(b"bytes", meta(), "Not A Module")
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
        // Objects without a manifest are nobody's to claim.
        fs::create_dir_all(root.join(OBJECTS)).unwrap();
        fs::write(root.join(OBJECTS).join("stray"), b"stray").unwrap();
        let error = writer.write(b"bytes", meta(), "test.writer").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert!(error.detail.contains("holds objects but no manifest"));
        assert!(!root.join(MANIFEST).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_corrupt_object_is_replaced_atomically_and_a_verified_read_names_it() {
        let directory = temp("corrupt");
        let root = directory.join("catalog.artifacts");
        let writer = writer(&root, "catalog-a");
        let bytes = b"bytes a corrupt object replaces".as_slice();
        let (record, _) = writer.write(bytes, meta(), "test.writer").unwrap();
        let object = object_path(&root, &record.id);
        let mut damaged = bytes.to_vec();
        damaged[0] ^= 0xff;
        fs::write(&object, &damaged).unwrap();
        let read = ArtifactRead {
            root: root.clone(),
            id: record.id.clone(),
            bytes: record.bytes,
            meta: meta(),
        };
        let never = AtomicBool::new(false);
        let error = read_verified(&read, &never).unwrap_err();
        assert_eq!(error.kind, ErrorKind::SourceUnavailable);
        assert_eq!(error.detail, format!("artifact {} is corrupt", record.id));
        // Publishing the same bytes again replaces the damaged object.
        writer.write(bytes, meta(), "test.writer").unwrap();
        assert_eq!(fs::read(&object).unwrap(), bytes);
        let verified = read_verified(&read, &never).unwrap();
        assert_eq!(&*verified.artifact.bytes, bytes);
        assert_eq!(
            verified.signature,
            source_signature(&object, &object.metadata().unwrap()),
            "the signature the bytes were read under"
        );
        fs::remove_file(&object).unwrap();
        assert_eq!(
            read_verified(&read, &never).unwrap_err().detail,
            format!("artifact {} is missing", record.id)
        );
        let cancel = AtomicBool::new(true);
        assert_eq!(
            read_verified(&read, &cancel).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn collection_removes_unrecorded_objects_that_are_not_live_and_stale_staging_only() {
        let directory = temp("collect");
        let root = directory.join("catalog.artifacts");
        let live = LiveArtifacts::default();
        let open = ArtifactWriter::new(root.clone(), "catalog-a".into(), live.clone());
        let (kept, _) = open.write(b"recorded", meta(), "test.writer").unwrap();
        let (fresh, _) = open
            .write(b"published while open", meta(), "test.writer")
            .unwrap();
        // Published by an earlier session, and no longer recorded.
        let (orphan, _) = writer(&root, "catalog-a")
            .write(b"an orphan", meta(), "test.writer")
            .unwrap();
        let stranger = root.join(OBJECTS).join("not-an-artifact");
        fs::write(&stranger, b"left alone").unwrap();
        let stale = root.join(TEMPORARY).join("stale");
        let recent = root.join(TEMPORARY).join("recent");
        fs::write(&stale, b"stale").unwrap();
        fs::write(&recent, b"recent").unwrap();
        File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - 2 * STALE_TEMPORARY)
            .unwrap();
        let collection = Collection {
            root: root.clone(),
            catalog_id: "catalog-a".into(),
            keep: HashSet::from([kept.id.clone()]),
            live,
            rows: 2,
        };
        let never = AtomicBool::new(false);
        assert_eq!(
            collect_files(&collection, &never).unwrap(),
            Collected {
                rows: 2,
                objects: 1,
                temporary: 1
            }
        );
        assert!(object_path(&root, &kept.id).exists(), "recorded");
        assert!(object_path(&root, &fresh.id).exists(), "live");
        assert!(!object_path(&root, &orphan.id).exists());
        assert!(
            stranger.exists(),
            "a name that is not a hash is never removed"
        );
        assert!(!stale.exists());
        assert!(recent.exists(), "a write that may still finish");
        // A root whose manifest names another catalog is never collected.
        let foreign = Collection {
            catalog_id: "catalog-b".into(),
            keep: HashSet::new(),
            ..collection
        };
        assert_eq!(
            collect_files(&foreign, &never).unwrap_err().kind,
            ErrorKind::Incompatible
        );
        assert!(object_path(&root, &kept.id).exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
