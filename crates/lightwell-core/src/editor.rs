use crate::{
    AssetId, ContentPoint, EntryId, Error, ErrorKind, HistoryEntry, Layer, LayerId, ModuleRegistry,
    Mutation, PreviewJob, Raster, Snapshot, SnapshotId, SourceImage, Transform, locate,
    modules::{
        ActionInput, ActionPlan, EffectStage, Stage, StageContext, action_label, check_parameters,
    },
    open_source, render,
    render::Evaluation,
    sample,
};
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::Metadata,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Entries store their rendered label, so a catalog written before format 3 is refused by name.
const CATALOG_FORMAT: i64 = 3;
const MAX_HISTORY_PAGE: usize = 100;
const MAX_VERSION_NAME: usize = 64;
const ASSET_COLUMNS: &str =
    "id,source_root,locator,fingerprint,file_identity,byte_len,width,height";

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
    pub available: bool,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceSignature {
    byte_len: u64,
    modified: Option<SystemTime>,
    file_identity: String,
    change_marker: Option<(i128, i128)>,
}

#[derive(Clone, Debug)]
struct CachedSource {
    asset_id: AssetId,
    signature: SourceSignature,
    source: SourceImage,
}

#[derive(Debug)]
pub struct EditorService {
    connection: Connection,
    source_cache: RefCell<Option<CachedSource>>,
    registry: Arc<ModuleRegistry>,
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
        Ok(Self {
            connection,
            source_cache: RefCell::new(None),
            registry,
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
                    height INTEGER NOT NULL
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
                 PRAGMA user_version={CATALOG_FORMAT};
                 COMMIT;"
            ))
            .map_err(catalog_error)
    }

    pub fn import(&mut self, path: &Path) -> Result<EditorState, Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot resolve source: {}", e.kind()),
            )
        })?;
        let before = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.to_string()))?;
        let identity = file_identity(&before, &canonical);
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
            return self.state(&AssetId::parse(existing)?);
        }
        let source = open_source(&canonical)?;
        let after = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.to_string()))?;
        if source_signature(&canonical, &before) != source_signature(&canonical, &after) {
            return Err(Error::new(
                ErrorKind::Conflict,
                "source changed while it was being imported",
            ));
        }
        let asset = AssetRecord {
            id: AssetId::new(),
            source_root: canonical.parent().unwrap_or(Path::new("")).to_path_buf(),
            locator: canonical.clone(),
            fingerprint: source.fingerprint.clone(),
            file_identity: identity,
            byte_len: after.len(),
            width: source.width,
            height: source.height,
        };
        let snapshot = Snapshot::original(asset.id.clone());
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
            "INSERT INTO assets VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
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
            ],
        )
        .map_err(catalog_error)?;
        insert_entry(&self.registry, &tx, &entry)?;
        tx.execute(
            "INSERT INTO asset_state VALUES (?1,?2,0,'[]')",
            params![asset.id.as_str(), entry.id.as_str()],
        )
        .map_err(catalog_error)?;
        tx.commit().map_err(catalog_error)?;
        self.source_cache.replace(Some(CachedSource {
            asset_id: asset.id.clone(),
            signature: source_signature(&canonical, &after),
            source,
        }));
        Ok(EditorState {
            asset,
            revision: 0,
            current_entry: entry,
            redo: Vec::new(),
        })
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
                    available: false,
                },
                Some((module, _)) => {
                    let descriptor = module.descriptor();
                    let (summary, available) = match &descriptor.availability {
                        crate::Availability::Unavailable { reason } => {
                            (format!("unavailable: {reason}"), false)
                        }
                        // A payload the provider cannot read is reported on its own row; the
                        // rest of the stack is still described.
                        crate::Availability::Available => match module.describe_layer(
                            &layer.effect_id,
                            layer.effect_format,
                            &layer.payload,
                        ) {
                            Ok(summary) => (summary, true),
                            Err(error) => (format!("unreadable payload: {}", error.detail), false),
                        },
                    };
                    LayerDescription {
                        id: layer.id.clone(),
                        effect: layer.effect_id.clone(),
                        module: Some(descriptor.id.clone()),
                        title: Some(descriptor.title.clone()),
                        summary,
                        available,
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

    /// A preview job for one entry. `layer_count` truncates the rendered stack to its first `n`
    /// layers, which the desktop uses to show a layer's input stage while drafting it; it must not
    /// exceed the entry's layer count.
    pub fn preview_job(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
        layer_count: Option<usize>,
    ) -> Result<PreviewJob, Error> {
        let state = self.state(asset_id)?;
        let entry = match entry_id {
            Some(entry_id) => self.entry(asset_id, entry_id)?,
            None => state.current_entry,
        };
        let layers = entry.snapshot.recipe.layers.len();
        if let Some(count) = layer_count.filter(|count| *count > layers) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("preview layer count {count} exceeds the {layers} layers of this entry"),
            ));
        }
        Ok(PreviewJob {
            source: self.verified_source(&state.asset)?,
            entry,
            registry: self.registry.clone(),
            layer_count,
        })
    }

    pub fn render_entry(&self, asset_id: &AssetId, entry_id: &EntryId) -> Result<Raster, Error> {
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let source = self.verified_source(&state.asset)?;
        render(
            &self.registry,
            &source,
            entry.snapshot.id.clone(),
            &entry.snapshot.recipe,
        )
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
        let source = self.verified_source(&state.asset)?;
        let sampled = sample(&self.registry, &source, &entry.snapshot.recipe, x, y)?;
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
            source_fingerprint: source.fingerprint,
            width: sampled.width,
            height: sampled.height,
            x,
            y,
            rgba,
        })
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
        let source = self.verified_source(&state.asset)?;
        locate(&self.registry, &source, &entry.snapshot.recipe, x, y)
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
        // The label comes from the action that was requested, which is not always the durable
        // action identity the entry stores: `transform` renders the label, `rotate-left` is stored.
        let label = action_label(action, &input.parameters);
        let request = request_input(&input, &mutation)?;
        if let Some(result) = self.request_result(asset_id, &mutation.request_id, &request)? {
            return Ok(result);
        }
        let state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        let source = self.verified_source(&state.asset)?;
        // The stack is compiled once; planning answers point queries and never rasterizes.
        let recipe = &state.current_entry.snapshot.recipe;
        let evaluation = Evaluation::new(&registry, &source, recipe)?;
        let sampler =
            |x: u32, y: u32| -> Result<Option<[u8; 4]>, Error> { Ok(evaluation.pixel(x, y)) };
        // The stage one layer receives: compile the prefix before it. Compiling folds declared
        // output stages and allocates only the operation lists, so this copies no part of the stack
        // and rasterizes nothing. The whole recipe compiled above, so its format is known good.
        let stage_before = |index: usize| -> Result<Stage, Error> {
            Ok(registry
                .compile_layers(source.width, source.height, prefix(&recipe.layers, index)?)?
                .stage())
        };
        // One pixel of the stage a prefix produces, for a module planning against the position its
        // layer will take. Compiling the prefix costs O(layers) and the evaluation answers the
        // point per segment, so nothing is rasterized here either.
        let sample_before = |index: usize, x: u32, y: u32| -> Result<Option<[u8; 4]>, Error> {
            let prefix = prefix(&recipe.layers, index)?;
            Ok(Evaluation::over_layers(&registry, &source, prefix)?.pixel(x, y))
        };
        let insertion_index = |stage: EffectStage| registry.insertion_index(&recipe.layers, stage);
        let context = StageContext {
            stage: evaluation.stage(),
            layers: &recipe.layers,
            sampler: &sampler,
            stage_before: &stage_before,
            insertion_index: &insertion_index,
            sample_before: &sample_before,
        };
        let snapshot = match module.plan(&input, &context)? {
            ActionPlan::NoOp => {
                return self.persist_noop(asset_id, &mutation, &request, &state);
            }
            // The host places the layer: a pixel-stage effect goes before the geometry tail, so a
            // later crop change carries it instead of moving or invalidating it. An effect no
            // provider declares is appended and rejected by the whole-stack compile below.
            ActionPlan::Commit(layer) => {
                let index = insertion_index(
                    registry
                        .effect_stage(&layer.effect_id)
                        .unwrap_or(EffectStage::Geometry),
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
            &source,
            CommittedAction { input, label },
        )
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

    fn verified_source(&self, asset: &AssetRecord) -> Result<SourceImage, Error> {
        let before = asset.locator.metadata().map_err(|_| {
            Error::new(
                ErrorKind::SourceUnavailable,
                "original source is unavailable",
            )
        })?;
        let signature = source_signature(&asset.locator, &before);
        if signature.byte_len != asset.byte_len
            || signature.byte_len > 128 * 1024 * 1024
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

        let source = open_source(&asset.locator).map_err(|_| {
            Error::new(
                ErrorKind::SourceUnavailable,
                "original source is unavailable or changed",
            )
        })?;
        let after = asset.locator.metadata().map_err(|_| {
            Error::new(
                ErrorKind::SourceUnavailable,
                "original source is unavailable",
            )
        })?;
        let after_signature = source_signature(&asset.locator, &after);
        if after_signature != signature || source.fingerprint != asset.fingerprint {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source fingerprint changed",
            ));
        }
        self.source_cache.replace(Some(CachedSource {
            asset_id: asset.id.clone(),
            signature,
            source: source.clone(),
        }));
        Ok(source)
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
    fn commit_snapshot(
        &mut self,
        asset_id: &AssetId,
        mutation: Mutation,
        request: Value,
        snapshot: Snapshot,
        source: &SourceImage,
        action: CommittedAction,
    ) -> Result<MutationResult, Error> {
        let mut state = self.state(asset_id)?;
        ensure_revision(&state, mutation.expected_revision)?;
        self.registry.validate_recipe(&snapshot.recipe)?;
        Evaluation::new(&self.registry, source, &snapshot.recipe)?;
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
        insert_entry(&self.registry, &tx, &entry)?;
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
        insert_entry(&self.registry, &tx, &entry)?;
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

fn insert_entry(
    registry: &ModuleRegistry,
    tx: &Transaction<'_>,
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

fn source_signature(path: &Path, metadata: &Metadata) -> SourceSignature {
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
        Processing, Stage, ToolModule,
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
        let first = service.preview_job(&state.asset.id, None, None).unwrap();
        let second = service.preview_job(&state.asset.id, None, None).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &first.source.rgba,
            &second.source.rgba
        ));
        let raster = render(
            service.registry(),
            &first.source,
            first.entry.snapshot.id.clone(),
            &first.entry.snapshot.recipe,
        )
        .unwrap();
        assert!(std::sync::Arc::ptr_eq(&first.source.rgba, &raster.rgba));
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
        service.preview_job(&asset, None, None).unwrap();
        let replacement =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-2.jpg");
        assert_eq!(
            std::fs::metadata(&source).unwrap().len(),
            std::fs::metadata(&replacement).unwrap().len()
        );
        std::fs::copy(replacement, &source).unwrap();
        assert_eq!(
            service.preview_job(&asset, None, None).unwrap_err().kind,
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
            "catalog format 2 is not supported; expected 3; choose a new catalog path"
        );
        assert_eq!(
            std::fs::read(&catalog).unwrap(),
            before,
            "a refused catalog is left byte for byte as it was"
        );
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
                notes: "test".into(),
            };
            let action = |id: &str| ActionDescriptor {
                id: id.into(),
                title: "Shrink".into(),
                notes: "test".into(),
                summary: Some("Shrink {width}x{height}".into()),
                parameters: vec![extent("width"), extent("height")],
            };
            let effect = |id: &str| EffectDescriptor {
                id: id.into(),
                format: EFFECT_FORMAT,
                stage: EffectStage::Geometry,
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
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                availability: Availability::Available,
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
            let job = service.preview_job(&asset, None, layer_count).unwrap();
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
            .preview_job(&asset, None, Some(3))
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
                    sample(&registry, &source, &recipe, x, y).unwrap().rgba,
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
