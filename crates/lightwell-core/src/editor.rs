use crate::{
    AssetId, ContentPoint, Draft, DraftId, EntryId, Error, ErrorKind, HistoryEntry, Layer, LayerId,
    ModuleRegistry, Mutation, PreviewJob, PreviewSource, ProxyBounds, Raster, Recipe, Snapshot,
    SnapshotId, Transform,
    analysis::AnalysisIdentity,
    artifacts::{ArtifactId, LiveArtifacts, PreparedArtifact, PreparedArtifacts},
    modules::{
        ActionInput, ActionPlan, EffectStage, Stage, StageContext, action_label, check_parameters,
    },
    open_source_bytes, read_bounded_file, render,
    render::{Evaluation, locate_dimensions},
    render_linear, sample_linear,
    source::{PreparedSource, RawPrepared},
};
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{File, Metadata},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod artifact_store;
#[cfg(test)]
mod artifact_tests;

/// Format 5 adds the catalog identity and the derived-artifact tables, so a catalog written before
/// it is refused by name and kept intact.
const CATALOG_FORMAT: i64 = 5;
const MAX_HISTORY_PAGE: usize = 100;
const MAX_VERSION_NAME: usize = 64;
const ASSET_COLUMNS: &str =
    "id,source_root,locator,fingerprint,file_identity,byte_len,width,height,source_json";

fn catalog_error(error: rusqlite::Error) -> Error {
    let kind = match &error {
        rusqlite::Error::SqliteFailure(problem, _)
            if matches!(
                problem.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            ErrorKind::Conflict
        }
        _ => ErrorKind::Catalog,
    };
    Error::new(kind, error.to_string())
}

fn json_error(context: &str, error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Incompatible, format!("{context}: {error}"))
}

fn encode<T: Serialize>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|e| json_error("cannot encode catalog value", e))
}

fn decode<T: DeserializeOwned>(context: &str, value: String) -> Result<T, Error> {
    serde_json::from_str(&value).map_err(|e| json_error(context, e))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceKind {
    Jpeg,
    Raw { metadata: Value },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRecord {
    pub id: AssetId,
    pub source_root: PathBuf,
    pub locator: PathBuf,
    pub fingerprint: String,
    pub file_identity: String,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    pub source: SourceKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorState {
    pub asset: AssetRecord,
    pub revision: u64,
    pub current_entry: HistoryEntry,
    pub redo: Vec<EntryId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MutationOutcome {
    Applied,
    Navigated,
    NoOp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationResult {
    pub outcome: MutationOutcome,
    pub revision: u64,
    pub current_entry_id: EntryId,
    pub created_entry_id: Option<EntryId>,
    pub deduplicated: bool,
}

/// One evaluated output pixel of a saved history entry, with the identities that produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelSample {
    pub entry_id: EntryId,
    pub snapshot_id: SnapshotId,
    pub source_fingerprint: String,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub rgba: [u8; 4],
    /// Present when the sample was evaluated against an open draft instead of the stored stack, so
    /// a client can tell which settings produced this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftStamp>,
}

/// Which evaluated stack an analysis job should describe: the asset's current entry, one frozen
/// historical entry, or the caller's own open draft.
#[derive(Clone, Copy, Debug)]
pub enum AnalysisSelection<'a> {
    Current,
    Entry(&'a EntryId),
    Draft(&'a Draft),
}

/// One planned analysis job. `failure` is set when the effective recipe resolved but has no output
/// stage the host can evaluate — an unavailable provider, or a payload the registry refuses — in
/// which case the job is recorded failed and no worker is started.
#[derive(Debug)]
pub struct AnalysisPlan {
    pub identity: AnalysisIdentity,
    /// The buffer the worker renders, or `None` when `failure` says the stack has no output stage
    /// at all: there is nothing to render, so the original is never prepared for it.
    pub source: Option<PreviewSource>,
    pub registry: Arc<ModuleRegistry>,
    pub recipe: Recipe,
    pub failure: Option<Error>,
    /// The verified bytes of every artifact `recipe` references, which the job holds while it runs.
    pub artifacts: Vec<Arc<PreparedArtifact>>,
}

/// One asset's current entry bound for point sampling on a worker, as the catalog owner found it
/// at request time: the samples describe that entry whatever is committed meanwhile. Holding it
/// pins the artifacts its stack binds and shares the source's allocation.
pub(crate) struct SamplePlan {
    source: PreviewSource,
    registry: Arc<ModuleRegistry>,
    recipe: Recipe,
    /// Held, never read: compilation finds the verified bytes while the plan is sampled.
    _artifacts: Vec<Arc<PreparedArtifact>>,
}

impl SamplePlan {
    /// The pixels at the centres of a `side` × `side` grid over the entry's output stage, row by
    /// row from the top-left: `O(side² × layers)` and no frame. `checkpoint` is asked before each
    /// point.
    pub(crate) fn grid(
        &self,
        side: u32,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Vec<[u8; 4]>, Error> {
        self.source
            .sample_grid(&self.registry, &self.recipe, side, checkpoint)
    }
}

/// Which draft, at which revision, a sample, a preview or an analysis was evaluated against.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftStamp {
    pub draft_id: DraftId,
    pub draft_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub next_before_sequence: Option<u64>,
}

/// One step on the undo-parent chain, without the entry's full stack.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineageStep {
    pub entry_id: EntryId,
    pub sequence: u64,
    pub action_id: String,
    pub undo_parent: Option<EntryId>,
}

/// The chain of undo parents from one entry back towards Original, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lineage {
    pub steps: Vec<LineageStep>,
    /// The next parent to continue from when the page limit stopped the walk.
    pub next_entry_id: Option<EntryId>,
}

/// One stored layer of an entry as the recipe panel reads it. `available` is false when no
/// provider is registered for the effect or the registered one reports itself unavailable; the
/// summary then carries the reason instead of a description.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerDescription {
    pub id: LayerId,
    pub effect: String,
    pub module: Option<String>,
    pub title: Option<String>,
    pub summary: String,
    /// The parameter values this stored layer represents, as its module reports them. Empty for a
    /// module that declares none and for a layer whose provider is missing or unavailable.
    #[serde(default)]
    pub values: Map<String, Value>,
    pub available: bool,
    /// The derived artifacts the stored layer references, in its order. Omitted when it has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactId>,
}

/// One entry's ordered layers with their provider and summary. Reading only: no source, no render.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeDescription {
    pub entry_id: EntryId,
    pub layers: Vec<LayerDescription>,
}

/// A named reference to one retained history entry: the Lightroom-style saved state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub asset_id: AssetId,
    pub name: String,
    pub entry_id: EntryId,
    pub entry_sequence: u64,
    pub actor: String,
    pub created_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionResult {
    pub outcome: MutationOutcome,
    pub version: Option<Version>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceSignature {
    byte_len: u64,
    modified: Option<SystemTime>,
    file_identity: String,
    change_marker: Option<(i128, i128)>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedFile {
    pub(crate) canonical: PathBuf,
    pub(crate) signature: SourceSignature,
    pub(crate) source: PreparedSource,
    pub(crate) fingerprint: String,
}

#[derive(Clone, Debug)]
pub(crate) struct RawDevelopment {
    pub(crate) asset_id: AssetId,
    pub(crate) signature: SourceSignature,
    pub(crate) fingerprint: String,
    pub(crate) sensor: Arc<lightwell_raw::RawSource>,
    pub(crate) gains: [f32; 3],
}

#[derive(Debug)]
struct CachedSource {
    asset_id: AssetId,
    signature: SourceSignature,
    source: PreparedSource,
}

#[derive(Debug)]
pub struct EditorService {
    connection: Connection,
    source_cache: RefCell<Option<CachedSource>>,
    allow_sync_source: bool,
    registry: Arc<ModuleRegistry>,
    /// This catalog's own identity, which its artifact root's manifest must name.
    catalog_id: String,
    /// Where this catalog's artifacts live: `<catalog stem>.artifacts` beside the catalog file, or
    /// the directory `artifact.relocate` verified and recorded.
    artifact_root: PathBuf,
    /// Verified artifact bytes kept ready for evaluation, bounded and least recently used first out.
    prepared_artifacts: RefCell<PreparedArtifacts>,
    /// The manifest signature the root was last checked under, so an unchanged root costs one stat.
    checked_manifest: RefCell<Option<SourceSignature>>,
    /// Artifacts published while this service is open, which no collection removes.
    live_artifacts: LiveArtifacts,
}

impl EditorService {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::open_with(path, Arc::new(ModuleRegistry::builtin()))
    }

    /// Open a catalog served by a specific set of providers. Registration is cheap and happens
    /// before any catalog or image work.
    pub fn open_with(path: &Path, registry: Arc<ModuleRegistry>) -> Result<Self, Error> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::new(
                    ErrorKind::Catalog,
                    format!("cannot create catalog directory: {}", e.kind()),
                )
            })?;
        }
        let connection = Connection::open(path).map_err(catalog_error)?;
        connection
            .busy_timeout(Duration::from_millis(100))
            .map_err(catalog_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON;
                 PRAGMA synchronous=FULL;
                 PRAGMA locking_mode=EXCLUSIVE;
                 BEGIN IMMEDIATE;
                 COMMIT;",
            )
            .map_err(catalog_error)?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(catalog_error)?;
        match version {
            0 => Self::create_schema(&connection)?,
            CATALOG_FORMAT => {}
            other => {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    format!(
                        "catalog format {other} is not supported; expected {CATALOG_FORMAT}; choose a new catalog path"
                    ),
                ));
            }
        }
        let meta = |key: &str| -> Result<Option<String>, Error> {
            connection
                .query_row(
                    "SELECT value FROM catalog_meta WHERE key=?1",
                    [key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(catalog_error)
        };
        let catalog_id = meta("catalog_id")?.ok_or_else(|| {
            Error::new(
                ErrorKind::Incompatible,
                "catalog has no identity; choose a new catalog path",
            )
        })?;
        // The default root follows the catalog file, so moving both together keeps it; a relocated
        // root is recorded as the canonical directory the relocation verified.
        let artifact_root = match meta("artifact_root")? {
            Some(root) => PathBuf::from(root),
            None => {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let stem = canonical
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "catalog".into());
                canonical
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(format!("{stem}.artifacts"))
            }
        };
        Ok(Self {
            connection,
            source_cache: RefCell::new(None),
            allow_sync_source: true,
            registry,
            catalog_id,
            artifact_root,
            prepared_artifacts: RefCell::new(PreparedArtifacts::default()),
            checked_manifest: RefCell::new(None),
            live_artifacts: LiveArtifacts::default(),
        })
    }

    /// The providers this service validates, plans and renders with.
    pub fn registry(&self) -> &Arc<ModuleRegistry> {
        &self.registry
    }

    fn create_schema(connection: &Connection) -> Result<(), Error> {
        let occupied: bool = connection
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |row| {
                row.get(0)
            })
            .map_err(catalog_error)?;
        if occupied {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "unmarked catalog is not empty; choose a new catalog path",
            ));
        }
        connection
            .execute_batch(&format!(
                "BEGIN IMMEDIATE;
                 CREATE TABLE assets (
                    id TEXT PRIMARY KEY,
                    source_root TEXT NOT NULL,
                    locator TEXT NOT NULL,
                    canonical_locator TEXT NOT NULL UNIQUE,
                    file_identity TEXT NOT NULL UNIQUE,
                    fingerprint TEXT NOT NULL,
                    byte_len INTEGER NOT NULL,
                    width INTEGER NOT NULL,
                    height INTEGER NOT NULL,
                    source_json TEXT NOT NULL
                 );
                 CREATE TABLE entries (
                    id TEXT PRIMARY KEY,
                    asset_id TEXT NOT NULL REFERENCES assets(id),
                    sequence INTEGER NOT NULL,
                    action_id TEXT NOT NULL,
                    undo_parent_id TEXT,
                    entry_json TEXT NOT NULL,
                    UNIQUE(asset_id, sequence)
                 );
                 CREATE TABLE asset_state (
                    asset_id TEXT PRIMARY KEY REFERENCES assets(id),
                    current_entry_id TEXT NOT NULL REFERENCES entries(id),
                    revision INTEGER NOT NULL,
                    redo_json TEXT NOT NULL
                 );
                 CREATE TABLE requests (
                    asset_id TEXT NOT NULL REFERENCES assets(id),
                    request_id TEXT NOT NULL,
                    input_hash TEXT NOT NULL,
                    result_json TEXT NOT NULL,
                    PRIMARY KEY(asset_id, request_id)
                 );
                 CREATE TABLE versions (
                    asset_id TEXT NOT NULL REFERENCES assets(id),
                    name TEXT NOT NULL COLLATE NOCASE,
                    entry_id TEXT NOT NULL REFERENCES entries(id),
                    actor TEXT NOT NULL,
                    created_ms INTEGER NOT NULL,
                    PRIMARY KEY(asset_id, name)
                 );
                 CREATE TRIGGER entries_are_immutable BEFORE UPDATE ON entries BEGIN
                    SELECT RAISE(ABORT, 'history entries are immutable');
                 END;
                 CREATE TABLE catalog_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                 );
                 CREATE TABLE artifacts (
                    id TEXT PRIMARY KEY,
                    sha256 TEXT NOT NULL,
                    bytes INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    width INTEGER,
                    height INTEGER,
                    colour TEXT,
                    module_id TEXT NOT NULL,
                    created_ms INTEGER NOT NULL
                 );
                 CREATE TABLE artifact_refs (
                    entry_id TEXT NOT NULL REFERENCES entries(id),
                    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
                    PRIMARY KEY(entry_id, artifact_id)
                 );
                 CREATE INDEX artifact_refs_by_artifact ON artifact_refs(artifact_id);
                 CREATE TRIGGER artifact_refs_are_permanent BEFORE DELETE ON artifact_refs BEGIN
                    SELECT RAISE(ABORT, 'artifact references are permanent');
                 END;
                 INSERT INTO catalog_meta VALUES ('catalog_id', '{catalog_id}');
                 PRAGMA user_version={CATALOG_FORMAT};
                 COMMIT;",
                catalog_id = uuid::Uuid::new_v4()
            ))
            .map_err(catalog_error)
    }

    /// The catalog owner disables synchronous source misses; workers prepare these separately.
    pub(crate) fn disable_sync_source(&mut self) {
        self.allow_sync_source = false;
    }

    pub(crate) fn request_signature(path: &Path) -> Result<(PathBuf, SourceSignature), Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot resolve source: {}", e.kind()),
            )
        })?;
        let metadata = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        if !metadata.is_file() {
            return Err(Error::new(
                ErrorKind::UnsupportedInput,
                "expected a regular file",
            ));
        }
        Ok((canonical.clone(), source_signature(&canonical, &metadata)))
    }

    /// Read, hash and decode from the same bounded, stable read-only file handle on a worker.
    pub(crate) fn prepare_file(path: &Path) -> Result<PreparedFile, Error> {
        Self::prepare_file_cancel(path, &AtomicBool::new(false))
    }

    pub(crate) fn prepare_file_cancel(
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<PreparedFile, Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot resolve source: {}", e.kind()),
            )
        })?;
        let mut file = File::open(&canonical)
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let handle_before = file
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let path_before = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let signature = source_signature(&canonical, &handle_before);
        if signature != source_signature(&canonical, &path_before) {
            return Err(Error::new(
                ErrorKind::Conflict,
                "source changed before preparation",
            ));
        }
        let bytes = read_bounded_file(&mut file)?;
        let (source, fingerprint) = if bytes.starts_with(&[0xff, 0xd8]) {
            let image = open_source_bytes(bytes)?;
            let fingerprint = image.fingerprint.clone();
            (PreparedSource::Jpeg(image), fingerprint)
        } else {
            let fingerprint = format!("{:x}", Sha256::digest(&bytes));
            let raw = RawPrepared::decode(bytes, fingerprint.clone(), cancel)?;
            (PreparedSource::Raw(raw), fingerprint)
        };
        let handle_after = file
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let path_after = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        if signature != source_signature(&canonical, &handle_after)
            || signature != source_signature(&canonical, &path_after)
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                "source changed during preparation",
            ));
        }
        Ok(PreparedFile {
            canonical,
            signature,
            source,
            fingerprint,
        })
    }

    pub(crate) fn known_fingerprint(&self, path: &Path) -> Result<Option<String>, Error> {
        let (canonical, signature) = Self::request_signature(path)?;
        self.connection.query_row(
            "SELECT fingerprint FROM assets WHERE canonical_locator=?1 OR file_identity=?2 LIMIT 1",
            params![canonical.to_string_lossy(), signature.file_identity],
            |row| row.get(0),
        ).optional().map_err(catalog_error)
    }

    /// A repeated import can reuse the one verified immutable decode without a new worker job.
    pub(crate) fn cached_import(&self, path: &Path) -> Result<Option<EditorState>, Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot resolve source: {}", e.kind()),
            )
        })?;
        let metadata = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let signature = source_signature(&canonical, &metadata);
        let id = self
            .connection
            .query_row(
                "SELECT id FROM assets WHERE canonical_locator=?1 OR file_identity=?2 LIMIT 1",
                params![canonical.to_string_lossy(), signature.file_identity],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(catalog_error)?;
        let Some(id) = id else { return Ok(None) };
        let state = self.state(&AssetId::parse(id)?)?;
        if state.asset.file_identity != signature.file_identity
            || state.asset.byte_len != signature.byte_len
        {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source fingerprint changed",
            ));
        }
        let cache = self.source_cache.borrow();
        Ok(cache
            .as_ref()
            .filter(|cached| cached.asset_id == state.asset.id && cached.signature == signature)
            .map(|_| state))
    }

    /// Persisted source interpretation and current in-memory readiness, with no decode or frame work.
    pub fn inspect_source(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<Value, Error> {
        let state = self.state(asset_id)?;
        let entry = match entry_id {
            Some(id) => self.entry(asset_id, id)?,
            None => state.current_entry,
        };
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        let cached = self.cached_state(asset_id)?.is_some();
        let needs_development =
            cached && self.raw_development(asset_id, Some(&entry.id))?.is_some();
        Ok(json!({
            "asset_id": asset_id,
            "entry_id": entry.id,
            "fingerprint": state.asset.fingerprint,
            "width": state.asset.width,
            "height": state.asset.height,
            "source": state.asset.source,
            "readiness": if !cached { "preparation-required" } else if needs_development { "development-required" } else { "ready" },
        }))
    }

    pub(crate) fn raw_development(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<Option<RawDevelopment>, Error> {
        let state = self.state(asset_id)?;
        let entry = match entry_id {
            Some(id) => self.entry(asset_id, id)?,
            None => state.current_entry,
        };
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        let cache = self.source_cache.borrow();
        let Some(cached) = cache.as_ref().filter(|cached| cached.asset_id == *asset_id) else {
            return Ok(None);
        };
        let PreparedSource::Raw(raw) = &cached.source else {
            return Ok(None);
        };
        let payload = raw_payload(&entry.snapshot.recipe)?;
        let gains = match payload.wb_mode {
            crate::WhiteBalanceMode::AsShot => raw.sensor.metadata().as_shot_gains,
            crate::WhiteBalanceMode::Custom => payload.gains,
        };
        if gains == raw.gains && raw.linear.is_some() {
            return Ok(None);
        }
        Ok(Some(RawDevelopment {
            asset_id: asset_id.clone(),
            signature: cached.signature.clone(),
            fingerprint: state.asset.fingerprint,
            sensor: raw.sensor.clone(),
            gains,
        }))
    }

    /// Release the cache's float planes before the sole source worker allocates another development.
    /// Preview jobs may still hold the old Arc; the worker's memory gate waits for those to finish.
    pub(crate) fn evict_development(&self) {
        if let Some(cached) = self.source_cache.borrow_mut().as_mut()
            && let PreparedSource::Raw(raw) = &mut cached.source
        {
            raw.linear.take();
        }
    }

    pub(crate) fn install_development(
        &self,
        request: &RawDevelopment,
        developed: RawPrepared,
    ) -> Result<EditorState, Error> {
        let state = self.state(&request.asset_id)?;
        if state.asset.fingerprint != request.fingerprint
            || developed.gains != request.gains
            || developed
                .linear
                .as_ref()
                .is_none_or(|image| image.fingerprint() != request.fingerprint)
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                "RAW development identity changed",
            ));
        }
        let current = Self::request_signature(&state.asset.locator)?.1;
        if current != request.signature {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source changed during RAW development",
            ));
        }
        let mut cache = self.source_cache.borrow_mut();
        let Some(cached) = cache.as_mut().filter(|cached| {
            cached.asset_id == request.asset_id && cached.signature == request.signature
        }) else {
            return Err(Error::new(
                ErrorKind::Conflict,
                "RAW source cache was replaced during development",
            ));
        };
        let PreparedSource::Raw(previous) = &cached.source else {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "RAW development targeted a JPEG source",
            ));
        };
        if !Arc::ptr_eq(&previous.sensor, &request.sensor) {
            return Err(Error::new(
                ErrorKind::Conflict,
                "RAW mosaic changed during development",
            ));
        }
        cached.source = PreparedSource::Raw(developed);
        Ok(state)
    }

    pub(crate) fn cached_state(&self, asset_id: &AssetId) -> Result<Option<EditorState>, Error> {
        let state = self.state(asset_id)?;
        let metadata = state.asset.locator.metadata().map_err(|_| {
            Error::new(
                ErrorKind::SourceUnavailable,
                "original source is unavailable",
            )
        })?;
        let signature = source_signature(&state.asset.locator, &metadata);
        if signature.file_identity != state.asset.file_identity
            || signature.byte_len != state.asset.byte_len
        {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source fingerprint changed",
            ));
        }
        let cache = self.source_cache.borrow();
        Ok(cache
            .as_ref()
            .filter(|cached| cached.asset_id == state.asset.id && cached.signature == signature)
            .map(|_| state))
    }

    /// Direct service clients may prepare synchronously; the API owner never calls this path.
    pub fn import(&mut self, path: &Path) -> Result<EditorState, Error> {
        self.import_prepared(Self::prepare_file(path)?)
            .map(|(state, _)| state)
    }

    /// Complete an import only after a worker has verified and decoded its exact source bytes.
    /// The bool says whether a new asset was inserted for event publication.
    pub(crate) fn import_prepared(
        &mut self,
        prepared: PreparedFile,
    ) -> Result<(EditorState, bool), Error> {
        let PreparedFile {
            canonical,
            signature,
            source,
            fingerprint,
        } = prepared;
        let current = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::SourceUnavailable, e.kind().to_string()))?;
        if source_signature(&canonical, &current) != signature {
            return Err(Error::new(
                ErrorKind::Conflict,
                "source changed before import completion",
            ));
        }
        let identity = signature.file_identity.clone();
        let canonical_text = canonical.to_string_lossy().into_owned();
        if let Some(existing) = self
            .connection
            .query_row(
                "SELECT id FROM assets WHERE canonical_locator=?1 OR file_identity=?2 LIMIT 1",
                params![canonical_text, identity],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(catalog_error)?
        {
            let state = self.state(&AssetId::parse(existing)?)?;
            if state.asset.fingerprint != fingerprint {
                return Err(Error::new(
                    ErrorKind::SourceUnavailable,
                    "original source fingerprint changed",
                ));
            }
            let interpretation = match source.metadata() {
                None => SourceKind::Jpeg,
                Some(metadata) => SourceKind::Raw {
                    metadata: serde_json::to_value(metadata)
                        .map_err(|e| json_error("RAW metadata", e))?,
                },
            };
            if !source_kinds_equal(&state.asset.source, &interpretation)? {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    "original source interpretation changed",
                ));
            }
            self.source_cache.replace(Some(CachedSource {
                asset_id: state.asset.id.clone(),
                signature,
                source,
            }));
            return Ok((state, false));
        }
        let (width, height) = source.dimensions();
        let source_kind = match source.metadata() {
            None => SourceKind::Jpeg,
            Some(metadata) => SourceKind::Raw {
                metadata: serde_json::to_value(metadata)
                    .map_err(|e| json_error("RAW metadata", e))?,
            },
        };
        let asset = AssetRecord {
            id: AssetId::new(),
            source_root: canonical.parent().unwrap_or(Path::new("")).to_path_buf(),
            locator: canonical.clone(),
            fingerprint: fingerprint.clone(),
            file_identity: identity,
            byte_len: signature.byte_len,
            width,
            height,
            source: source_kind,
        };
        let mut snapshot = Snapshot::original(asset.id.clone());
        if matches!(asset.source, SourceKind::Raw { .. }) {
            snapshot = snapshot.with_layer_inserted(
                0,
                crate::RawPayload::for_as_shot(
                    source.metadata().expect("RAW metadata").as_shot_gains,
                    source.metadata().expect("RAW metadata").cam_xyz,
                )?
                .layer(LayerId::new()),
            )?;
        }
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.id.clone(),
            sequence: 0,
            action_id: "original".into(),
            label: "Original".into(),
            parameters: json!({}),
            actor: "system".into(),
            timestamp_ms: now_ms(),
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot,
            undo_parent: None,
            restore_target: None,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        tx.execute(
            "INSERT INTO assets VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                asset.id.as_str(),
                asset.source_root.to_string_lossy(),
                asset.locator.to_string_lossy(),
                canonical_text,
                asset.file_identity,
                asset.fingerprint,
                i64::try_from(asset.byte_len).map_err(|_| Error::new(
                    ErrorKind::ResourceLimit,
                    "source length exceeds catalog range"
                ))?,
                i64::from(asset.width),
                i64::from(asset.height),
                encode(&asset.source)?,
            ],
        )
        .map_err(catalog_error)?;
        insert_entry(&self.registry, &tx, &self.artifact_root, &entry)?;
        tx.execute(
            "INSERT INTO asset_state VALUES (?1,?2,0,'[]')",
            params![asset.id.as_str(), entry.id.as_str()],
        )
        .map_err(catalog_error)?;
        tx.commit().map_err(catalog_error)?;
        self.source_cache.replace(Some(CachedSource {
            asset_id: asset.id.clone(),
            signature,
            source,
        }));
        Ok((
            EditorState {
                asset,
                revision: 0,
                current_entry: entry,
                redo: Vec::new(),
            },
            true,
        ))
    }

    pub fn state(&self, asset_id: &AssetId) -> Result<EditorState, Error> {
        state_from(&self.connection, asset_id)
    }

    /// Every referenced asset in import order.
    pub fn assets(&self) -> Result<Vec<AssetRecord>, Error> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {ASSET_COLUMNS} FROM assets ORDER BY rowid"
            ))
            .map_err(catalog_error)?;
        let rows = statement
            .query_map([], asset_record)
            .map_err(catalog_error)?;
        rows.map(|row| row.map_err(catalog_error)).collect()
    }

    pub fn history(
        &self,
        asset_id: &AssetId,
        before_sequence: Option<u64>,
        limit: usize,
    ) -> Result<HistoryPage, Error> {
        if limit == 0 || limit > MAX_HISTORY_PAGE {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("history page limit must be 1..={MAX_HISTORY_PAGE}"),
            ));
        }
        let before = before_sequence
            .unwrap_or(i64::MAX as u64)
            .min(i64::MAX as u64) as i64;
        let mut statement = self
            .connection
            .prepare("SELECT entry_json FROM entries WHERE asset_id=?1 AND sequence<?2 ORDER BY sequence DESC LIMIT ?3")
            .map_err(catalog_error)?;
        let rows = statement
            .query_map(params![asset_id.as_str(), before, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(catalog_error)?;
        let mut entries: Vec<HistoryEntry> = Vec::new();
        for row in rows {
            entries.push(decode(
                "invalid history entry",
                row.map_err(catalog_error)?,
            )?);
        }
        let next_before_sequence =
            (entries.len() == limit).then(|| entries.last().unwrap().sequence);
        Ok(HistoryPage {
            entries,
            next_before_sequence,
        })
    }

    pub fn entry(&self, asset_id: &AssetId, entry_id: &EntryId) -> Result<HistoryEntry, Error> {
        entry_from(&self.connection, asset_id, entry_id)
    }

    /// Describe one entry's stored layers for the recipe panel: `O(layers)` registry lookups and
    /// payload reads, with no decode, no render and no source access. A layer whose provider is
    /// missing or unavailable is listed with the reason, never omitted.
    pub fn describe_entry(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<RecipeDescription, Error> {
        let entry = match entry_id {
            Some(entry_id) => self.entry(asset_id, entry_id)?,
            None => self.state(asset_id)?.current_entry,
        };
        let mut layers = Vec::with_capacity(entry.snapshot.recipe.layers.len());
        for layer in &entry.snapshot.recipe.layers {
            let described = match self.registry.effect(&layer.effect_id) {
                None => LayerDescription {
                    id: layer.id.clone(),
                    effect: layer.effect_id.clone(),
                    module: None,
                    title: None,
                    summary: "no provider".into(),
                    values: Map::new(),
                    available: false,
                    artifacts: layer.artifacts.clone(),
                },
                Some((module, _)) => {
                    let descriptor = module.descriptor();
                    let unreadable = |error: Error| {
                        (
                            format!("unreadable payload: {}", error.detail),
                            Map::new(),
                            false,
                        )
                    };
                    let (summary, values, available) = match &descriptor.availability {
                        crate::Availability::Unavailable { reason } => {
                            (format!("unavailable: {reason}"), Map::new(), false)
                        }
                        // A payload the provider cannot read is reported on its own row; the
                        // rest of the stack is still described.
                        crate::Availability::Available => match module.describe_layer(
                            &layer.effect_id,
                            layer.effect_format,
                            &layer.payload,
                        ) {
                            Ok(summary) => match module.values(
                                &layer.effect_id,
                                layer.effect_format,
                                &layer.payload,
                            ) {
                                Ok(values) => (summary, values, true),
                                Err(error) => unreadable(error),
                            },
                            Err(error) => unreadable(error),
                        },
                    };
                    LayerDescription {
                        id: layer.id.clone(),
                        effect: layer.effect_id.clone(),
                        module: Some(descriptor.id.clone()),
                        title: Some(descriptor.title.clone()),
                        summary,
                        values,
                        available,
                        artifacts: layer.artifacts.clone(),
                    }
                }
            };
            layers.push(described);
        }
        Ok(RecipeDescription {
            entry_id: entry.id,
            layers,
        })
    }

    pub fn render_current(&self, asset_id: &AssetId) -> Result<Raster, Error> {
        let state = self.state(asset_id)?;
        self.render_entry(asset_id, &state.current_entry.id)
    }

    /// A preview job for one entry, or for an open draft's effective recipe. `layer_count`
    /// truncates the rendered stack to its first `n` layers, which the desktop uses to show a
    /// layer's input stage while drafting it; it must not exceed the rendered stack's layer count.
    /// A draft previews the current entry, so naming a historical one beside it is refused.
    /// `proxy` offers the display bounds the frame will be shown in; the queue decides what to do
    /// with them, and this call reads no pixels either way.
    pub fn preview_job(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
        layer_count: Option<usize>,
        draft: Option<&Draft>,
        proxy: Option<ProxyBounds>,
    ) -> Result<PreviewJob, Error> {
        let state = self.state(asset_id)?;
        if draft.is_some() && entry_id.is_some_and(|entry_id| entry_id != &state.current_entry.id) {
            return Err(Error::new(
                ErrorKind::Validation,
                "a draft previews the current entry, not a historical one",
            ));
        }
        let entry = match entry_id {
            Some(entry_id) => self.entry(asset_id, entry_id)?,
            None => state.current_entry.clone(),
        };
        // The draft's effective recipe is planned, not persisted, and costs point queries only.
        let (recipe, draft_revision) = match draft {
            Some(draft) => {
                let (recipe, _) = self.draft_recipe(asset_id, draft)?;
                (recipe, Some(draft.draft_revision))
            }
            None => (entry.snapshot.recipe.clone(), None),
        };
        let layers = recipe.layers.len();
        if let Some(count) = layer_count.filter(|count| *count > layers) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("preview layer count {count} exceeds the {layers} layers of this entry"),
            ));
        }
        // The artifacts the rendered stack references are bound before anything compiles it, and
        // the job holds their verified bytes, so a cache eviction never breaks it on the worker.
        let artifacts = self.require_artifacts(&recipe)?;
        // A draft's effective recipe decides the RAW development settings too, so a drafted
        // exposure previews the value the gesture holds rather than the committed one.
        let source = self.preview_source(&state.asset, &recipe)?;
        // The identity is computed exactly as an analysis job's is, so a report the preview worker
        // produces from this frame is a cache hit for a later `analysis.request`.
        let draft_stamp = draft.map(|draft| DraftStamp {
            draft_id: draft.draft_id.clone(),
            draft_revision: draft.draft_revision,
        });
        let (identity, _) = self.analysis_identity(
            asset_id,
            source.fingerprint(),
            source.dimensions(),
            &entry,
            &recipe,
            draft_stamp,
        )?;
        Ok(PreviewJob {
            source,
            entry,
            registry: self.registry.clone(),
            recipe,
            layer_count,
            draft_revision,
            identity,
            analyse: false,
            // A truncated job renders a layer prefix, whose output stage a plan computed from the
            // whole stack does not describe, and the desktop shows it only as a drafting aid. It
            // therefore never has a proxy phase, whatever bounds the caller offered.
            proxy: proxy.filter(|_| layer_count.is_none()),
            artifacts,
        })
    }

    /// The identity of the analysis of one evaluated stack, and the reason that stack has no output
    /// stage when the host cannot compile it. `O(layers)`: it compiles the stack to learn its output
    /// dimensions and hashes the recipe, and it reads no pixels and rasterizes nothing, so the
    /// catalog owner may call it while building a job. A stack whose artifacts are missing or not
    /// prepared is an error rather than a stack without an output stage: it is not unevaluable,
    /// only not evaluable yet.
    pub fn analysis_identity(
        &self,
        asset_id: &AssetId,
        source_fingerprint: &str,
        source_dimensions: (u32, u32),
        entry: &HistoryEntry,
        recipe: &Recipe,
        draft: Option<DraftStamp>,
    ) -> Result<(AnalysisIdentity, Option<Error>), Error> {
        let _artifacts = self.require_artifacts(recipe)?;
        let stage = self
            .registry
            .compile(source_dimensions.0, source_dimensions.1, recipe)
            .map(|compiled| {
                let stage = compiled.stage();
                (stage.width, stage.height)
            });
        let failure = stage.as_ref().err().cloned();
        let identity = AnalysisIdentity::of(
            asset_id,
            source_fingerprint,
            entry,
            recipe,
            draft,
            stage.ok(),
        )?;
        Ok((identity, failure))
    }

    /// The immutable buffer a preview or an analysis worker renders, chosen by the asset's source
    /// interpretation: the decoded JPEG, or the developed RAW mosaic with the linear settings the
    /// given recipe asks for. A RAW stack whose white balance the prepared image does not hold
    /// reports `preparation-required` rather than rendering a stale development.
    fn preview_source(&self, asset: &AssetRecord, recipe: &Recipe) -> Result<PreviewSource, Error> {
        match self.verified_prepared(asset)? {
            PreparedSource::Jpeg(image) => {
                validate_source_recipe(asset, recipe)?;
                Ok(PreviewSource::Jpeg(image))
            }
            PreparedSource::Raw(raw) => {
                let settings = raw_settings(&raw, recipe)?;
                Ok(PreviewSource::Raw {
                    image: raw.linear.ok_or_else(|| {
                        Error::new(ErrorKind::PreparationRequired, "RAW development required")
                    })?,
                    settings,
                })
            }
        }
    }

    /// Everything one analysis job needs, planned on the catalog owner: the identity that names the
    /// result, the cached verified source, the shared registry and the effective recipe to render.
    /// Costs a state read, a cached source verification and an `O(layers)` plan and compile; no
    /// frame is allocated here and nothing is persisted.
    pub fn analysis_plan(
        &self,
        asset_id: &AssetId,
        selection: AnalysisSelection<'_>,
    ) -> Result<AnalysisPlan, Error> {
        let state = self.state(asset_id)?;
        let (entry, recipe, draft) = match selection {
            AnalysisSelection::Current => {
                let entry = state.current_entry.clone();
                let recipe = entry.snapshot.recipe.clone();
                (entry, recipe, None)
            }
            // A historical entry answers from its own immutable stack, so a later commit by any
            // client never relabels this result as current.
            AnalysisSelection::Entry(entry_id) => {
                let entry = self.entry(asset_id, entry_id)?;
                let recipe = entry.snapshot.recipe.clone();
                (entry, recipe, None)
            }
            // A draft is evaluated at the revision it holds now: its effective recipe is planned
            // against the current stack and never persisted.
            AnalysisSelection::Draft(draft) => {
                let (recipe, drafted) = self.draft_recipe(asset_id, draft)?;
                let stamp = DraftStamp {
                    draft_id: draft.draft_id.clone(),
                    draft_revision: draft.draft_revision,
                };
                (drafted.current_entry, recipe, Some(stamp))
            }
        };
        // The job holds the verified bytes of every artifact its stack references, so a cache
        // eviction never breaks it on the worker.
        let artifacts = self.require_artifacts(&recipe)?;
        // The identity and the output stage come from the asset record, so a stack the host cannot
        // evaluate at all is reported failed without decoding or developing the original: there is
        // no frame for that job to render. Only an evaluable stack asks for the prepared source.
        let (identity, failure) = self.analysis_identity(
            asset_id,
            &state.asset.fingerprint,
            (state.asset.width, state.asset.height),
            &entry,
            &recipe,
            draft,
        )?;
        let source = match failure {
            Some(_) => None,
            None => Some(self.preview_source(&state.asset, &recipe)?),
        };
        Ok(AnalysisPlan {
            identity,
            source,
            registry: self.registry.clone(),
            recipe,
            failure,
            artifacts,
        })
    }

    pub fn render_entry(&self, asset_id: &AssetId, entry_id: &EntryId) -> Result<Raster, Error> {
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let _artifacts = self.require_artifacts(&entry.snapshot.recipe)?;
        match self.verified_prepared(&state.asset)? {
            PreparedSource::Jpeg(source) => {
                validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
                render(
                    &self.registry,
                    &source,
                    entry.snapshot.id.clone(),
                    &entry.snapshot.recipe,
                )
            }
            PreparedSource::Raw(raw) => {
                let settings = raw_settings(&raw, &entry.snapshot.recipe)?;
                render_linear(
                    &self.registry,
                    raw.linear.as_ref().ok_or_else(|| {
                        Error::new(ErrorKind::PreparationRequired, "RAW development required")
                    })?,
                    entry.snapshot.id.clone(),
                    &entry.snapshot.recipe,
                    settings,
                )
            }
        }
    }

    /// Evaluate one output pixel of a saved entry without rasterizing the image.
    pub fn sample_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<PixelSample, Error> {
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let _artifacts = self.require_artifacts(&entry.snapshot.recipe)?;
        let source = self.preview_source(&state.asset, &entry.snapshot.recipe)?;
        let sampled = source.sample(&self.registry, &entry.snapshot.recipe, x, y)?;
        pixel_sample(entry, &state.asset.fingerprint, sampled, x, y, None)
    }

    /// Bind the asset's current entry for sampling off the catalog owner: its verified source, its
    /// recipe, compiled once here so a stack the host cannot evaluate is refused now, and the
    /// verified bytes of every artifact it references. It binds the stack like
    /// [`Self::sample_entry`], so an unprepared source or artifact is `preparation-required`. A
    /// state read, a cached source verification and an `O(layers)` compile; no pixel is read.
    pub(crate) fn sample_plan(&self, asset_id: &AssetId) -> Result<SamplePlan, Error> {
        let state = self.state(asset_id)?;
        let recipe = state.current_entry.snapshot.recipe;
        let artifacts = self.require_artifacts(&recipe)?;
        let source = self.preview_source(&state.asset, &recipe)?;
        let (width, height) = source.dimensions();
        self.registry.compile(width, height, &recipe)?;
        Ok(SamplePlan {
            source,
            registry: self.registry.clone(),
            recipe,
            _artifacts: artifacts,
        })
    }

    /// One output pixel of an open draft's effective recipe, evaluated the same way: the draft's
    /// action is planned against the current stack and the resulting recipe answers the point. No
    /// frame is rasterized and nothing is persisted.
    pub fn sample_draft(
        &self,
        asset_id: &AssetId,
        draft: &Draft,
        x: u32,
        y: u32,
    ) -> Result<PixelSample, Error> {
        let (recipe, state) = self.draft_recipe(asset_id, draft)?;
        let _artifacts = self.require_artifacts(&recipe)?;
        let source = self.preview_source(&state.asset, &recipe)?;
        let sampled = source.sample(&self.registry, &recipe, x, y)?;
        let fingerprint = state.asset.fingerprint.clone();
        pixel_sample(
            state.current_entry,
            &fingerprint,
            sampled,
            x,
            y,
            Some(DraftStamp {
                draft_id: draft.draft_id.clone(),
                draft_revision: draft.draft_revision,
            }),
        )
    }

    /// Map one output pixel of a saved entry back to the pixel of the content stage it shows: the
    /// source after EXIF orientation, which is the stage a pixel-stage edit addresses. Like
    /// `sample_entry` it answers from the compiled stack and rasterizes nothing.
    pub fn locate_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        x: u32,
        y: u32,
    ) -> Result<ContentPoint, Error> {
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        let _artifacts = self.require_artifacts(&entry.snapshot.recipe)?;
        locate_dimensions(
            &self.registry,
            state.asset.width,
            state.asset.height,
            &entry.snapshot.recipe,
            x,
            y,
        )
    }

    /// One action request for every caller: the desktop, the JSON API and headless clients all
    /// arrive here with an action identity and its declared parameters.
    pub fn apply_action(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        action_id: &str,
        parameters: Value,
    ) -> Result<MutationResult, Error> {
        mutation.validate()?;
        let registry = self.registry.clone();
        let (module, action) = registry.action(action_id).ok_or_else(|| {
            Error::new(ErrorKind::Validation, format!("unknown action {action_id}"))
        })?;
        let checked = check_parameters(action, &parameters)?;
        let input = module.parse(action_id, &checked)?;
        // The module labels a request its template cannot describe, such as a field patch; the
        // fallback comes from the action that was requested, which is not always the durable action
        // identity the entry stores: `transform` renders the label, `rotate-left` is stored.
        let label = module
            .label(&input)
            .unwrap_or_else(|| action_label(action, &input.parameters));
        let request = request_input(&input, &mutation)?;
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &request)? {
            return Ok(result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let recipe = &state.current_entry.snapshot.recipe;
        validate_source_recipe(&state.asset, recipe)?;
        // Both planning paths compile the current stack, so its artifacts are bound first.
        let _artifacts = self.require_artifacts(recipe)?;
        if module.descriptor().id == "lightwell.raw" {
            let (width, height) = (state.asset.width, state.asset.height);
            let stage = registry.compile(width, height, recipe)?.stage();
            let unavailable = |_: u32, _: u32| -> Result<Option<[u8; 4]>, Error> {
                Err(Error::new(
                    ErrorKind::Incompatible,
                    "RAW source action does not sample pixels",
                ))
            };
            let stage_before = |index: usize| -> Result<Stage, Error> {
                Ok(registry
                    .compile_layers(width, height, prefix(&recipe.layers, index)?)?
                    .stage())
            };
            let sample_before = |_: usize, _: u32, _: u32| -> Result<Option<[u8; 4]>, Error> {
                Err(Error::new(
                    ErrorKind::Incompatible,
                    "RAW source action does not sample pixels",
                ))
            };
            let insertion_index = |effect_stage: EffectStage| {
                registry.insertion_index(&recipe.layers, effect_stage, 0)
            };
            let insertion_index_for =
                |effect_id: &str| registry.insertion_index_for(&recipe.layers, effect_id);
            let raw_source = if action_id == "pick-raw-neutral" {
                Some(self.verified_prepared(&state.asset)?)
            } else {
                None
            };
            let neutral_sampler = raw_source.as_ref().map(|prepared| {
                move |x: u32, y: u32| -> Result<[f32; 3], Error> {
                    match prepared {
                        PreparedSource::Raw(raw) => crate::source::neutral_at(raw, x, y),
                        PreparedSource::Jpeg(_) => Err(Error::new(
                            ErrorKind::Validation,
                            "RAW neutral picker requires a RAW original",
                        )),
                    }
                }
            });
            let context = StageContext {
                stage,
                layers: &recipe.layers,
                sampler: &unavailable,
                stage_before: &stage_before,
                insertion_index: &insertion_index,
                insertion_index_for: &insertion_index_for,
                sample_before: &sample_before,
                sensor_neutral: neutral_sampler
                    .as_ref()
                    .map(|sample| sample as &dyn Fn(u32, u32) -> Result<[f32; 3], Error>),
            };
            let snapshot = match module.plan(&input, &context)? {
                ActionPlan::NoOp => {
                    return self.persist_noop(asset_id, &mutation, &request, &state);
                }
                ActionPlan::Update(layer) => {
                    state.current_entry.snapshot.with_layer_replaced(layer)?
                }
                ActionPlan::Commit(_) => {
                    return Err(Error::new(
                        ErrorKind::Validation,
                        "RAW source action may only update its required layer",
                    ));
                }
            };
            return self.commit_snapshot(
                asset_id,
                mutation,
                request,
                snapshot,
                &state.asset,
                CommittedAction { input, label },
            );
        }
        let source = self.verified_prepared(&state.asset)?;
        let snapshot = match self.plan_input(&state, &source, module, &input)? {
            ActionPlan::NoOp => {
                return self.persist_noop(asset_id, &mutation, &request, &state);
            }
            // The host places the layer by the effect's declared stage and order: a pixel-stage
            // effect goes before the geometry tail, so a later crop change carries it instead of
            // moving or invalidating it. An effect no provider declares is placed as a geometry one
            // would be and rejected by the whole-stack compile below.
            ActionPlan::Commit(layer) => {
                let index = registry.insertion_index_for(
                    &state.current_entry.snapshot.recipe.layers,
                    &layer.effect_id,
                );
                state
                    .current_entry
                    .snapshot
                    .with_layer_inserted(index, layer)?
            }
            // An update keeps the layer's identity and position; a missing identity is rejected
            // before anything is written.
            ActionPlan::Update(layer) => state.current_entry.snapshot.with_layer_replaced(layer)?,
        };
        self.commit_snapshot(
            asset_id,
            mutation,
            request,
            snapshot,
            &state.asset,
            CommittedAction { input, label },
        )
    }

    /// Ask a module what one parsed request would do to this stack. The stack is compiled once and
    /// every question the module may ask is a point query or a prefix compile, so planning costs
    /// `O(layers)` and rasterizes nothing. Shared by a commit and by a draft's effective recipe, so
    /// a drafted preview evaluates exactly what committing that draft would produce.
    fn plan_input(
        &self,
        state: &EditorState,
        source: &PreparedSource,
        module: &dyn crate::ToolModule,
        input: &ActionInput,
    ) -> Result<ActionPlan, Error> {
        self.with_stage_context(source, &state.current_entry.snapshot.recipe, |context| {
            module.plan(input, context)
        })
    }

    /// Build the questions a module may ask about one stack and hand them to `answer`.
    ///
    /// The stack is compiled once and every question is a point query or a prefix compile, so this
    /// costs `O(layers)` per question and rasterizes nothing. Planning an action and answering a
    /// read-only query share it, which is what makes a query see exactly the stage a commit would
    /// address.
    fn with_stage_context<T>(
        &self,
        source: &PreparedSource,
        recipe: &Recipe,
        answer: impl FnOnce(&StageContext<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let registry = &self.registry;
        let (width, height) = source.dimensions();
        // A JPEG stack compiles once into one evaluation that answers every point. A RAW stack has
        // no 8-bit buffer to evaluate, so its points come from the linear sampler at the settings
        // this recipe asks for; either way nothing is rasterized.
        let jpeg_evaluation = match source {
            PreparedSource::Jpeg(image) => Some(Evaluation::new(registry, image, recipe)?),
            PreparedSource::Raw(_) => None,
        };
        let raw_settings = match source {
            PreparedSource::Jpeg(_) => None,
            PreparedSource::Raw(raw) => Some(raw_settings(raw, recipe)?),
        };
        let raw_linear = || -> Result<&crate::LinearImage, Error> {
            match source {
                PreparedSource::Raw(raw) => raw.linear.as_ref().ok_or_else(|| {
                    Error::new(ErrorKind::PreparationRequired, "RAW development required")
                }),
                PreparedSource::Jpeg(_) => Err(Error::new(
                    ErrorKind::Incompatible,
                    "JPEG source has no linear image",
                )),
            }
        };
        let stage = registry.compile(width, height, recipe)?.stage();
        let sampler = |x: u32, y: u32| -> Result<Option<[u8; 4]>, Error> {
            match &jpeg_evaluation {
                Some(evaluation) => evaluation.pixel(x, y),
                None => Ok(sample_linear(
                    registry,
                    raw_linear()?,
                    recipe,
                    raw_settings.expect("RAW settings"),
                    x,
                    y,
                )?
                .rgba),
            }
        };
        // The stage one layer receives: compile the prefix before it. Compiling folds declared
        // output stages and allocates only the operation lists, so this copies no part of the stack
        // and rasterizes nothing. The whole recipe compiled above, so its format is known good.
        let stage_before = |index: usize| -> Result<Stage, Error> {
            Ok(registry
                .compile_layers(width, height, prefix(&recipe.layers, index)?)?
                .stage())
        };
        // One pixel of the stage a prefix produces, for a module planning against the position its
        // layer will take. Compiling the prefix costs O(layers) and the evaluation answers the
        // point per segment, so nothing is rasterized here either.
        let sample_before = |index: usize, x: u32, y: u32| -> Result<Option<[u8; 4]>, Error> {
            let layers = prefix(&recipe.layers, index)?;
            match source {
                PreparedSource::Jpeg(image) => {
                    Evaluation::over_layers(registry, image, layers)?.pixel(x, y)
                }
                PreparedSource::Raw(_) => {
                    let prefix_recipe = Recipe {
                        format: recipe.format,
                        layers: layers.to_vec(),
                    };
                    Ok(sample_linear(
                        registry,
                        raw_linear()?,
                        &prefix_recipe,
                        raw_settings.expect("RAW settings"),
                        x,
                        y,
                    )?
                    .rgba)
                }
            }
        };
        let insertion_index =
            |stage: EffectStage| registry.insertion_index(&recipe.layers, stage, 0);
        let insertion_index_for =
            |effect_id: &str| registry.insertion_index_for(&recipe.layers, effect_id);
        let context = StageContext {
            stage,
            layers: &recipe.layers,
            sampler: &sampler,
            stage_before: &stage_before,
            insertion_index: &insertion_index,
            insertion_index_for: &insertion_index_for,
            sample_before: &sample_before,
            sensor_neutral: None,
        };
        answer(&context)
    }

    /// Answer one module query about a saved entry's stack: the read-only counterpart of
    /// [`EditorService::apply_action`].
    ///
    /// The query is resolved from the same registry discovery lists, its parameters go through the
    /// same generic check, and it is handed the same [`StageContext`] a commit is planned against.
    /// Nothing is written: no snapshot, no history entry, no request row and no event, so two
    /// clients asking the same question concurrently get the same answer and neither disturbs the
    /// other. Cost is `O(layers)` per point the module samples and no frame is allocated.
    pub fn run_query(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        query_id: &str,
        parameters: Value,
    ) -> Result<Value, Error> {
        let registry = self.registry.clone();
        let (module, query) = registry.query(query_id).ok_or_else(|| {
            Error::new(ErrorKind::Validation, format!("unknown query {query_id}"))
        })?;
        if !module.descriptor().is_available() {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!(
                    "unavailable module {} cannot answer {query_id}",
                    module.descriptor().id
                ),
            ));
        }
        let checked = check_parameters(query, &parameters)?;
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        let _artifacts = self.require_artifacts(&entry.snapshot.recipe)?;
        let source = self.verified_prepared(&state.asset)?;
        self.with_stage_context(&source, &entry.snapshot.recipe, |context| {
            module.query(query_id, &checked, context)
        })
    }

    /// The recipe an open draft would produce: the current snapshot with the draft's action planned
    /// against it and its plan applied, computed on demand and never persisted. A `NoOp` plan means
    /// the current recipe unchanged, so a gesture that returned to its start previews exactly what
    /// is committed. Nothing is rendered here; the caller decides what to do with the recipe.
    pub fn draft_recipe(
        &self,
        asset_id: &AssetId,
        draft: &Draft,
    ) -> Result<(Recipe, EditorState), Error> {
        if &draft.asset_id != asset_id {
            return Err(Error::new(
                ErrorKind::Validation,
                "draft belongs to another asset",
            ));
        }
        let registry = self.registry.clone();
        let (module, action) = registry.action(&draft.action).ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!("unknown action {}", draft.action),
            )
        })?;
        let checked = check_parameters(action, &Value::Object(draft.fields.clone()))?;
        let input = module.parse(&draft.action, &checked)?;
        let state = self.state(asset_id)?;
        validate_source_recipe(&state.asset, &state.current_entry.snapshot.recipe)?;
        // Planning compiles the current stack. The effective recipe may reference an artifact the
        // current one does not, so each caller binds that recipe's artifacts before evaluating it.
        let _artifacts = self.require_artifacts(&state.current_entry.snapshot.recipe)?;
        let source = self.verified_prepared(&state.asset)?;
        let recipe = match self.plan_input(&state, &source, module, &input)? {
            ActionPlan::NoOp => state.current_entry.snapshot.recipe.clone(),
            ActionPlan::Commit(layer) => {
                let index = registry.insertion_index_for(
                    &state.current_entry.snapshot.recipe.layers,
                    &layer.effect_id,
                );
                state
                    .current_entry
                    .snapshot
                    .recipe
                    .with_layer_inserted(index, layer)?
            }
            ActionPlan::Update(layer) => state
                .current_entry
                .snapshot
                .recipe
                .with_layer_replaced(layer)?,
        };
        Ok((recipe, state))
    }

    pub fn apply_pixel(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        x: u32,
        y: u32,
        rgb: [u8; 3],
    ) -> Result<MutationResult, Error> {
        self.apply_action(
            asset_id,
            mutation,
            "set-pixel",
            json!({"x":x,"y":y,"rgb":rgb}),
        )
    }

    fn verified_prepared(&self, asset: &AssetRecord) -> Result<PreparedSource, Error> {
        let before = asset.locator.metadata().map_err(|_| {
            Error::new(
                ErrorKind::SourceUnavailable,
                "original source is unavailable",
            )
        })?;
        let signature = source_signature(&asset.locator, &before);
        let raw_source = matches!(&asset.source, SourceKind::Raw { .. });
        let max_source_bytes = if raw_source {
            lightwell_raw::MAX_SOURCE_BYTES as u64
        } else {
            128 * 1024 * 1024
        };
        if signature.byte_len != asset.byte_len
            || signature.byte_len > max_source_bytes
            || signature.file_identity != asset.file_identity
        {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source fingerprint changed",
            ));
        }
        if let Some(cached) = self.source_cache.borrow().as_ref()
            && cached.asset_id == asset.id
            && cached.signature == signature
        {
            return Ok(cached.source.clone());
        }
        if !self.allow_sync_source {
            return Err(Error::new(
                ErrorKind::PreparationRequired,
                "source preparation required",
            ));
        }
        let prepared = Self::prepare_file(&asset.locator)?;
        if prepared.signature != signature || prepared.fingerprint != asset.fingerprint {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source fingerprint changed",
            ));
        }
        match (&asset.source, &prepared.source) {
            (SourceKind::Jpeg, PreparedSource::Jpeg(_)) => {}
            (SourceKind::Raw { .. }, PreparedSource::Raw(raw))
                if source_kinds_equal(
                    &asset.source,
                    &SourceKind::Raw {
                        metadata: serde_json::to_value(raw.sensor.metadata())
                            .map_err(|e| json_error("RAW metadata", e))?,
                    },
                )? => {}
            _ => {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    "original source interpretation changed",
                ));
            }
        }
        self.source_cache.replace(Some(CachedSource {
            asset_id: asset.id.clone(),
            signature,
            source: prepared.source.clone(),
        }));
        Ok(prepared.source)
    }

    pub fn apply_transform(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        transform: Transform,
    ) -> Result<MutationResult, Error> {
        self.apply_action(
            asset_id,
            mutation,
            "transform",
            json!({"transform":transform}),
        )
    }

    /// Persist one resulting stack: the same path for an appended and an updated layer. The
    /// resulting recipe is validated and compiled against the cached verified source first, so a
    /// stack that leaves a later layer addressing a stage that no longer exists is rejected with
    /// the compile error and nothing is written. Compiling is O(layers) and rasterizes nothing.
    /// Every artifact the stack lists must be recorded with a present file before it is bound and
    /// compiled; the transaction checks that again and records the entry's references with it.
    fn commit_snapshot(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        request: Value,
        snapshot: Snapshot,
        asset: &AssetRecord,
        action: CommittedAction,
    ) -> Result<MutationResult, Error> {
        let mut state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        self.registry.validate_recipe(&snapshot.recipe)?;
        validate_source_recipe(asset, &snapshot.recipe)?;
        artifact_store::recorded_artifacts(
            &self.connection,
            &self.artifact_root,
            &snapshot.recipe,
        )?;
        let _artifacts = self.require_artifacts(&snapshot.recipe)?;
        self.registry
            .compile(asset.width, asset.height, &snapshot.recipe)?;
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset_id.clone(),
            sequence: next_sequence(&self.connection, asset_id)?,
            action_id: action.input.action_id,
            label: action.label,
            parameters: Value::Object(action.input.parameters),
            actor: mutation.actor.clone(),
            timestamp_ms: now_ms(),
            request_id: Some(mutation.request_id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot,
            undo_parent: Some(state.current_entry.id.clone()),
            restore_target: None,
        };
        let result = MutationResult {
            outcome: MutationOutcome::Applied,
            revision: entry.result_revision,
            current_entry_id: entry.id.clone(),
            created_entry_id: Some(entry.id.clone()),
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        ensure_request_absent(&tx, asset_id, &mutation.request_id)?;
        insert_entry(&self.registry, &tx, &self.artifact_root, &entry)?;
        tx.execute("UPDATE asset_state SET current_entry_id=?2,revision=?3,redo_json='[]' WHERE asset_id=?1", params![asset_id.as_str(), entry.id.as_str(), entry.result_revision as i64]).map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, &request, &result)?;
        tx.commit().map_err(catalog_error)?;
        state.current_entry = entry;
        Ok(result)
    }

    pub fn undo(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
    ) -> Result<MutationResult, Error> {
        self.navigate(asset_id, mutation, Navigation::Undo)
    }

    pub fn redo(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
    ) -> Result<MutationResult, Error> {
        self.navigate(asset_id, mutation, Navigation::Redo)
    }

    fn navigate(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        navigation: Navigation,
    ) -> Result<MutationResult, Error> {
        mutation.validate()?;
        let action = match navigation {
            Navigation::Undo => "undo",
            Navigation::Redo => "redo",
        };
        let input = json!({"action":action,"mutation":mutation});
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &input)? {
            return Ok(result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let mut redo = state.redo.clone();
        let target = match navigation {
            Navigation::Undo => {
                let Some(parent) = state.current_entry.undo_parent.clone() else {
                    return self.persist_noop(asset_id, &mutation, &input, &state);
                };
                redo.push(state.current_entry.id.clone());
                parent
            }
            Navigation::Redo => {
                let Some(entry) = redo.pop() else {
                    return self.persist_noop(asset_id, &mutation, &input, &state);
                };
                entry
            }
        };
        let _ = self.entry(asset_id, &target)?;
        let result = MutationResult {
            outcome: MutationOutcome::Navigated,
            revision: state.revision + 1,
            current_entry_id: target.clone(),
            created_entry_id: None,
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        tx.execute(
            "UPDATE asset_state SET current_entry_id=?2,revision=?3,redo_json=?4 WHERE asset_id=?1",
            params![
                asset_id.as_str(),
                target.as_str(),
                result.revision as i64,
                encode(&redo)?
            ],
        )
        .map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, &input, &result)?;
        tx.commit().map_err(catalog_error)?;
        Ok(result)
    }

    pub fn restore(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        target_id: &EntryId,
    ) -> Result<MutationResult, Error> {
        mutation.validate()?;
        let input = json!({"action":"restore","mutation":mutation,"target_entry_id":target_id});
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &input)? {
            return Ok(result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let target = self.entry(asset_id, target_id)?;
        // Restoring a stack the current providers cannot evaluate fails explicitly; browsing it
        // with undo, redo and history stays available.
        self.registry.validate_recipe(&target.snapshot.recipe)?;
        if target.snapshot.recipe == state.current_entry.snapshot.recipe {
            return self.persist_noop(asset_id, &mutation, &input, &state);
        }
        let snapshot = Snapshot {
            id: SnapshotId::new(),
            asset_id: asset_id.clone(),
            recipe: target.snapshot.recipe.clone(),
        };
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset_id.clone(),
            sequence: next_sequence(&self.connection, asset_id)?,
            action_id: "restore".into(),
            label: format!("Restore entry {}", target.sequence),
            parameters: json!({"target_entry_id":target_id}),
            actor: mutation.actor.clone(),
            timestamp_ms: now_ms(),
            request_id: Some(mutation.request_id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot,
            undo_parent: Some(state.current_entry.id.clone()),
            restore_target: Some(target_id.clone()),
        };
        let result = MutationResult {
            outcome: MutationOutcome::Applied,
            revision: entry.result_revision,
            current_entry_id: entry.id.clone(),
            created_entry_id: Some(entry.id.clone()),
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        insert_entry(&self.registry, &tx, &self.artifact_root, &entry)?;
        tx.execute("UPDATE asset_state SET current_entry_id=?2,revision=?3,redo_json='[]' WHERE asset_id=?1", params![asset_id.as_str(), entry.id.as_str(), entry.result_revision as i64]).map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, &input, &result)?;
        tx.commit().map_err(catalog_error)?;
        Ok(result)
    }

    /// Walk undo parents from `from` (default: current) towards Original, newest first.
    pub fn lineage(
        &self,
        asset_id: &AssetId,
        from: Option<&EntryId>,
        limit: usize,
    ) -> Result<Lineage, Error> {
        if limit == 0 || limit > MAX_HISTORY_PAGE {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("lineage limit must be 1..={MAX_HISTORY_PAGE}"),
            ));
        }
        let mut next = Some(match from {
            Some(entry_id) => entry_id.clone(),
            None => self.state(asset_id)?.current_entry.id,
        });
        let mut steps = Vec::new();
        while let Some(entry_id) = next.take() {
            if steps.len() == limit {
                next = Some(entry_id);
                break;
            }
            let (sequence, action_id, parent): (i64, String, Option<String>) = self
                .connection
                .query_row(
                    "SELECT sequence,action_id,undo_parent_id FROM entries WHERE id=?1 AND asset_id=?2",
                    params![entry_id.as_str(), asset_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(catalog_error)?
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Validation,
                        "history entry does not belong to this asset",
                    )
                })?;
            let undo_parent = parent.map(EntryId::parse).transpose()?;
            next = undo_parent.clone();
            steps.push(LineageStep {
                entry_id,
                sequence: u64::try_from(sequence)
                    .map_err(|_| Error::new(ErrorKind::Catalog, "invalid history sequence"))?,
                action_id,
                undo_parent,
            });
        }
        Ok(Lineage {
            steps,
            next_entry_id: next,
        })
    }

    /// Saved versions of one asset in creation order.
    pub fn versions(&self, asset_id: &AssetId) -> Result<Vec<Version>, Error> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT v.name,v.entry_id,e.sequence,v.actor,v.created_ms FROM versions v
                 JOIN entries e ON e.id = v.entry_id
                 WHERE v.asset_id=?1 ORDER BY v.created_ms, v.name",
            )
            .map_err(catalog_error)?;
        let rows = statement
            .query_map([asset_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(catalog_error)?;
        let mut versions = Vec::new();
        for row in rows {
            let (name, entry_id, sequence, actor, created_ms) = row.map_err(catalog_error)?;
            versions.push(Version {
                asset_id: asset_id.clone(),
                name,
                entry_id: EntryId::parse(entry_id)?,
                entry_sequence: u64::try_from(sequence)
                    .map_err(|_| Error::new(ErrorKind::Catalog, "invalid history sequence"))?,
                actor,
                created_ms,
            });
        }
        Ok(versions)
    }

    /// Name a retained entry (default: current). Re-creating the same name on the same entry is a no-op;
    /// on a different entry it is a conflict. Names are unique per asset ignoring case.
    pub fn create_version(
        &mut self,
        asset_id: &AssetId,
        name: &str,
        entry_id: Option<&EntryId>,
        actor: &str,
    ) -> Result<VersionResult, Error> {
        let name = valid_version_name(name)?;
        if actor.is_empty() || actor.len() > 128 {
            return Err(Error::new(
                ErrorKind::Validation,
                "actor must contain 1..128 characters",
            ));
        }
        let entry = match entry_id {
            Some(entry_id) => self.entry(asset_id, entry_id)?,
            None => self.state(asset_id)?.current_entry,
        };
        if let Some(existing) = self
            .versions(asset_id)?
            .into_iter()
            .find(|version| version.name.eq_ignore_ascii_case(&name))
        {
            if existing.entry_id == entry.id {
                return Ok(VersionResult {
                    outcome: MutationOutcome::NoOp,
                    version: Some(existing),
                });
            }
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("version {:?} already names another entry", existing.name),
            ));
        }
        let version = Version {
            asset_id: asset_id.clone(),
            name,
            entry_id: entry.id,
            entry_sequence: entry.sequence,
            actor: actor.into(),
            created_ms: now_ms(),
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        tx.execute(
            "INSERT INTO versions VALUES (?1,?2,?3,?4,?5)",
            params![
                asset_id.as_str(),
                version.name,
                version.entry_id.as_str(),
                version.actor,
                version.created_ms
            ],
        )
        .map_err(catalog_error)?;
        tx.commit().map_err(catalog_error)?;
        Ok(VersionResult {
            outcome: MutationOutcome::Applied,
            version: Some(version),
        })
    }

    /// Remove a version name. The entry it named is retained history and stays reachable.
    pub fn delete_version(
        &mut self,
        asset_id: &AssetId,
        name: &str,
    ) -> Result<VersionResult, Error> {
        let name = valid_version_name(name)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        let removed = tx
            .execute(
                "DELETE FROM versions WHERE asset_id=?1 AND name=?2",
                params![asset_id.as_str(), name],
            )
            .map_err(catalog_error)?;
        tx.commit().map_err(catalog_error)?;
        Ok(VersionResult {
            outcome: if removed == 0 {
                MutationOutcome::NoOp
            } else {
                MutationOutcome::Applied
            },
            version: None,
        })
    }

    fn persist_noop(
        &mut self,
        asset_id: &AssetId,
        mutation: &Mutation,
        input: &Value,
        state: &EditorState,
    ) -> Result<MutationResult, Error> {
        let result = MutationResult {
            outcome: MutationOutcome::NoOp,
            revision: state.revision,
            current_entry_id: state.current_entry.id.clone(),
            created_entry_id: None,
            deduplicated: false,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        insert_request(&tx, asset_id, &mutation.request_id, input, &result)?;
        tx.commit().map_err(catalog_error)?;
        Ok(result)
    }

    fn request_result(
        &self,
        asset_id: &AssetId,
        request_id: &str,
        input: &Value,
    ) -> Result<Option<MutationResult>, Error> {
        let found = self
            .connection
            .query_row(
                "SELECT input_hash,result_json FROM requests WHERE asset_id=?1 AND request_id=?2",
                params![asset_id.as_str(), request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(catalog_error)?;
        let Some((stored_hash, result_json)) = found else {
            return Ok(None);
        };
        if stored_hash != input_hash(input)? {
            return Err(Error::new(
                ErrorKind::Conflict,
                "request_id was already used with different input",
            ));
        }
        let mut result: MutationResult = decode("invalid saved request result", result_json)?;
        result.deduplicated = true;
        Ok(Some(result))
    }
}

enum Navigation {
    Undo,
    Redo,
}

/// What one action records on the entry it commits: the durable identity and stored parameters the
/// module parsed, and the label the host rendered from the action that was requested.
struct CommittedAction {
    input: ActionInput,
    label: String,
}

/// One evaluated point with the identities that produced it. A point outside the rendered image is
/// a validation error naming the stage it missed.
fn pixel_sample(
    entry: HistoryEntry,
    source_fingerprint: &str,
    sampled: crate::Sample,
    x: u32,
    y: u32,
    draft: Option<DraftStamp>,
) -> Result<PixelSample, Error> {
    let rgba = sampled.rgba.ok_or_else(|| {
        Error::new(
            ErrorKind::Validation,
            format!(
                "sample ({x}, {y}) is outside the {}x{} rendered image",
                sampled.width, sampled.height
            ),
        )
    })?;
    Ok(PixelSample {
        entry_id: entry.id,
        snapshot_id: entry.snapshot.id,
        source_fingerprint: source_fingerprint.to_owned(),
        width: sampled.width,
        height: sampled.height,
        x,
        y,
        rgba,
        draft,
    })
}

/// The ordered layers before a position in the stack, which is what a module asks about when it
/// plans against the stage that position receives. A position past the end is a validation error.
fn prefix(layers: &[Layer], index: usize) -> Result<&[Layer], Error> {
    layers.get(..index).ok_or_else(|| {
        Error::new(
            ErrorKind::Validation,
            format!(
                "layer index {index} is outside the {} layers of the stack",
                layers.len()
            ),
        )
    })
}

fn ensure_revision(state: &EditorState, expected: u64) -> Result<(), Error> {
    if state.revision == expected {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "stale revision {expected}; current revision is {}",
                state.revision
            ),
        ))
    }
}

/// The deduplicated request identity: the durable action, the mutation envelope and the parsed
/// parameters as top-level fields.
fn request_input(input: &ActionInput, mutation: &Mutation) -> Result<Value, Error> {
    let mut request = serde_json::Map::new();
    request.insert("action".into(), Value::from(input.action_id.as_str()));
    request.insert(
        "mutation".into(),
        serde_json::to_value(mutation).map_err(|e| json_error("cannot encode request", e))?,
    );
    for (name, value) in &input.parameters {
        request.insert(name.clone(), value.clone());
    }
    Ok(Value::Object(request))
}

fn input_hash(input: &Value) -> Result<String, Error> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(input).map_err(|e| json_error("cannot encode request", e))?
        )
    ))
}

fn insert_request(
    tx: &Transaction<'_>,
    asset_id: &AssetId,
    request_id: &str,
    input: &Value,
    result: &MutationResult,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO requests VALUES (?1,?2,?3,?4)",
        params![
            asset_id.as_str(),
            request_id,
            input_hash(input)?,
            encode(result)?
        ],
    )
    .map_err(catalog_error)?;
    Ok(())
}

fn ensure_request_absent(
    tx: &Transaction<'_>,
    asset_id: &AssetId,
    request_id: &str,
) -> Result<(), Error> {
    let found: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM requests WHERE asset_id=?1 AND request_id=?2)",
            params![asset_id.as_str(), request_id],
            |row| row.get(0),
        )
        .map_err(catalog_error)?;
    if found {
        Err(Error::new(
            ErrorKind::Conflict,
            "request was committed concurrently",
        ))
    } else {
        Ok(())
    }
}

fn valid_version_name(name: &str) -> Result<String, Error> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_VERSION_NAME
        || name.chars().any(char::is_control)
    {
        return Err(Error::new(
            ErrorKind::Validation,
            format!("version name must contain 1..={MAX_VERSION_NAME} printable characters"),
        ));
    }
    Ok(name.to_string())
}

fn validate_source_recipe(asset: &AssetRecord, recipe: &crate::Recipe) -> Result<(), Error> {
    match asset.source {
        SourceKind::Jpeg => {
            if recipe
                .layers
                .iter()
                .any(|layer| layer.effect_id == crate::RAW_EFFECT)
            {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    "JPEG recipe contains a RAW source layer",
                ));
            }
        }
        SourceKind::Raw { ref metadata } => {
            let payload = raw_payload(recipe)?;
            let stored = parse_raw_interpretation(metadata, "RAW interpretation")?;
            if payload.as_shot_gains != stored.as_shot_gains || payload.cam_xyz != stored.cam_xyz {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    "RAW source layer calibration differs from original",
                ));
            }
        }
    }
    Ok(())
}

fn parse_raw_interpretation(
    metadata: &Value,
    context: &str,
) -> Result<lightwell_raw::RawMetadata, Error> {
    let parsed: lightwell_raw::RawMetadata =
        serde_json::from_value(metadata.clone()).map_err(|e| json_error(context, e))?;
    let is_dng = parsed.mode.requires_dng_corrections();
    if (is_dng
        && (!metadata
            .as_object()
            .is_some_and(|fields| fields.contains_key("dng_corrections"))
            || parsed.dng_corrections.is_none()))
        || (!is_dng && parsed.dng_corrections.is_some())
    {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("{context}: RAW correction record differs from mode"),
        ));
    }
    Ok(parsed)
}

/// Compare typed camera interpretation after JSON roundtrip. A stored f32 has a shortest decimal
/// encoding while a fresh serde_json::Value can hold its exact f64 widening; comparing those
/// Values directly can reject an unchanged original on reopen (seen on the Fuji corpus file).
/// Parsing back to RawMetadata preserves strict field equality at the native f32 precision.
fn source_kinds_equal(left: &SourceKind, right: &SourceKind) -> Result<bool, Error> {
    match (left, right) {
        (SourceKind::Jpeg, SourceKind::Jpeg) => Ok(true),
        (SourceKind::Raw { metadata: a }, SourceKind::Raw { metadata: b }) => {
            let a = parse_raw_interpretation(a, "stored RAW interpretation")?;
            let b = parse_raw_interpretation(b, "RAW interpretation")?;
            Ok(
                serde_json::to_value(a).map_err(|e| json_error("stored RAW interpretation", e))?
                    == serde_json::to_value(b).map_err(|e| json_error("RAW interpretation", e))?,
            )
        }
        _ => Ok(false),
    }
}

fn raw_settings(raw: &RawPrepared, recipe: &crate::Recipe) -> Result<crate::LinearSettings, Error> {
    let payload = raw_payload(recipe)?;
    let gains = match payload.wb_mode {
        crate::WhiteBalanceMode::AsShot => raw.sensor.metadata().as_shot_gains,
        crate::WhiteBalanceMode::Custom => payload.gains,
    };
    if gains != raw.gains || raw.linear.is_none() {
        return Err(Error::new(
            ErrorKind::PreparationRequired,
            "RAW white balance development required",
        ));
    }
    Ok(crate::LinearSettings {
        exposure_ev: payload.exposure_ev,
    })
}

fn raw_payload(recipe: &crate::Recipe) -> Result<crate::RawPayload, Error> {
    let Some(layer) = recipe.layers.first() else {
        return Err(Error::new(
            ErrorKind::Incompatible,
            "RAW recipe is missing its required source layer",
        ));
    };
    if layer.effect_id != crate::RAW_EFFECT
        || recipe
            .layers
            .iter()
            .skip(1)
            .any(|layer| layer.effect_id == crate::RAW_EFFECT)
    {
        return Err(Error::new(
            ErrorKind::Incompatible,
            "RAW recipe needs exactly one source layer at index zero",
        ));
    }
    crate::RawPayload::from_layer(layer)
}

/// Every path that writes a history entry comes through here, inside its own transaction, so the
/// entry's artifact references are checked and recorded with it or not at all: a snapshot can
/// never point at an artifact the catalog does not hold.
fn insert_entry(
    registry: &ModuleRegistry,
    tx: &Transaction<'_>,
    artifact_root: &Path,
    entry: &HistoryEntry,
) -> Result<(), Error> {
    registry.validate_recipe(&entry.snapshot.recipe)?;
    tx.execute(
        "INSERT INTO entries (id,asset_id,sequence,action_id,undo_parent_id,entry_json)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            entry.id.as_str(),
            entry.asset_id.as_str(),
            entry.sequence as i64,
            entry.action_id,
            entry.undo_parent.as_ref().map(EntryId::as_str),
            encode(entry)?
        ],
    )
    .map_err(catalog_error)?;
    artifact_store::link_artifacts(tx, artifact_root, entry)
}

fn next_sequence(connection: &Connection, asset_id: &AssetId) -> Result<u64, Error> {
    let sequence: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(sequence),-1)+1 FROM entries WHERE asset_id=?1",
            [asset_id.as_str()],
            |row| row.get(0),
        )
        .map_err(catalog_error)?;
    u64::try_from(sequence).map_err(|_| Error::new(ErrorKind::Catalog, "invalid history sequence"))
}

fn asset_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRecord> {
    let id = AssetId::parse(row.get::<_, String>(0)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(AssetRecord {
        id,
        source_root: PathBuf::from(row.get::<_, String>(1)?),
        locator: PathBuf::from(row.get::<_, String>(2)?),
        fingerprint: row.get(3)?,
        file_identity: row.get(4)?,
        byte_len: row.get::<_, i64>(5)? as u64,
        width: row.get::<_, i64>(6)? as u32,
        height: row.get::<_, i64>(7)? as u32,
        source: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

fn state_from(connection: &Connection, asset_id: &AssetId) -> Result<EditorState, Error> {
    let asset = connection
        .query_row(
            &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id=?1"),
            [asset_id.as_str()],
            asset_record,
        )
        .optional()
        .map_err(catalog_error)?
        .ok_or_else(|| Error::new(ErrorKind::Validation, "unknown asset"))?;
    if let SourceKind::Raw { metadata } = &asset.source {
        parse_raw_interpretation(metadata, "stored RAW interpretation")?;
    }
    let (current, revision, redo): (String, i64, String) = connection
        .query_row(
            "SELECT current_entry_id,revision,redo_json FROM asset_state WHERE asset_id=?1",
            [asset_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(catalog_error)?;
    let current_id = EntryId::parse(current)?;
    Ok(EditorState {
        asset,
        revision: revision as u64,
        current_entry: entry_from(connection, asset_id, &current_id)?,
        redo: decode("invalid redo state", redo)?,
    })
}

fn entry_from(
    connection: &Connection,
    asset_id: &AssetId,
    entry_id: &EntryId,
) -> Result<HistoryEntry, Error> {
    let json: String = connection
        .query_row(
            "SELECT entry_json FROM entries WHERE id=?1 AND asset_id=?2",
            params![entry_id.as_str(), asset_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(catalog_error)?
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                "history entry does not belong to this asset",
            )
        })?;
    decode("invalid history entry", json)
}

pub(crate) fn source_signature(path: &Path, metadata: &Metadata) -> SourceSignature {
    SourceSignature {
        byte_len: metadata.len(),
        modified: metadata.modified().ok(),
        file_identity: file_identity(metadata, path),
        change_marker: metadata_change_marker(metadata),
    }
}

#[cfg(unix)]
fn metadata_change_marker(metadata: &Metadata) -> Option<(i128, i128)> {
    use std::os::unix::fs::MetadataExt;
    Some((
        i128::from(metadata.ctime()),
        i128::from(metadata.ctime_nsec()),
    ))
}

#[cfg(windows)]
fn metadata_change_marker(metadata: &Metadata) -> Option<(i128, i128)> {
    use std::os::windows::fs::MetadataExt;
    Some((i128::from(metadata.last_write_time()), 0))
}

#[cfg(not(any(unix, windows)))]
fn metadata_change_marker(_: &Metadata) -> Option<(i128, i128)> {
    None
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata, _: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(windows)]
fn file_identity(metadata: &Metadata, canonical: &Path) -> String {
    use std::os::windows::fs::MetadataExt;
    match (metadata.volume_serial_number(), metadata.file_index()) {
        (Some(volume), Some(index)) => format!("windows:{volume}:{index}"),
        _ => format!("path:{}", canonical.to_string_lossy().to_lowercase()),
    }
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_: &Metadata, canonical: &Path) -> String {
    format!("path:{}", canonical.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionDescriptor, Availability, CROP_EFFECT, CropPayload, CropStage, EFFECT_FORMAT,
        EffectDescriptor, EffectStage, ExactGeometry, Layer, LayerId, ModuleDescriptor,
        ORIENTATION_EFFECT, PIXEL_EFFECT, ParameterDescriptor, ParameterKind, PreviewQueue,
        Processing, Stage, ToolModule, open_source,
    };
    use serde_json::Map;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lightwell-editor-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
    }
    fn mutation(revision: u64, request: &str) -> Mutation {
        Mutation {
            expected_revision: revision,
            request_id: request.into(),
            actor: "test".into(),
        }
    }

    #[test]
    fn raw_interpretation_json_roundtrip_is_strict_at_native_precision() {
        use lightwell_raw::{RawMetadata, RawMode, RawRect};
        let rect = RawRect {
            x: 0,
            y: 0,
            width: 32,
            height: 32,
        };
        let metadata = RawMetadata {
            make: "Test".into(),
            model: "Camera".into(),
            mode: RawMode::NikonZ6Lossless14,
            sensor_width: 32,
            sensor_height: 32,
            active_area: rect,
            default_crop: rect,
            cfa_width: 2,
            cfa_height: 2,
            cfa: vec![0, 1, 1, 2],
            black_cfa: vec![0, 1, 3, 2],
            black_base: 12.125,
            black_channels: [0.1, 0.2, 0.3, 0.4],
            black_repeat_width: 1,
            black_repeat_height: 1,
            black_repeat: vec![0.12345678],
            sensor_white: 16383.0,
            as_shot_gains: [1.2345678, 1.0, 1.8765432],
            libraw_flip: 0,
            rgb_cam: [[0.12345678; 4]; 3],
            cam_xyz: [[0.12345678; 3]; 4],
            backend: "pinned backend".into(),
            exif_orientation: 1,
            libraw_inset: Some(rect),
            format_identity: "test-format".into(),
            warnings: vec![],
            dng_corrections: None,
        };
        let fresh = SourceKind::Raw {
            metadata: serde_json::to_value(&metadata).unwrap(),
        };
        let stored: SourceKind =
            serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        assert!(source_kinds_equal(&stored, &fresh).unwrap());
        for field in ["backend", "default_crop", "cam_xyz"] {
            let mut changed = fresh.clone();
            if let SourceKind::Raw { metadata } = &mut changed {
                match field {
                    "backend" => metadata["backend"] = json!("other backend"),
                    "default_crop" => metadata["default_crop"]["x"] = json!(1),
                    "cam_xyz" => metadata["cam_xyz"][0][0] = json!(0.5),
                    _ => unreachable!(),
                }
            }
            assert!(!source_kinds_equal(&stored, &changed).unwrap(), "{field}");
        }
        let mut unknown = fresh;
        if let SourceKind::Raw { metadata } = &mut unknown {
            metadata["unexpected"] = json!(true);
        }
        assert!(source_kinds_equal(&stored, &unknown).is_err());
        if let SourceKind::Raw { metadata } = &stored {
            assert!(metadata.get("dng_corrections").is_none());
        }
        let mut dng = metadata.clone();
        dng.mode = RawMode::DjiAir2sDng16;
        dng.dng_corrections = Some(lightwell_raw::DngCorrectionMetadata {
            interpretation: "test-stage3-v1".into(),
            applied: [9_u32, 1]
                .map(|id| lightwell_raw::DngOpcodeProvenance {
                    list: 51022,
                    id,
                    version: 0x0103_0000,
                    flags: 0,
                    payload_sha256: format!("{id:064x}"),
                })
                .to_vec(),
            skipped_optional: vec![],
            calibration: lightwell_raw::DngCalibrationMetadata {
                illuminants: [17, 21],
                color_matrix1_sha256: "1".repeat(64),
                color_matrix2_sha256: "2".repeat(64),
                selected: "ColorMatrix2-D65-fixed-XYZ-to-camera".into(),
            },
        });
        let dng = SourceKind::Raw {
            metadata: serde_json::to_value(dng).unwrap(),
        };
        let dng_stored: SourceKind =
            serde_json::from_str(&serde_json::to_string(&dng).unwrap()).unwrap();
        assert!(source_kinds_equal(&dng_stored, &dng).unwrap());
        let mut changed_correction = dng.clone();
        if let SourceKind::Raw { metadata } = &mut changed_correction {
            metadata["dng_corrections"]["applied"][0]["payload_sha256"] = json!("changed");
        }
        assert!(!source_kinds_equal(&dng_stored, &changed_correction).unwrap());
        let mut missing_correction = dng.clone();
        if let SourceKind::Raw { metadata } = &mut missing_correction {
            metadata["dng_corrections"] = Value::Null;
        }
        assert_eq!(
            source_kinds_equal(&missing_correction, &dng)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible
        );
        let mut absent_correction = dng.clone();
        if let SourceKind::Raw { metadata } = &mut absent_correction {
            metadata.as_object_mut().unwrap().remove("dng_corrections");
        }
        assert_eq!(
            source_kinds_equal(&absent_correction, &dng)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible
        );
        assert!(!source_kinds_equal(&stored, &SourceKind::Jpeg).unwrap());

        let asset = AssetRecord {
            id: AssetId::new(),
            source_root: PathBuf::new(),
            locator: PathBuf::from("test.nef"),
            fingerprint: "test".into(),
            file_identity: "test".into(),
            byte_len: 1,
            width: 32,
            height: 32,
            source: stored,
        };
        let payload =
            crate::RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz).unwrap();
        let layer_id = LayerId::new();
        let snapshot = Snapshot::original(asset.id.clone())
            .with_layer_inserted(0, payload.layer(layer_id.clone()))
            .unwrap();
        validate_source_recipe(&asset, &snapshot.recipe).unwrap();
        let mut incompatible_asset = asset.clone();
        if let (
            SourceKind::Raw { metadata },
            SourceKind::Raw {
                metadata: dng_metadata,
            },
        ) = (&mut incompatible_asset.source, &dng)
        {
            metadata["dng_corrections"] = dng_metadata["dng_corrections"].clone();
        }
        assert_eq!(
            validate_source_recipe(&incompatible_asset, &snapshot.recipe)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible
        );
        for calibration in ["as_shot_gains", "cam_xyz"] {
            let mut corrupted = payload.clone();
            if calibration == "as_shot_gains" {
                corrupted.as_shot_gains[0] += 0.1;
            } else {
                corrupted.cam_xyz[0][0] += 0.1;
            }
            let mut recipe = snapshot.recipe.clone();
            recipe.layers[0] = corrupted.layer(layer_id.clone());
            assert_eq!(
                validate_source_recipe(&asset, &recipe).unwrap_err().kind,
                ErrorKind::Incompatible,
                "{calibration}"
            );
        }
    }

    /// Run with LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF or RAF.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn raw_import_wb_history_redevelopment_and_reopen_preserve_original() {
        let path = PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
        let original_hash = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
        let catalog = temp("raw-history.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let initial = service.import(&path).unwrap();
        assert_eq!(initial.asset.fingerprint, original_hash);
        assert!(matches!(initial.asset.source, SourceKind::Raw { .. }));
        assert_eq!(initial.current_entry.snapshot.recipe.layers.len(), 1);
        let source_layer = initial.current_entry.snapshot.recipe.layers[0].id.clone();
        let original = service
            .preview_job(&initial.asset.id, None, None, None, None)
            .unwrap();
        assert!(matches!(original.source, PreviewSource::Raw { .. }));
        let as_shot = raw_payload(&initial.current_entry.snapshot.recipe).unwrap();
        assert_eq!(as_shot.as_shot_gains, as_shot.gains);
        let exposure = service
            .apply_action(
                &initial.asset.id,
                mutation(0, "raw-exposure"),
                "set-raw-exposure",
                json!({"ev":1.5}),
            )
            .unwrap();
        let exposed = service.state(&initial.asset.id).unwrap();
        assert_eq!(
            exposed.current_entry.snapshot.recipe.layers[0].id,
            source_layer
        );
        assert_eq!(
            raw_payload(&exposed.current_entry.snapshot.recipe)
                .unwrap()
                .exposure_ev,
            1.5
        );
        let gain = (f64::from(as_shot.gains[0]) * 1.1).min(16.0);
        let changed = service
            .apply_action(
                &initial.asset.id,
                mutation(exposure.revision, "raw-red"),
                "set-raw-red-gain",
                json!({"gain":gain}),
            )
            .unwrap();
        let state = service.state(&initial.asset.id).unwrap();
        let payload = raw_payload(&state.current_entry.snapshot.recipe).unwrap();
        assert_eq!(
            state.current_entry.snapshot.recipe.layers[0].id,
            source_layer
        );
        assert_eq!(payload.wb_mode, crate::WhiteBalanceMode::Custom);
        assert_eq!(
            payload.gains[2], as_shot.gains[2],
            "partial custom edit retains camera blue gain"
        );
        assert_eq!(
            service
                .preview_job(&initial.asset.id, None, None, None, None)
                .unwrap_err()
                .kind,
            ErrorKind::PreparationRequired
        );
        let request = service
            .raw_development(&initial.asset.id, None)
            .unwrap()
            .unwrap();
        let developed = RawPrepared::develop(
            request.sensor.clone(),
            request.fingerprint.clone(),
            request.gains,
            &AtomicBool::new(false),
        )
        .unwrap();
        service.install_development(&request, developed).unwrap();
        assert!(matches!(
            service
                .preview_job(&initial.asset.id, None, None, None, None)
                .unwrap()
                .source,
            PreviewSource::Raw { .. }
        ));
        let undo = service
            .undo(&initial.asset.id, mutation(changed.revision, "undo-red"))
            .unwrap();
        assert_eq!(undo.outcome, MutationOutcome::Navigated);
        let state = service.state(&initial.asset.id).unwrap();
        assert_eq!(
            raw_payload(&state.current_entry.snapshot.recipe)
                .unwrap()
                .wb_mode,
            crate::WhiteBalanceMode::AsShot
        );
        drop(service);
        let reopened = EditorService::open(&catalog).unwrap();
        let state = reopened.state(&initial.asset.id).unwrap();
        assert_eq!(
            state.current_entry.snapshot.recipe.layers[0].id,
            source_layer
        );
        assert_eq!(
            reopened.inspect_source(&initial.asset.id, None).unwrap()["readiness"],
            "preparation-required"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap())),
            original_hash
        );
        drop(reopened);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn persistent_history_journey_retains_snapshots_navigation_and_source() {
        let catalog = temp("journey.sqlite");
        let source_path = fixture();
        let source_bytes = std::fs::read(&source_path).unwrap();
        let (asset, original, a, b);
        {
            let mut service = EditorService::open(&catalog).unwrap();
            let state = service.import(&source_path).unwrap();
            asset = state.asset.id.clone();
            original = state.current_entry.id.clone();
            let result = service
                .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
                .unwrap();
            a = result.current_entry_id;
            let result = service
                .apply_pixel(&asset, mutation(1, "b"), 0, 0, [4, 5, 6])
                .unwrap();
            b = result.current_entry_id;
            assert_eq!(
                service.render_current(&asset).unwrap().pixel(0, 0),
                Some([4, 5, 6, 255])
            );
            service.undo(&asset, mutation(2, "undo")).unwrap();
            assert_eq!(
                service.render_current(&asset).unwrap().pixel(0, 0),
                Some([1, 2, 3, 255])
            );
            service.redo(&asset, mutation(3, "redo")).unwrap();
            service.restore(&asset, mutation(4, "restore"), &a).unwrap();
            service
                .apply_pixel(&asset, mutation(5, "c"), 1, 0, [7, 8, 9])
                .unwrap();
            assert_eq!(service.history(&asset, None, 20).unwrap().entries.len(), 5);
        }
        let service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        assert_eq!(state.revision, 6);
        assert!(
            service
                .entry(&asset, &original)
                .unwrap()
                .snapshot
                .recipe
                .layers
                .is_empty()
        );
        assert_eq!(
            service.render_entry(&asset, &b).unwrap().pixel(0, 0),
            Some([4, 5, 6, 255])
        );
        assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn retries_noops_conflicts_pagination_and_wrong_assets_are_safe() {
        let catalog = temp("requests.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        let original_pixel = service.render_current(&asset).unwrap().pixel(0, 0).unwrap();
        let no_op = service
            .apply_pixel(
                &asset,
                mutation(0, "noop"),
                0,
                0,
                [original_pixel[0], original_pixel[1], original_pixel[2]],
            )
            .unwrap();
        assert_eq!(no_op.outcome, MutationOutcome::NoOp);
        let first = service
            .apply_pixel(&asset, mutation(0, "same"), 0, 0, [1, 2, 3])
            .unwrap();
        let retry = service
            .apply_pixel(&asset, mutation(0, "same"), 0, 0, [1, 2, 3])
            .unwrap();
        assert_eq!(first.current_entry_id, retry.current_entry_id);
        assert!(retry.deduplicated);
        assert!(
            service
                .apply_pixel(&asset, mutation(0, "same"), 0, 0, [9, 9, 9])
                .is_err()
        );
        assert_eq!(
            service
                .apply_pixel(&asset, mutation(0, "stale"), 1, 0, [9, 9, 9])
                .unwrap_err()
                .kind,
            ErrorKind::Conflict
        );
        assert_eq!(service.history(&asset, None, 1).unwrap().entries.len(), 1);
        assert!(service.history(&asset, None, 101).is_err());
        assert!(
            service
                .entry(&AssetId::new(), &first.current_entry_id)
                .is_err()
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn aliases_reuse_asset_but_copies_do_not_and_changed_sources_fail() {
        let dir = temp("aliases");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.jpg");
        let hard = dir.join("hard.jpg");
        let copy = dir.join("copy.jpg");
        std::fs::copy(fixture(), &source).unwrap();
        std::fs::hard_link(&source, &hard).unwrap();
        std::fs::copy(&source, &copy).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let a = service.import(&source).unwrap().asset.id;
        assert_eq!(service.import(&hard).unwrap().asset.id, a);
        assert_ne!(service.import(&copy).unwrap().asset.id, a);
        let listed: Vec<AssetId> = service
            .assets()
            .unwrap()
            .into_iter()
            .map(|asset| asset.id)
            .collect();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0], a);
        std::fs::write(&source, b"changed").unwrap();
        assert_eq!(
            service.render_current(&a).unwrap_err().kind,
            ErrorKind::SourceUnavailable
        );
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unchanged_sources_reuse_one_decoded_pixel_allocation() {
        let catalog = temp("source-cache.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let first = service
            .preview_job(&state.asset.id, None, None, None, None)
            .unwrap();
        let second = service
            .preview_job(&state.asset.id, None, None, None, None)
            .unwrap();
        let (PreviewSource::Jpeg(first_source), PreviewSource::Jpeg(second_source)) =
            (&first.source, &second.source)
        else {
            panic!("JPEG preview expected")
        };
        assert!(std::sync::Arc::ptr_eq(
            &first_source.rgba,
            &second_source.rgba
        ));
        let raster = render(
            service.registry(),
            first_source,
            first.entry.snapshot.id.clone(),
            &first.entry.snapshot.recipe,
        )
        .unwrap();
        assert!(std::sync::Arc::ptr_eq(&first_source.rgba, &raster.rgba));
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn same_length_source_replacement_invalidates_the_decode_cache() {
        let dir = temp("source-cache-invalidation");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.jpg");
        std::fs::copy(fixture(), &source).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        service.preview_job(&asset, None, None, None, None).unwrap();
        let replacement =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-2.jpg");
        assert_eq!(
            std::fs::metadata(&source).unwrap().len(),
            std::fs::metadata(&replacement).unwrap().len()
        );
        std::fs::copy(replacement, &source).unwrap();
        assert_eq!(
            service
                .preview_job(&asset, None, None, None, None)
                .unwrap_err()
                .kind,
            ErrorKind::SourceUnavailable
        );
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn versions_name_retained_entries_and_survive_reopen() {
        let catalog = temp("versions.sqlite");
        let (asset, a, original);
        {
            let mut service = EditorService::open(&catalog).unwrap();
            let state = service.import(&fixture()).unwrap();
            asset = state.asset.id.clone();
            original = state.current_entry.id.clone();
            a = service
                .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
                .unwrap()
                .current_entry_id;
            let created = service
                .create_version(&asset, " Keeper ", None, "test")
                .unwrap();
            assert_eq!(created.outcome, MutationOutcome::Applied);
            assert_eq!(created.version.as_ref().unwrap().name, "Keeper");
            assert_eq!(created.version.as_ref().unwrap().entry_id, a);
            let again = service
                .create_version(&asset, "keeper", Some(&a), "test")
                .unwrap();
            assert_eq!(again.outcome, MutationOutcome::NoOp);
            assert_eq!(
                service
                    .create_version(&asset, "KEEPER", Some(&original), "test")
                    .unwrap_err()
                    .kind,
                ErrorKind::Conflict
            );
            assert!(service.create_version(&asset, "", None, "test").is_err());
            assert!(
                service
                    .create_version(&asset, "bad\u{7}", None, "test")
                    .is_err()
            );
            service.undo(&asset, mutation(1, "undo")).unwrap();
            service
                .create_version(&asset, "Start", None, "test")
                .unwrap();
            let names: Vec<(String, u64)> = service
                .versions(&asset)
                .unwrap()
                .into_iter()
                .map(|version| (version.name, version.entry_sequence))
                .collect();
            assert_eq!(names, [("Keeper".to_string(), 1), ("Start".to_string(), 0)]);
        }
        let mut service = EditorService::open(&catalog).unwrap();
        let versions = service.versions(&asset).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].entry_id, original);
        service
            .restore(&asset, mutation(2, "restore-keeper"), &versions[0].entry_id)
            .unwrap();
        assert_eq!(
            service.render_current(&asset).unwrap().pixel(0, 0),
            Some([1, 2, 3, 255])
        );
        assert_eq!(
            service.delete_version(&asset, "keeper").unwrap().outcome,
            MutationOutcome::Applied
        );
        assert_eq!(
            service.delete_version(&asset, "keeper").unwrap().outcome,
            MutationOutcome::NoOp
        );
        assert_eq!(service.versions(&asset).unwrap().len(), 1);
        assert!(
            service.entry(&asset, &a).is_ok(),
            "deleting a version keeps its entry"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn lineage_follows_undo_parents_and_skips_abandoned_branches() {
        let catalog = temp("lineage.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        let original = state.current_entry.id;
        let a = service
            .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
            .unwrap()
            .current_entry_id;
        let b = service
            .apply_pixel(&asset, mutation(1, "b"), 1, 0, [4, 5, 6])
            .unwrap()
            .current_entry_id;
        service.undo(&asset, mutation(2, "undo")).unwrap();
        let c = service
            .apply_pixel(&asset, mutation(3, "c"), 2, 0, [7, 8, 9])
            .unwrap()
            .current_entry_id;
        let ids = |lineage: Lineage| -> Vec<EntryId> {
            lineage
                .steps
                .into_iter()
                .map(|step| step.entry_id)
                .collect()
        };
        assert_eq!(
            ids(service.lineage(&asset, None, 10).unwrap()),
            [c.clone(), a.clone(), original.clone()]
        );
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 4);
        assert_eq!(
            ids(service.lineage(&asset, Some(&b), 10).unwrap()),
            [b.clone(), a.clone(), original.clone()]
        );
        let page = service.lineage(&asset, None, 2).unwrap();
        assert_eq!(page.steps.len(), 2);
        assert_eq!(page.next_entry_id, Some(original.clone()));
        assert_eq!(
            ids(service
                .lineage(&asset, page.next_entry_id.as_ref(), 2)
                .unwrap()),
            [original]
        );
        assert!(service.lineage(&asset, None, 0).is_err());
        assert!(service.lineage(&AssetId::new(), Some(&c), 5).is_err());
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn transform_composition_is_persistent_and_exact() {
        let catalog = temp("transform.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        service
            .apply_transform(&asset, mutation(1, "right"), Transform::RotateRight)
            .unwrap();
        let state = service.state(&asset).unwrap();
        assert_eq!(
            (
                service.render_current(&asset).unwrap().width,
                service.render_current(&asset).unwrap().height
            ),
            (320, 480)
        );
        assert_eq!(state.current_entry.snapshot.recipe.layers.len(), 2);
        service.undo(&asset, mutation(2, "undo-transform")).unwrap();
        assert_eq!(
            (
                service.render_current(&asset).unwrap().width,
                service.render_current(&asset).unwrap().height
            ),
            (480, 320)
        );
        drop(service);
        let service = EditorService::open(&catalog).unwrap();
        assert_eq!(service.state(&asset).unwrap().revision, 3);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Four Rotate right actions are four history entries and one neutral orientation layer whose
    /// render is the source itself. Every entry keeps its own stack, so undo walks back through
    /// three, two and one quarter turn with the dimensions and pixels each of them produced.
    #[test]
    fn four_quarter_turns_leave_one_neutral_orientation_layer_and_four_entries() {
        let catalog = temp("orientation-collapse.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap();
        assert_eq!((original.width, original.height), (480, 320));

        let mut turned = Vec::new();
        let mut layer_id = None;
        for turn in 0..4u64 {
            service
                .apply_transform(
                    &asset,
                    mutation(turn, &format!("turn-{turn}")),
                    Transform::RotateRight,
                )
                .unwrap();
            let stack = service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers;
            assert_eq!(stack.len(), 1, "turn {turn} keeps one orientation layer");
            assert_eq!(stack[0].effect_id, ORIENTATION_EFFECT);
            assert_eq!(
                stack[0].payload,
                json!({"mirror": false, "turns": (turn + 1) % 4}),
                "turn {turn}"
            );
            match &layer_id {
                None => layer_id = Some(stack[0].id.clone()),
                Some(id) => assert_eq!(&stack[0].id, id, "turn {turn} updates the same layer"),
            }
            turned.push(service.render_current(&asset).unwrap());
        }
        let entries = service.history(&asset, None, 50).unwrap().entries;
        assert_eq!(entries.len(), 5, "Original and one entry per action");
        for entry in entries.iter().take(4) {
            assert_eq!(entry.action_id, "rotate-right");
        }
        let neutral = &turned[3];
        assert_eq!((neutral.width, neutral.height), (480, 320));
        assert_eq!(neutral.rgba, original.rgba, "four turns render the source");

        // Undo walks back through the three, two and one turn states.
        for (step, back) in [(0usize, 2usize), (1, 1), (2, 0)] {
            let at = service.state(&asset).unwrap().revision;
            service
                .undo(&asset, mutation(at, &format!("undo-{step}")))
                .unwrap();
            let state = service.state(&asset).unwrap();
            assert_eq!(state.current_entry.snapshot.recipe.layers.len(), 1);
            assert_eq!(
                state.current_entry.snapshot.recipe.layers[0].payload,
                json!({"mirror": false, "turns": back + 1})
            );
            let raster = service.render_current(&asset).unwrap();
            assert_eq!(
                (raster.width, raster.height),
                (turned[back].width, turned[back].height),
                "undo {step}"
            );
            assert_eq!(raster.rgba, turned[back].rgba, "undo {step}");
        }
        let at = service.state(&asset).unwrap().revision;
        service.undo(&asset, mutation(at, "undo-3")).unwrap();
        assert!(
            service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
                .is_empty(),
            "undoing the first turn returns to the original empty stack"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The planning rule at the service: a transform composes into the last layer of the stack and
    /// otherwise appends. A crop after an orientation layer ends the tail, so the next transform
    /// starts a second one; a pixel layer never does, because the host puts it before the tail.
    #[test]
    fn a_transform_updates_the_stacks_last_orientation_layer_and_otherwise_appends() {
        let catalog = temp("orientation-placement.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let revision = |service: &EditorService| service.state(&asset).unwrap().revision;
        let layers = |service: &EditorService| -> Vec<Layer> {
            service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
        };

        // A pixel layer is not a geometry layer, so the first transform appends the tail.
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "first"), Transform::RotateRight)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, ORIENTATION_EFFECT]
        );
        let first_orientation = stack[1].id.clone();

        // A further pixel edit joins the stack before the tail, so the tail is still last and the
        // next transform composes into it.
        let at = revision(&service);
        service
            .apply_pixel(&asset, mutation(at, "pixel-2"), 1, 0, [4, 5, 6])
            .unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "second"), Transform::MirrorHorizontal)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, PIXEL_EFFECT, ORIENTATION_EFFECT]
        );
        assert_eq!(stack[2].id, first_orientation, "the same layer, in place");
        assert_eq!(stack[2].payload, json!({"mirror": true, "turns": 3}));

        // A crop appends after the orientation layer, so the next transform starts a second one:
        // order stays observable and the later quarter turn carries the visible crop.
        let at = revision(&service);
        service
            .apply_action(
                &asset,
                mutation(at, "crop"),
                "crop",
                json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
            )
            .unwrap();
        let cropped = service.render_current(&asset).unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "third"), Transform::RotateRight)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [
                PIXEL_EFFECT,
                PIXEL_EFFECT,
                ORIENTATION_EFFECT,
                CROP_EFFECT,
                ORIENTATION_EFFECT
            ],
            "two orientation layers around the crop, by design"
        );
        assert_eq!(stack[2].id, first_orientation, "the first one is untouched");
        assert_eq!(stack[2].payload, json!({"mirror": true, "turns": 3}));
        let second_orientation = stack[4].id.clone();
        assert_eq!(stack[4].payload, json!({"mirror": false, "turns": 1}));
        // The quarter turn after the crop carries the visible crop and swaps its ratio.
        let turned = service.render_current(&asset).unwrap();
        assert_eq!(
            (turned.width, turned.height),
            (cropped.height, cropped.width)
        );

        // And a further transform composes into that appended layer, not the first one.
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "fourth"), Transform::FlipVertical)
            .unwrap();
        let stack = layers(&service);
        assert_eq!(stack.len(), 5);
        assert_eq!(stack[4].id, second_orientation, "the same layer, in place");
        assert_eq!(stack[4].payload, json!({"mirror": true, "turns": 1}));
        assert_eq!(stack[2].id, first_orientation);
        assert_eq!(stack[2].payload, json!({"mirror": true, "turns": 3}));
        // The reflection acts on the crop's output stage, so it carries the off-center composition
        // with the image instead of re-cutting it: every row of the turned crop, bottom to top.
        let flipped = service.render_current(&asset).unwrap();
        assert_eq!(
            (flipped.width, flipped.height),
            (turned.width, turned.height)
        );
        let row = turned.width as usize * 4;
        for y in 0..turned.height as usize {
            let from = (turned.height as usize - 1 - y) * row;
            assert_eq!(
                &flipped.rgba[y * row..y * row + row],
                &turned.rgba[from..from + row],
                "row {y}"
            );
        }
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn competing_catalog_owners_are_rejected() {
        let catalog = temp("owner.sqlite");
        let owner = EditorService::open(&catalog).unwrap();
        assert_eq!(
            EditorService::open(&catalog).unwrap_err().kind,
            ErrorKind::Conflict
        );
        drop(owner);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn unsupported_catalog_formats_are_rejected_without_rewriting_data() {
        for marker in [0, CATALOG_FORMAT - 1, CATALOG_FORMAT + 1] {
            let catalog = temp("unsupported-format.sqlite");
            let mut service = EditorService::open(&catalog).unwrap();
            service.import(&fixture()).unwrap();
            drop(service);
            let connection = Connection::open(&catalog).unwrap();
            connection
                .pragma_update(None, "user_version", marker)
                .unwrap();
            drop(connection);
            let before = std::fs::read(&catalog).unwrap();
            let error = EditorService::open(&catalog).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert!(error.detail.contains("choose a new catalog path"));
            assert_eq!(std::fs::read(&catalog).unwrap(), before);
            std::fs::remove_file(catalog).unwrap();
        }
    }

    #[test]
    fn a_format_2_catalog_is_refused_by_name_without_rewriting_it() {
        let catalog = temp("format-2.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        service.import(&fixture()).unwrap();
        drop(service);
        // Entries before format 3 carry no label, so the marker refuses them rather than guessing.
        let connection = Connection::open(&catalog).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);
        let before = std::fs::read(&catalog).unwrap();
        let error = EditorService::open(&catalog).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            "catalog format 2 is not supported; expected 5; choose a new catalog path"
        );
        assert_eq!(
            std::fs::read(&catalog).unwrap(),
            before,
            "a refused catalog is left byte for byte as it was"
        );
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_format_4_catalog_is_refused_by_name_and_left_untouched() {
        let catalog = temp("format-4.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 1, 1, [9, 8, 7])
            .unwrap();
        drop(service);
        // A format 4 catalog is this schema without the catalog identity and the artifact tables.
        let connection = Connection::open(&catalog).unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER artifact_refs_are_permanent;
                 DROP TABLE artifact_refs;
                 DROP TABLE artifacts;
                 DROP TABLE catalog_meta;
                 PRAGMA user_version=4;",
            )
            .unwrap();
        drop(connection);
        let before = std::fs::read(&catalog).unwrap();
        let error = EditorService::open(&catalog).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            "catalog format 4 is not supported; expected 5; choose a new catalog path"
        );
        assert_eq!(
            std::fs::read(&catalog).unwrap(),
            before,
            "the refused catalog keeps every byte, and no artifact directory is created"
        );
        let stem = catalog.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(!catalog.with_file_name(format!("{stem}.artifacts")).exists());
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn every_entry_carries_the_label_its_history_row_shows() {
        let catalog = temp("labels.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        assert_eq!(state.current_entry.label, "Original");
        let label =
            |service: &EditorService, entry: &EntryId| service.entry(&asset, entry).unwrap().label;
        let pixel = service
            .apply_pixel(&asset, mutation(0, "pixel"), 3, 4, [1, 2, 3])
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &pixel), "Pixel 3, 4");
        // The requested action renders the label; the entry stores the durable transform identity.
        let rotated = service
            .apply_transform(&asset, mutation(1, "rotate"), Transform::RotateLeft)
            .unwrap()
            .current_entry_id;
        let entry = service.entry(&asset, &rotated).unwrap();
        assert_eq!(
            (entry.action_id.as_str(), entry.label.as_str()),
            ("rotate-left", "Rotate left")
        );
        let cropped = service
            .apply_action(
                &asset,
                mutation(2, "crop"),
                "crop",
                json!({"angle":3.5,"x":0.1,"y":0.1,"width":0.5,"height":0.5}),
            )
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &cropped), "Crop 3.5°");
        let fitted = service
            .apply_action(
                &asset,
                mutation(3, "fit"),
                "crop-fit",
                json!({"aspect":"16:9"}),
            )
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &fitted), "Crop 16:9");
        // An action without a template is labelled by its title.
        let reset = service
            .apply_action(&asset, mutation(4, "reset"), "crop-reset", json!({}))
            .unwrap()
            .current_entry_id;
        assert_eq!(label(&service, &reset), "Reset crop");
        let restored = service
            .restore(&asset, mutation(5, "restore"), &pixel)
            .unwrap()
            .current_entry_id;
        assert_eq!(
            label(&service, &restored),
            "Restore entry 1",
            "a restore names the sequence it copied"
        );
        // Labels are stored with the entries, so reopening reads the same rows.
        drop(service);
        let service = EditorService::open(&catalog).unwrap();
        let labels: Vec<String> = service
            .history(&asset, None, 50)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.label)
            .collect();
        assert_eq!(
            labels,
            [
                "Restore entry 1",
                "Reset crop",
                "Crop 16:9",
                "Crop 3.5°",
                "Rotate left",
                "Pixel 3, 4",
                "Original",
            ]
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn an_entrys_layers_are_described_in_order_with_their_provider() {
        let catalog = temp("describe.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.state(&asset).unwrap().current_entry.id;
        assert_eq!(
            service.describe_entry(&asset, None).unwrap(),
            RecipeDescription {
                entry_id: original.clone(),
                layers: Vec::new(),
            },
            "the original entry has no layers"
        );
        service
            .apply_action(
                &asset,
                mutation(0, "crop"),
                "crop",
                json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
            )
            .unwrap();
        service
            .apply_pixel(&asset, mutation(1, "pixel"), 1, 2, [4, 5, 6])
            .unwrap();
        // Two quarter turns after the crop: one orientation layer at the end of the tail, whose row
        // names the orientation it holds rather than the two actions that reached it.
        for (revision, request) in [(2, "turn-a"), (3, "turn-b")] {
            service
                .apply_action(
                    &asset,
                    mutation(revision, request),
                    "transform",
                    json!({"transform":"rotate-right"}),
                )
                .unwrap();
        }
        let described = service.describe_entry(&asset, None).unwrap();
        // The pixel layer is in the content stage, so it sits before the geometry tail however
        // late it was committed; the crop and the orientation keep their own order behind it.
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| (
                    layer.module.as_deref(),
                    layer.title.as_deref(),
                    layer.summary.as_str(),
                    layer.available
                ))
                .collect::<Vec<_>>(),
            [
                (
                    Some("lightwell.pixel"),
                    Some("Pixel"),
                    "Pixel 1, 2 → 4,5,6",
                    true
                ),
                (
                    Some("lightwell.crop"),
                    Some("Crop and straighten"),
                    "50% × 50%",
                    true
                ),
                (
                    Some("lightwell.transform"),
                    Some("Transforms"),
                    "Rotate 180°",
                    true
                ),
            ]
        );
        let current = service.state(&asset).unwrap().current_entry;
        assert_eq!(described.entry_id, current.id);
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect::<Vec<_>>(),
            current
                .snapshot
                .recipe
                .layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect::<Vec<_>>(),
            "the stored order and identities"
        );
        // An earlier entry describes its own stack.
        assert!(
            service
                .describe_entry(&asset, Some(&original))
                .unwrap()
                .layers
                .is_empty()
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn failed_entry_write_rolls_back_snapshot_state_and_request() {
        let catalog = temp("rollback.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let before = service.state(&asset).unwrap();
        service
            .connection
            .execute_batch(
                "CREATE TRIGGER injected_entry_failure BEFORE INSERT ON entries
                 WHEN NEW.sequence > 0 BEGIN SELECT RAISE(ABORT, 'injected'); END;",
            )
            .unwrap();
        assert!(
            service
                .apply_pixel(&asset, mutation(0, "will-fail"), 0, 0, [1, 2, 3])
                .is_err()
        );
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 1);
        service
            .connection
            .execute_batch("DROP TRIGGER injected_entry_failure")
            .unwrap();
        assert!(
            service
                .apply_pixel(&asset, mutation(0, "will-fail"), 0, 0, [1, 2, 3])
                .is_ok(),
            "a failed transaction must not retain a false request result"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn rejected_actions_leave_state_history_and_the_request_table_untouched() {
        let catalog = temp("actions.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let before = service.state(&asset).unwrap();
        for (case, action, parameters, fragment) in [
            ("unknown action", "paint", json!({}), "unknown action paint"),
            (
                "missing required parameter",
                "set-pixel",
                json!({"x":0,"y":0}),
                "missing required parameter rgb",
            ),
            (
                "unknown parameter",
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[1,2,3],"z":1}),
                "unknown parameter z",
            ),
            (
                "integer out of range",
                "set-pixel",
                json!({"x":-1,"y":0,"rgb":[1,2,3]}),
                "parameter x must be an integer within 0..=16383",
            ),
            (
                "malformed color",
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[1,2]}),
                "parameter rgb must be three sRGB channels 0..=255",
            ),
            (
                "unknown enum option",
                "transform",
                json!({"transform":"rotate-sideways"}),
                "parameter transform must be one of",
            ),
            (
                "outside the content stage",
                "set-pixel",
                json!({"x":9000,"y":0,"rgb":[1,2,3]}),
                "pixel (9000, 0) is outside the 480x320 content stage",
            ),
        ] {
            let error = service
                .apply_action(&asset, mutation(0, case), action, parameters)
                .expect_err(case);
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(fragment), "{case}: {error}");
        }
        assert_eq!(service.state(&asset).unwrap(), before);
        let count = |table: &str| -> i64 {
            service
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(count("entries"), 1, "no history row was written");
        assert_eq!(count("requests"), 0, "no request result was recorded");
        assert_eq!(before.revision, 0);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn actions_wrappers_and_no_op_detection_agree() {
        let catalog = temp("action-equivalence.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap().pixel(0, 0).unwrap();
        let repeated = service
            .apply_action(
                &asset,
                mutation(0, "same-value"),
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[original[0],original[1],original[2]]}),
            )
            .unwrap();
        assert_eq!(repeated.outcome, MutationOutcome::NoOp);
        let through_action = service
            .apply_action(
                &asset,
                mutation(0, "action"),
                "set-pixel",
                json!({"x":0,"y":0,"rgb":[1,2,3]}),
            )
            .unwrap();
        let entry = service
            .entry(&asset, &through_action.current_entry_id)
            .unwrap();
        assert_eq!(entry.action_id, "set-pixel");
        assert_eq!(entry.parameters, json!({"x":0,"y":0,"rgb":[1,2,3]}));
        let rotated = service
            .apply_action(
                &asset,
                mutation(1, "rotate"),
                "transform",
                json!({"transform":"rotate-right"}),
            )
            .unwrap();
        let entry = service.entry(&asset, &rotated.current_entry_id).unwrap();
        assert_eq!(entry.action_id, "rotate-right");
        assert_eq!(entry.parameters, json!({"transform":"rotate-right"}));
        // The action keeps its durable identity; the stack records the orientation it reached.
        assert_eq!(
            entry.snapshot.recipe.layers[1].effect_id,
            ORIENTATION_EFFECT
        );
        assert_eq!(
            entry.snapshot.recipe.layers[1].payload,
            json!({"mirror":false,"turns":1})
        );
        // The wrapper retries the same request and deduplicates through the same hash.
        let retry = service
            .apply_transform(&asset, mutation(1, "rotate"), Transform::RotateRight)
            .unwrap();
        assert!(retry.deduplicated);
        assert_eq!(retry.current_entry_id, rotated.current_entry_id);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    const SHRINK_EFFECT: &str = "test.geometry.shrink";
    const SHRINK_TAIL_EFFECT: &str = "test.geometry.tail";
    const SHRINK_ACTION: &str = "test-shrink";
    const TAIL_ACTION: &str = "test-shrink-tail";
    const MISSING_ACTION: &str = "test-shrink-missing";

    /// A test-only geometry module that proves the host's in-place update path: `test-shrink`
    /// updates its own layer when the stack already has one and appends one otherwise,
    /// `test-shrink-tail` always commits a second geometry layer, which the tail carries after the
    /// first one, and `test-shrink-missing` plans an update for an identity that is not in the
    /// stack.
    struct ShrinkModule(ModuleDescriptor);

    impl ShrinkModule {
        fn new() -> Self {
            let extent = |name: &str| ParameterDescriptor {
                name: name.into(),
                kind: ParameterKind::Integer { min: 1, max: 16383 },
                required: true,
                default: None,
                unit: Some("px".into()),
                step: None,
                precision: None,
                notes: "test".into(),
                soft_min: None,
                soft_max: None,
                fine_step: None,
                zero: None,
            };
            let action = |id: &str| ActionDescriptor {
                id: id.into(),
                title: "Shrink".into(),
                notes: "test".into(),
                summary: Some("Shrink {width}x{height}".into()),
                patch: false,
                parameters: vec![extent("width"), extent("height")],
            };
            let effect = |id: &str| EffectDescriptor {
                id: id.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Geometry,
                order: 0,
                artifacts: false,
            };
            Self(ModuleDescriptor {
                id: "test.shrink".into(),
                title: "Shrink".into(),
                hint: None,
                effects: vec![effect(SHRINK_EFFECT), effect(SHRINK_TAIL_EFFECT)],
                actions: vec![
                    action(SHRINK_ACTION),
                    action(TAIL_ACTION),
                    action(MISSING_ACTION),
                ],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            })
        }

        fn layer(id: LayerId, width: u32, height: u32) -> Layer {
            Self::layer_of(SHRINK_EFFECT, id, width, height)
        }

        fn layer_of(effect_id: &str, id: LayerId, width: u32, height: u32) -> Layer {
            Layer {
                id,
                effect_id: effect_id.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"width": width, "height": height}),
                artifacts: Vec::new(),
            }
        }

        fn extents(value: &Value) -> Result<(u32, u32), Error> {
            let read = |name: &str| {
                value
                    .get(name)
                    .and_then(Value::as_u64)
                    .filter(|extent| (1..=16383).contains(extent))
                    .map(|extent| extent as u32)
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::Validation,
                            format!("parameter {name} must be an integer within 1..=16383"),
                        )
                    })
            };
            Ok((read("width")?, read("height")?))
        }

        /// The registry the update tests use: the built-ins plus this module.
        fn registry() -> Arc<ModuleRegistry> {
            let mut registry = ModuleRegistry::builtin();
            registry.register(Arc::new(Self::new())).unwrap();
            Arc::new(registry)
        }
    }

    impl ToolModule for ShrinkModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }

        fn parse(
            &self,
            action_id: &str,
            parameters: &Map<String, Value>,
        ) -> Result<ActionInput, Error> {
            let (width, height) = Self::extents(&Value::Object(parameters.clone()))?;
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: json!({"width": width, "height": height})
                    .as_object()
                    .expect("an object")
                    .clone(),
            })
        }

        fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
            let (width, height) = Self::extents(&Value::Object(input.parameters.clone()))?;
            if input.action_id == MISSING_ACTION {
                return Ok(ActionPlan::Update(Self::layer(
                    LayerId::new(),
                    width,
                    height,
                )));
            }
            if input.action_id == TAIL_ACTION {
                return Ok(ActionPlan::Commit(Self::layer_of(
                    SHRINK_TAIL_EFFECT,
                    LayerId::new(),
                    width,
                    height,
                )));
            }
            match stage
                .layers
                .iter()
                .find(|layer| layer.effect_id == SHRINK_EFFECT)
            {
                Some(existing) => Ok(ActionPlan::Update(Self::layer(
                    existing.id.clone(),
                    width,
                    height,
                ))),
                None => Ok(ActionPlan::Commit(Self::layer(
                    LayerId::new(),
                    width,
                    height,
                ))),
            }
        }

        fn validate_payload(&self, _: &str, _: u32, payload: &Value) -> Result<(), Error> {
            Self::extents(payload).map(|_| ())
        }

        fn describe_layer(&self, _: &str, _: u32, payload: &Value) -> Result<String, Error> {
            let (width, height) = Self::extents(payload)?;
            Ok(format!("Shrink to {width}x{height}"))
        }

        fn compile(
            &self,
            _: &str,
            _: u32,
            payload: &Value,
            stage: Stage,
        ) -> Result<Processing, Error> {
            let (width, height) = Self::extents(payload)?;
            if width > stage.width || height > stage.height {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!(
                        "shrink {width}x{height} is larger than the {}x{} input stage",
                        stage.width, stage.height
                    ),
                ));
            }
            Ok(Processing::ExactGeometry(ExactGeometry {
                a: 1,
                b: 0,
                c: 0,
                d: 1,
                tx: 0,
                ty: 0,
                output_width: width,
                output_height: height,
            }))
        }
    }

    fn shrink(width: u32, height: u32) -> Value {
        json!({"width": width, "height": height})
    }

    fn rows(service: &EditorService, table: &str) -> i64 {
        service
            .connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn an_update_replaces_its_layer_in_place_and_leaves_earlier_snapshots_alone() {
        let catalog = temp("update.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let appended = service
            .apply_action(
                &asset,
                mutation(0, "append"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        let first = service.entry(&asset, &appended.current_entry_id).unwrap();
        let layer_id = first.snapshot.recipe.layers[0].id.clone();
        // A pixel layer addresses the content stage, so the host puts it before the geometry tail
        // and the shrink layer keeps its own identity and position after it.
        service
            .apply_pixel(&asset, mutation(1, "pixel"), 10, 10, [1, 2, 3])
            .unwrap();
        let updated = service
            .apply_action(&asset, mutation(2, "update"), SHRINK_ACTION, shrink(50, 50))
            .unwrap();
        assert_eq!(updated.outcome, MutationOutcome::Applied);
        let entry = service.entry(&asset, &updated.current_entry_id).unwrap();
        assert_eq!(
            entry.snapshot.recipe.layers.len(),
            2,
            "an update adds no layer"
        );
        assert_eq!(
            entry.snapshot.recipe.layers[0].effect_id, PIXEL_EFFECT,
            "the pixel layer stays before the geometry tail"
        );
        assert_eq!(
            entry.snapshot.recipe.layers[1].id, layer_id,
            "the updated layer keeps its identity and position"
        );
        assert_eq!(entry.snapshot.recipe.layers[1].payload, shrink(50, 50));
        assert_ne!(
            entry.snapshot.id, first.snapshot.id,
            "an update produces a new snapshot identity"
        );
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 4);
        assert_eq!(
            service
                .entry(&asset, &first.id)
                .unwrap()
                .snapshot
                .recipe
                .layers[0]
                .payload,
            shrink(100, 100),
            "the earlier entry keeps its own stack"
        );
        let raster = service.render_current(&asset).unwrap();
        assert_eq!((raster.width, raster.height), (50, 50));
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn an_update_that_invalidates_a_later_layer_is_rejected_atomically() {
        let catalog = temp("update-invalid.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "append"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        // A second geometry layer follows the first and addresses the stage it produces.
        service
            .apply_action(&asset, mutation(1, "tail"), TAIL_ACTION, shrink(90, 90))
            .unwrap();
        let before = service.state(&asset).unwrap();
        let (entries, requests) = (rows(&service, "entries"), rows(&service, "requests"));
        let error = service
            .apply_action(
                &asset,
                mutation(2, "shrink-too-far"),
                SHRINK_ACTION,
                shrink(50, 50),
            )
            .expect_err("the tail layer would fall outside the new stage");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("shrink 90x90 is larger than the 50x50 input stage"),
            "{error}"
        );
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_eq!(
            rows(&service, "entries"),
            entries,
            "no history row was written"
        );
        assert_eq!(
            rows(&service, "requests"),
            requests,
            "no request result was recorded"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A stored stack whose finish layer precedes a geometry layer is refused by planning exactly
    /// as it is by compilation: the request fails with the named validation error, no history row
    /// and no request result are written, and the stack stays readable through `state`.
    ///
    /// The host never builds that order, so the stack is planted the only way it can arise: a
    /// registry in which the effect is a colour effect writes it, and a registry in which the same
    /// effect is a finish effect reads it back.
    #[test]
    fn a_stored_finish_layer_before_geometry_is_refused_without_being_rewritten() {
        use crate::modules::{STAGE_ACTION, STAGE_EFFECT, StageModule};
        let registry = |stage: EffectStage| {
            let mut registry = ModuleRegistry::builtin();
            registry
                .register(StageModule::shared(
                    "test.stage",
                    STAGE_EFFECT,
                    STAGE_ACTION,
                    stage,
                    0,
                ))
                .unwrap();
            Arc::new(registry)
        };
        let catalog = temp("finish-before-geometry.sqlite");
        let mut service = EditorService::open_with(&catalog, registry(EffectStage::Color)).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(&asset, mutation(0, "stage"), STAGE_ACTION, json!({}))
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "turn"),
                "transform",
                json!({"transform":"rotate-left"}),
            )
            .unwrap();
        let written = service.state(&asset).unwrap();
        assert_eq!(
            written
                .current_entry
                .snapshot
                .recipe
                .layers
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            vec![STAGE_EFFECT, ORIENTATION_EFFECT],
        );
        drop(service);

        let mut service =
            EditorService::open_with(&catalog, registry(EffectStage::Finish)).unwrap();
        let before = service.state(&asset).unwrap();
        let (entries, requests) = (rows(&service, "entries"), rows(&service, "requests"));
        let error = service
            .apply_pixel(&asset, mutation(2, "pixel"), 0, 0, [1, 2, 3])
            .expect_err("the stored order has no stage a new layer could address");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error.detail.starts_with("finish layer precedes geometry"),
            "{}",
            error.detail
        );
        assert_eq!(
            service.state(&asset).unwrap(),
            before,
            "the refused stack is left exactly as it stands"
        );
        assert_eq!(rows(&service, "entries"), entries, "no history row");
        assert_eq!(rows(&service, "requests"), requests, "no request result");
        assert_eq!(
            before.current_entry.snapshot.recipe.layers.len(),
            2,
            "both layers stay readable"
        );
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 3);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn an_update_naming_a_layer_that_is_not_in_the_stack_is_rejected() {
        let catalog = temp("update-missing.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let before = service.state(&asset).unwrap();
        let error = service
            .apply_action(
                &asset,
                mutation(0, "missing"),
                MISSING_ACTION,
                shrink(10, 10),
            )
            .expect_err("the planned identity is not in the stack");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "plan updates a layer that is not in the stack"
        );
        assert_eq!(service.state(&asset).unwrap(), before);
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 1);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_truncated_preview_job_renders_the_layer_prefix_and_rejects_an_out_of_range_count() {
        let catalog = temp("truncated-preview.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.render_current(&asset).unwrap().pixel(0, 0).unwrap();
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 0, 0, [1, 2, 3])
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "shrink"),
                SHRINK_ACTION,
                shrink(100, 100),
            )
            .unwrap();
        let rendered = |service: &EditorService, layer_count: Option<usize>| -> Raster {
            let job = service
                .preview_job(&asset, None, layer_count, None, None)
                .unwrap();
            let mut queue = PreviewQueue::default();
            queue.request(job);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = queue.poll() {
                    return result.result.unwrap();
                }
                assert!(Instant::now() < deadline, "the preview worker answered");
                std::thread::yield_now();
            }
        };
        let full = rendered(&service, None);
        assert_eq!((full.width, full.height), (100, 100));
        let prefix = rendered(&service, Some(1));
        assert_eq!(
            (prefix.width, prefix.height),
            (480, 320),
            "one layer renders the crop's input stage"
        );
        assert_eq!(prefix.pixel(0, 0), Some([1, 2, 3, 255]));
        let none = rendered(&service, Some(0));
        assert_eq!((none.width, none.height), (480, 320));
        assert_eq!(none.pixel(0, 0), Some(original), "no layer, no edit");
        assert_ne!(none.pixel(0, 0), prefix.pixel(0, 0));
        let error = service
            .preview_job(&asset, None, Some(3), None, None)
            .expect_err("an out-of-range layer count");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(
            error
                .detail
                .contains("preview layer count 3 exceeds the 2 layers"),
            "{error}"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn undoing_restore_returns_to_the_preceding_current_entry() {
        let catalog = temp("restore-undo.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let asset = state.asset.id;
        let a = service
            .apply_pixel(&asset, mutation(0, "a"), 0, 0, [1, 2, 3])
            .unwrap()
            .current_entry_id;
        let b = service
            .apply_pixel(&asset, mutation(1, "b"), 0, 0, [4, 5, 6])
            .unwrap()
            .current_entry_id;
        service
            .restore(&asset, mutation(2, "restore-a"), &a)
            .unwrap();
        service.undo(&asset, mutation(3, "undo-restore")).unwrap();
        assert_eq!(service.state(&asset).unwrap().current_entry.id, b);
        assert_eq!(
            service.render_current(&asset).unwrap().pixel(0, 0),
            Some([4, 5, 6, 255])
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The 480x320 fixture's crop journey: an exact copy at angle zero, in-place updates that keep
    /// one layer and one entry per commit, a resampled angled crop, composition with a later
    /// quarter turn and a content-stage pixel before the tail, reset, navigation and reopen.
    #[test]
    fn the_crop_journey_keeps_exact_pixels_one_layer_and_every_snapshot() {
        let catalog = temp("crop-journey.sqlite");
        let source_path = fixture();
        let source_bytes = std::fs::read(&source_path).unwrap();
        let (asset, layer_id, first_entry, angled_entry, before_reopen);
        {
            let mut service = EditorService::open(&catalog).unwrap();
            let state = service.import(&source_path).unwrap();
            asset = state.asset.id.clone();
            let original = service.render_current(&asset).unwrap();
            assert_eq!((original.width, original.height), (480, 320));
            let revision = |service: &EditorService| service.state(&asset).unwrap().revision;
            let layers = |service: &EditorService| -> Vec<Layer> {
                service
                    .state(&asset)
                    .unwrap()
                    .current_entry
                    .snapshot
                    .recipe
                    .layers
            };

            // An angle-zero crop copies the source rectangle byte for byte.
            let at = revision(&service);
            let first = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-a"),
                    "crop",
                    json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
                )
                .unwrap();
            assert_eq!(first.outcome, MutationOutcome::Applied);
            first_entry = first.current_entry_id.clone();
            let entry = service.entry(&asset, &first_entry).unwrap();
            assert_eq!(entry.action_id, "crop");
            assert_eq!(
                entry.parameters,
                json!({"angle":0.0,"x":0.25,"y":0.25,"width":0.5,"height":0.5})
            );
            assert_eq!(entry.snapshot.recipe.layers.len(), 1);
            layer_id = entry.snapshot.recipe.layers[0].id.clone();
            assert_eq!(entry.snapshot.recipe.layers[0].effect_id, CROP_EFFECT);
            let cropped = service.render_current(&asset).unwrap();
            assert_eq!((cropped.width, cropped.height), (240, 160));
            let row_bytes = 240 * 4;
            for y in 0..160usize {
                let start = (80 + y) * 480 * 4 + 120 * 4;
                assert_eq!(
                    &cropped.rgba[y * row_bytes..(y + 1) * row_bytes],
                    &original.rgba[start..start + row_bytes],
                    "crop row {y} is not an exact copy of the source"
                );
            }

            // A second rectangle updates the same layer; the earlier snapshot keeps its own stack.
            let at = revision(&service);
            let second = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-b"),
                    "crop",
                    json!({"x":0.5,"y":0.0,"width":0.5,"height":0.5}),
                )
                .unwrap();
            let updated = service.entry(&asset, &second.current_entry_id).unwrap();
            assert_eq!(
                updated.snapshot.recipe.layers.len(),
                1,
                "an update adds no layer"
            );
            assert_eq!(updated.snapshot.recipe.layers[0].id, layer_id);
            assert_ne!(updated.snapshot.id, entry.snapshot.id, "a new snapshot");
            assert_eq!(
                service.entry(&asset, &first_entry).unwrap(),
                entry,
                "the earlier entry and snapshot are untouched"
            );
            assert_eq!(
                service.history(&asset, None, 50).unwrap().entries.len(),
                3,
                "one entry per commit, plus the import's original"
            );
            let moved = service.render_current(&asset).unwrap();
            assert_eq!((moved.width, moved.height), (240, 160));
            for y in 0..160usize {
                let start = y * 480 * 4 + 240 * 4;
                assert_eq!(
                    &moved.rgba[y * row_bytes..(y + 1) * row_bytes],
                    &original.rgba[start..start + row_bytes],
                    "moved crop row {y}"
                );
            }
            // The same rectangle again is a reported no-op with no history row.
            let at = revision(&service);
            let repeated = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-b-again"),
                    "crop",
                    json!({"x":0.5,"y":0.0,"width":0.5,"height":0.5}),
                )
                .unwrap();
            assert_eq!(repeated.outcome, MutationOutcome::NoOp);
            assert_eq!(service.history(&asset, None, 50).unwrap().entries.len(), 3);

            // An angled crop resamples; the rendered stage is exactly the payload's output rect.
            let angled_payload = CropPayload {
                angle: 5.0,
                x: 0.2,
                y: 0.2,
                width: 0.6,
                height: 0.6,
            };
            let expected = angled_payload
                .output_rect(&CropStage {
                    width: 480,
                    height: 320,
                    angle: 5.0,
                })
                .expect("the angled rectangle is covered");
            let at = revision(&service);
            angled_entry = service
                .apply_action(
                    &asset,
                    mutation(at, "crop-angled"),
                    "crop",
                    json!({"angle":5.0,"x":0.2,"y":0.2,"width":0.6,"height":0.6}),
                )
                .unwrap()
                .current_entry_id;
            let raster = service.render_current(&asset).unwrap();
            assert_eq!(
                (raster.width, raster.height),
                (expected.width, expected.height)
            );
            assert_eq!(layers(&service)[0].id, layer_id);

            // A quarter turn after the crop swaps the dimensions and stays after the crop layer.
            let at = revision(&service);
            service
                .apply_transform(&asset, mutation(at, "rotate"), Transform::RotateRight)
                .unwrap();
            let rotated = service.render_current(&asset).unwrap();
            assert_eq!(
                (rotated.width, rotated.height),
                (expected.height, expected.width)
            );
            let stack = layers(&service);
            assert_eq!(stack.len(), 2);
            assert_eq!(stack[0].effect_id, CROP_EFFECT);
            assert_eq!(stack[1].effect_id, ORIENTATION_EFFECT);

            // Re-cropping after the quarter turn updates in place; the transform still follows.
            let at = revision(&service);
            service
                .apply_action(
                    &asset,
                    mutation(at, "crop-c"),
                    "crop",
                    json!({"x":0.1,"y":0.1,"width":0.5,"height":0.5}),
                )
                .unwrap();
            let stack = layers(&service);
            assert_eq!(stack.len(), 2);
            assert_eq!(stack[0].id, layer_id, "the crop layer keeps its identity");
            assert_eq!(
                stack[1].effect_id, ORIENTATION_EFFECT,
                "the transform still follows the crop"
            );
            let recropped = service.render_current(&asset).unwrap();
            assert_eq!((recropped.width, recropped.height), (160, 240));

            // A pixel edit addresses the content stage, so the host puts it before the crop and a
            // rectangle that no longer covers it is accepted instead of rejected.
            let at = revision(&service);
            service
                .apply_pixel(&asset, mutation(at, "pixel"), 150, 230, [1, 2, 3])
                .unwrap();
            let stack = layers(&service);
            assert_eq!(stack.len(), 3);
            assert_eq!(
                stack[0].effect_id, PIXEL_EFFECT,
                "the pixel layer joins the stack before the geometry tail"
            );
            assert_eq!(stack[1].id, layer_id, "the crop layer keeps its position");
            assert_eq!(stack[2].effect_id, ORIENTATION_EFFECT);
            let at = revision(&service);
            assert_eq!(
                service
                    .apply_action(
                        &asset,
                        mutation(at, "crop-smaller"),
                        "crop",
                        json!({"x":0.1,"y":0.1,"width":0.25,"height":0.25}),
                    )
                    .unwrap()
                    .outcome,
                MutationOutcome::Applied,
                "a smaller rectangle is never rejected because of a content-stage pixel"
            );

            // Reset returns the crop layer's output to its own input stage.
            let at = revision(&service);
            let reset = service
                .apply_action(&asset, mutation(at, "crop-reset"), "crop-reset", json!({}))
                .unwrap();
            assert_eq!(reset.outcome, MutationOutcome::Applied);
            let stack = layers(&service);
            assert_eq!(stack[1].id, layer_id);
            assert_eq!(
                stack[1].payload,
                json!({"angle":0.0,"x":0.0,"y":0.0,"width":1.0,"height":1.0})
            );
            let full = service.render_current(&asset).unwrap();
            assert_eq!(
                (full.width, full.height),
                (320, 480),
                "the whole input stage, turned by the later quarter turn"
            );
            let at = revision(&service);
            assert_eq!(
                service
                    .apply_action(
                        &asset,
                        mutation(at, "crop-reset-again"),
                        "crop-reset",
                        json!({})
                    )
                    .unwrap()
                    .outcome,
                MutationOutcome::NoOp,
                "an already neutral crop layer"
            );

            // Undo, redo and restore keep every entry and every snapshot.
            let recorded = service.history(&asset, None, 50).unwrap().entries;
            let reset_entry = reset.current_entry_id.clone();
            let at = revision(&service);
            service.undo(&asset, mutation(at, "undo")).unwrap();
            assert_ne!(service.state(&asset).unwrap().current_entry.id, reset_entry);
            let at = revision(&service);
            service.redo(&asset, mutation(at, "redo")).unwrap();
            assert_eq!(service.state(&asset).unwrap().current_entry.id, reset_entry);
            let at = revision(&service);
            let restored = service
                .restore(&asset, mutation(at, "restore-first"), &first_entry)
                .unwrap();
            assert_eq!(
                service
                    .entry(&asset, &restored.current_entry_id)
                    .unwrap()
                    .snapshot
                    .recipe,
                service.entry(&asset, &first_entry).unwrap().snapshot.recipe,
                "restore copies the first crop's stack"
            );
            for entry in &recorded {
                assert_eq!(
                    &service.entry(&asset, &entry.id).unwrap(),
                    entry,
                    "entry {} and its snapshot are unchanged",
                    entry.id
                );
            }
            before_reopen = service.render_current(&asset).unwrap();
            assert_eq!(
                (before_reopen.width, before_reopen.height),
                (240, 160),
                "the restored first crop"
            );
        }

        let service = EditorService::open(&catalog).unwrap();
        let stack = service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .layers;
        assert_eq!(stack.len(), 1);
        assert_eq!(
            stack[0].id, layer_id,
            "the crop layer's identity survives reopen"
        );
        let reopened = service.render_current(&asset).unwrap();
        assert_eq!(reopened.rgba, before_reopen.rgba, "identical pixels");
        let angled = service.render_entry(&asset, &angled_entry).unwrap();
        assert!(
            angled.width < 480 && angled.height < 320,
            "the angled entry still renders its trimmed stage"
        );
        assert_eq!(
            std::fs::read(&source_path).unwrap(),
            source_bytes,
            "source unchanged"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A pixel edit addresses the content stage: the source after EXIF orientation. The host puts
    /// it before the geometry tail, so moving, growing and shrinking the crop never moves the
    /// edit, is never rejected because of it, and a rectangle that hides it keeps it.
    #[test]
    fn a_pixel_edit_holds_its_content_pixel_through_every_crop_change() {
        let catalog = temp("content-stage.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let revision = |service: &EditorService| service.state(&asset).unwrap().revision;
        let layers = |service: &EditorService| -> Vec<Layer> {
            service
                .state(&asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
        };

        // The tail: an angle-zero rectangle at (48, 32) of the 480x320 source, then a quarter turn.
        service
            .apply_action(
                &asset,
                mutation(0, "crop"),
                "crop",
                json!({"x":0.1,"y":0.1,"width":0.5,"height":0.5}),
            )
            .unwrap();
        let at = revision(&service);
        service
            .apply_transform(&asset, mutation(at, "rotate"), Transform::RotateRight)
            .unwrap();
        let before = service.render_current(&asset).unwrap();
        assert_eq!((before.width, before.height), (160, 240));

        // The edit joins the stack before the tail and shows where its content pixel is drawn.
        let at = revision(&service);
        service
            .apply_pixel(&asset, mutation(at, "visible"), 150, 100, [1, 2, 3])
            .unwrap();
        let stack = layers(&service);
        assert_eq!(
            stack
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, CROP_EFFECT, ORIENTATION_EFFECT]
        );
        let edited = service.render_current(&asset).unwrap();
        // Content (150, 100) less the crop origin is (102, 68) of a 240x160 stage, turned right.
        let shown = (160 - 1 - 68, 102);
        assert_eq!(edited.pixel(shown.0, shown.1), Some([1, 2, 3, 255]));
        for y in 0..edited.height {
            for x in 0..edited.width {
                if (x, y) != shown {
                    assert_eq!(edited.pixel(x, y), before.pixel(x, y), "({x}, {y})");
                }
            }
        }

        // A content pixel the crop does not cover is accepted and simply not drawn.
        let at = revision(&service);
        let hidden = service
            .apply_pixel(&asset, mutation(at, "hidden"), 150, 230, [4, 5, 6])
            .unwrap();
        assert_eq!(hidden.outcome, MutationOutcome::Applied);
        assert_eq!(layers(&service).len(), 4);
        assert_eq!(
            service.render_current(&asset).unwrap().rgba,
            edited.rgba,
            "an edit outside the cropped output changes no rendered pixel"
        );

        // A coordinate outside the content stage is a validation error naming that stage.
        let at = revision(&service);
        let error = service
            .apply_pixel(&asset, mutation(at, "outside"), 500, 10, [7, 8, 9])
            .expect_err("500 is outside the 480 pixel wide content stage");
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            "pixel (500, 10) is outside the 480x320 content stage"
        );

        // Replacing a content pixel with the value the content already holds is a no-op.
        let at = revision(&service);
        assert_eq!(
            service
                .apply_pixel(&asset, mutation(at, "same"), 150, 100, [1, 2, 3])
                .unwrap()
                .outcome,
            MutationOutcome::NoOp
        );

        // Moving and growing the crop keep the same content pixel edited.
        for (request, rectangle, origin, stage) in [
            (
                "crop-moved",
                json!({"x":0.3,"y":0.1,"width":0.5,"height":0.5}),
                (144u32, 32u32),
                (240u32, 160u32),
            ),
            (
                "crop-grown",
                json!({"x":0.0,"y":0.0,"width":1.0,"height":1.0}),
                (0, 0),
                (480, 320),
            ),
        ] {
            let at = revision(&service);
            let result = service
                .apply_action(&asset, mutation(at, request), "crop", rectangle)
                .unwrap();
            assert_eq!(result.outcome, MutationOutcome::Applied, "{request}");
            let raster = service.render_current(&asset).unwrap();
            assert_eq!(
                (raster.width, raster.height),
                (stage.1, stage.0),
                "{request}"
            );
            let (x, y) = (150 - origin.0, 100 - origin.1);
            assert_eq!(
                raster.pixel(stage.1 - 1 - y, x),
                Some([1, 2, 3, 255]),
                "{request}"
            );
        }

        // A rectangle that hides the edit is accepted, and growing it back shows it again.
        let at = revision(&service);
        let shrunk = service
            .apply_action(
                &asset,
                mutation(at, "crop-shrunk"),
                "crop",
                json!({"x":0.0,"y":0.0,"width":0.1,"height":0.1}),
            )
            .unwrap();
        assert_eq!(
            shrunk.outcome,
            MutationOutcome::Applied,
            "a shrink that hides the pixel is accepted"
        );
        let hidden = service.render_current(&asset).unwrap();
        assert_eq!((hidden.width, hidden.height), (32, 48));
        assert!(
            hidden
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel[..3] != [1, 2, 3]),
            "the hidden edit draws nothing"
        );
        let at = revision(&service);
        service
            .apply_action(
                &asset,
                mutation(at, "crop-regrown"),
                "crop",
                json!({"x":0.0,"y":0.0,"width":1.0,"height":1.0}),
            )
            .unwrap();
        assert_eq!(
            service.render_current(&asset).unwrap().pixel(219, 150),
            Some([1, 2, 3, 255]),
            "the edit was hidden, not lost"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Through a straightened crop the resample carries the content edit: the rendered frame
    /// matches the point sampler everywhere, the edit shows as one small cluster of blended output
    /// pixels, and replacing the content pixel with its content value is still a reported no-op
    /// although the output shows a different value there.
    #[test]
    fn a_content_edit_under_a_straightened_crop_matches_the_reference_sampler() {
        let catalog = temp("content-angled.sqlite");
        let source_path = fixture();
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&source_path).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 150, 100, [1, 2, 3])
            .unwrap();
        let at = service.state(&asset).unwrap().revision;
        service
            .apply_action(
                &asset,
                mutation(at, "crop-angled"),
                "crop",
                json!({"angle":5.0,"x":0.2,"y":0.2,"width":0.6,"height":0.6}),
            )
            .unwrap();
        let recipe = service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .clone();
        assert_eq!(
            recipe
                .layers
                .iter()
                .map(|layer| layer.effect_id.as_str())
                .collect::<Vec<_>>(),
            [PIXEL_EFFECT, CROP_EFFECT],
            "the edit stays before the crop that resamples it"
        );
        let raster = service.render_current(&asset).unwrap();
        let registry = ModuleRegistry::builtin();
        let source = open_source(&source_path).unwrap();

        // The rendered frame and the point sampler evaluate the same stack by different paths.
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    crate::sample(&registry, &source, &recipe, x, y)
                        .unwrap()
                        .rgba,
                    raster.pixel(x, y),
                    "({x}, {y})"
                );
            }
        }

        // Against the same crop without the edit, the difference is one small cluster.
        let mut plain = recipe.clone();
        plain.layers.retain(|layer| layer.effect_id != PIXEL_EFFECT);
        let unedited = render(&registry, &source, SnapshotId::new(), &plain).unwrap();
        assert_eq!(
            (unedited.width, unedited.height),
            (raster.width, raster.height)
        );
        let differing: Vec<(u32, u32)> = (0..raster.height)
            .flat_map(|y| (0..raster.width).map(move |x| (x, y)))
            .filter(|(x, y)| raster.pixel(*x, *y) != unedited.pixel(*x, *y))
            .collect();
        assert!(
            !differing.is_empty(),
            "the resample carries the content edit into the output"
        );
        let span = |axis: fn(&(u32, u32)) -> u32| {
            differing.iter().map(axis).max().unwrap() - differing.iter().map(axis).min().unwrap()
        };
        assert!(
            span(|point| point.0) <= 1 && span(|point| point.1) <= 1,
            "one content pixel blends into its own neighbourhood: {differing:?}"
        );
        assert!(
            differing
                .iter()
                .all(|(x, y)| raster.pixel(*x, *y) != Some([1, 2, 3, 255])),
            "the output shows the resampled blend, not the stored value"
        );

        // The no-op is decided in the content stage, not against what the output shows.
        let at = service.state(&asset).unwrap().revision;
        assert_eq!(
            service
                .apply_pixel(&asset, mutation(at, "same"), 150, 100, [1, 2, 3])
                .unwrap()
                .outcome,
            MutationOutcome::NoOp
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
