//! Merges and their stacks (`docs/design/hdr-merge.md`, "The stack and the originals", "Remove,
//! undo and retention"): the immutable provenance a merge records with its result, the result-led
//! stack of its exact inputs, and the lifecycle state the library journal changes.
//!
//! A merge is identified by its [`MergeId`], which is also its stack's identity: neither depends
//! on an index [`FileId`](super::FileId) or a computed moment, so removing or rebuilding the index
//! changes neither. Its inputs are original files, named by path, volume and [`FileSignature`],
//! each optionally the catalog photograph it was developed into; never another merge's result.
use super::{FileSignature, VolumeId};
use crate::{
    AssetId, DerivedSourceId, Error,
    merge::hdr::{
        AlignmentOptions, Coverage, ExposureOptions, FrameMetadata, FusionDiagnostics,
        FusionParameters, MAX_INPUTS, MIN_INPUTS, MotionDiagnostics, Registration,
        ResolvedExposures,
    },
    model::identifier,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::PathBuf};

identifier!(
    /// A merge, `merge-…`, and the stack its result leads: stable for the catalog's life,
    /// whatever happens to the index or the moments it was suggested from.
    MergeId,
    "merge-"
);

/// The one provenance shape this build reads and writes ([`MergeProvenance::format`]). Any other
/// is `incompatible` and kept as stored.
pub const MERGE_PROVENANCE_FORMAT: u32 = 1;

/// What a merge combines. Only `hdr` is accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeKind {
    Hdr,
}

impl MergeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hdr => "hdr",
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

/// A merge's immutable provenance (`merges.provenance_json`): authoritative catalog data, never an
/// EXIF name or an index or moment key. Inputs are in the stack's order, which the merge decided
/// (capture order, the request's breaking ties); every index in `resolved` is a position in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeProvenance {
    pub format: u32,
    pub kind: MergeKind,
    pub provider: MergeProvider,
    pub inputs: Vec<MergeInput>,
    pub parameters: HdrParameters,
    pub resolved: HdrResolution,
}

impl MergeProvenance {
    /// Refuse a provenance this build does not write, or one whose parts disagree: another format
    /// or kind, an input count outside 2–9, an input named twice or by a relative path, or
    /// resolved values that do not cover exactly the inputs.
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != MERGE_PROVENANCE_FORMAT {
            return Err(Error::incompatible(format!(
                "merge provenance format {} is not supported; expected {MERGE_PROVENANCE_FORMAT}",
                self.format
            )));
        }
        let count = self.inputs.len();
        if !(MIN_INPUTS..=MAX_INPUTS).contains(&count) {
            return Err(Error::validation(format!(
                "a merge takes {MIN_INPUTS} to {MAX_INPUTS} inputs, not {count}"
            )));
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
        let resolved = &self.resolved;
        if resolved.exposures.inputs.len() != count
            || resolved.registration.inputs.len() != count
            || resolved.exposures.reference >= count
            || resolved.registration.reference != resolved.exposures.reference
        {
            return Err(Error::validation(
                "a merge's resolved exposures and registration must cover exactly its inputs \
                 about one reference",
            ));
        }
        self.parameters.fusion.validate()
    }

    /// The reference input.
    pub fn reference(&self) -> &MergeInput {
        &self.inputs[self.resolved.exposures.reference]
    }

    /// The exact-match key (`merges.match_key`): the SHA-256 of the kind, the provider and its
    /// version, each input's path and signature in order, and the parameters. Two requests with
    /// one key would produce the same result, so the second reuses the first's.
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
    parameters: &HdrParameters,
) -> Result<String, Error> {
    let inputs: Vec<_> = inputs.into_iter().collect();
    let text = serde_json::to_vec(&(kind, provider, inputs, parameters))
        .map_err(|error| Error::internal(format!("cannot encode a merge key: {error}")))?;
    Ok(format!("{:x}", Sha256::digest(text)))
}

/// One member of a stack (`merge_inputs`): an exact input in the stack's order with its resolved
/// EV offset from the reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackMember {
    pub ordinal: u32,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    pub offset_ev: f64,
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

/// The active stack that already claims one of a request's inputs: a second merge of it is
/// refused, naming this stack, until that merge is removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackClaim {
    pub path: PathBuf,
    pub merge_id: MergeId,
    pub result_asset_id: AssetId,
}
