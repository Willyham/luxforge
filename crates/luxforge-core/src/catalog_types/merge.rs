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

    /// The catalog job a merge of this kind runs as.
    pub const fn job(self) -> &'static CatalogJob {
        match self {
            Self::Hdr => &super::jobs::HDR_MERGE,
            Self::Panorama => &super::jobs::PANORAMA_MERGE,
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

/// How a panorama's output plane maps to viewing directions (`docs/design/panorama-merge.md`,
/// "Projection and output size"): chosen by the person, or `auto`, which registration resolves
/// from the registered field of view. A resolved projection is never `auto`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PanoramaProjection {
    #[default]
    Auto,
    /// Rectilinear: straight lines stay straight; fields under about 120° wide.
    Perspective,
    /// Wide single rows.
    Cylindrical,
    /// Equirectangular: tall, multi-row or 360° panoramas.
    Spherical,
}

impl PanoramaProjection {
    pub const ALL: [Self; 4] = [
        Self::Auto,
        Self::Perspective,
        Self::Cylindrical,
        Self::Spherical,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Perspective => "perspective",
            Self::Cylindrical => "cylindrical",
            Self::Spherical => "spherical",
        }
    }
}

/// The largest Boundary Warp: the covered region warped onto its whole bounding rectangle.
pub const MAX_BOUNDARY_WARP: u8 = 100;

/// The options a panorama merge ran with: the projection, the Boundary Warp amount (0 crops to the
/// largest clean rectangle, 100 warps the covered region onto its bounding rectangle) and the
/// reference the person named, as a position in the stack's order (none: the frame nearest the
/// panorama's centre).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaParameters {
    pub projection: PanoramaProjection,
    pub boundary_warp: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<usize>,
}

/// The lens a panorama was registered and stitched under: the lens profile baked into every RAW
/// frame before registration, when the lens module's first-open detection selected one for the
/// reference frame, or none; the shared focal length and radial term, and where each came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaLensResolution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<crate::merge::panorama::frames::BakedLensProfile>,
    /// The frames' upright size, full-resolution pixels.
    pub width: u32,
    pub height: u32,
    pub focal_px: f64,
    pub focal_origin: crate::merge::panorama::FocalOrigin,
    /// The radial term `k`: fitted, or fixed at 0 when a profile was baked.
    pub radial: f64,
    pub radial_origin: crate::merge::panorama::RadialOrigin,
}

/// A stitch's seam and blend diagnostics, per frame in the stack's order where per frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaSeams {
    /// Each frame's seam length, in output pixels.
    pub seam_px: Vec<u64>,
    /// The output pixels each frame contributes most to.
    pub contributing_px: Vec<u64>,
    /// Output pixels taken from clipped samples.
    pub clipped_px: u64,
    /// The largest overshoot the blend introduced, relative to the samples it blended; nothing is
    /// clamped.
    pub max_overshoot: f64,
}

/// A Boundary Warp's mesh, solved on the proxies: `columns × rows` vertices over the stitch's
/// bounding rectangle, row by row, each its displacement in output pixels at the warp's amount.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaWarpMesh {
    pub columns: u32,
    pub rows: u32,
    pub displacements: Vec<[f32; 2]>,
}

/// What a panorama's stitch resolved: the projection (never `auto`) and field of view, the output
/// scale against the reference frame's centre pixel (1 at most), the stitch's bounding rectangle
/// at that scale and the crop the result is, what the crop loses of the covered region, each
/// frame's photometric gains (one a channel, equal for RAW frames), the seams and blend, and the
/// Boundary Warp's mesh when the amount is above 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaStitch {
    pub projection: PanoramaProjection,
    /// Horizontal and vertical, in degrees.
    pub field_of_view_deg: [f64; 2],
    pub scale: f64,
    pub bounds_width: u32,
    pub bounds_height: u32,
    pub crop: crate::merge::hdr::PixelRect,
    /// The fraction of the covered region the crop drops.
    pub crop_loss: f64,
    pub gains: Vec<[f64; 3]>,
    pub seams: PanoramaSeams,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<PanoramaWarpMesh>,
}

impl PanoramaStitch {
    /// This stitch with its per-frame values in `order` (`order[k]` is the frame at position `k`).
    pub(crate) fn permuted(&self, order: &[usize]) -> Self {
        let pick = |values: &[u64]| -> Vec<u64> {
            order
                .iter()
                .map(|&at| values.get(at).copied().unwrap_or(0))
                .collect()
        };
        Self {
            gains: order
                .iter()
                .map(|&at| self.gains.get(at).copied().unwrap_or([1.0; 3]))
                .collect(),
            seams: PanoramaSeams {
                seam_px: pick(&self.seams.seam_px),
                contributing_px: pick(&self.seams.contributing_px),
                ..self.seams.clone()
            },
            ..self.clone()
        }
    }
}

/// What a panorama merge resolved and measured, every frame index a position in the stack's order
/// (left to right in the result, top to bottom then left to right for several rows): the reference
/// and how it was chosen, the RAW frames' common development, each frame's exposure normalization
/// from its settings, the lens, each frame's rotation and evidence, every pair's evidence (which
/// records the frames' connectivity), the levelling, the residuals and the stitch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaResolution {
    pub reference: usize,
    pub reference_origin: crate::merge::panorama::ReferenceOrigin,
    /// The white balance and camera matrix every RAW frame was developed with: the first frame
    /// read's; none for JPEG frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub development: Option<HdrDevelopment>,
    /// Each frame's scale to the reference's exposure from its recorded settings, `2^−offset`;
    /// 1 where nothing is recorded.
    pub exposure_scales: Vec<f64>,
    pub lens: PanoramaLensResolution,
    pub rotations: Vec<crate::merge::panorama::Rotation>,
    pub frames: Vec<crate::merge::panorama::FrameEvidence>,
    pub pairs: Vec<crate::merge::panorama::PairEvidence>,
    pub levelling: crate::merge::panorama::Levelling,
    pub rms_residual_px: f64,
    pub max_residual_px: f64,
    pub stitch: PanoramaStitch,
}

/// The options a merge ran with, tagged by its kind (`{"hdr": {...}}`, `{"panorama": {...}}`):
/// what, with its inputs and provider, decides its result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeParameters {
    Hdr(HdrParameters),
    Panorama(PanoramaParameters),
}

impl MergeParameters {
    pub fn kind(&self) -> MergeKind {
        match self {
            Self::Hdr(_) => MergeKind::Hdr,
            Self::Panorama(_) => MergeKind::Panorama,
        }
    }
}

/// What a merge resolved and measured, tagged by its kind (`{"hdr": {...}}`, `{"panorama":
/// {...}}`). A panorama's is boxed, being much the larger; HDR's stays inline, as one is made and
/// read once a merge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub enum MergeResolution {
    Hdr(HdrResolution),
    Panorama(Box<PanoramaResolution>),
}

impl MergeResolution {
    pub fn kind(&self) -> MergeKind {
        match self {
            Self::Hdr(_) => MergeKind::Hdr,
            Self::Panorama(_) => MergeKind::Panorama,
        }
    }

    /// The reference input's position in the stack's order.
    pub fn reference(&self) -> usize {
        match self {
            Self::Hdr(resolved) => resolved.exposures.reference,
            Self::Panorama(resolved) => resolved.reference,
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
            Self::Panorama(_) => None,
        }
    }

    /// The inputs' EV range about the reference, lowest first, for a kind that has one.
    pub fn exposure_range_ev(&self) -> Option<[f64; 2]> {
        match self {
            Self::Hdr(resolved) => {
                let (low, high) = resolved.exposures.range_ev();
                Some([low, high])
            }
            Self::Panorama(_) => None,
        }
    }

    /// A panorama's resolved projection and field of view.
    pub fn panorama_view(&self) -> Option<(PanoramaProjection, [f64; 2])> {
        match self {
            Self::Hdr(_) => None,
            Self::Panorama(resolved) => Some((
                resolved.stitch.projection,
                resolved.stitch.field_of_view_deg,
            )),
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
            (MergeParameters::Panorama(parameters), MergeResolution::Panorama(resolved)) => {
                let per_frame = [
                    resolved.exposure_scales.len(),
                    resolved.rotations.len(),
                    resolved.frames.len(),
                    resolved.stitch.gains.len(),
                ];
                if per_frame.iter().any(|&len| len != count)
                    || resolved.reference >= count
                    || parameters.reference.is_some_and(|at| at >= count)
                    || resolved
                        .pairs
                        .iter()
                        .any(|pair| pair.a >= count || pair.b >= count)
                {
                    return Err(Error::validation(
                        "a panorama merge's resolution must cover exactly its frames about one \
                         reference",
                    ));
                }
                if parameters.boundary_warp > MAX_BOUNDARY_WARP
                    || resolved.stitch.projection == PanoramaProjection::Auto
                {
                    return Err(Error::validation(
                        "a panorama merge records a Boundary Warp of 0 to 100 and a resolved \
                         projection",
                    ));
                }
                Ok(())
            }
            _ => Err(Error::validation(
                "a merge's parameters and resolution are of different kinds",
            )),
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

    /// The result's optics ledger ([`crate::SourceOptics::derived_ledger`]): every correction
    /// applied when it was made; a panorama that baked a lens profile names it as its distortion's.
    pub fn optics_ledger(&self) -> luxforge_raw::OpticalLedger {
        let mut ledger = crate::SourceOptics::derived_ledger();
        if let MergeResolution::Panorama(resolved) = &self.resolved
            && let Some(profile) = &resolved.lens.profile
        {
            ledger.distortion = profile.ledger_entry();
        }
        ledger
    }
}

/// [`MergeProvenance::match_key`] from its parts, for a request planned before it has provenance.
///
/// A panorama's key does not depend on the order its frames are named in, since registration, not
/// the request, decides the stack's order: its inputs are hashed by path and a named reference as
/// its path. An HDR merge's inputs are hashed in the stack's order.
pub fn match_key<'a>(
    kind: MergeKind,
    provider: &MergeProvider,
    inputs: impl IntoIterator<Item = (&'a PathBuf, &'a FileSignature)>,
    parameters: &MergeParameters,
) -> Result<String, Error> {
    let mut inputs: Vec<_> = inputs.into_iter().collect();
    let text = match parameters {
        MergeParameters::Panorama(panorama) => {
            let reference = panorama
                .reference
                .and_then(|at| inputs.get(at))
                .map(|(path, _)| *path);
            let canonical = (
                panorama.projection,
                panorama.boundary_warp,
                reference.cloned(),
            );
            inputs.sort_by(|a, b| a.0.cmp(b.0));
            serde_json::to_vec(&(kind, provider, inputs, canonical))
        }
        MergeParameters::Hdr(_) => serde_json::to_vec(&(kind, provider, inputs, parameters)),
    }
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
/// coverage crop), the free space where it spools, and the limits it is held to. A panorama's size
/// is estimated from its frames' focal length, sensor size and count, `provisional` because
/// registration decides the final size, with the scale the large-source limits predict when they
/// would scale it down.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// The output's predicted scale against the reference frame's centre pixel, when the limits
    /// predict one below 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// The size is an estimate that registration, which needs pixels, decides: a panorama's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provisional: bool,
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

/// A panorama merge's options as `merge.plan`, `merge.create`, `merge.preview` and `merge.rerun`
/// take them, under `panorama`: the projection (`auto` by default), the Boundary Warp amount (0 to
/// 100, 0 by default) and the reference frame by path (the frame nearest the panorama's centre by
/// default). A field left out is the default, or for `merge.rerun` the recorded value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<PanoramaProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_warp: Option<u8>,
}

/// A merge's options by kind, as one preview revision drew them: `{"hdr": HdrOptions}` or
/// `{"panorama": PanoramaOptions}`. A merge given that preview's `preview_id` must name the same
/// options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum MergeOptions {
    Hdr(HdrOptions),
    Panorama(PanoramaOptions),
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

/// What a panorama preview measured, for its status line and output: the proxies' long side, the
/// resolved projection and field of view, the fraction of the covered region the crop loses, the
/// reference, the registration's largest residual in full-resolution pixels, and the frames in
/// the stack's order, which the overlays' frame outlines and `reference` number.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaPreviewDiagnostics {
    pub proxy_side: u32,
    pub projection: PanoramaProjection,
    /// Horizontal and vertical, in degrees.
    pub field_of_view_deg: [f64; 2],
    pub crop_loss: f64,
    /// The reference's position in the stack's order.
    pub reference: u32,
    pub max_residual_px: f64,
    /// Each frame's path, in the stack's order: left to right in the result (top to bottom, then
    /// left to right, for several rows).
    pub frames: Vec<PathBuf>,
}

/// A preview's diagnostics by kind: `{"hdr": HdrPreviewDiagnostics}` or `{"panorama":
/// PanoramaPreviewDiagnostics}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum MergePreviewDiagnostics {
    Hdr(HdrPreviewDiagnostics),
    Panorama(PanoramaPreviewDiagnostics),
}

/// An HDR preview's overlay data, in the picture's pixels: each moving region's piece with the
/// input it is shown from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrPreviewOverlays {
    pub motion: Vec<crate::merge::hdr::MotionOverlay>,
}

/// One frame's outline in a panorama preview: its position in the stack's order (the sheet numbers
/// it one more) and its edge as a closed polygon in the picture's pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaFrameOutline {
    pub frame: u32,
    pub outline: Vec<[f64; 2]>,
}

/// A panorama preview's overlay data, in the picture's pixels, which show the whole stitch: the
/// covered region's outline (each a closed polygon), the crop rectangle the result keeps and each
/// frame's outline.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaPreviewOverlays {
    pub covered: Vec<Vec<[f64; 2]>>,
    pub crop: [f64; 4],
    pub frames: Vec<PanoramaFrameOutline>,
}

/// A preview's overlays by kind: `{"hdr": HdrPreviewOverlays}` or `{"panorama":
/// PanoramaPreviewOverlays}`. A client draws them, so an agent reads the same facts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum MergePreviewOverlays {
    Hdr(HdrPreviewOverlays),
    Panorama(PanoramaPreviewOverlays),
}

/// What the merge will save: its size, its scale against the reference (1 for HDR; a panorama's
/// output scale against its reference's centre pixel) and its source's bytes.
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
    /// A panorama's resolved projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<PanoramaProjection>,
    /// A panorama's field of view, horizontal and vertical, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_of_view_deg: Option<[f64; 2]>,
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
    /// A panorama's resolved projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<PanoramaProjection>,
    /// A panorama's field of view, horizontal and vertical, in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_of_view_deg: Option<[f64; 2]>,
    /// The result's optics ledger: every correction its making baked in, reported applied; a
    /// panorama's distortion names the lens profile it baked into its frames, when it baked one.
    pub optics: luxforge_raw::OpticalLedger,
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
