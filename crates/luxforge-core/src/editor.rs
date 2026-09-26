//! The editor service: [`EditorService`], which owns one catalog, and the types its API speaks.
//!
//! One service, split by concern: `catalog` holds the schema, the format marker and the row
//! storage; `history` the admission and the transactions that write history; `source` source
//! preparation, the source cache and RAW settings; `evaluate` preview and analysis jobs, renders
//! and samples; `plan` action planning and drafts; `masks` the `mask.*` commands and mask targets;
//! `describe` the read-only views; `entries` the cache of hydrated entries and asset heads those
//! reads are answered from; and `artifact_store` derived artifacts.
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AssetId, Draft, DraftId, EntryId, Error, HistoryEntry, HistoryRow, LayerId, MaskId,
    ModuleRegistry, PreviewSource, Recipe, RenderContext, RenderOptions, SnapshotId, StageSize,
    analysis::AnalysisIdentity,
    artifacts::{ArtifactId, LiveArtifacts, PreparedArtifacts},
    source::PreparedSource,
};
use catalog::{CATALOG_FORMAT, default_artifact_root};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

mod artifact_store;
#[cfg(test)]
mod artifact_tests;
mod catalog;
mod describe;
mod entries;
mod evaluate;
mod history;
mod masks;
mod plan;
mod source;
#[cfg(test)]
mod test_support;

pub(crate) use catalog::{decode, encode, now_ms, write};
pub(crate) use evaluate::PointPlan;
pub use masks::{MASK_FIELD, mask_target_parameter};
pub(crate) use plan::prefix;
pub use source::RawInterpretation;
pub(crate) use source::{FilePreparation, source_signature};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceKind {
    Jpeg,
    Raw { metadata: RawInterpretation },
}

/// What reads cost the catalog, counted per thread, for the tests that prove a cached read decodes
/// and hashes nothing: every JSON decode of a stored value and every stroke address computed.
#[cfg(test)]
pub(crate) mod read_counts {
    use std::cell::Cell;

    thread_local! {
        static DECODED: Cell<u64> = const { Cell::new(0) };
        static HASHED: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn decoded() {
        DECODED.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn hashed() {
        HASHED.with(|count| count.set(count.get() + 1));
    }

    /// The decodes and the stroke hashes this thread made since it last asked.
    pub(crate) fn take() -> (u64, u64) {
        (
            DECODED.with(|count| count.replace(0)),
            HASHED.with(|count| count.replace(0)),
        )
    }
}

/// How many stacks were validated against the registry, counted per thread, for the test that proves
/// a commit validates the stack it writes once.
#[cfg(test)]
pub(crate) mod validations {
    use std::cell::Cell;

    thread_local! {
        static VALIDATED: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn validated() {
        VALIDATED.with(|count| count.set(count.get() + 1));
    }

    /// The validations this thread made since it last asked.
    pub(crate) fn take() -> u64 {
        VALIDATED.with(|count| count.replace(0))
    }
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

/// What one action answers with, and what the request table stores for its retry: the mutation
/// result, flattened so `outcome`, `revision` and `deduplicated` read exactly where every other
/// mutation puts them, and what a host action says it touched.
///
/// A module action touches one layer the stack already names, so it reports the envelope alone and
/// its answer serializes exactly as a [`MutationResult`]. A `mask.*` command reports the label it
/// committed, the mask and component it addressed or minted, and the layers a destructive delete
/// removed. The whole answer is stored with the request in the same transaction as the change, so a
/// deduplicated retry answers with the identities the first attempt created rather than
/// reconstructing them; a no-op wrote no entry and reports the envelope alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    #[serde(flatten)]
    pub mutation: MutationResult,
    /// The history label a host command committed, which is the durable record of what it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<crate::ComponentId>,
    /// The layers a `mask.delete` removed. A destructive command says what it removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_layers: Vec<crate::mask::commands::RemovedLayer>,
}

impl ActionResult {
    /// The envelope and nothing else: a module action, a navigation, a no-op.
    pub(crate) fn plain(mutation: MutationResult) -> Self {
        Self {
            mutation,
            label: None,
            mask: None,
            component: None,
            removed_layers: Vec::new(),
        }
    }
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
    pub context: RenderContext,
    /// The effective recipe, bound with the verified bytes of every artifact it references, which
    /// the job holds while it runs.
    pub recipe: Recipe,
    pub failure: Option<Error>,
}

/// One asset's current entry bound for point sampling on a worker, as the catalog owner found it
/// at request time: the samples describe that entry whatever is committed meanwhile. Its recipe
/// carries the artifacts its stack binds, and it shares the source's allocation.
pub(crate) struct SamplePlan {
    source: PreviewSource,
    registry: Arc<ModuleRegistry>,
    context: RenderContext,
    recipe: Recipe,
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
        crate::render(
            &self.registry,
            &self.source,
            &self.recipe,
            RenderOptions::default(),
            &self.context,
        )?
        .grid(side, checkpoint)
    }
}

/// Which draft, at which revision, a sample, a preview or an analysis was evaluated against.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftStamp {
    pub draft_id: DraftId,
    pub draft_revision: u64,
}

/// One page of an asset's history rows, newest first. `next_before_sequence` continues it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryPage {
    pub entries: Vec<HistoryRow>,
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
    /// Whether the stored layer changes nothing, as its provider answers
    /// ([`crate::ModuleRegistry::layer_neutral`]): a neutral field patch, a whole-image crop, the
    /// identity orientation or a RAW development at As shot and 0 EV. False for a layer whose
    /// provider is missing or unavailable or whose payload it cannot read. A client's "edited" mark
    /// reads this rather than parsing a payload.
    pub neutral: bool,
    /// The stage this layer receives: the source's extents for the first layer and the output of
    /// the layers before it for every later one, by the core's own stage fold
    /// ([`crate::ModuleRegistry::input_stages`]). It is the stage the layer's payload addresses, so
    /// a client reads a crop's pixel rectangle or its ratio from it without folding geometry itself.
    /// `null` for every layer after one whose output cannot be known: a missing or unavailable
    /// provider, or a payload its provider cannot compile.
    #[serde(default)]
    pub input_stage: Option<StageSize>,
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
    pub(crate) sensor: Arc<luxforge_raw::RawSource>,
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
    /// Hydrated entries and asset heads, so a read after the first decodes nothing. Every write
    /// that moves a head updates it where it commits.
    entries: RefCell<entries::EntryCache>,
    source_cache: RefCell<Option<CachedSource>>,
    allow_sync_source: bool,
    registry: Arc<ModuleRegistry>,
    /// The budgets and the estimate store every evaluation this service plans shares: its own
    /// samples and exports, and the preview and analysis jobs it hands to workers.
    render: RenderContext,
    /// This catalog's own identity, which its artifact root's manifest must name.
    catalog_id: String,
    /// Where this catalog's artifacts live: `<catalog stem>.artifacts` beside the catalog file. It
    /// moves with the catalog.
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
                Error::catalog(format!("cannot create catalog directory: {}", e.kind()))
            })?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA synchronous=FULL;
             PRAGMA locking_mode=EXCLUSIVE;
             BEGIN IMMEDIATE;
             COMMIT;",
        )?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => Self::create_schema(&mut connection)?,
            CATALOG_FORMAT => {}
            other => {
                return Err(Error::incompatible(format!(
                    "catalog format {other} is not supported; expected {CATALOG_FORMAT}; choose a new catalog path"
                )));
            }
        }
        let catalog_id: Option<String> = connection
            .query_row(
                "SELECT value FROM catalog_meta WHERE key='catalog_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let catalog_id = catalog_id.ok_or_else(|| {
            Error::incompatible("catalog has no identity; choose a new catalog path")
        })?;
        // The root follows the catalog file, so moving both together keeps it valid.
        let artifact_root = default_artifact_root(path);
        Ok(Self {
            connection,
            // Opening starts empty: nothing read before a reopen is trusted after it.
            entries: RefCell::new(entries::EntryCache::default()),
            source_cache: RefCell::new(None),
            allow_sync_source: true,
            registry,
            render: RenderContext::new(),
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

    /// The render context every evaluation this service plans reads.
    pub fn render_context(&self) -> &RenderContext {
        &self.render
    }
}
