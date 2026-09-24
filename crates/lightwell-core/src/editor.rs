use crate::{
    AssetId, ContentPoint, Draft, DraftId, EntryId, Error, ErrorKind, HistoryEntry, Layer, LayerId,
    MaskId, ModuleRegistry, Mutation, PreviewJob, PreviewSource, ProxyBounds, Raster, Recipe,
    Snapshot, SnapshotId, StageTransform, Transform,
    analysis::AnalysisIdentity,
    artifacts::{ArtifactId, LiveArtifacts, PreparedArtifact, PreparedArtifacts},
    mask::commands::{
        MaskChange, MaskCommand, MaskCommandResult, MaskListing, MaskOutcome, MaskTarget,
    },
    modules::{
        ActionInput, ActionPlan, EffectStage, MAX_COMPOSE_STEPS, Stage, StageContext, action_label,
        check_parameters,
    },
    open_source_bytes, read_bounded_file, render,
    render::{Evaluation, locate_dimensions, stage_transform},
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

/// Format 7 is the merged shape. It holds the mask table a recipe carries and the layer's mask
/// reference, the content-addressed stroke store a painted path is kept in — so no catalog ever
/// holds embedded stroke points — the preset library, and the catalog's own identity with the
/// derived-artifact tables. Two branches each claimed format **6** for one half of that, the masks
/// and strokes on one and the catalog identity and artifact tables on the other, exactly as two
/// earlier branches each claimed format 5; the merged shape is neither, so a catalog written by
/// either is refused by name rather than read as the other and is left byte for byte as it was.
/// Format 4 made entry records the only stored copy of a stack and format 3 stored each entry's
/// rendered label. Every other marker, earlier or later, is refused by name and left as it is;
/// choose a new catalog path.
const CATALOG_FORMAT: i64 = 7;
const MAX_HISTORY_PAGE: usize = 100;
const MAX_VERSION_NAME: usize = 64;
const ASSET_COLUMNS: &str =
    "id,source_root,locator,fingerprint,file_identity,byte_len,width,height,source_json";

pub(crate) fn catalog_error(error: rusqlite::Error) -> Error {
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

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|e| json_error("cannot encode catalog value", e))
}

pub(crate) fn decode<T: DeserializeOwned>(context: &str, value: String) -> Result<T, Error> {
    serde_json::from_str(&value).map_err(|e| json_error(context, e))
}

pub(crate) fn now_ms() -> i64 {
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

/// One **input** pixel of a masked operation, which is what a mask's value-based parts are evaluated
/// on, in linear sRGB, with the stage it was read from.
///
/// It is not a [`PixelSample`] and must not be confused with one: that is the *output* the picture
/// shows, this is the input the operation a mask modulates receives. The two are different colours
/// wherever the operation does anything at all, which is exactly why a client cannot read this one
/// off the frame. `r`, `g` and `b` are top-level numbers because that is what a canvas pick submits
/// to `mask.add-<kind>-sample`, whose own parameters carry those names.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelInput {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub x: u32,
    pub y: u32,
    /// The stage the masked layer receives, which is the stage `x` and `y` address.
    pub width: u32,
    pub height: u32,
}

/// One 8-bit RGBA sample as the linear-sRGB triple a mask's value-based parts evaluate on, through the
/// delivered decode and nothing else, so one definition of "linear sRGB" serves the whole editor.
fn linear_triple(rgba: [u8; 4]) -> [f64; 3] {
    let linear = crate::render::decode_pixel([rgba[0], rgba[1], rgba[2]]);
    [
        f64::from(linear[0]),
        f64::from(linear[1]),
        f64::from(linear[2]),
    ]
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
    /// The mask this layer is modulated by, when it carries one. The recipe is the durable order of
    /// processing, so a client reading it must be able to tell a masked layer from a global one
    /// without asking a second question; `mask.list` answers the same relation from the other side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskId>,
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
    /// The original's file name, which the activity board shows beside the development; the
    /// development itself reads only the retained sensor data.
    pub(crate) file_name: Option<String>,
}

#[derive(Debug)]
struct CachedSource {
    asset_id: AssetId,
    signature: SourceSignature,
    source: PreparedSource,
}

#[derive(Debug)]
pub struct EditorService {
    /// The catalog. The preset library in `presets::library` keeps its own table here.
    pub(crate) connection: Connection,
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
            None => default_artifact_root(path),
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
                 CREATE TABLE presets (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL COLLATE NOCASE,
                    group_name TEXT NOT NULL COLLATE NOCASE,
                    record_json TEXT NOT NULL,
                    source_text TEXT,
                    UNIQUE(group_name, name)
                 );
                 CREATE TABLE strokes (
                    id TEXT PRIMARY KEY,
                    stroke_json TEXT NOT NULL
                 );
                 CREATE TRIGGER strokes_are_immutable BEFORE UPDATE ON strokes BEGIN
                    SELECT RAISE(ABORT, 'stored strokes are immutable');
                 END;
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
            file_name: state
                .asset
                .locator
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
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
                    mask: layer.mask.clone(),
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
                        mask: layer.mask.clone(),
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
        // exposure previews the value the gesture holds rather than the committed one. A drafted
        // temperature or tint the developed planes do not hold is approximated on them, and only
        // here: this is the one evaluation that may, because its frame is a gesture's preview and
        // is labelled so, never analysed and replaced by the exact frame once the release
        // redevelops. A preview without a draft is strict, as every other evaluation is.
        let mode = if draft.is_some() {
            RawSettingsMode::DraftPreview
        } else {
            RawSettingsMode::Strict
        };
        let source = self.preview_source(&state.asset, &recipe, mode)?;
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
            // A coverage grid is asked for by the client that will draw it, through
            // `PreviewJob::with_mask_overlay`, which validates it against this stack.
            mask_overlay: None,
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
    /// reports `preparation-required` rather than rendering a stale development, except under
    /// [`RawSettingsMode::DraftPreview`], where the settings approximate it and the source says so
    /// ([`PreviewSource::approximate_white_balance`]).
    fn preview_source(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        mode: RawSettingsMode,
    ) -> Result<PreviewSource, Error> {
        match self.verified_prepared(asset)? {
            PreparedSource::Jpeg(image) => {
                validate_source_recipe(asset, recipe)?;
                Ok(PreviewSource::Jpeg(image))
            }
            PreparedSource::Raw(raw) => {
                let settings = raw_settings(&raw, recipe, mode)?;
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
        // An analysis is a number, so it is never taken from an approximate white balance.
        let source = match failure {
            Some(_) => None,
            None => Some(self.preview_source(&state.asset, &recipe, RawSettingsMode::Strict)?),
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
                let settings = raw_settings(&raw, &entry.snapshot.recipe, RawSettingsMode::Strict)?;
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
        let source = self.preview_source(
            &state.asset,
            &entry.snapshot.recipe,
            RawSettingsMode::Strict,
        )?;
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
        // Samples are numbers sent to a provider, so a white balance the planes do not hold is
        // `preparation-required` here, as it is for `render.sample`.
        let source = self.preview_source(&state.asset, &recipe, RawSettingsMode::Strict)?;
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
        // A sampled code is a number, so a drafted white balance the planes do not hold is
        // `preparation-required` here even while the draft's preview approximates it.
        let source = self.preview_source(&state.asset, &recipe, RawSettingsMode::Strict)?;
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

    /// The content-to-output affine of a saved entry's geometry tail, both ways. `locate_entry`
    /// answers one point; this answers all of them at once, so a gesture over the photograph maps
    /// pointer positions itself instead of asking per move. Like `locate_entry` it reads the compiled
    /// stack only and rasterizes nothing.
    pub fn transform_entry(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
    ) -> Result<StageTransform, Error> {
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        stage_transform(
            &self.registry,
            state.asset.width,
            state.asset.height,
            &entry.snapshot.recipe,
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
        // An unavailable provider keeps its descriptor so its stored layers stay readable, but it
        // changes nothing. A module with effects would be refused by the whole-stack compile at
        // commit anyway; one with none, such as presets, would otherwise commit through it.
        if !module.descriptor().is_available() {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!("unavailable module {}", module.descriptor().id),
            ));
        }
        // The host's one optional target field, taken before the action's own parameters are checked
        // so the module receives exactly its declared fields and never learns a mask was involved.
        let mut parameters = parameters;
        let mask = take_mask_target(&registry, action_id, &mut parameters)?;
        let checked = check_parameters(action, &parameters)?;
        let input = module.parse(action_id, &checked)?;
        // The module labels a request its template cannot describe, such as a field patch; the
        // fallback comes from the action that was requested, which is not always the durable action
        // identity the entry stores: `transform` renders the label, `rotate-left` is stored.
        let label = module
            .label(&input)
            .unwrap_or_else(|| action_label(action, &input.parameters));
        let request = request_input(&input, &mutation, mask.as_ref())?;
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &request)? {
            return Ok(result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let recipe = &state.current_entry.snapshot.recipe;
        validate_source_recipe(&state.asset, recipe)?;
        // Both planning paths compile the current stack, so its artifacts are bound first.
        let _artifacts = self.require_artifacts(recipe)?;
        // A target the stack does not hold is refused here, before a module plans anything.
        resolve_mask_target(recipe, mask.as_ref())?;
        // A masked module edit always names its mask, where a `mask.*` command names one only once
        // the stack holds more than one. The difference is not an inconsistency but the ambiguity
        // each one actually has: `Update Linear 1` is unmistakable while a recipe holds one mask,
        // but `Exposure +2.00 EV` is exactly what this module's *global* edit writes, so a single
        // mask is already enough for a history row to show two entries nothing distinguishes.
        let label = match mask
            .as_ref()
            .and_then(|id| recipe.masks.iter().find(|mask| &mask.id == id))
        {
            Some(mask) => format!("{} · {label}", mask.name),
            None => label,
        };
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
                    .compile_layers(
                        width,
                        height,
                        prefix(&recipe.layers, index)?,
                        &recipe.masks,
                        &recipe.strokes,
                    )?
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
                ActionPlan::Commit(_) | ActionPlan::Compose(_) => {
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
        let plan = self.plan_input(
            &state,
            &source,
            module,
            &input,
            registry.action_accepts_mask(action_id),
            mask.as_ref(),
        )?;
        let Some(recipe) = self.resolve_plan(&source, recipe, plan, mask.as_ref())? else {
            return self.persist_noop(asset_id, &mutation, &request, &state);
        };
        let snapshot = state.current_entry.snapshot.with_recipe(recipe)?;
        self.commit_snapshot(
            asset_id,
            mutation,
            request,
            snapshot,
            &state.asset,
            CommittedAction { input, label },
        )
    }

    /// One `mask.*` host command.
    ///
    /// Masks are host commands in their own namespace and not a tool module, because a module commits
    /// layers through [`ActionPlan`] and must never rewrite the recipe, while every one of these
    /// rewrites the mask table beside the layers. Everything else is the delivered path and not a
    /// second implementation of it: the parameters go through the same [`check_parameters`], the
    /// request identity and its deduplication are built by the same [`request_input`], the revision
    /// is checked by the same [`ensure_revision`], a change is persisted by the same
    /// [`Self::commit_snapshot`] — one history entry, one immutable snapshot, one validated and
    /// compiled recipe — and a change that changes nothing takes the same [`Self::persist_noop`].
    pub fn apply_mask_command(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        command: &'static MaskCommand,
        parameters: Value,
        target: MaskTarget,
    ) -> Result<MaskCommandResult, Error> {
        mutation.validate()?;
        command.checked_target(&target)?;
        let checked = check_parameters(&command.action, &parameters)?;
        // The entry stores the declared parameters and the envelope fields naming what they addressed,
        // which is also the deduplication identity: the same request id with a different mask is a
        // different request and must conflict rather than return the first one's result.
        let input = ActionInput {
            action_id: command.method.to_owned(),
            parameters: crate::mask::commands::stored_parameters(&checked, &target),
        };
        // No separate mask argument: a mask command's target is already one of the stored
        // parameters above, so it is hashed with them. The field a module action passes here names
        // the *layer* an edit addressed, which is a different question a mask command never asks.
        let request = request_input(&input, &mutation, None)?;
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &request)? {
            return self.mask_report(asset_id, result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let recipe = &state.current_entry.snapshot.recipe;
        validate_source_recipe(&state.asset, recipe)?;
        let registry = self.registry.clone();
        // A stroke that asks to be limited to a colour is seeded here, by the host, from the pixel
        // the operation this mask modulates receives at the position the stroke began. The request
        // named the limit and never the colour, so nothing a client sends can put a colour in a
        // stroke that the photograph does not have at that position, and the planner below stays
        // pure — it is handed the pixel rather than reading one.
        let seed = self.mask_colour_seed(&state, command, recipe, &target, &checked)?;
        match crate::mask::commands::plan(command, recipe, &target, &checked, &registry, seed)? {
            MaskOutcome::NoOp => Ok(MaskCommandResult::plain(
                self.persist_noop(asset_id, &mutation, &request, &state)?,
            )),
            MaskOutcome::Change(MaskChange {
                recipe,
                label,
                mask,
                component,
                removed_layers,
            }) => {
                let snapshot = Snapshot {
                    id: SnapshotId::new(),
                    asset_id: asset_id.clone(),
                    recipe,
                };
                let mutation = self.commit_snapshot(
                    asset_id,
                    mutation,
                    request,
                    snapshot,
                    &state.asset,
                    CommittedAction {
                        input,
                        label: label.clone(),
                    },
                )?;
                Ok(MaskCommandResult {
                    mutation,
                    label: Some(label),
                    mask,
                    component,
                    removed_layers,
                })
            }
        }
    }

    /// The report of a deduplicated retry, read back from the entry the original call wrote so the
    /// retry answers identically. A retried no-op wrote no entry and reports the envelope alone,
    /// exactly as the no-op itself did.
    fn mask_report(
        &self,
        asset_id: &AssetId,
        result: MutationResult,
    ) -> Result<MaskCommandResult, Error> {
        let Some(entry_id) = result.created_entry_id.clone() else {
            return Ok(MaskCommandResult::plain(result));
        };
        let entry = self.entry(asset_id, &entry_id)?;
        let parent = match &entry.undo_parent {
            Some(parent) => Some(self.entry(asset_id, parent)?),
            None => None,
        };
        Ok(crate::mask::commands::report_of(
            result,
            &entry,
            parent.as_ref(),
            &self.registry,
        ))
    }

    /// The pixel a limited stroke is seeded on, as the three sRGB codes the host sampled, or `None`
    /// when the command asks for no limit.
    ///
    /// The **rule** — which layer's input, and what a mask no layer is bound to means — is the
    /// command family's, stated once in `mask::commands::input_layer_index`; the **pixel** is read
    /// here, because the editor is the only thing that can evaluate one. That split is what makes a
    /// stored seed a colour the photograph has: a request carries a flag and a path, never a colour,
    /// so no client can put anything else in a stroke.
    fn mask_colour_seed(
        &self,
        state: &EditorState,
        command: &MaskCommand,
        recipe: &Recipe,
        target: &MaskTarget,
        parameters: &Map<String, Value>,
    ) -> Result<Option<[u8; 3]>, Error> {
        let Some(request) =
            crate::mask::commands::colour_limit_request(command, recipe, target, parameters)?
        else {
            return Ok(None);
        };
        let source = self.verified_prepared(&state.asset)?;
        self.with_stage_context(&source, recipe, |context| {
            let stage = (context.stage_before)(request.layer)?;
            // The stroke's positions are normalized against the stage its mask is compiled against,
            // which is the stage this layer receives, so the pixel is that stage's own. A stroke that
            // began outside the picture — an ordinary gesture, which the stored range allows — has no
            // input pixel to read and is refused by name rather than clamped to an edge whose colour
            // nobody chose.
            let pixel = |value: f64, side: u32| -> Option<u32> {
                let index = (value * f64::from(side)).floor();
                (index >= 0.0 && index < f64::from(side)).then_some(index as u32)
            };
            let outside = || {
                Error::new(
                    ErrorKind::Validation,
                    format!(
                        "a stroke limited to a colour must begin inside the picture, and \
                         ({:.4}, {:.4}) is outside the {}x{} stage the masked layer receives",
                        request.x, request.y, stage.width, stage.height
                    ),
                )
            };
            let x = pixel(request.x, stage.width).ok_or_else(outside)?;
            let y = pixel(request.y, stage.height).ok_or_else(outside)?;
            let rgba = (context.sample_before)(request.layer, x, y)?.ok_or_else(outside)?;
            // The codes, not the decoded colour: a stroke is addressed by the hash of its bytes, and
            // an integer survives a JSON round trip exactly where an `f64` does not. Decoding is the
            // delivered one and happens where the stroke is compiled.
            Ok(Some([rgba[0], rgba[1], rgba[2]]))
        })
    }

    /// `mask.sample-input`: the pixel the operation one mask modulates receives, at one content
    /// position of the stage that operation's layer receives, in linear sRGB.
    ///
    /// Read-only: it reads one entry's snapshot, writes nothing, emits nothing and touches no session
    /// state. It is where a canvas pick gets the colour a colour range's swatch is, and it exists so
    /// a client never has to decode one: the frame a client can see holds the masked operation's
    /// *output*, and a range selection is evaluated on its *input*, so a colour read from the picture
    /// would be a different colour. Cost is one `O(layers)` point evaluation and no frame is
    /// allocated.
    pub fn mask_input_sample(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        mask: &MaskId,
        x: u32,
        y: u32,
    ) -> Result<PixelInput, Error> {
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let asset = &state.asset;
        let recipe = &entry.snapshot.recipe;
        validate_source_recipe(asset, recipe)?;
        let layer = crate::mask::commands::input_layer_index(recipe, mask)?;
        let source = self.verified_prepared(asset)?;
        self.with_stage_context(&source, recipe, |context| {
            let stage = (context.stage_before)(layer)?;
            if x >= stage.width || y >= stage.height {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!(
                        "outside the stage: ({x}, {y}) is not inside the {}x{} stage the masked \
                         layer receives",
                        stage.width, stage.height
                    ),
                ));
            }
            let rgba = (context.sample_before)(layer, x, y)?.ok_or_else(|| {
                Error::new(
                    ErrorKind::Validation,
                    format!("outside the stage: ({x}, {y}) has no pixel to read"),
                )
            })?;
            let [r, g, b] = linear_triple(rgba);
            Ok(PixelInput {
                r,
                g,
                b,
                x,
                y,
                width: stage.width,
                height: stage.height,
            })
        })
    }

    /// `mask.list`: every mask of one stored stack with its components, its values and the layers
    /// bound to it. Read-only in every sense — it reads one entry's snapshot and writes nothing,
    /// emits nothing and touches no session state.
    pub fn mask_listing(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
    ) -> Result<MaskListing, Error> {
        let entry = self.entry(asset_id, entry_id)?;
        Ok(crate::mask::commands::listing(
            entry.id.clone(),
            &entry.snapshot.recipe,
            &self.registry,
        ))
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
        accepts_mask: bool,
        mask: Option<&MaskId>,
    ) -> Result<ActionPlan, Error> {
        // The module plans against the stack of one target: the global layer and each mask are
        // distinct targets, so a module that owns one layer still owns one per target and finds it by
        // the same scan it has always made.
        let recipe = recipe_for_target(
            &self.registry,
            &state.current_entry.snapshot.recipe,
            accepts_mask,
            mask,
        );
        self.with_stage_context(source, &recipe, |context| module.plan(input, context))
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
            PreparedSource::Raw(raw) => Some(raw_settings(raw, recipe, RawSettingsMode::Strict)?),
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
                .compile_layers(
                    width,
                    height,
                    prefix(&recipe.layers, index)?,
                    &recipe.masks,
                    &recipe.strokes,
                )?
                .stage())
        };
        // One pixel of the stage a prefix produces, for a module planning against the position its
        // layer will take. Compiling the prefix costs O(layers) and the evaluation answers the
        // point per segment, so nothing is rasterized here either.
        let sample_before = |index: usize, x: u32, y: u32| -> Result<Option<[u8; 4]>, Error> {
            let layers = prefix(&recipe.layers, index)?;
            match source {
                PreparedSource::Jpeg(image) => Evaluation::over_layers(
                    registry,
                    image,
                    layers,
                    &recipe.masks,
                    &recipe.strokes,
                )?
                .pixel(x, y),
                PreparedSource::Raw(_) => {
                    let prefix_recipe = Recipe {
                        format: recipe.format,
                        layers: layers.to_vec(),
                        // A prefix keeps the whole mask table: the masks a prefix layer references
                        // are the recipe's, not the prefix's, and dropping them would make a valid
                        // stack look as if it named a mask that does not exist.
                        masks: recipe.masks.clone(),
                        // And the strokes those masks resolved to, for the same reason.
                        strokes: recipe.strokes.clone(),
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
        // A query carries no mask target, so it asks about the global layer, and the target view hides
        // the masked layers of the module's own effect. Without it a module that owns one layer would
        // refuse its own query as ambiguous as soon as a mask held a layer of that effect, and the
        // pixels it reads are unchanged: what it samples is the stage *before* its own layer, and a
        // masked layer of the same effect is always after the global one.
        let recipe = recipe_for_target(
            &self.registry,
            &entry.snapshot.recipe,
            module
                .descriptor()
                .effects
                .iter()
                .any(|effect| effect.maskable),
            None,
        );
        self.with_stage_context(&source, &recipe, |context| {
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
        // A drafted host command previews through the same planner that commits it, so a gesture
        // shows exactly the stack releasing it would write.
        if let Some(command) = crate::mask::commands::find(&draft.action) {
            let state = self.state(asset_id)?;
            let current = &state.current_entry.snapshot.recipe;
            validate_source_recipe(&state.asset, current)?;
            let checked = check_parameters(&command.action, &Value::Object(draft.fields.clone()))?;
            let target = draft.target.clone().unwrap_or_default();
            // The same seed the commit will store, read the same way, so a drafted limited stroke
            // previews the stroke it is about to become rather than an unlimited one.
            let seed = self.mask_colour_seed(&state, command, current, &target, &checked)?;
            let recipe = match crate::mask::commands::plan(
                command, current, &target, &checked, &registry, seed,
            )? {
                MaskOutcome::NoOp => current.clone(),
                MaskOutcome::Change(change) => change.recipe,
            };
            return Ok((recipe, state));
        }
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
        // The draft's target is the one the commit will carry: none for a global gesture, and the
        // mask a masked slider was opened on. The target view hides the layers of the drafted
        // module's effect that belong to another target, which is what lets a global slider drag and
        // a masked one each keep working on a stack that holds both.
        let current = &state.current_entry.snapshot.recipe;
        let mask = draft
            .target
            .as_ref()
            .and_then(|target| target.mask.as_ref());
        let plan = self.plan_input(
            &state,
            &source,
            module,
            &input,
            registry.action_accepts_mask(&draft.action),
            mask,
        )?;
        let recipe = self
            .resolve_plan(&source, current, plan, mask)?
            .unwrap_or_else(|| current.clone());
        Ok((recipe, state))
    }

    /// The stack one plan produces from `recipe`, or `None` when it changes nothing. A commit and a
    /// draft's effective recipe both resolve their plan here, so a drafted preview is exactly what
    /// committing it would produce, composites included.
    ///
    /// `Commit` places the new layer by its effect's declared stage and order: a pixel-stage effect
    /// goes before the geometry tail, so a later crop change carries it instead of moving or
    /// invalidating it, and an effect no provider declares is placed as a geometry one would be and
    /// refused by the whole-stack compile at commit. `Update` keeps the layer's identity and
    /// position, and a missing identity is refused before anything is written.
    ///
    /// `Compose` runs each step exactly as that action would run alone, against the stack the steps
    /// before it produced: the registry finds the action, which must be a field patch of an
    /// available module; the generic check and the module's `parse` take its fields; and the module
    /// plans against the intermediate stack through the same [`StageContext`] construction. A step
    /// that is itself a composite is refused, and so is any refused step, before anything is
    /// written. The final stack is `None` when it equals the starting one. Each step plans by
    /// comparing payloads, so a composite costs `O(steps × layers)` and rasterizes nothing.
    fn resolve_plan(
        &self,
        source: &PreparedSource,
        recipe: &Recipe,
        plan: ActionPlan,
        mask: Option<&MaskId>,
    ) -> Result<Option<Recipe>, Error> {
        let steps = match plan {
            ActionPlan::Compose(steps) => steps,
            plan => return self.apply_plan(recipe, plan, mask),
        };
        if steps.len() > MAX_COMPOSE_STEPS {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "a composite action holds {} steps, more than {MAX_COMPOSE_STEPS}",
                    steps.len()
                ),
            ));
        }
        let registry = self.registry.clone();
        let mut resolved = recipe.clone();
        for step in steps {
            let action_id = step.action_id.as_str();
            let (module, action) = registry.action(action_id).ok_or_else(|| {
                Error::new(ErrorKind::Validation, format!("unknown action {action_id}"))
            })?;
            if !action.patch {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!("{action_id} is not a field-patch action"),
                ));
            }
            let descriptor = module.descriptor();
            if !descriptor.is_available() {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    format!("unavailable module {}", descriptor.id),
                ));
            }
            let checked = check_parameters(action, &Value::Object(step.parameters))?;
            let input = module.parse(action_id, &checked)?;
            let plan =
                self.with_stage_context(source, &resolved, |context| module.plan(&input, context))?;
            if let Some(next) = self.apply_plan(&resolved, plan, mask)? {
                resolved = next;
            }
        }
        Ok((resolved != *recipe).then_some(resolved))
    }

    /// One step's plan applied to a stack: the placement rules of [`Self::resolve_plan`] for a
    /// single layer. A composite here is a step of another composite, which the host refuses.
    fn apply_plan(
        &self,
        recipe: &Recipe,
        plan: ActionPlan,
        mask: Option<&MaskId>,
    ) -> Result<Option<Recipe>, Error> {
        match plan {
            ActionPlan::NoOp => Ok(None),
            // Within the region the effect's stage and order choose, a masked layer follows the
            // global layer of its effect and the masked layers of earlier masks, so overlapping
            // masks apply in the order the mask list shows. The target is the host's to write: a
            // module returns a layer without one, because it never saw the field.
            ActionPlan::Commit(layer) => {
                let index = self.registry.insertion_index_for_target(
                    &recipe.layers,
                    &layer.effect_id,
                    mask,
                    &recipe.masks,
                );
                let layer = Layer {
                    mask: mask.cloned(),
                    ..layer
                };
                Ok(Some(recipe.with_layer_inserted(index, layer)?))
            }
            ActionPlan::Update(layer) => Ok(Some(recipe.with_layer_replaced(layer)?)),
            ActionPlan::Compose(_) => Err(Error::new(
                ErrorKind::Validation,
                "composite actions do not nest",
            )),
        }
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
pub(crate) fn prefix(layers: &[Layer], index: usize) -> Result<&[Layer], Error> {
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

/// The deduplicated request identity: the durable action, the mutation envelope, the mask target and
/// the parsed parameters as top-level fields.
///
/// The target belongs here because it is part of what the request *is*: the same fields sent to the
/// global layer and to a mask are two different edits, and a client that reused one request id for
/// both must not receive the first one's result for the second.
fn request_input(
    input: &ActionInput,
    mutation: &Mutation,
    mask: Option<&MaskId>,
) -> Result<Value, Error> {
    let mut request = serde_json::Map::new();
    request.insert("action".into(), Value::from(input.action_id.as_str()));
    request.insert(
        "mutation".into(),
        serde_json::to_value(mutation).map_err(|e| json_error("cannot encode request", e))?,
    );
    if let Some(mask) = mask {
        request.insert(MASK_FIELD.into(), Value::from(mask.as_str()));
    }
    for (name, value) in &input.parameters {
        request.insert(name.clone(), value.clone());
    }
    Ok(Value::Object(request))
}

/// The host's one optional top-level request field on every action of a maskable effect.
pub const MASK_FIELD: &str = "mask";

/// Take the `mask` target out of a request before the action's own parameters are checked, so no
/// module's `parse`, `plan` or `compile` ever sees it (`docs/design/masking.md`, "How a mask reaches
/// an effect").
///
/// Sending it to an action that does not accept one is a `validation` error naming the action, not a
/// silently ignored field: an agent that believes it edited through a mask must be told it did not.
fn take_mask_target(
    registry: &ModuleRegistry,
    action_id: &str,
    parameters: &mut Value,
) -> Result<Option<MaskId>, Error> {
    let Some(object) = parameters.as_object_mut() else {
        return Ok(None);
    };
    let Some(field) = object.remove(MASK_FIELD) else {
        return Ok(None);
    };
    if !registry.action_accepts_mask(action_id) {
        return Err(Error::new(
            ErrorKind::Validation,
            format!("action {action_id} does not accept a mask target"),
        ));
    }
    let id: MaskId = serde_json::from_value(field.clone()).map_err(|_| {
        Error::new(
            ErrorKind::Validation,
            format!("mask target {field} is not a mask identity"),
        )
    })?;
    Ok(Some(id))
}

/// The mask a request named, resolved against the stack it will edit. A target the recipe does not
/// hold is refused before anything is planned, so an edit never creates a layer bound to a mask that
/// does not exist.
fn resolve_mask_target<'a>(
    recipe: &'a Recipe,
    mask: Option<&MaskId>,
) -> Result<Option<&'a crate::Mask>, Error> {
    let Some(id) = mask else {
        return Ok(None);
    };
    recipe
        .masks
        .iter()
        .find(|mask| &mask.id == id)
        .map(Some)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!("unknown mask {id} for this asset"),
            )
        })
}

/// The stack as one target sees it: the layers of every maskable effect that belong to some *other*
/// target are hidden, and everything else is exactly where it was.
///
/// This is what makes a target a target without a single line of module code. A module finds its own
/// layer by scanning the layers it is given — and refuses a stack that holds two of its own, which is
/// the same refusal the host makes — so handing it the one target's layers is what lets
/// `edit.set-basic {mask, exposure}` commit and update the masked layer while `edit.set-basic
/// {exposure}` keeps editing the global one.
///
/// Every stage answer the context gives is unchanged by the hiding, because only a colour-, pixel-
/// or spatial-stage effect may be maskable and none of those changes the stage's dimensions: the
/// geometry tail is never hidden, so `stage`, `stage_before` and `insertion_index` answer exactly
/// what they answer for the whole stack. What a sampler reads does change — it no longer includes the
/// other targets' colour — and that is why the filtered view is used only for planning an action of a
/// maskable module and for that module's own queries, where the layers before the module's own layer
/// are what is sampled and a masked layer of the same effect is never among them.
///
/// A recipe with no masks is handed back as it is, so the ordinary path allocates nothing.
fn recipe_for_target<'a>(
    registry: &ModuleRegistry,
    recipe: &'a Recipe,
    accepts_mask: bool,
    mask: Option<&MaskId>,
) -> std::borrow::Cow<'a, Recipe> {
    if !accepts_mask || recipe.masks.is_empty() {
        return std::borrow::Cow::Borrowed(recipe);
    }
    std::borrow::Cow::Owned(Recipe {
        format: recipe.format,
        layers: recipe
            .layers
            .iter()
            .filter(|layer| {
                layer.mask.as_ref() == mask || !registry.effect_maskable(&layer.effect_id)
            })
            .cloned()
            .collect(),
        masks: recipe.masks.clone(),
        strokes: recipe.strokes.clone(),
    })
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

/// Whether an evaluation may approximate a RAW white balance the developed planes do not hold.
///
/// [`Self::DraftPreview`] is taken in exactly one place, [`EditorService::preview_job`] for an
/// open draft. Every other evaluation — a committed or historical preview, `render_entry` and so
/// every export, `sample_entry` and `sample_draft` and so the readout and `render.sample`,
/// `analysis_plan`, and the stage context every plan and query is answered from — is
/// [`Self::Strict`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawSettingsMode {
    /// The developed planes must hold the recipe's white balance; anything else is
    /// `preparation-required`, and the mosaic is redeveloped for it.
    Strict,
    /// A white balance the planes do not hold is approximated on them by
    /// [`crate::WhiteBalanceApproximation`], and the frame is labelled approximate. Used only for
    /// the preview of an open draft, so the drag shows something before its release redevelops.
    DraftPreview,
}

fn raw_settings(
    raw: &RawPrepared,
    recipe: &crate::Recipe,
    mode: RawSettingsMode,
) -> Result<crate::LinearSettings, Error> {
    let payload = raw_payload(recipe)?;
    let metadata = raw.sensor.metadata();
    resolve_raw_settings(
        &payload,
        metadata.as_shot_gains,
        metadata.rgb_cam,
        raw.gains,
        raw.linear.is_some(),
        mode,
    )
}

/// The linear settings one RAW recipe asks for over planes developed at `developed` gains, which
/// exist when `developed_present`. `rgb_cam` is the camera-to-linear-sRGB matrix the development
/// applied; its fourth column is validated zero at decode and is not read.
///
/// Planes that hold the recipe's gains need no approximation: the settings are exactly the ones a
/// committed render uses. Planes at other gains are `preparation-required` under
/// [`RawSettingsMode::Strict`]; under [`RawSettingsMode::DraftPreview`] they carry the matrix that
/// approximates the recipe's gains on them, and a camera matrix with no usable inverse stays
/// `preparation-required` rather than rendering a frame the matrix cannot describe. Missing planes
/// are `preparation-required` in both modes. `O(1)`: it reads no pixel.
fn resolve_raw_settings(
    payload: &crate::RawPayload,
    as_shot_gains: [f32; 3],
    rgb_cam: [[f32; 4]; 3],
    developed: [f32; 3],
    developed_present: bool,
    mode: RawSettingsMode,
) -> Result<crate::LinearSettings, Error> {
    let gains = match payload.wb_mode {
        crate::WhiteBalanceMode::AsShot => as_shot_gains,
        crate::WhiteBalanceMode::Custom => payload.gains,
    };
    let required = |detail: String| Error::new(ErrorKind::PreparationRequired, detail);
    if !developed_present {
        return Err(required("RAW white balance development required".into()));
    }
    let white_balance = if gains == developed {
        None
    } else {
        match mode {
            RawSettingsMode::Strict => {
                return Err(required("RAW white balance development required".into()));
            }
            RawSettingsMode::DraftPreview => {
                let camera_to_srgb =
                    rgb_cam.map(|row| [f64::from(row[0]), f64::from(row[1]), f64::from(row[2])]);
                let approximation =
                    crate::WhiteBalanceApproximation::between(camera_to_srgb, developed, gains)
                        .map_err(|error| {
                            required(format!(
                                "RAW white balance development required: {}",
                                error.detail
                            ))
                        })?;
                Some(approximation)
            }
        }
    };
    Ok(crate::LinearSettings {
        exposure_ev: payload.exposure_ev,
        white_balance,
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
/// The artifact directory a catalog uses when nothing has relocated it: `<stem>.artifacts` beside
/// the catalog file. One rule, so anything writing an entry without an open service — a test on the
/// production write path — names the same directory the service would.
fn default_artifact_root(catalog: &Path) -> PathBuf {
    let canonical = catalog
        .canonicalize()
        .unwrap_or_else(|_| catalog.to_path_buf());
    let stem = canonical
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "catalog".into());
    canonical
        .parent()
        .unwrap_or(Path::new(""))
        .join(format!("{stem}.artifacts"))
}

fn insert_entry(
    registry: &ModuleRegistry,
    tx: &Transaction<'_>,
    artifact_root: &Path,
    entry: &HistoryEntry,
) -> Result<(), Error> {
    registry.validate_recipe(&entry.snapshot.recipe)?;
    store_strokes(tx, &entry.snapshot.recipe)?;
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

/// Write this recipe's strokes to the content-addressed store, once each.
///
/// The address is the content's, so a stroke a later entry references again is already there and
/// the insert does nothing: that is the whole of "stored once", and it needs no reference count and
/// no check of what else points at it. The entry's own JSON carries only the addresses, so nothing
/// written here is ever written into an entry.
///
/// A reference the recipe could not resolve writes nothing and is not an error at this boundary: an
/// unresolvable reference is retained data, and the paths that would *draw* it refuse it by name.
fn store_strokes(tx: &Transaction<'_>, recipe: &Recipe) -> Result<(), Error> {
    let references = recipe.stroke_references()?;
    if references.is_empty() {
        return Ok(());
    }
    let mut statement = tx
        .prepare("INSERT OR IGNORE INTO strokes (id,stroke_json) VALUES (?1,?2)")
        .map_err(catalog_error)?;
    for (_, id) in &references {
        let Some(stroke) = recipe.strokes.get(id) else {
            continue;
        };
        let text = String::from_utf8(stroke.canonical())
            .map_err(|e| Error::new(ErrorKind::Internal, format!("cannot store stroke: {e}")))?;
        statement
            .execute(params![id.as_str(), text])
            .map_err(catalog_error)?;
    }
    Ok(())
}

/// Resolve this recipe's stroke references against the store, one lookup each.
///
/// Nothing is replayed and no earlier entry is read: an entry is a complete snapshot, and this is
/// the lookup that turns its addresses back into the strokes they name. A reference the store does
/// not hold, or whose stored bytes are not the bytes the address names, is recorded as a fault
/// rather than raised here, so reading, listing, undoing and carrying the stack forward keep
/// working; the refusal happens where the recipe is compiled, which is every path that would draw
/// it.
fn hydrate_strokes(
    connection: &Connection,
    recipe: &mut Recipe,
    origin: &str,
) -> Result<(), Error> {
    let references = recipe.stroke_references()?;
    if references.is_empty() {
        return Ok(());
    }
    let mut table = crate::path::StrokeTable::new(origin);
    let mut statement = connection
        .prepare("SELECT stroke_json FROM strokes WHERE id=?1")
        .map_err(catalog_error)?;
    for (_, id) in references {
        if table.get(&id).is_some() {
            continue;
        }
        let stored: Option<String> = statement
            .query_row(params![id.as_str()], |row| row.get(0))
            .optional()
            .map_err(catalog_error)?;
        match stored {
            None => table.fault(id, crate::path::StrokeFault::Missing),
            Some(text) => match crate::path::Stroke::from_stored(&id, text.as_bytes()) {
                Ok(stroke) => {
                    table.insert(stroke);
                }
                Err(_) => table.fault(id, crate::path::StrokeFault::Corrupt),
            },
        }
    }
    recipe.strokes = table;
    Ok(())
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
    let mut entry: HistoryEntry = decode("invalid history entry", json)?;
    // One lookup per referenced stroke, here and nowhere else: every path that evaluates an entry —
    // state, preview, undo, redo, Restore, export — reads it through this function, and a listing,
    // which never draws anything, keeps the stored addresses and pays nothing.
    let origin = format!("entry {}", entry.id);
    hydrate_strokes(connection, &mut entry.snapshot.recipe, &origin)?;
    Ok(entry)
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
        ActionDescriptor, Availability, CROP_EFFECT, Component, ComponentMode, CropPayload,
        CropStage, EFFECT_FORMAT, EffectDescriptor, EffectStage, ExactGeometry, Layer, LayerId,
        Mask, ModuleDescriptor, ORIENTATION_EFFECT, PIXEL_EFFECT, ParameterDescriptor,
        ParameterKind, PreviewQueue, Processing, Stage, ToolModule, open_source,
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

    /// A camera matrix with rows summing to one and strong cross terms, as a real `rgb_cam` has.
    const RGB_CAM: [[f32; 4]; 3] = [
        [1.72, -0.61, -0.11, 0.0],
        [-0.18, 1.49, -0.31, 0.0],
        [0.04, -0.52, 1.48, 0.0],
    ];

    fn custom(gains: [f32; 3], exposure_ev: f64) -> crate::RawPayload {
        crate::RawPayload {
            exposure_ev,
            wb_mode: crate::WhiteBalanceMode::Custom,
            gains,
            as_shot_gains: [2.0, 1.0, 1.5],
            ..crate::RawPayload::default()
        }
    }

    /// Planes that hold the recipe's white balance are evaluated exactly in both modes. Planes at
    /// another white balance are `preparation-required` when strict, and approximated by
    /// `R · diag(g'/g) · R⁻¹` only for a drafted preview. Missing planes, or a camera matrix with no
    /// inverse, are `preparation-required` in both modes: never a silent wrong frame.
    #[test]
    fn only_a_drafted_preview_approximates_a_white_balance_the_planes_do_not_hold() {
        use RawSettingsMode::{DraftPreview, Strict};
        let developed = [2.0_f32, 1.0, 1.5];
        let target = [1.6_f32, 1.0, 2.2];
        let camera = RGB_CAM.map(|row| [row[0], row[1], row[2]].map(f64::from));
        for mode in [Strict, DraftPreview] {
            let held = resolve_raw_settings(
                &custom(developed, 0.4),
                developed,
                RGB_CAM,
                developed,
                true,
                mode,
            )
            .unwrap();
            assert_eq!(
                held,
                crate::LinearSettings {
                    exposure_ev: 0.4,
                    white_balance: None,
                },
                "{mode:?}: planes that hold the white balance need no approximation"
            );
            // As shot resolves to the camera's gains, which these planes hold.
            let as_shot = crate::RawPayload {
                wb_mode: crate::WhiteBalanceMode::AsShot,
                ..custom(target, 0.0)
            };
            assert_eq!(
                resolve_raw_settings(&as_shot, developed, RGB_CAM, developed, true, mode)
                    .unwrap()
                    .white_balance,
                None
            );
            let missing = resolve_raw_settings(
                &custom(developed, 0.0),
                developed,
                RGB_CAM,
                developed,
                false,
                mode,
            )
            .unwrap_err();
            assert_eq!(missing.kind, ErrorKind::PreparationRequired, "{mode:?}");
        }

        let strict = resolve_raw_settings(
            &custom(target, 0.4),
            developed,
            RGB_CAM,
            developed,
            true,
            Strict,
        )
        .unwrap_err();
        assert_eq!(strict.kind, ErrorKind::PreparationRequired);

        let drafted = resolve_raw_settings(
            &custom(target, 0.4),
            developed,
            RGB_CAM,
            developed,
            true,
            DraftPreview,
        )
        .unwrap();
        assert_eq!(drafted.exposure_ev, 0.4);
        assert_eq!(
            drafted.white_balance,
            Some(crate::WhiteBalanceApproximation::between(camera, developed, target).unwrap()),
            "the drafted gains over the developed ones, through the camera matrix"
        );

        let mut singular = RGB_CAM;
        singular[2] = [
            2.0 * singular[0][0],
            2.0 * singular[0][1],
            2.0 * singular[0][2],
            0.0,
        ];
        let error = resolve_raw_settings(
            &custom(target, 0.0),
            developed,
            singular,
            developed,
            true,
            DraftPreview,
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::PreparationRequired);
        assert!(error.detail.contains("singular"), "{}", error.detail);
    }

    /// On a real RAW file: a drafted temperature previews through the approximation, and every
    /// other evaluation of the same drafted or committed white balance — the draft's point sample,
    /// its analysis, and once committed the preview, render (the export path), sample, analysis and
    /// the stage context an action is planned in — stays `preparation-required` until the mosaic is
    /// redeveloped. Run with LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_raw_draft_preview_approximates_and_every_strict_path_refuses() {
        let path = PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
        let catalog = temp("raw-approximate.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&path).unwrap();
        let asset = state.asset.id.clone();
        let is_required = |error: Error| error.kind == ErrorKind::PreparationRequired;

        let committed = service.preview_job(&asset, None, None, None, None).unwrap();
        assert!(!committed.source.approximate_white_balance());

        let mut draft = Draft::new("set-raw-temperature", asset.clone(), state.revision);
        draft.merge(Map::from_iter([("kelvin".to_owned(), json!(3200.0))]));
        let drafted = service
            .preview_job(&asset, None, None, Some(&draft), None)
            .expect("a drafted white balance previews");
        assert!(drafted.source.approximate_white_balance());
        let PreviewSource::Raw { settings, .. } = &drafted.source else {
            panic!("a RAW source");
        };
        let metadata = match &state.asset.source {
            SourceKind::Raw { metadata } => {
                parse_raw_interpretation(metadata, "RAW interpretation").unwrap()
            }
            SourceKind::Jpeg => panic!("a RAW asset"),
        };
        // A temperature drafted from As shot keeps the camera's as-shot tint.
        let [_, as_shot_tint] =
            crate::RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz)
                .unwrap()
                .white_balance_controls();
        let target =
            crate::gains_from_temperature_tint(3200.0, as_shot_tint, metadata.cam_xyz).unwrap();
        let camera = metadata
            .rgb_cam
            .map(|row| [row[0], row[1], row[2]].map(f64::from));
        assert_eq!(
            settings.white_balance,
            Some(
                crate::WhiteBalanceApproximation::between(camera, metadata.as_shot_gains, target)
                    .unwrap()
            )
        );
        let rendered = drafted
            .source
            .render(
                &service.registry,
                drafted.entry.snapshot.id.clone(),
                &drafted.recipe,
            )
            .expect("the approximate frame renders");
        let exact = committed
            .source
            .render(
                &service.registry,
                committed.entry.snapshot.id.clone(),
                &committed.recipe,
            )
            .unwrap();
        assert_ne!(
            rendered.rgba, exact.rgba,
            "3200 K is not the as-shot picture"
        );

        // The drafted value's numbers are strict.
        assert!(is_required(
            service.sample_draft(&asset, &draft, 10, 10).unwrap_err()
        ));
        assert!(is_required(
            service
                .analysis_plan(&asset, AnalysisSelection::Draft(&draft))
                .unwrap_err()
        ));
        // A drafted exposure over planes that hold the white balance is exact.
        let mut exposure = Draft::new("set-raw-exposure", asset.clone(), state.revision);
        exposure.merge(Map::from_iter([("ev".to_owned(), json!(0.5))]));
        assert!(
            !service
                .preview_job(&asset, None, None, Some(&exposure), None)
                .unwrap()
                .source
                .approximate_white_balance()
        );

        // Committed, the same white balance is strict everywhere until it is redeveloped.
        let result = service
            .apply_action(
                &asset,
                mutation(state.revision, "temperature"),
                "set-raw-temperature",
                json!({"kelvin": 3200.0}),
            )
            .unwrap();
        let current = service.state(&asset).unwrap().current_entry.id;
        assert!(is_required(
            service
                .preview_job(&asset, None, None, None, None)
                .unwrap_err()
        ));
        assert!(is_required(
            service.render_entry(&asset, &current).unwrap_err()
        ));
        assert!(is_required(
            service.sample_entry(&asset, &current, 10, 10).unwrap_err()
        ));
        assert!(is_required(
            service
                .analysis_plan(&asset, AnalysisSelection::Current)
                .unwrap_err()
        ));
        // Planning a pixel-stage module's action answers from the stage context, which is strict
        // too. (A RAW source action plans without pixels, so it would not reach it.)
        assert!(is_required(
            service
                .apply_action(
                    &asset,
                    mutation(result.revision, "basic"),
                    "set-basic",
                    json!({"exposure": 0.5}),
                )
                .unwrap_err()
        ));
        drop(service);
        std::fs::remove_file(catalog).unwrap();
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
            // The predecessor format is refused by name like any other: its stacks carry no mask
            // table, and a marker that is only one behind is not a reason to guess at one.
            if marker != 0 {
                assert_eq!(
                    error.detail,
                    format!(
                        "catalog format {marker} is not supported; expected {CATALOG_FORMAT}; \
                         choose a new catalog path"
                    )
                );
            }
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
            "catalog format 2 is not supported; expected 7; choose a new catalog path"
        );
        assert_eq!(
            std::fs::read(&catalog).unwrap(),
            before,
            "a refused catalog is left byte for byte as it was"
        );
        std::fs::remove_file(catalog).unwrap();
    }

    /// One stored mask with a component the host can compile, which is what a persisted masked stack
    /// looks like. A component of a kind this build knows nothing about is its own case and is
    /// asserted where the kind table lives: the model proves the bytes survive a round trip, and the
    /// module registry proves every path that would have to draw the mask refuses it by name while
    /// reading, listing and validating still work.
    fn stored_mask() -> Mask {
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("linear");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.25, "y0": 0.5, "x1": 0.75, "y1": 0.5}),
        ));
        mask
    }

    /// The entry a masked stack would commit, over the current one: the same layers, with the last
    /// one bound to `mask`, and `masks` as given so a dangling reference can be planted too.
    fn masked_entry(state: &EditorState, mask: &Mask, masks: Vec<Mask>) -> HistoryEntry {
        let mut layers = state.current_entry.snapshot.recipe.layers.clone();
        layers.last_mut().expect("a layer to mask").mask = Some(mask.id.clone());
        HistoryEntry {
            id: EntryId::new(),
            sequence: state.current_entry.sequence + 1,
            label: "Mask 1 exposure +0.50".into(),
            undo_parent: Some(state.current_entry.id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: state.asset.id.clone(),
                recipe: Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers,
                    masks,
                    ..Recipe::default()
                },
            },
            ..state.current_entry.clone()
        }
    }

    /// Write one entry and make it current without going through a mutation. The `mask.*` commands
    /// arrive later, so this is the only way to hold a stored masked stack against reopen now; a
    /// dangling reference could not be written through [`insert_entry`] at all, which is its own
    /// guarantee and is asserted below.
    fn plant(catalog: &Path, entry: &HistoryEntry) {
        let connection = Connection::open(catalog).unwrap();
        connection
            .execute(
                "INSERT INTO entries (id,asset_id,sequence,action_id,undo_parent_id,entry_json)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    entry.id.as_str(),
                    entry.asset_id.as_str(),
                    entry.sequence as i64,
                    entry.action_id,
                    entry.undo_parent.as_ref().map(EntryId::as_str),
                    serde_json::to_string(entry).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
                params![
                    entry.id.as_str(),
                    entry.result_revision as i64,
                    entry.asset_id.as_str()
                ],
            )
            .unwrap();
    }

    fn stored_entry_json(catalog: &Path, entry: &EntryId) -> String {
        Connection::open(catalog)
            .unwrap()
            .query_row(
                "SELECT entry_json FROM entries WHERE id=?1",
                params![entry.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// One captured stroke, deterministic in the index so a session builds a different stroke per
    /// entry and the same one twice on demand.
    fn stroke(index: usize) -> crate::path::Stroke {
        let base = 0.05 + (index % 40) as f64 * 0.02;
        let points: Vec<[f64; 2]> = (0..100)
            .map(|step| {
                let t = step as f64 / 99.0;
                [
                    base + 0.4 * t,
                    0.2 + 0.3 * (t * 6.0 + index as f64).sin().abs(),
                ]
            })
            .collect();
        crate::path::Stroke::capture(&points, 0.04, 50.0, 100.0, index.is_multiple_of(7))
            .expect("a legal stroke")
    }

    /// A mask whose one component references these strokes by address, with the strokes themselves
    /// in the recipe's table. The component's kind is the one a brush will carry; this build has no
    /// provider for it, which is exactly the retention case, and nothing here needs one: the store
    /// is the host's and knows nothing about what references it.
    fn brushed(recipe: &Recipe, strokes: &[crate::path::Stroke]) -> Recipe {
        let mut table = crate::path::StrokeTable::new("the test session");
        let addresses: Vec<String> = strokes
            .iter()
            .map(|stroke| table.insert(stroke.clone()).to_string())
            .collect();
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("brush");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "brush",
            json!({ "strokes": addresses }),
        ));
        Recipe {
            masks: vec![mask],
            strokes: table,
            ..recipe.clone()
        }
    }

    /// Write one entry through the production write path, which is what stores its strokes.
    fn commit(catalog: &Path, entry: &HistoryEntry) {
        let registry = ModuleRegistry::builtin();
        let mut connection = Connection::open(catalog).unwrap();
        let tx = connection.transaction().unwrap();
        insert_entry(&registry, &tx, &default_artifact_root(catalog), entry).unwrap();
        tx.execute(
            "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
            params![
                entry.id.as_str(),
                entry.result_revision as i64,
                entry.asset_id.as_str()
            ],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    /// The next entry of a session, carrying `recipe` whole.
    fn next_entry(state: &EditorState, recipe: Recipe) -> HistoryEntry {
        HistoryEntry {
            id: EntryId::new(),
            sequence: state.current_entry.sequence + 1,
            label: "Brush 1".into(),
            undo_parent: Some(state.current_entry.id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: state.asset.id.clone(),
                recipe,
            },
            ..state.current_entry.clone()
        }
    }

    fn stored_strokes(catalog: &Path) -> i64 {
        Connection::open(catalog)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM strokes", [], |row| row.get(0))
            .unwrap()
    }

    /// A stroke is stored once under its address however many entries reference it, and an entry
    /// holds addresses and no positions at all.
    #[test]
    fn one_stroke_is_stored_once_and_referenced_from_every_entry_that_holds_it() {
        let catalog = temp("stroke-store.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let shared = stroke(1);
        let first = next_entry(
            &state,
            brushed(
                &state.current_entry.snapshot.recipe,
                std::slice::from_ref(&shared),
            ),
        );
        drop(service);
        commit(&catalog, &first);

        let service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        // A second entry drawn on top: the same stroke again, plus one more.
        let second = next_entry(
            &state,
            brushed(
                &state.current_entry.snapshot.recipe,
                &[shared.clone(), stroke(2)],
            ),
        );
        drop(service);
        commit(&catalog, &second);

        assert_eq!(
            stored_strokes(&catalog),
            2,
            "the shared stroke is one stored object, not one per entry"
        );
        // Neither entry's JSON holds a position: the addresses are there and the points are not.
        for entry in [&first, &second] {
            let json = stored_entry_json(&catalog, &entry.id);
            assert!(
                json.contains(shared.id().as_str()),
                "an entry references the stroke by address"
            );
            assert!(
                !json.contains(r#""points""#),
                "no catalog holds embedded stroke positions"
            );
        }

        // And reading an entry back resolves what it references, without replaying anything.
        let service = EditorService::open(&catalog).unwrap();
        let read = service.entry(&asset, &second.id).unwrap();
        assert_eq!(
            read.snapshot.recipe.strokes.get(&shared.id()),
            Some(&shared)
        );
        assert_eq!(
            read.snapshot.recipe.strokes.strokes().count(),
            2,
            "one resolved stroke per distinct address"
        );
        // The earlier entry is still its own complete snapshot and resolves on its own.
        let earlier = service.entry(&asset, &first.id).unwrap();
        assert_eq!(earlier.snapshot.recipe.strokes.strokes().count(), 1);
        assert_eq!(
            earlier.snapshot.recipe.masks[0].components[0].payload,
            first.snapshot.recipe.masks[0].components[0].payload,
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A referenced stroke that is gone, or whose stored bytes are not the bytes its address names,
    /// refuses every path that would draw the recipe and keeps everything it has.
    #[test]
    fn a_missing_or_corrupt_stroke_refuses_every_path_that_would_draw_it() {
        for what in [false, true] {
            let catalog = temp("broken-stroke.sqlite");
            let mut service = EditorService::open(&catalog).unwrap();
            let asset = service.import(&fixture()).unwrap().asset.id;
            let state = service.state(&asset).unwrap();
            let drawn = stroke(3);
            let entry = next_entry(
                &state,
                brushed(
                    &state.current_entry.snapshot.recipe,
                    std::slice::from_ref(&drawn),
                ),
            );
            drop(service);
            commit(&catalog, &entry);

            let before = stored_entry_json(&catalog, &entry.id);
            let connection = Connection::open(&catalog).unwrap();
            if what {
                // Tamper with the stored bytes under an address that still names the old ones.
                connection
                    .execute(
                        "DELETE FROM strokes WHERE id=?1",
                        params![drawn.id().as_str()],
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO strokes (id,stroke_json) VALUES (?1,?2)",
                        params![
                            drawn.id().as_str(),
                            r#"{"points":[[1,1]],"size":819,"feather":0.0,"flow":100.0,"erase":false}"#
                        ],
                    )
                    .unwrap();
            } else {
                connection
                    .execute(
                        "DELETE FROM strokes WHERE id=?1",
                        params![drawn.id().as_str()],
                    )
                    .unwrap();
            }
            drop(connection);

            let service = EditorService::open(&catalog).unwrap();
            // Reading, listing and lineage keep working: the stack is retained whole.
            let read = service.entry(&asset, &entry.id).unwrap();
            assert_eq!(
                read.snapshot.recipe.masks, entry.snapshot.recipe.masks,
                "the stored mask table is retained unchanged"
            );
            assert!(service.history(&asset, None, 10).is_ok());
            assert!(service.describe_entry(&asset, None).is_ok());
            assert!(service.state(&asset).is_ok());
            drop(service);
            assert_eq!(
                stored_entry_json(&catalog, &entry.id),
                before,
                "nothing was rewritten or discarded"
            );

            // And every path that would have to draw it refuses by name. `render` and `sample` are
            // the delivered evaluation paths and an image export is not implemented yet; all three
            // compile the recipe through one function, which is where this refusal lives, so the
            // refusal is asserted on the compile every one of them makes.
            let registry = ModuleRegistry::builtin();
            let source = crate::SourceImage {
                width: 8,
                height: 8,
                rgba: vec![255; 8 * 8 * 4].into(),
                fingerprint: "test".into(),
                orientation: 1,
            };
            let recipe = &read.snapshot.recipe;
            let expected = format!(
                "stroke {} of entry {} {} referenced by component Brush 1 of mask Mask 1",
                drawn.id(),
                entry.id,
                if what {
                    "does not match its stored content address"
                } else {
                    "is not in the stroke store"
                },
            );
            for error in [
                crate::render(&registry, &source, SnapshotId::new(), recipe).unwrap_err(),
                crate::sample(&registry, &source, recipe, 0, 0).unwrap_err(),
                crate::extents(&registry, &source, recipe).unwrap_err(),
                crate::stage_transform(&registry, source.width, source.height, recipe).unwrap_err(),
                registry
                    .compile(source.width, source.height, recipe)
                    .err()
                    .expect("compiling refuses a broken reference"),
            ] {
                assert_eq!(error.kind, ErrorKind::Incompatible);
                assert_eq!(error.detail, expected);
            }
            std::fs::remove_file(catalog).unwrap();
        }
    }

    /// Strokes per mask in the measured session below: the smaller of what the points-per-mask
    /// limit admits at 100 positions a stroke (81) and the brush component's own declared 64 strokes
    /// per component. It is why the 200-stroke session paints four masks — 200 strokes of 100
    /// positions cannot live in one mask at all — and the two assertions below are what keep the
    /// three limits from drifting apart.
    const STROKES_PER_MASK: usize = crate::mask::STROKES_PER_COMPONENT;
    const _: () = assert!(STROKES_PER_MASK * 100 <= crate::POINTS_PER_MASK);
    const _: () = assert!(STROKES_PER_MASK <= crate::mask::STROKES_PER_COMPONENT);

    /// A mask holding these strokes by address, with a component named for its ordinal so several
    /// masks in one recipe read apart.
    fn brush_mask(
        name: &str,
        table: &mut crate::path::StrokeTable,
        strokes: &[crate::path::Stroke],
    ) -> Mask {
        let addresses: Vec<String> = strokes
            .iter()
            .map(|stroke| table.insert(stroke.clone()).to_string())
            .collect();
        let mut mask = Mask::new(name);
        let component = mask.next_component_name("brush");
        mask.components.push(Component::new(
            component,
            ComponentMode::Add,
            "brush",
            json!({ "strokes": addresses }),
        ));
        mask
    }

    /// What one session of `count` strokes costs across its history, content-addressed against
    /// embedded, measured on the bytes that are actually written.
    ///
    /// The store does not change the *shape* of the growth: an entry still holds one reference per
    /// stroke, so the total is still quadratic in the stroke count. What it changes is the constant
    /// — a reference instead of a stroke — and that is the only claim measured here.
    fn session_bytes(catalog: &Path, count: usize) -> (usize, usize, usize) {
        let mut service = EditorService::open(catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let base = state.current_entry.snapshot.recipe.clone();
        drop(service);

        let registry = ModuleRegistry::builtin();
        let mut connection = Connection::open(catalog).unwrap();
        let tx = connection.transaction().unwrap();
        let mut table = crate::path::StrokeTable::new("the measured session");
        let mut drawn: Vec<Vec<crate::path::Stroke>> = Vec::new();
        let mut previous = state.current_entry.clone();
        let mut revision = state.revision;
        // What an entry embedding its strokes would have cost, accumulated beside what the
        // content-addressed entries actually cost.
        let mut embedded = 0_usize;
        for index in 0..count {
            let one = stroke(index);
            if index.is_multiple_of(STROKES_PER_MASK) {
                drawn.push(Vec::new());
            }
            drawn.last_mut().unwrap().push(one);
            let masks: Vec<Mask> = drawn
                .iter()
                .enumerate()
                .map(|(at, strokes)| brush_mask(&format!("Mask {}", at + 1), &mut table, strokes))
                .collect();
            let recipe = Recipe {
                masks,
                strokes: table.clone(),
                ..base.clone()
            };
            // The same snapshot with every stroke's positions written into the payload instead of
            // its address: the shape this store exists to avoid.
            embedded += drawn
                .iter()
                .flatten()
                .map(|stroke| stroke.canonical().len() + 1)
                .sum::<usize>();
            let entry = HistoryEntry {
                id: EntryId::new(),
                sequence: previous.sequence + 1,
                label: format!("Brush {}", index + 1),
                undo_parent: Some(previous.id.clone()),
                base_revision: revision,
                result_revision: revision + 1,
                snapshot: Snapshot {
                    id: SnapshotId::new(),
                    asset_id: asset.clone(),
                    recipe,
                },
                ..previous.clone()
            };
            revision += 1;
            insert_entry(&registry, &tx, &default_artifact_root(catalog), &entry).unwrap();
            previous = entry;
        }
        tx.commit().unwrap();

        let entries: i64 = connection
            .query_row("SELECT SUM(LENGTH(entry_json)) FROM entries", [], |row| {
                row.get(0)
            })
            .unwrap();
        let strokes: i64 = connection
            .query_row("SELECT SUM(LENGTH(stroke_json)) FROM strokes", [], |row| {
                row.get(0)
            })
            .unwrap();
        let addressed = entries as usize + strokes as usize;
        (
            addressed,
            addressed - strokes as usize + embedded,
            strokes as usize,
        )
    }

    /// The measurement the content-addressed store is justified by: what 200 strokes of 100
    /// positions cost across a history of 200 entries.
    ///
    /// Scope: `lightwell-core`'s own catalog, on the JPEG fixture, counting the bytes of every
    /// stored entry plus the bytes of the stroke store, against the same entries with each stroke's
    /// positions embedded in its payload. It is a byte count and not a timing, so no load average
    /// applies to it. The strokes are spread over four masks because the declared points-per-mask
    /// limit admits at most 81 strokes of 100 positions in one mask.
    #[test]
    fn two_hundred_strokes_cost_about_a_megabyte_rather_than_about_forty() {
        let catalog = temp("stroke-growth.sqlite");
        let (addressed, embedded, distinct) = session_bytes(&catalog, 200);
        std::fs::remove_file(&catalog).unwrap();
        let mb = |bytes: usize| bytes as f64 / 1_000_000.0;
        println!(
            "200 strokes of 100 positions: distinct stroke data {} KiB, content-addressed across \
             history {:.2} MB, embedded across history {:.2} MB, a factor of {:.1}; one stroke \
             serializes to {} bytes and one reference costs 35",
            distinct / 1024,
            mb(addressed),
            mb(embedded),
            embedded as f64 / addressed as f64,
            distinct / 200,
        );
        // The design's table carries these measured figures, and carried predicted ones before this
        // ran: it predicted 364 KiB of distinct stroke data and 37.5 MB embedded, from a stroke
        // serializing to about 1.8 KiB. A stored position is a whole grid step and not a decimal, so
        // a stroke serializes to about 968 bytes, and the table was corrected to what is measured
        // here rather than the prediction being kept. The content-addressed total was predicted at
        // 1.08 MB and measures 1.10 MB, because it is dominated by the 35-byte reference, which is
        // what the prediction got right.
        assert!(
            (0.95..1.25).contains(&mb(addressed)),
            "content-addressed history measured {:.3} MB, not the recorded 1.11 MB",
            mb(addressed),
        );
        assert!(
            (18.0..23.0).contains(&mb(embedded)),
            "embedded history measured {:.3} MB, not the recorded 20.4 MB",
            mb(embedded),
        );
        assert!(
            (170..210).contains(&(distinct / 1024)),
            "distinct stroke data measured {} KiB, not the recorded 189 KiB",
            distinct / 1024,
        );
        // The store shrinks the constant; it does not change the shape of the growth, which is
        // still quadratic in the stroke count. This is the constant, per stroke per entry.
        assert!(
            (25..32).contains(&(distinct / 200 / 35)),
            "one reference stands in for {} of its own size, not the recorded 28",
            distinct / 200 / 35,
        );
    }

    /// One sample of a painting session's stored cost, taken after the entry that carried its last
    /// stroke was committed.
    ///
    /// Everything here is a byte count taken from the catalog itself, so nothing in it depends on
    /// what else the host is doing; the one timed figure a session produces, reopen, is measured
    /// separately and quoted with its load average.
    #[derive(Clone, Copy)]
    struct Growth {
        strokes: usize,
        /// The bytes of every stored entry's JSON: the snapshots, which is where the growth is.
        entries: usize,
        /// The bytes of the content-addressed store: each distinct stroke once.
        store: usize,
        /// The catalog file on disk, which also carries the asset, the page overhead and the index.
        catalog: u64,
        /// What the same entries would have cost with each stroke's positions written into its
        /// payload instead of its address: the shape the store exists to avoid.
        embedded: usize,
    }

    impl Growth {
        /// Entries plus store: what a painting session costs a catalog, and the number the curve is
        /// read from.
        fn stored(self) -> usize {
            self.entries + self.store
        }
    }

    /// The host's one-minute load average, so every timed figure below can be quoted with the state
    /// of the machine that produced it. Byte counts do not need it and are not quoted with it.
    fn load_average() -> f64 {
        std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|text| {
                text.split_whitespace()
                    .nth(1)
                    .and_then(|first| first.parse().ok())
            })
            .unwrap_or(f64::NAN)
    }

    /// Pack a session's strokes into the densest mask table the declared limits admit, or `None`
    /// for a session too large to be a recipe at all.
    ///
    /// A component takes [`crate::mask::STROKES_PER_COMPONENT`] strokes and a mask takes components
    /// until its strokes' stored positions would pass [`crate::POINTS_PER_MASK`], so the point bound
    /// closes a mask long before its 32 components do for any stroke of usable length.
    /// [`crate::MASKS_PER_RECIPE`] masks of those is the ceiling, and a session past it is not a
    /// recipe this build will hold; that ceiling is the real end of the quadratic curve and is
    /// measured rather than assumed.
    fn packed(addresses: &[String], lengths: &[usize]) -> Option<Vec<Mask>> {
        fn close(mask: &mut Mask, component: &mut Vec<String>) {
            if component.is_empty() {
                return;
            }
            let name = mask.next_component_name("brush");
            mask.components.push(Component::new(
                name,
                ComponentMode::Add,
                "brush",
                json!({ "strokes": std::mem::take(component) }),
            ));
        }
        let mut masks: Vec<Mask> = Vec::new();
        let mut mask = Mask::new("Mask 1");
        let mut component: Vec<String> = Vec::new();
        let mut points = 0_usize;
        for (address, length) in addresses.iter().zip(lengths) {
            if points + length > crate::POINTS_PER_MASK
                || (component.len() == crate::mask::STROKES_PER_COMPONENT
                    && mask.components.len() == crate::COMPONENTS_PER_MASK)
            {
                close(&mut mask, &mut component);
                let next = Mask::new(format!("Mask {}", masks.len() + 2));
                masks.push(std::mem::replace(&mut mask, next));
                points = 0;
            }
            if component.len() == crate::mask::STROKES_PER_COMPONENT {
                close(&mut mask, &mut component);
            }
            component.push(address.clone());
            points += length;
        }
        close(&mut mask, &mut component);
        masks.push(mask);
        (masks.len() <= crate::MASKS_PER_RECIPE).then_some(masks)
    }

    /// Paint `count` strokes into `catalog` over `source`, one stroke per history entry written
    /// through the production write path, sampling the stored cost every `every` strokes.
    ///
    /// The strokes are packed by [`packed`], so the session is the densest one the declared limits
    /// admit and its cost is the worst case rather than an arrangement chosen to be cheap.
    ///
    /// The returned asset is the painted one, so a caller can time reopening the catalog it left.
    fn painting_session(
        catalog: &Path,
        source: &Path,
        count: usize,
        every: usize,
    ) -> (Vec<Growth>, AssetId) {
        let mut service = EditorService::open(catalog).unwrap();
        let asset = service.import(source).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let base = state.current_entry.snapshot.recipe.clone();
        drop(service);

        let registry = ModuleRegistry::builtin();
        let mut connection = Connection::open(catalog).unwrap();
        let mut table = crate::path::StrokeTable::new("the measured session");
        // One address per stroke, kept rather than recomputed, so building the recipe an entry
        // carries costs a clone of the table and not a rehash of every stroke drawn so far.
        let mut addresses: Vec<String> = Vec::with_capacity(count);
        let mut previous = state.current_entry.clone();
        let mut revision = state.revision;
        // The counterfactual, accumulated beside the real thing: `drawn` is what this session's
        // strokes serialize to in full, and every entry embeds all of them, so `embedded` grows by
        // the whole of `drawn` once per entry. That is the quadratic term with its large constant.
        // The entries' own bytes are added to it at each checkpoint, exactly as the 200-stroke
        // measurement does, so the two tables are read against each other directly.
        let mut drawn = 0_usize;
        let mut embedded = 0_usize;
        let mut lengths: Vec<usize> = Vec::with_capacity(count);
        let mut curve = Vec::new();
        for index in 0..count {
            let one = stroke(index);
            drawn += one.canonical().len() + 1;
            embedded += drawn;
            lengths.push(one.point_count());
            addresses.push(table.insert(one).to_string());
            let masks = packed(&addresses, &lengths)
                .expect("this session is past the per-recipe mask ceiling and is not a recipe");
            let entry = HistoryEntry {
                id: EntryId::new(),
                sequence: previous.sequence + 1,
                label: format!("Brush {}", index + 1),
                undo_parent: Some(previous.id.clone()),
                base_revision: revision,
                result_revision: revision + 1,
                snapshot: Snapshot {
                    id: SnapshotId::new(),
                    asset_id: asset.clone(),
                    recipe: Recipe {
                        masks,
                        strokes: table.clone(),
                        ..base.clone()
                    },
                },
                ..previous.clone()
            };
            revision += 1;
            let tx = connection.transaction().unwrap();
            insert_entry(&registry, &tx, &default_artifact_root(catalog), &entry).unwrap();
            tx.execute(
                "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
                params![entry.id.as_str(), revision as i64, asset.as_str()],
            )
            .unwrap();
            tx.commit().unwrap();
            previous = entry;
            if (index + 1).is_multiple_of(every) || index + 1 == count {
                let entries: i64 = connection
                    .query_row("SELECT SUM(LENGTH(entry_json)) FROM entries", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                let store: i64 = connection
                    .query_row("SELECT SUM(LENGTH(stroke_json)) FROM strokes", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                curve.push(Growth {
                    strokes: index + 1,
                    entries: entries as usize,
                    store: store as usize,
                    catalog: std::fs::metadata(catalog).map(|m| m.len()).unwrap_or(0),
                    embedded: entries as usize + embedded,
                });
            }
        }
        drop(connection);
        (curve, asset)
    }

    /// Least squares over `S(n) = a·n² + b·n + c`, returned as `(a, b, c)`.
    ///
    /// The point of fitting rather than asserting a ratio is that the quadratic term is then a
    /// number on the page: a session's cost is not linear in its stroke count and this is what says
    /// so.
    fn quadratic_fit(curve: &[Growth]) -> (f64, f64, f64) {
        // Normal equations for the three-column design matrix [n², n, 1]. Six moments of n and three
        // of S are all it needs, and the 3×3 solve is written out rather than looped.
        let (mut s0, mut s1, mut s2, mut s3, mut s4) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let (mut t0, mut t1, mut t2) = (0.0, 0.0, 0.0);
        for point in curve {
            let n = point.strokes as f64;
            let y = point.stored() as f64;
            s0 += 1.0;
            s1 += n;
            s2 += n * n;
            s3 += n * n * n;
            s4 += n * n * n * n;
            t0 += y;
            t1 += n * y;
            t2 += n * n * y;
        }
        let m = [[s4, s3, s2], [s3, s2, s1], [s2, s1, s0]];
        let rhs = [t2, t1, t0];
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        let solve = |column: usize| {
            let mut c = m;
            for (row, value) in rhs.iter().enumerate() {
                c[row][column] = *value;
            }
            (c[0][0] * (c[1][1] * c[2][2] - c[1][2] * c[2][1])
                - c[0][1] * (c[1][0] * c[2][2] - c[1][2] * c[2][0])
                + c[0][2] * (c[1][0] * c[2][1] - c[1][1] * c[2][0]))
                / det
        };
        (solve(0), solve(1), solve(2))
    }

    /// The largest session of these strokes a recipe can hold: [`crate::MASKS_PER_RECIPE`] masks,
    /// each filled to [`crate::POINTS_PER_MASK`] stored positions. It is where the quadratic curve
    /// stops, which is what makes its far end a bounded number rather than an extrapolation, and it
    /// is measured by the test below rather than reasoned out — these strokes are captured at 100
    /// positions and decimate to between 67 and 78, so the arithmetic on 100 would be wrong.
    const CEILING: usize = 1809;

    /// The independent, larger-scale confirmation of the store's figures: a painting session run to
    /// the per-recipe ceiling on 24 MP and 60 MP, sampled every fifty strokes, with the curve it
    /// traces, the counterfactual beside it, and the time to reopen the catalog it left.
    ///
    /// Peak process memory belongs to the process, so a run measures one source at a time: set
    /// `LIGHTWELL_MASK_GROWTH_SOURCE` to a fixture's path and wrap the run in `/usr/bin/time -l`,
    /// which is where the recorded peak resident set comes from. Without it both sources run in one
    /// process and only the byte counts are attributable.
    ///
    /// Printed rather than asserted, because a timing gate does not belong in the test suite; the
    /// tests below hold the design's figures, the ceiling and the bound in place.
    ///
    /// ```text
    /// cargo test --release --package lightwell-core --lib measure_mask_growth -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a measurement, not a gate"]
    fn measure_mask_growth_across_a_painting_session() {
        let generated = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated");
        let chosen = std::env::var("LIGHTWELL_MASK_GROWTH_SOURCE").ok();
        let sources: Vec<(String, PathBuf)> = match &chosen {
            Some(path) => vec![(path.clone(), PathBuf::from(path))],
            None => ["24mp.jpg", "60mp.jpg"]
                .iter()
                .map(|name| ((*name).to_owned(), generated.join(name)))
                .filter(|(_, path)| path.exists())
                .collect(),
        };
        assert!(
            !sources.is_empty(),
            "generate fixtures first: cargo xtask generate-fixtures --output fixtures/generated"
        );
        println!(
            "load average at the start of this run: {:.2}",
            load_average()
        );
        for (name, source) in &sources {
            let catalog = temp("mask-growth-measured.sqlite");
            let (curve, asset) = painting_session(&catalog, source, CEILING, 50);
            println!(
                "\n{name}: strokes, stored entries + store (MB), catalog file (MB), embedded \
                 counterfactual (MB), factor"
            );
            for point in &curve {
                println!(
                    "  {:>4}  {:>8.3}  {:>8.3}  {:>9.3}  {:>5.1}x",
                    point.strokes,
                    point.stored() as f64 / 1e6,
                    point.catalog as f64 / 1e6,
                    point.embedded as f64 / 1e6,
                    point.embedded as f64 / point.stored() as f64,
                );
            }
            let (a, b, c) = quadratic_fit(&curve);
            let at = |n: usize| {
                curve
                    .iter()
                    .find(|point| point.strokes == n)
                    .copied()
                    .expect("a sampled stroke count")
            };
            println!(
                "  fit S(n) = {a:.4}·n² + {b:.1}·n + {c:.0} bytes; S(1000)/S(500) = {:.2} and \
                 S(500)/S(250) = {:.2} (4 is quadratic, 2 would be linear)",
                at(1000).stored() as f64 / at(500).stored() as f64,
                at(500).stored() as f64 / at(250).stored() as f64,
            );
            println!(
                "  one stroke serializes to {} bytes; the ceiling's {CEILING} distinct strokes hold \
                 {} KiB",
                at(CEILING).store / CEILING,
                at(CEILING).store / 1024,
            );
            for round in 0..3 {
                let started = Instant::now();
                let service = EditorService::open(&catalog).unwrap();
                let state = service.state(&asset).unwrap();
                let elapsed = started.elapsed();
                println!(
                    "  reopen {round}: {:.1} ms at load {:.2} ({} masks, {} strokes resolved)",
                    elapsed.as_secs_f64() * 1e3,
                    load_average(),
                    state.current_entry.snapshot.recipe.masks.len(),
                    state
                        .current_entry
                        .snapshot
                        .recipe
                        .strokes
                        .strokes()
                        .count(),
                );
            }
            std::fs::remove_file(&catalog).unwrap();
        }
    }

    /// The growth is quadratic, and its square term is the one the design records.
    ///
    /// Scope: `lightwell-core`'s own catalog on the 24 MP generated fixture when it has been
    /// generated and on the small JPEG fixture otherwise — the catalog's bytes do not depend on the
    /// source's pixel dimensions, which the measurement above confirms by measuring both — one
    /// stroke of 100 positions per history entry, counting the bytes of every stored entry plus the
    /// bytes of the stroke store. Byte counts, so no load average applies.
    ///
    /// It gates the shape at 400 strokes and leaves the thousand-stroke and ceiling figures to the
    /// measurement above, so the suite does not carry a twenty-second session to learn what four
    /// hundred strokes already say.
    #[test]
    fn a_painting_session_grows_with_the_square_of_its_stroke_count() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/24mp.jpg");
        let source = if source.exists() { source } else { fixture() };
        let catalog = temp("quadratic-growth.sqlite");
        let (curve, _) = painting_session(&catalog, &source, 400, 100);
        std::fs::remove_file(&catalog).unwrap();
        let at = |n: usize| {
            curve
                .iter()
                .find(|point| point.strokes == n)
                .copied()
                .expect("a sampled stroke count")
        };
        let (quadratic, linear, _) = quadratic_fit(&curve);
        println!(
            "400 strokes: {:.3} MB stored, {:.1} MB embedded; S(n) = {quadratic:.2}·n² + {linear:.0}·n",
            at(400).stored() as f64 / 1e6,
            at(400).embedded as f64 / 1e6,
        );
        // Doubling the stroke count multiplies the stored bytes by about four. That is the whole
        // claim about the shape, and it is what forbids anyone writing that the store made the
        // growth linear. The ratio is a little under four because the linear term has not washed
        // out at these counts; it approaches four as the session grows.
        let doubling = at(400).stored() as f64 / at(200).stored() as f64;
        assert!(
            (3.2..4.3).contains(&doubling),
            "doubling the strokes multiplied the bytes by {doubling:.2}; quadratic growth doubles \
             to about four and linear growth to two",
        );
        // The fitted square term is the design's recorded curve: about twenty bytes per stroke per
        // stroke, which is the 35-byte reference paid by half the entries on average, plus the mask
        // and component structure it hangs on.
        assert!(
            (17.0..23.0).contains(&quadratic),
            "the fitted n² coefficient is {quadratic:.2} bytes, not the recorded 19.6",
        );
    }

    /// A recipe of 100-position strokes has a ceiling, and it is the masks-per-recipe limit rather
    /// than anything about the store.
    ///
    /// This is what makes the quadratic curve's far end a bounded number: the design's figure for
    /// 2400 strokes describes a session no recipe of strokes this length can hold, because the
    /// per-mask point bound admits about 113 of them and a recipe holds sixteen masks.
    #[test]
    fn a_session_of_long_strokes_ends_at_the_masks_per_recipe_ceiling() {
        let lengths: Vec<usize> = (0..CEILING * 2)
            .map(|index| stroke(index).point_count())
            .collect();
        let addresses: Vec<String> = (0..CEILING * 2).map(|i| format!("{i:032x}")).collect();
        let ceiling = (1..lengths.len())
            .take_while(|count| packed(&addresses[..*count], &lengths[..*count]).is_some())
            .last()
            .expect("at least one stroke packs");
        println!(
            "the measured session's strokes hold {}..={} positions each and {ceiling} of them is \
             the most a recipe can hold",
            lengths.iter().min().unwrap(),
            lengths.iter().max().unwrap(),
        );
        // The measurement paints to CEILING, so CEILING has to be a session that packs, and the
        // stroke past it has to be one that does not: that is what makes the curve's far end the
        // real end rather than a number chosen to be round.
        assert_eq!(ceiling, CEILING);
        let full = packed(&addresses[..ceiling], &lengths[..ceiling]).expect("the ceiling packs");
        assert_eq!(full.len(), crate::MASKS_PER_RECIPE);
        for mask in &full {
            let points: usize = mask
                .components
                .iter()
                .flat_map(|component| {
                    crate::path::references(&component.payload, "a packed component").unwrap()
                })
                .map(|id| {
                    let at = usize::from_str_radix(id.as_str(), 16).expect("a positional address");
                    lengths[at]
                })
                .sum();
            assert!(points <= crate::POINTS_PER_MASK);
            assert!(mask.components.len() <= crate::COMPONENTS_PER_MASK);
        }
    }

    /// One single-position stroke, distinct per index, for the sessions that press a count rather
    /// than a length.
    fn tiny_stroke(index: usize) -> crate::path::Stroke {
        let x = 0.1 + (index % 4096) as f64 / 16384.0;
        let y = 0.1 + (index / 4096) as f64 / 16384.0;
        crate::path::Stroke::capture(&[[x, y]], 0.04, 50.0, 100.0, false).expect("a legal stroke")
    }

    /// The per-recipe serialized mask bound is the one that keeps a painting session's snapshots
    /// bounded, so it is refused by name and the refusal writes nothing.
    ///
    /// The bound is reached with single-position strokes because that is the shape that presses it:
    /// the per-mask point bound stops a session of long strokes long before its references fill
    /// 256 KiB.
    #[test]
    fn a_recipe_over_the_serialized_mask_bound_names_it_and_leaves_the_catalog_as_it_was() {
        let catalog = temp("serialized-mask-bound.sqlite");
        let (_, asset) = painting_session(&catalog, &fixture(), 4, 4);

        // The durable state the refusal must not touch: the catalog's own bytes, and what a reopen
        // reads back out of them.
        let before = std::fs::read(&catalog).unwrap();
        let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
        let service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        let entries = service.history(&asset, None, 64).unwrap().entries.len();
        let base = state.current_entry.snapshot.recipe.clone();
        let current = state.current_entry.id.clone();
        drop(service);

        // Fill masks to their declared component and stroke counts until the mask table serializes
        // past the bound. Nothing else is at its limit: each mask holds 2048 stored positions
        // against a bound of 8192, and the recipe holds four masks against a bound of sixteen.
        let mut table = crate::path::StrokeTable::new("the over-bound session");
        let mut masks: Vec<Mask> = vec![Mask::new("Full 1")];
        let mut drawn = 0_usize;
        let mut under = 0_usize;
        while serde_json::to_vec(&masks).unwrap().len() <= crate::MASK_BYTES_PER_RECIPE {
            under = drawn;
            if masks.last().unwrap().components.len() == crate::COMPONENTS_PER_MASK {
                masks.push(Mask::new(format!("Full {}", masks.len() + 1)));
                assert!(
                    masks.len() <= crate::MASKS_PER_RECIPE,
                    "the mask-count bound was reached before the byte bound, so this test is \
                     pressing the wrong limit"
                );
            }
            let addresses: Vec<String> = (0..crate::mask::STROKES_PER_COMPONENT)
                .map(|_| {
                    drawn += 1;
                    table.insert(tiny_stroke(drawn)).to_string()
                })
                .collect();
            let mask = masks.last_mut().unwrap();
            let name = mask.next_component_name("brush");
            mask.components.push(Component::new(
                name,
                ComponentMode::Add,
                "brush",
                json!({ "strokes": addresses }),
            ));
        }
        let bytes = serde_json::to_vec(&masks).unwrap().len();
        // The byte bound is also the absolute ceiling on a recipe's stroke count, because a
        // reference costs 35 bytes whatever it points at: no recipe of any shape holds more strokes
        // than this, whatever its strokes are, and no snapshot a painting session writes is larger
        // than the bound.
        println!(
            "{under} single-position strokes over {} masks are the most that fit the {} KiB bound; \
             {drawn} of them serialize to {bytes} bytes and are refused",
            masks.len(),
            crate::MASK_BYTES_PER_RECIPE / 1024,
        );
        let over = HistoryEntry {
            id: EntryId::new(),
            sequence: state.current_entry.sequence + 1,
            label: "Brush past the bound".into(),
            undo_parent: Some(current.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: asset.clone(),
                recipe: Recipe {
                    masks,
                    strokes: table,
                    ..base
                },
            },
            ..state.current_entry.clone()
        };

        let registry = ModuleRegistry::builtin();
        let mut connection = Connection::open(&catalog).unwrap();
        let tx = connection.transaction().unwrap();
        let error = insert_entry(&registry, &tx, &default_artifact_root(&catalog), &over)
            .expect_err("past the serialized bound");
        drop(tx);
        drop(connection);
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "recipe masks serialize to {bytes} bytes; the limit is {} serialized mask bytes \
                 per recipe",
                crate::MASK_BYTES_PER_RECIPE
            ),
        );

        // Byte for byte as it was: the bound is checked before anything is written, so the refused
        // entry left neither an entry row nor a stroke in the store.
        let after = std::fs::read(&catalog).unwrap();
        assert_eq!(
            digest(&before),
            digest(&after),
            "the refused write changed the catalog file"
        );
        let service = EditorService::open(&catalog).unwrap();
        let reopened = service.state(&asset).unwrap();
        assert_eq!(reopened.current_entry.id, current);
        assert_eq!(
            service.history(&asset, None, 64).unwrap().entries.len(),
            entries
        );
        drop(service);
        std::fs::remove_file(&catalog).unwrap();
    }

    /// The declared points-per-mask limit is enforced where the strokes are in hand, and names
    /// itself.
    #[test]
    fn a_mask_over_the_points_per_mask_limit_names_the_limit() {
        let catalog = temp("points-per-mask.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let base = state.current_entry.snapshot.recipe.clone();
        drop(service);
        std::fs::remove_file(&catalog).unwrap();

        let registry = ModuleRegistry::builtin();
        let at_bound: Vec<crate::path::Stroke> = (0..STROKES_PER_MASK).map(stroke).collect();
        let points: usize = at_bound.iter().map(crate::path::Stroke::point_count).sum();
        let recipe = |strokes: &[crate::path::Stroke]| {
            let mut table = crate::path::StrokeTable::new("the test session");
            Recipe {
                masks: vec![brush_mask("Mask 1", &mut table, strokes)],
                strokes: table,
                ..base.clone()
            }
        };
        // Under the bound the recipe compiles: the brush kind is evaluable and the limit has not
        // been reached, so nothing refuses.
        assert!(
            registry.compile(64, 48, &recipe(&at_bound)).is_ok(),
            "a mask under the bound should compile"
        );
        assert!(points <= crate::POINTS_PER_MASK);

        let mut over = at_bound.clone();
        while over
            .iter()
            .map(crate::path::Stroke::point_count)
            .sum::<usize>()
            <= crate::POINTS_PER_MASK
        {
            over.push(stroke(over.len()));
        }
        let total: usize = over.iter().map(crate::path::Stroke::point_count).sum();
        let error = registry
            .compile(64, 48, &recipe(&over))
            .err()
            .expect("past the bound");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "mask Mask 1 holds {total} stored path positions; the limit is {} points per mask",
                crate::POINTS_PER_MASK
            )
        );
    }

    /// Masks ride inside the snapshot every entry already stores, so reopen returns them unchanged
    /// and an ordinary later edit carries them with no second persistence path.
    #[test]
    fn masks_ride_in_the_stored_snapshot_and_reopen_unchanged() {
        let catalog = temp("masks.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "basic"),
                "set-basic",
                json!({"exposure": 0.5}),
            )
            .unwrap();
        let state = service.state(&asset).unwrap();
        let mask = stored_mask();
        let entry = masked_entry(&state, &mask, vec![mask.clone()]);
        drop(service);
        plant(&catalog, &entry);

        let mut service = EditorService::open(&catalog).unwrap();
        let reopened = service.state(&asset).unwrap();
        assert_eq!(
            reopened.current_entry, entry,
            "every field of the entry, masks included, survives reopen"
        );
        let stored = &reopened.current_entry.snapshot.recipe.masks[0].components[0];
        assert_eq!(stored.kind, "linear", "the stored kind is kept as it is");
        assert_eq!(
            serde_json::to_string(&stored.payload).unwrap(),
            serde_json::to_string(&mask.components[0].payload).unwrap(),
            "its payload is retained byte for byte, unparsed"
        );
        assert_eq!(
            reopened
                .current_entry
                .snapshot
                .recipe
                .layers
                .last()
                .unwrap()
                .mask
                .as_ref(),
            Some(&mask.id)
        );
        assert_eq!(
            service.entry(&asset, &entry.id).unwrap(),
            entry,
            "the same entry reads the same by identity"
        );
        // The recipe panel still lists every layer, so a masked stack is never hidden from it.
        assert_eq!(
            service.describe_entry(&asset, None).unwrap().layers.len(),
            reopened.current_entry.snapshot.recipe.layers.len()
        );
        // A later mutation writes the mask table on in its own snapshot: one persistence path.
        let next = service
            .apply_pixel(
                &asset,
                mutation(reopened.revision, "pixel"),
                1,
                1,
                [9, 9, 9],
            )
            .unwrap()
            .current_entry_id;
        let committed = service.entry(&asset, &next).unwrap().snapshot.recipe;
        assert_eq!(committed.masks, vec![mask.clone()]);
        assert!(
            committed
                .layers
                .iter()
                .any(|layer| layer.mask.as_ref() == Some(&mask.id)),
            "the masked layer keeps its mask through an unrelated edit"
        );
        // History keeps the earlier unmasked snapshot as it was: nothing was rewritten.
        assert!(
            service
                .entry(&asset, &state.current_entry.id)
                .unwrap()
                .snapshot
                .recipe
                .masks
                .is_empty()
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The same entry with a mask table and no layer bound to anything: the state a client is in
    /// after `mask.create`, which arrives with its own task, so the table is planted here instead.
    fn planted_masks(state: &EditorState, masks: Vec<Mask>) -> HistoryEntry {
        HistoryEntry {
            id: EntryId::new(),
            sequence: state.current_entry.sequence + 1,
            label: "Add linear".into(),
            undo_parent: Some(state.current_entry.id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: state.asset.id.clone(),
                recipe: Recipe {
                    masks,
                    ..state.current_entry.snapshot.recipe.clone()
                },
            },
            ..state.current_entry.clone()
        }
    }

    /// The whole of what the `mask` request field does, through the one action path a GUI gesture and
    /// a JSON client share: it commits the masked layer on the first non-neutral field, updates that
    /// same layer in place afterwards, leaves the global layer alone, places each masked layer after
    /// the global one and in its mask's order, and refuses an action that has no target to give.
    #[test]
    fn the_mask_target_commits_updates_and_orders_one_layer_per_target() {
        let catalog = temp("mask-target.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        // One global Basic layer first, so the ordering rule has something to place a mask after.
        service
            .apply_action(
                &asset,
                mutation(0, "global"),
                "set-basic",
                json!({"exposure": 0.5}),
            )
            .unwrap();
        let state = service.state(&asset).unwrap();
        let first = stored_mask();
        let mut second = stored_mask();
        second.name = "Mask 2".into();
        let planted = planted_masks(&state, vec![first.clone(), second.clone()]);
        drop(service);
        plant(&catalog, &planted);
        let mut service = EditorService::open(&catalog).unwrap();

        fn revision(service: &EditorService, asset: &AssetId) -> u64 {
            service.state(asset).unwrap().revision
        }
        fn layers(service: &EditorService, asset: &AssetId) -> Vec<Layer> {
            service
                .state(asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
                .clone()
        }
        let global_layer = layers(&service, &asset)[0].id.clone();
        assert_eq!(
            layers(&service, &asset).len(),
            1,
            "one global Basic layer to begin with"
        );

        // A neutral first field through a mask commits nothing at all: masking nothing is nothing.
        let quiet = service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "neutral-masked"),
                "set-basic",
                json!({"mask": first.id, "exposure": 0.0}),
            )
            .unwrap();
        assert_eq!(quiet.outcome, MutationOutcome::NoOp);
        assert_eq!(layers(&service, &asset).len(), 1);

        // The first non-neutral field commits the masked layer, after the global one.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "mask-one"),
                "set-basic",
                json!({"mask": first.id, "exposure": 0.8}),
            )
            .unwrap();
        let committed = layers(&service, &asset);
        assert_eq!(committed.len(), 2);
        assert_eq!(committed[0].id, global_layer, "the global layer is first");
        assert_eq!(committed[0].mask, None);
        assert_eq!(committed[1].mask.as_ref(), Some(&first.id));
        let masked_layer = committed[1].id.clone();
        assert_eq!(committed[1].payload["exposure"], json!(0.8));

        // A later field updates that same layer in place, keeping its identity and position.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "mask-one-again"),
                "set-basic",
                json!({"mask": first.id, "exposure": 0.9}),
            )
            .unwrap();
        let updated = layers(&service, &asset);
        assert_eq!(updated.len(), 2, "no second layer for the same target");
        assert_eq!(updated[1].id, masked_layer, "the masked layer's identity");
        assert_eq!(updated[1].payload["exposure"], json!(0.9));
        // And sending the same value again is the no-op it looks like.
        assert_eq!(
            service
                .apply_action(
                    &asset,
                    mutation(revision(&service, &asset), "mask-one-noop"),
                    "set-basic",
                    json!({"mask": first.id, "exposure": 0.9}),
                )
                .unwrap()
                .outcome,
            MutationOutcome::NoOp
        );

        // The global layer is still edited by the same action without the field, and the masked
        // layer is untouched by it.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "global-again"),
                "set-basic",
                json!({"exposure": 0.25}),
            )
            .unwrap();
        let both = layers(&service, &asset);
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].id, global_layer);
        assert_eq!(both[0].payload["exposure"], json!(0.25));
        assert_eq!(both[1].id, masked_layer);
        assert_eq!(both[1].payload["exposure"], json!(0.9));

        // A layer in the second mask is legal for the same single-layer effect and lands after the
        // first mask's, because that is the order the mask list shows.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "mask-two"),
                "set-basic",
                json!({"mask": second.id, "exposure": -1.0}),
            )
            .unwrap();
        let three = layers(&service, &asset);
        assert_eq!(three.len(), 3);
        assert_eq!(
            three
                .iter()
                .map(|layer| layer.mask.clone())
                .collect::<Vec<_>>(),
            vec![None, Some(first.id.clone()), Some(second.id.clone())],
            "the global layer, then the masks in their own order"
        );
        // The stack renders and samples: a masked layer is evaluable the moment it is creatable.
        service.render_current(&asset).unwrap();

        // A target the stack does not hold is refused, and nothing is written.
        let absent = Mask::new("Mask 9");
        let error = service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "absent-mask"),
                "set-basic",
                json!({"mask": absent.id, "exposure": 0.1}),
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            format!("unknown mask {} for this asset", absent.id)
        );

        // The field belongs to the actions of a maskable effect and nowhere else, and the refusal
        // names the action that was asked.
        for (action, parameters) in [
            (
                "set-pixel",
                json!({"mask": first.id, "x": 0, "y": 0, "rgb": [1, 2, 3]}),
            ),
            (
                "transform",
                json!({"mask": first.id, "transform": "rotate-left"}),
            ),
            ("set-vignette", json!({"mask": first.id, "amount": -40.0})),
        ] {
            let error = service
                .apply_action(
                    &asset,
                    mutation(revision(&service, &asset), action),
                    action,
                    parameters,
                )
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{action}");
            assert_eq!(
                error.detail,
                format!("action {action} does not accept a mask target"),
                "{action}"
            );
        }
        assert_eq!(
            layers(&service, &asset).len(),
            3,
            "no refusal changed the stack"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A stored layer naming a mask its own snapshot does not carry is incompatible data: every
    /// evaluation path refuses it by name, the stack stays readable and its stored bytes do not
    /// change. The host cannot write such a stack in the first place, which is asserted here too.
    #[test]
    fn a_stored_layer_naming_a_missing_mask_is_refused_on_every_path_and_left_as_it_is() {
        let catalog = temp("dangling-mask.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "basic"),
                "set-basic",
                json!({"exposure": 0.5}),
            )
            .unwrap();
        let state = service.state(&asset).unwrap();
        let mask = stored_mask();
        let dangling = masked_entry(&state, &mask, Vec::new());
        // Nothing valid can be written from it either: every write validates the whole recipe.
        assert_eq!(
            service
                .registry()
                .validate_recipe(&dangling.snapshot.recipe)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible,
            "insert_entry validates the same recipe before any row is written"
        );
        drop(service);
        plant(&catalog, &dangling);
        let before = stored_entry_json(&catalog, &dangling.id);

        let mut service = EditorService::open(&catalog).unwrap();
        let revision = service.state(&asset).unwrap().revision;
        let expected = format!(
            "layer {} references mask {}, which this recipe does not carry",
            dangling.snapshot.recipe.layers.last().unwrap().id,
            mask.id
        );
        let refusals = [
            service.render_current(&asset).unwrap_err(),
            service.render_entry(&asset, &dangling.id).unwrap_err(),
            service
                .sample_entry(&asset, &dangling.id, 0, 0)
                .unwrap_err(),
            service
                .locate_entry(&asset, &dangling.id, 0, 0)
                .unwrap_err(),
            // The preview worker's own render of the job the owner built for it.
            service
                .preview_job(&asset, None, None, None, None)
                .and_then(|job| {
                    job.source
                        .render(&job.registry, job.entry.snapshot.id.clone(), &job.recipe)
                })
                .unwrap_err(),
            // Planning compiles the stored stack before it asks a module for a plan.
            service
                .apply_pixel(&asset, mutation(revision, "pixel"), 0, 0, [1, 2, 3])
                .unwrap_err(),
            service
                .apply_transform(&asset, mutation(revision, "turn"), Transform::RotateLeft)
                .unwrap_err(),
        ];
        for error in refusals {
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(error.detail, expected);
        }
        // Readable, unchanged and still listed in history: refusing is not discarding.
        let state = service.state(&asset).unwrap();
        assert_eq!(state.current_entry, dangling);
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 3);
        drop(service);
        assert_eq!(
            stored_entry_json(&catalog, &dangling.id),
            before,
            "the refused entry's stored JSON is untouched"
        );
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_format_5_catalog_is_refused_by_name_and_left_untouched() {
        let catalog = temp("format-5.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 1, 1, [9, 8, 7])
            .unwrap();
        drop(service);
        // A format 5 catalog is this schema without the catalog identity and the artifact tables.
        let connection = Connection::open(&catalog).unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER artifact_refs_are_permanent;
                 DROP TABLE artifact_refs;
                 DROP TABLE artifacts;
                 DROP TABLE catalog_meta;
                 PRAGMA user_version=5;",
            )
            .unwrap();
        drop(connection);
        let before = std::fs::read(&catalog).unwrap();
        let error = EditorService::open(&catalog).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            "catalog format 5 is not supported; expected 7; choose a new catalog path"
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
                maskable: false,
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
                layout: crate::ModuleLayout::Stacked,
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
                mask: None,
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
