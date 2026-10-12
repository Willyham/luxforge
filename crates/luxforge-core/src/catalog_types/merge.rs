//! Merges and their stacks (`docs/design/hdr-merge.md`, "The stack and the originals", "Remove,
//! undo and retention"; `docs/design/panorama-merge.md`, "Generalizing the merge boundary"): the
//! kinds of merge, the immutable kind-tagged provenance a merge records with its result, the
//! result-led stack of its exact inputs, and the lifecycle state the library journal changes.
//!
//! A merge is identified by its [`MergeId`], which is also its stack's identity: neither depends
//! on an index [`FileId`](super::FileId) or a computed moment, so removing or rebuilding the index
//! changes neither. Its inputs are original files, named by path, volume and [`FileSignature`],
//! each optionally the catalog photograph it was developed into; never another merge's result.
use super::{CatalogFolderId, FileSignature, VolumeId, jobs::CatalogJob};
use crate::{
    AssetId, DerivedSourceId, Error,
    merge::hdr::{
        AlignmentMode, AlignmentOptions, Coverage, DeghostMode, ExposureOptions, FrameMetadata,
        FusionDiagnostics, FusionParameters, MAX_INPUTS, MIN_INPUTS, MotionDiagnostics,
        Registration, ResolvedExposures,
    },
    model::identifier,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, ops::RangeInclusive, path::PathBuf};

identifier!(
    /// A client's open merge preview, `merge-preview-…`: session state on the owner, never stored
    /// in the catalog.
    MergePreviewId,
    "merge-preview-"
);

identifier!(
    /// A merge, `merge-…`, and the stack its result leads: stable for the catalog's life,
    /// whatever happens to the index or the moments it was suggested from.
    MergeId,
    "merge-"
);

/// The one provenance shape this build reads and writes ([`MergeProvenance::format`]): kind-tagged
/// parameters and resolution. Any other is `incompatible` and kept as stored.
pub const MERGE_PROVENANCE_FORMAT: u32 = 2;

const MIB: u64 = 1024 * 1024;

/// What a merge combines: an exposure bracket into one HDR photograph, or overlapping frames into
/// one panorama. The catalog records and browses both; a kind merges only while this build
/// registers its provider (`crate::merge::providers`), and any request of another kind is refused
/// `not-ready`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeKind {
    Hdr,
    Panorama,
}

impl MergeKind {
    /// Every kind, in the order schemas and menus list them.
    pub const ALL: [Self; 2] = [Self::Hdr, Self::Panorama];

    /// The most originals a stack of any kind has: the largest of the kinds' input bounds.
    pub const MAX_STACK: usize = 32;

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hdr => "hdr",
            Self::Panorama => "panorama",
        }
    }

    /// The kind a stored or requested name is, exactly.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// How a message names the kind: "HDR merging", "panorama merging".
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Hdr => "HDR",
            Self::Panorama => "panorama",
        }
    }

    /// The module that provides the kind's merges, whether or not this build carries it.
    pub const fn module(self) -> &'static str {
        match self {
            Self::Hdr => "luxforge.hdr",
            Self::Panorama => "luxforge.panorama",
        }
    }

    /// How many inputs a merge of this kind takes.
    pub const fn inputs(self) -> RangeInclusive<usize> {
        match self {
            Self::Hdr => MIN_INPUTS..=MAX_INPUTS,
            Self::Panorama => 2..=32,
        }
    }

    /// The provisional temporary-disk ceiling of one merge: its spool and its result's objects.
    pub const fn temp_ceiling(self) -> u64 {
        match self {
            // Nine maximum-size float inputs plus the result.
            Self::Hdr => 16 * 1024 * MIB,
            // 32 frames of 60 MP at 14 bytes a pixel, plus a 512 MP result.
            Self::Panorama => 64 * 1024 * MIB,
        }
    }

    /// The scratch one merge may hold beside its one development.
    pub const fn scratch_limit(self) -> u64 {
        match self {
            Self::Hdr => 256 * MIB,
            Self::Panorama => 512 * MIB,
        }
    }

    /// The catalog job a merge of this kind runs as: none until its provider is registered.
    pub const fn job(self) -> Option<&'static CatalogJob> {
        match self {
            Self::Hdr => Some(&super::jobs::HDR_MERGE),
            Self::Panorama => None,
        }
    }
}

/// Where a merge's result and stack stand (`merge_stacks.state`), the value of its `merge-stack`
/// journal item.
///
/// - `active`: the result is visible and its stack collapses its inputs beneath it; each input is
///   claimed by this stack alone.
/// - `removed`: Remove merge (or Remove from catalog) moved the result to Removed and deactivated
///   the stack; Put back makes it active again.
/// - `undone`: the merge's creation was undone; the result is retained in Removed, labelled as an
///   undone merge, and redo or Put back restores it without merging again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeState {
    Active,
    Removed,
    Undone,
}

impl MergeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Removed => "removed",
            Self::Undone => "undone",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "active" => Ok(Self::Active),
            "removed" => Ok(Self::Removed),
            "undone" => Ok(Self::Undone),
            other => Err(Error::incompatible(format!(
                "merge state {other} is not supported"
            ))),
        }
    }
}

/// A `merge-stack` row's value: the merge's [`MergeState`]. Absent once Empty Removed has deleted
/// the merge with its result, which no undo or redo brings back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeStackValue {
    pub state: MergeState,
}

/// The provider that produced a result, and the algorithm version a rerun must match.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeProvider {
    pub module: String,
    pub version: u32,
}

/// One exact input, as it was when the merge read it: its durable locator (the canonical path and
/// the volume it is on), its signature then, the catalog photograph it was, if it was one, and
/// how the merge interpreted it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeInput {
    pub path: PathBuf,
    pub volume_id: VolumeId,
    pub signature: FileSignature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    pub frame: FrameMetadata,
}

/// The options an HDR merge ran with: what, with its inputs and provider, decides its result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrParameters {
    pub exposure: ExposureOptions,
    pub alignment: AlignmentOptions,
    pub fusion: FusionParameters,
}

/// The common sensor development every input was brought to linear sRGB/D65 with: the
/// reference's white-balance gains and camera matrix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrDevelopment {
    pub white_balance: [f32; 3],
    pub cam_xyz: [[f32; 3]; 4],
}

/// What an HDR merge resolved and measured: each input's exposure and its origin, the reference,
/// the common development, the registration with each input's transform and evidence, the output
/// coverage, and the motion and fusion diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrResolution {
    pub exposures: ResolvedExposures,
    pub development: HdrDevelopment,
    pub registration: Registration,
    pub coverage: Coverage,
    pub motion: MotionDiagnostics,
    pub fusion: FusionDiagnostics,
}

/// The options a merge ran with, tagged by its kind (`{"hdr": {...}}`): what, with its inputs and
/// provider, decides its result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeParameters {
    Hdr(HdrParameters),
}

impl MergeParameters {
    pub fn kind(&self) -> MergeKind {
        match self {
            Self::Hdr(_) => MergeKind::Hdr,
        }
    }
}

/// What a merge resolved and measured, tagged by its kind (`{"hdr": {...}}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeResolution {
    Hdr(HdrResolution),
}

impl MergeResolution {
    pub fn kind(&self) -> MergeKind {
        match self {
            Self::Hdr(_) => MergeKind::Hdr,
        }
    }

    /// The reference input's position in the stack's order.
    pub fn reference(&self) -> usize {
        match self {
            Self::Hdr(resolved) => resolved.exposures.reference,
        }
    }

    /// The resolved EV offset from the reference of the input at `position`, for a kind whose
    /// inputs have one: HDR's do; a panorama's have none.
    pub fn offset_ev(&self, position: usize) -> Option<f64> {
        match self {
            Self::Hdr(resolved) => resolved
                .exposures
                .inputs
                .get(position)
                .map(|exposure| exposure.offset_ev),
        }
    }

    /// The inputs' EV range about the reference, lowest first, for a kind that has one.
    pub fn exposure_range_ev(&self) -> Option<[f64; 2]> {
        match self {
            Self::Hdr(resolved) => {
                let (low, high) = resolved.exposures.range_ev();
                Some([low, high])
            }
        }
    }
}

/// A merge's immutable provenance (`merges.provenance_json`): authoritative catalog data, never an
/// EXIF name or an index or moment key. Inputs are in the stack's order, which the merge decided
/// (capture order, the request's breaking ties); every index in `resolved` is a position in it.
/// `parameters` and `resolved` are the kind's own, tagged by it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeProvenance {
    pub format: u32,
    pub kind: MergeKind,
    pub provider: MergeProvider,
    pub inputs: Vec<MergeInput>,
    pub parameters: MergeParameters,
    pub resolved: MergeResolution,
}

impl MergeProvenance {
    /// Refuse a provenance this build does not write, or one whose parts disagree: another format,
    /// parameters or a resolution of another kind than `kind`, an input count outside the kind's
    /// bounds, an input named twice or by a relative path, or resolved values that do not cover
    /// exactly the inputs.
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != MERGE_PROVENANCE_FORMAT {
            return Err(Error::incompatible(format!(
                "merge provenance format {} is not supported; expected {MERGE_PROVENANCE_FORMAT}",
                self.format
            )));
        }
        let kind = self.kind;
        if self.parameters.kind() != kind || self.resolved.kind() != kind {
            return Err(Error::validation(format!(
                "a {} merge records {} parameters and a {} resolution; both must be its own",
                kind.as_str(),
                self.parameters.kind().as_str(),
                self.resolved.kind().as_str()
            ))
            .with_data(json!({"kind": kind})));
        }
        let count = self.inputs.len();
        let bounds = kind.inputs();
        if !bounds.contains(&count) {
            return Err(Error::validation(format!(
                "a {} merge takes {} to {} inputs, not {count}",
                kind.as_str(),
                bounds.start(),
                bounds.end()
            ))
            .with_data(json!({"kind": kind, "count": count})));
        }
        let mut paths = HashSet::with_capacity(count);
        for input in &self.inputs {
            if !input.path.is_absolute() {
                return Err(Error::validation(format!(
                    "merge input {} is not an absolute path",
                    input.path.display()
                )));
            }
            if !paths.insert(&input.path) {
                return Err(Error::validation(format!(
                    "merge input {} is named twice",
                    input.path.display()
                )));
            }
        }
        match (&self.parameters, &self.resolved) {
            (MergeParameters::Hdr(parameters), MergeResolution::Hdr(resolved)) => {
                if resolved.exposures.inputs.len() != count
                    || resolved.registration.inputs.len() != count
                    || resolved.exposures.reference >= count
                    || resolved.registration.reference != resolved.exposures.reference
                {
                    return Err(Error::validation(
                        "an HDR merge's resolved exposures and registration must cover exactly \
                         its inputs about one reference",
                    ));
                }
                parameters.fusion.validate()
            }
        }
    }

    /// The reference input's position in the stack's order, from the resolution.
    pub fn reference(&self) -> usize {
        self.resolved.reference()
    }

    /// The reference input.
    pub fn reference_input(&self) -> &MergeInput {
        &self.inputs[self.reference()]
    }

    /// The exact-match key (`merges.match_key`): the SHA-256 of the kind, the provider and its
    /// version, each input's path and signature in order, and the kind-tagged parameters. Two
    /// requests with one key would produce the same result, so the second reuses the first's.
    pub fn match_key(&self) -> Result<String, Error> {
        match_key(
            self.kind,
            &self.provider,
            self.inputs
                .iter()
                .map(|input| (&input.path, &input.signature)),
            &self.parameters,
        )
    }
}

/// [`MergeProvenance::match_key`] from its parts, for a request planned before it has provenance.
pub fn match_key<'a>(
    kind: MergeKind,
    provider: &MergeProvider,
    inputs: impl IntoIterator<Item = (&'a PathBuf, &'a FileSignature)>,
    parameters: &MergeParameters,
) -> Result<String, Error> {
    let inputs: Vec<_> = inputs.into_iter().collect();
    let text = serde_json::to_vec(&(kind, provider, inputs, parameters))
        .map_err(|error| Error::internal(format!("cannot encode a merge key: {error}")))?;
    Ok(format!("{:x}", Sha256::digest(text)))
}

/// One member of a stack (`merge_inputs`): an exact input in the stack's order with, for a kind
/// whose inputs have one (HDR), its resolved EV offset from the reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackMember {
    pub ordinal: u32,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_ev: Option<f64>,
}

/// A merge as the catalog records it: its result, the source the result holds, its state, its
/// stack's members and its provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeRecord {
    pub merge_id: MergeId,
    pub kind: MergeKind,
    pub result_asset_id: AssetId,
    pub source_id: DerivedSourceId,
    pub state: MergeState,
    pub created_ms: i64,
    pub members: Vec<StackMember>,
    pub provenance: MergeProvenance,
}

/// A recorded merge another request matches exactly ([`MergeProvenance::match_key`]): reuse its
/// result while it is active; offer Put back merge while it is in Removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeMatch {
    pub merge_id: MergeId,
    pub result_asset_id: AssetId,
    pub state: MergeState,
}

/// The active stack, of any kind, that already claims one of a request's inputs: a second merge of
/// it is refused, naming this stack, until that merge is removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackClaim {
    pub path: PathBuf,
    pub merge_id: MergeId,
    pub kind: MergeKind,
    pub result_asset_id: AssetId,
}

/// A person's exposure for one input (`exposure_overrides`), named by the input's path: its EV
/// offset from the reference, which replaces what the metadata or the fit would resolve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExposureOverride {
    pub path: PathBuf,
    pub offset_ev: f64,
}

/// An HDR merge's options as `merge.plan`, `merge.create` and `merge.rerun` take them, under `hdr`:
/// the reference and the exposure overrides by path, alignment and deghosting. A field left out is
/// the default, or for `merge.rerun` the recorded value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrOptions {
    /// The input whose exposure and white balance the result keeps, by its path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<PathBuf>,
    /// Inputs' EV offsets from the reference, replacing what their metadata or the fit resolves;
    /// for `merge.rerun`, `[]` clears the recorded ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_overrides: Option<Vec<ExposureOverride>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<AlignmentMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deghost: Option<DeghostMode>,
}

/// One input of a planned merge, in the stack's order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedInput {
    pub ordinal: u32,
    pub path: PathBuf,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<super::FileId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    /// Whether the file is where the request names it now.
    pub available: bool,
    /// The interpretation suitability was judged on: the index's or the catalog's header, before
    /// any decoding. The job validates the decoded interpretation again.
    pub frame: FrameMetadata,
}

/// Where a merge's result goes, when the folder is in the catalog already: the folder the
/// proposal names (`existing`: one an earlier Develop of the reference's event made, or the folder
/// its folder on disk's photographs went into), or the day folder of its year and place
/// (`place`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderProposal {
    pub folder_id: CatalogFolderId,
    pub name: String,
    pub from: String,
}

/// What a merge of the planned inputs would take, from their header dimensions: the temporary
/// storage it spools with the result's objects, the result's own bytes at most (before its
/// coverage crop), the free space where it spools, and the limits it is held to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeEstimates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_bytes: Option<u64>,
    pub temp_ceiling_bytes: u64,
    pub scratch_limit_bytes: u64,
}

/// The provider a merge needs: this build's, whether it can merge now, and why not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderState {
    pub module: String,
    pub version: u32,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What `merge.plan` answers: the inputs in the stack's order, whether their headers make a
/// bracket and why not, the exposures their metadata resolves (or that the job will measure them),
/// the reference, the folder the result would go into, the estimates, the provider, an exact
/// recorded match and any active stack claiming an input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePlan {
    pub kind: MergeKind,
    /// The plan's identity: `merge.create`'s `plan_id` is refused once the inputs or the options
    /// differ from what was planned.
    pub plan_id: String,
    pub inputs: Vec<PlannedInput>,
    pub suitable: bool,
    /// Why the headers are not a bracket: a typed refusal (`reason` and the inputs it names) with
    /// its `message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<serde_json::Value>,
    /// The exposures the metadata resolves, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposures: Option<ResolvedExposures>,
    /// When the metadata cannot resolve them, the job measures them from the aligned pixels.
    pub needs_fit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<u32>,
    pub parameters: MergeParameters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderProposal>,
    /// The folder choice a Develop would file the reference frame by, when it would make a folder
    /// for the reference's capture day: `merge.create`'s default, which makes those folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_folder: Option<super::FolderChoice>,
    pub estimates: MergeEstimates,
    pub provider: ProviderState,
    /// A recorded merge of exactly these inputs, options and provider: reused while active,
    /// offered as Put back merge while removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<MergeMatch>,
    /// Active stacks that claim an input already: a merge is refused until each is removed.
    pub claimed: Vec<StackClaim>,
}

/// A merge's options by kind, as one preview revision drew them: `{"hdr": HdrOptions}`. A merge
/// given that preview's `preview_id` must name the same options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum MergeOptions {
    Hdr(HdrOptions),
}

/// What an HDR preview measured, for its status line and output: the proxies' long side, the
/// alignment and its largest residual, deghosting, whether the merge extends the reference's
/// highlights or shadows, each input's resolved offset and the reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrPreviewDiagnostics {
    pub proxy_side: u32,
    pub alignment: AlignmentMode,
    /// The largest fitted residual of any input's alignment, in full-resolution pixels; none
    /// with alignment off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_residual_px: Option<f64>,
    pub deghost: DeghostMode,
    pub highlights_extended: bool,
    pub shadows_extended: bool,
    /// Each input's EV offset from the reference, in the stack's order.
    pub exposures: Vec<f64>,
    pub reference: u32,
}

/// A preview's diagnostics by kind: `{"hdr": HdrPreviewDiagnostics}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum MergePreviewDiagnostics {
    Hdr(HdrPreviewDiagnostics),
}

/// An HDR preview's overlay data, in the picture's pixels: each moving region's piece with the
/// input it is shown from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrPreviewOverlays {
    pub motion: Vec<crate::merge::hdr::MotionOverlay>,
}

/// A preview's overlays by kind: `{"hdr": HdrPreviewOverlays}`. A client draws them, so an agent
/// reads the same facts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum MergePreviewOverlays {
    Hdr(HdrPreviewOverlays),
}

/// What the merge will save: its size, its scale against the reference (1 for HDR) and its
/// source's bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeOutput {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub source_bytes: u64,
}

/// What a `merge-preview` job answers (`merge.preview`, `merge.preview-update`): one revision of a
/// client's open preview. `path` is a display-referred JPEG under the catalog's preview cache, for
/// viewing only, never the saved pixels. A refused preview answers its typed `refusal` (`reason`,
/// the inputs it names and `message`) and no picture; its session stays open for another setting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePreview {
    pub preview_id: MergePreviewId,
    pub revision: u32,
    pub kind: MergeKind,
    pub options: MergeOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<MergePreviewDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlays: Option<MergePreviewOverlays>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<MergeOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<serde_json::Value>,
}

/// What a merge job answers once it ends `ready`: the published or reused merge, its result and
/// source, where the result went, what it merged and the library change that made it visible.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeAnswer {
    pub merge_id: MergeId,
    pub kind: MergeKind,
    pub result_asset_id: AssetId,
    pub source_id: DerivedSourceId,
    pub file_name: String,
    pub catalog_folder_id: CatalogFolderId,
    /// A merged result, never one capture.
    pub merged: bool,
    pub inputs: u32,
    /// The bracket's EV range about the reference, lowest first: an HDR merge's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_range_ev: Option<[f64; 2]>,
    /// An exact recorded match answered the request: nothing was merged or published.
    pub reused: bool,
    /// The merge a rerun replaced as the inputs' stack, now in Removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<MergeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<super::LibraryChangeSeq>,
    pub deduplicated: bool,
    /// What the job held while it merged, when it merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting: Option<serde_json::Value>,
}

/// Whether a merge's saved source can be read, by stat alone: `available`, or `missing` or
/// `corrupt` (an object absent or of another length) with the message preparation gives. A
/// damaged source is never merged again automatically.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAvailability {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One input as `merge.inspect` reports it: its member row and whether its file is still the one
/// merged (`available`), changed since (`changed`) or gone (`missing`), by one stat. A
/// regeneration diagnostic only: the saved result never needs it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectedInput {
    #[serde(flatten)]
    pub member: StackMember,
    pub file: String,
}

/// What `merge.inspect` answers: the merge as recorded, its result's source availability, its
/// stack's inputs with their regeneration diagnostics, whether this build can rerun it, its
/// summary and its provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeInspection {
    pub merge_id: MergeId,
    pub kind: MergeKind,
    pub state: MergeState,
    pub result_asset_id: AssetId,
    pub source_id: DerivedSourceId,
    pub created_ms: i64,
    pub source: SourceAvailability,
    pub inputs: Vec<InspectedInput>,
    pub merged: bool,
    /// The bracket's EV range about the reference, lowest first: an HDR merge's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_range_ev: Option<[f64; 2]>,
    pub parameters: MergeParameters,
    pub provider: ProviderState,
    /// Whether `merge.rerun` would start now, and why not.
    pub rerunnable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun_reason: Option<String>,
    pub provenance: MergeProvenance,
}

/// How a client shows one stack in its views (`stack.set-expanded`): session view state, never a
/// library change, read by every view the client evaluates.
///
/// - `collapsed`: the result stands for its originals that the view holds and its filter matches.
/// - `expanded`: those originals follow the result; over the catalog, the inputs never developed
///   follow it too, as files.
/// - `all`: every one of the stack's originals follows the result, those the view's source or
///   filter leaves out labelled outside the filter (Show all N originals).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StackExpansion {
    #[default]
    Collapsed,
    Expanded,
    All,
}

impl StackExpansion {
    pub const ALL: [Self; 3] = [Self::Collapsed, Self::Expanded, Self::All];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collapsed => "collapsed",
            Self::Expanded => "expanded",
            Self::All => "all",
        }
    }
}

/// Where a view row stands in a stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StackRole {
    /// The merge's result, collapsed or followed by its originals.
    Leader,
    /// An original following its expanded leader.
    Original,
    /// An original shown on its own, with its stack's badge: its leader is not in the view, or the
    /// view's filter does not match it.
    Separate,
}

/// A view row's stack (`browse.rows`' `stack`): the stack it belongs to, its kind, its role there
/// and the stack's size; for a leader, how many originals it stands for in this view and how the
/// client shows it; for an original, its place and, in an HDR stack, its EV offset, and whether
/// only Show all revealed it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowStack {
    pub merge_id: MergeId,
    pub kind: MergeKind,
    pub role: StackRole,
    /// The stack's originals, N: "HDR · N exposures".
    pub size: u32,
    /// A collapsed leader: its originals the view holds beneath it, which its count leaves out.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub collapsed: u32,
    /// A leader: how this client shows the stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion: Option<StackExpansion>,
    /// An original's place in the stack's order, from 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_ev: Option<f64>,
    /// An original revealed solely by Show all N originals: the view's source or filter leaves it
    /// out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outside_filter: bool,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// One original of a stack as `stack.inspect` lists it: its place, path and, in an HDR stack, EV
/// offset, whether it is the reference, whether it is where it was, its index row and pick, and the catalog
/// photograph it was developed into, if any.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackOriginal {
    pub ordinal: u32,
    pub path: PathBuf,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_ev: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reference: bool,
    pub availability: super::FileAvailability,
    /// Why it is not available: its volume is offline, or the index does not list it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<super::FileId>,
    pub picked: bool,
    /// The catalog photograph developed from it, when it is in the catalog and not removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
}

/// What `stack.inspect` answers: the stack's identity, kind, state and result, how the caller
/// shows it, and its originals `from..from + originals.len()` of `size`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackInspection {
    pub merge_id: MergeId,
    pub kind: MergeKind,
    pub state: MergeState,
    pub result_asset_id: AssetId,
    pub file_name: String,
    /// The result is in Removed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    pub expansion: StackExpansion,
    /// The stack's originals, N.
    pub size: u32,
    pub from: u32,
    pub originals: Vec<StackOriginal>,
}
