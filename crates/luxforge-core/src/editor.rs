//! The editor service: [`EditorService`], which owns one catalog, and the types its API speaks.
//!
//! One service, split by concern: `catalog` holds the lock and the journal, the schema, the format
//! marker and the row storage; `history` the admission and the transactions that write history;
//! `source` source preparation, the source cache and RAW settings; `evaluate` preview and analysis
//! jobs, renders and samples; `plan` action planning and drafts; `masks` the `mask.*` commands and
//! mask targets; `describe` the read-only views; `entries` the cache of hydrated entries and asset
//! heads those reads are answered from; and `artifact_store` derived artifacts.
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AssetId, Draft, DraftId, EntryId, Error, HistoryEntry, HistoryRow, LayerId, MaskId,
    ModuleRegistry, Orientation, RenderContext, SnapshotId, StageSize,
    analysis::AnalysisIdentity,
    artifacts::{ArtifactId, LiveArtifacts, PREPARED_ARTIFACT_BYTES, PreparedArtifacts},
    preferences::RawLook,
    source::PreparedSource,
};
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
mod catalog_rows;
mod collapse;
mod describe;
mod entries;
mod evaluate;
mod history;
mod masks;
pub(crate) mod pixels;
mod plan;
mod source;
#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use source::original_work;
#[cfg(test)]
pub(crate) use test_support::{
    distinct_jpeg, mutation, mutation_json, recast_as_raw, synthetic_raw_metadata,
};

pub(crate) use catalog::{
    CATALOG_FORMAT, decode, default_artifact_root, encode, insert_entry, now_ms, write,
};
#[cfg(test)]
pub(crate) use catalog_rows::capture_of;
pub(crate) use catalog_rows::{
    NewAsset, insert_asset, insert_capture, insert_catalog_folder, insert_collection,
    insert_indexed_folder, insert_member, insert_pick, upsert_volume,
};
// The indexed folders and known volumes.
pub(crate) use catalog_rows::folder_rows;
// The library journal's item rows.
pub(crate) use catalog_rows::library_rows;
pub use evaluate::Evaluation;
pub(crate) use evaluate::PointPlan;
pub(crate) use history::{MAX_HISTORY_PAGE, MAX_VERSION_NAME};
pub use masks::MASK_FIELD;
pub(crate) use masks::mask_target_parameter;
pub(crate) use plan::{Prepared as PreparedAction, prefix};
pub use source::RawInterpretation;
pub(crate) use source::{
    FilePreparation, NewPhotograph, Prepared, Preparing, ReadContent, ReadOriginal, SourceWork,
    insert_photograph, original_recipe, original_signature, source_signature,
    source_signature_for_handle,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceKind {
    Jpeg,
    Raw { metadata: RawInterpretation },
}

impl SourceKind {
    /// Source-stage colour capability, shared by API admission and the panel.
    pub fn white_balance_available(&self) -> bool {
        match self {
            Self::Jpeg => true,
            Self::Raw { metadata } => metadata.layout != luxforge_raw::RawLayout::Monochrome,
        }
    }

    /// The kind's tag, exactly as this value serializes it in `kind`.
    pub fn tag(&self) -> SourceTag {
        match self {
            Self::Jpeg => SourceTag::Jpeg,
            Self::Raw { .. } => SourceTag::Raw,
        }
    }
}

/// A source kind without its interpretation: the `kind` tag `asset.state` reports for a photo's
/// source, and what an effect's declared `sources` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceTag {
    Jpeg,
    Raw,
}

impl SourceTag {
    /// Every source kind, in the order the tags are documented.
    pub const ALL: [Self; 2] = [Self::Jpeg, Self::Raw];

    /// The kind as a sentence names it: `a JPEG photo`, `a RAW photo`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Raw => "RAW",
        }
    }

    /// The tag exactly as `kind` serializes it, and as the catalog stores it in its own column
    /// beside the interpretation, so a listing reads the kind without decoding the interpretation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Raw => "raw",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tag| tag.as_str() == value)
    }
}

/// What reads cost the catalog, counted per thread, for the tests that prove a cached read decodes
/// and hashes nothing: every JSON decode of a stored value and every stroke address computed, and,
/// asked for separately by `take_queried`, every stroke row looked up in the store.
#[cfg(test)]
pub(crate) mod read_counts {
    use std::cell::Cell;

    thread_local! {
        static DECODED: Cell<u64> = const { Cell::new(0) };
        static HASHED: Cell<u64> = const { Cell::new(0) };
        static QUERIED: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn decoded() {
        DECODED.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn hashed() {
        HASHED.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn queried() {
        QUERIED.with(|count| count.set(count.get() + 1));
    }

    /// The decodes and the stroke hashes this thread made since it last asked.
    pub(crate) fn take() -> (u64, u64) {
        (
            DECODED.with(|count| count.replace(0)),
            HASHED.with(|count| count.replace(0)),
        )
    }

    /// The stroke rows this thread looked up in the store since it last asked.
    pub(crate) fn take_queried() -> u64 {
        QUERIED.with(|count| count.replace(0))
    }
}

/// How many stroke rows a commit issued to the content-addressed store, counted per thread, for the
/// test that proves a commit writes only the strokes captured since the entry's recipe was last
/// read — not the whole mask table's references, most of which are already durable.
#[cfg(test)]
pub(crate) mod stroke_writes {
    use std::cell::Cell;

    thread_local! {
        static WRITTEN: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn written() {
        WRITTEN.with(|count| count.set(count.get() + 1));
    }

    /// The stroke rows written this thread since it last asked.
    pub(crate) fn take() -> u64 {
        WRITTEN.with(|count| count.replace(0))
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
    /// The entry an edit of the same control superseded, which auto-collapse hid from history: the
    /// new entry continues from that entry's undo parent, or, when the edit returned the control to
    /// where the chain began, no entry was written and the head moved back to that parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed_entry_id: Option<EntryId>,
}

/// What one module's first-open action came to when a photograph was first opened
/// ([`crate::ToolModule::first_open`]): the entry it committed by the `system` actor, or why it
/// committed nothing. `job.read` of that preparation lists one per module that proposed an action
/// or could not plan one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstOpen {
    pub module_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<EntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::jobs::JobError>,
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
    pub(crate) removed_layers: Vec<crate::mask::commands::RemovedLayer>,
    /// The settings a composite action — a preset — left out because they do not apply to the
    /// photo: a step whose module does not apply to its kind, and a field superseded on its global
    /// target. A skip is not a refusal; a composite that applies nothing is a no-op that still
    /// reports what it skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedSetting>,
}

/// One setting a composite action did not apply to a photo, and why: the step's action, the field
/// when only that field was left out, and the refusal the host would have given it alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedSetting {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
    pub reason: String,
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
            skipped: Vec::new(),
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
    /// The renderer that drew the pixel: the GPU, or the reference renderer and why the GPU did
    /// not draw it, as a session names its renderer ([`crate::Renderer`]).
    pub renderer: crate::Renderer,
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
    /// The renderer that drew the pixel, as [`PixelSample::renderer`] names it.
    pub renderer: crate::Renderer,
}

/// Which evaluated stack an analysis job should describe: the asset's current entry, one frozen
/// historical entry, the caller's own open draft, or one historical entry framed by another
/// entry's geometry.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AnalysisSelection<'a> {
    Current,
    Entry(&'a EntryId),
    Draft(&'a Draft),
    /// `entry`'s stack with its geometry layers (orientation, straighten, crop) replaced by those
    /// of `geometry`, both entries of one asset: what Compare shows, so the Original is framed as
    /// the entry the client was looking at and only the adjustments differ. Read-only; nothing is
    /// committed.
    Framed {
        entry: &'a EntryId,
        geometry: &'a EntryId,
    },
}

impl<'a> AnalysisSelection<'a> {
    /// One saved entry, framed by `geometry`'s geometry when one is given.
    pub(crate) fn framed(entry: &'a EntryId, geometry: Option<&'a EntryId>) -> Self {
        match geometry {
            Some(geometry) => Self::Framed { entry, geometry },
            None => Self::Entry(entry),
        }
    }
}

/// One planned analysis job: the identity that names its result, and the evaluation its worker
/// renders or the reason the effective recipe has no output stage the host can evaluate — an
/// unavailable provider, or a payload the registry refuses — in which case the job is recorded
/// failed and no worker is started. A stack without an output stage has nothing to render, so its
/// original is never prepared for it.
#[derive(Debug)]
pub(crate) struct AnalysisPlan {
    pub identity: AnalysisIdentity,
    pub evaluation: Result<Evaluation, Error>,
}

/// One saved entry's export, frozen on the catalog owner: whatever is committed afterwards, the job
/// renders exactly this evaluation, whose recipe carries the verified bytes of the artifacts it
/// references and whose source shares the cached allocation.
pub(crate) struct ExportPlan {
    /// The asset, entry, snapshot, recipe hash, source fingerprint and output stage.
    pub identity: AnalysisIdentity,
    pub evaluation: Evaluation,
    /// The original's supported EXIF fields, read once when its source was prepared.
    pub capture: Arc<crate::export::CaptureMetadata>,
}

/// What `export.plan` answers from: the entry's evaluation, which reads no source, and the
/// original's path.
pub(crate) struct ExportTarget {
    pub evaluation: Evaluation<()>,
    pub original: PathBuf,
}

/// One asset's current entry bound for point sampling on a worker, as the catalog owner found it
/// at request time: the samples describe that entry whatever is committed meanwhile. Its recipe
/// carries the artifacts its stack binds, and it shares the source's allocation.
pub(crate) struct SamplePlan {
    evaluation: Evaluation,
}

impl SamplePlan {
    /// The pixels at the centres of a `side` × `side` grid over the entry's output stage, row by
    /// row from the top-left, from the evaluation's one compilation: `O(side² × layers)` and no
    /// frame. `checkpoint` is asked before each point.
    pub(crate) fn grid(
        &self,
        side: u32,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Vec<[u8; 4]>, Error> {
        self.evaluation
            .exact(&crate::Cancel::never())?
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
    /// ([`crate::ModuleRegistry::layer_report`]): a neutral field patch, a whole-image crop, the
    /// identity orientation or a RAW development at As shot and 0 EV. False for a layer whose
    /// provider is missing or unavailable or whose payload it cannot read. A client's "edited" mark
    /// reads this rather than parsing a payload.
    pub neutral: bool,
    /// The stage this layer receives: the source's extents for the first layer and the output of
    /// the layers before it for every later one, by the core's own stage fold
    /// (`crate::ModuleRegistry::stages`). It is the stage the layer's payload addresses, so
    /// a client reads a crop's pixel rectangle or its ratio from it without folding geometry itself.
    /// `null` for every layer after one whose output cannot be known: a missing or unavailable
    /// provider, or a payload its provider cannot compile.
    #[serde(default)]
    pub input_stage: Option<StageSize>,
    /// The exact orientation — reflection and quarter turns — the orientation layers before this
    /// one gave the stage it receives, composed in stack order by the core. A client that holds a
    /// frame on this stage carries it through a turn committed ahead of it from this, without
    /// reading an orientation payload. `null` exactly when `input_stage` is, or after an
    /// orientation layer whose payload cannot be read.
    #[serde(default)]
    pub input_orientation: Option<Orientation>,
}

/// One entry's ordered layers with their provider and summary. Reading only: no source, no render.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeDescription {
    pub entry_id: EntryId,
    pub layers: Vec<LayerDescription>,
    /// The stage the whole stack produces, by the same fold as each row's `input_stage`: what a
    /// layer appended to the stack would receive. `null` when some layer's output cannot be known.
    #[serde(default)]
    pub output_stage: Option<StageSize>,
    /// The orientation the whole stack gives its output, as each row's `input_orientation` does
    /// for that row; `null` exactly when `output_stage` is, or when an orientation cannot be read.
    #[serde(default)]
    pub output_orientation: Option<Orientation>,
    /// Bounded geometry diagnostics from the same compilation the renderer uses.
    #[serde(default)]
    pub geometry: Option<Value>,
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
pub(crate) struct VersionResult {
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
    /// The photograph whose original was prepared.
    pub(crate) asset_id: AssetId,
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
    /// The original's kept EXIF fields, which the development carries on unchanged.
    pub(crate) capture: Arc<crate::export::CaptureMetadata>,
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
    /// A RAW source's second development, which goes with the source it develops.
    second: crate::source::SecondDevelopment,
}

#[derive(Debug)]
pub struct EditorService {
    /// The catalog. The preset library in `presets::library` keeps its own table here.
    pub(crate) connection: Connection,
    /// Hydrated entries and asset heads, so a read after the first decodes nothing. Every write
    /// that moves a head updates it where it commits.
    entries: RefCell<entries::EntryCache>,
    source_cache: RefCell<Option<CachedSource>>,
    /// What the last one-file Develop read of its file, kept for the preparation that follows it
    /// (the open): the next preparation takes it ([`source::ReadOriginal`]).
    read_original: RefCell<Option<source::ReadOriginal>>,
    pub(crate) pixel_reads: RefCell<pixels::PixelReads>,
    registry: Arc<ModuleRegistry>,
    /// The budgets every evaluation this service plans shares: its own
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
    /// Where this catalog's index lives: `<catalog stem>.index` beside the catalog file.
    index_dir: PathBuf,
    /// The index database, opened on first use ([`Self::index`]) rather than with the catalog, so
    /// a catalog that never browses a file gets no index directory.
    index: RefCell<Option<crate::index::IndexDb>>,
    /// Whether an edit that sets the same control as the entry before it collapses that entry
    /// ([`Self::set_auto_collapse`]). Off until the host sets it from the person's preference.
    auto_collapse: bool,
    /// Whether a new asset's first open asks the lens module for its action
    /// ([`Self::set_auto_lens_profile`]). On until the host sets it from the person's preference.
    auto_lens_profile: bool,
    /// The look a new RAW photograph's Original starts from, which modules read when the host
    /// builds it ([`Self::set_raw_look`]). Standard until the host sets it from the person's
    /// preference.
    raw_look: RawLook,
}

impl EditorService {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::open_with(path, Arc::new(ModuleRegistry::builtin()))
    }

    /// Open a catalog served by a specific set of providers. Registration is cheap and happens
    /// before any catalog or image work.
    pub fn open_with(path: &Path, registry: Arc<ModuleRegistry>) -> Result<Self, Error> {
        // Control variants name other modules, so they are checked once the set is complete.
        registry.check_complete()?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::catalog(format!("cannot create catalog directory: {}", e.kind()))
            })?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(100))?;
        // The format is checked under the lock and before the journal is set, so a refused file —
        // an unsupported catalog, or a database that is not one — keeps every byte, its journal
        // mode included.
        let version = catalog::lock(&connection)?;
        match version {
            0 => catalog::require_empty(&connection)?,
            CATALOG_FORMAT => {}
            other => {
                return Err(Error::incompatible(format!(
                    "catalog format {other} is not supported; expected {CATALOG_FORMAT}; choose a new catalog path"
                )));
            }
        }
        // A commit is durable wherever durable writes reach the drive (`atomic_file::FLUSHES`),
        // which only a test build turns off.
        catalog::configure(&connection, crate::atomic_file::FLUSHES)?;
        if version == 0 {
            Self::create_schema(&mut connection, &uuid::Uuid::new_v4().simple().to_string())?;
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
        // The roots follow the catalog file, so moving them together keeps them valid.
        let artifact_root = default_artifact_root(path);
        let index_dir = crate::index::index_dir(path);
        Ok(Self {
            connection,
            // Opening starts empty: nothing read before a reopen is trusted after it.
            entries: RefCell::new(entries::EntryCache::default()),
            source_cache: RefCell::new(None),
            read_original: RefCell::new(None),
            pixel_reads: RefCell::new(pixels::PixelReads::default()),
            registry,
            render: RenderContext::new(),
            catalog_id,
            artifact_root,
            prepared_artifacts: RefCell::new(PreparedArtifacts::new(PREPARED_ARTIFACT_BYTES)),
            checked_manifest: RefCell::new(None),
            live_artifacts: LiveArtifacts::default(),
            index_dir,
            index: RefCell::new(None),
            auto_collapse: false,
            auto_lens_profile: true,
            raw_look: RawLook::default(),
        })
    }

    /// Collapse history from now on, or stop: the person's "Auto collapse history" preference,
    /// which the catalog owner sets when it starts and whenever the preference changes. An edit
    /// already written stays as it was either way.
    pub fn set_auto_collapse(&mut self, enabled: bool) {
        self.auto_collapse = enabled;
    }

    /// Whether edits collapse history now ([`Self::set_auto_collapse`]).
    pub fn auto_collapse(&self) -> bool {
        self.auto_collapse
    }

    /// Commit a new RAW photo's detected lens profile when its first preparation first opens it, or stop: the
    /// person's "Correct lens distortion on new RAW photos" preference, which the catalog owner
    /// sets when it starts and whenever the preference changes. Off, the lens module is not asked
    /// for a first-open action, so the Lens section offers the detected profile instead. A photo
    /// already in the catalog keeps its history either way.
    pub fn set_auto_lens_profile(&mut self, enabled: bool) {
        self.auto_lens_profile = enabled;
    }

    /// Whether a new asset's first open asks the lens module now ([`Self::set_auto_lens_profile`]).
    pub fn auto_lens_profile(&self) -> bool {
        self.auto_lens_profile
    }

    /// Start new RAW photographs from this look: the person's "Starting look for new RAW photos"
    /// preference, which the catalog owner sets when it starts and whenever the preference
    /// changes. Modules read it through [`crate::OriginalContext`] when the host builds a new
    /// photograph's Original ([`crate::ToolModule::original`]), so it applies to photographs
    /// created from then on. A photo already in the catalog keeps its history either way.
    pub fn set_raw_look(&mut self, look: RawLook) {
        self.raw_look = look;
    }

    /// The look new RAW photographs start from now ([`Self::set_raw_look`]).
    pub fn raw_look(&self) -> RawLook {
        self.raw_look
    }

    /// The providers this service validates, plans and renders with.
    pub fn registry(&self) -> &Arc<ModuleRegistry> {
        &self.registry
    }

    /// The render context every evaluation this service plans reads.
    pub fn render_context(&self) -> &RenderContext {
        &self.render
    }

    /// This catalog's identity.
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    /// Where this catalog's index lives, whether or not it has been opened.
    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    /// The catalog's index database, opened on first use: created when there is none, and
    /// discarded and recreated when it cannot be used, never touching the catalog
    /// ([`crate::IndexDb::open`]).
    pub fn index(&self) -> Result<std::cell::RefMut<'_, crate::index::IndexDb>, Error> {
        let mut slot = self.index.borrow_mut();
        if slot.is_none() {
            let (index, _) = crate::index::IndexDb::open(&self.index_dir, &self.catalog_id)?;
            *slot = Some(index);
        }
        Ok(std::cell::RefMut::map(slot, |slot| {
            slot.as_mut().expect("the index was opened above")
        }))
    }

    /// The catalog's index database when it is open, never opening it: the catalog owner's index
    /// lane reads the index only once a thread of the index lane has opened it
    /// ([`Self::adopt_index`]).
    pub(crate) fn index_open(&self) -> Option<std::cell::RefMut<'_, crate::index::IndexDb>> {
        std::cell::RefMut::filter_map(self.index.borrow_mut(), Option::as_mut).ok()
    }

    /// Take an index opened elsewhere, off the owner, as this service's own, unless one is open
    /// already.
    pub(crate) fn adopt_index(&self, index: crate::index::IndexDb) {
        let mut slot = self.index.borrow_mut();
        if slot.is_none() {
            *slot = Some(index);
        }
    }
}
