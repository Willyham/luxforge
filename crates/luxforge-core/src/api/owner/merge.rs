//! The merge lane and `merge.plan`, `merge.create`, `merge.inspect`, `merge.remove`,
//! `merge.restore` and `merge.rerun` on the owner (`docs/design/hdr-merge.md`, "Jobs, commands and
//! client state"). The worker's job is `crate::merge::job`; the catalog side, publication and the
//! stack's lifecycle, is `crate::library::merge`.
//!
//! - **The lane** is a library lane of its own ([`LaneKind::Merge`]): one worker thread, started
//!   on the first merge, running one merge while at most [`MERGE_QUEUE`] waits; a further request
//!   is `resource-limit`, never dropped. A merge job belongs to no client: no selection, view or
//!   workspace change and no disconnect cancels it, only `job.cancel`.
//! - **Planning and admission** read only the catalog, the index and one stat an input: the inputs
//!   in the stack's order (capture order, the request's breaking ties), their header-level
//!   suitability and exposures, the folder, the estimates, an exact recorded match and the active
//!   stacks that claim an input. Everything that needs pixels is the job's.
//! - **Publication** is the job's answer, committed on the owner when the job ends: one
//!   transaction ([`EditorService::publish_merge`](crate::EditorService::publish_merge)) and one
//!   library change, announced as one event, unless `job.cancel` reached the owner first, when
//!   nothing is published and the durable objects the job wrote are unreferenced, for a later
//!   collection. A cancel after it finds the job `ready`.
//! - **Pick merged** is `merge.create` with `add_to_development_set`: the set current at admission
//!   is captured (and refused when the request's `set_id` is another), carried to publication, and
//!   the result's membership joins the publication's one library change. An exact active match is
//!   added to the set in a library change of its own, unless it is a member already.
//! - **Requests across a restart.** Each admitted request keeps a small marker in its own folder
//!   under the artifact root (`merges/<key>/request.json`, `key` the SHA-256 of its method, actor
//!   and request identity), beside the job's spool files. A retry while its job is queued or
//!   running, or a new request for the same exact merge, is answered with that job; a retry is
//!   answered otherwise from the journal
//!   when its first attempt published; from its marker when that attempt ended without a result
//!   (its error) or never ended because the process stopped (`cancelled`, with `data.reason`
//!   `interrupted` and no asset), and a new request identity merges again; and as a new request
//!   otherwise. A published merge's marker is removed; at most [`MAX_MARKERS`] are kept. Each job
//!   removes its own spool files, and the spool files of any other request it finds as it starts,
//!   which only a stopped process leaves.
use super::{
    Call, ClientId, Owner,
    catalog::Poster,
    library::{JobContext, LaneKind, LibraryLane, Task, change},
};
use crate::{
    AssetId, EditorService, Error, ErrorKind, JobId, MutationRequest, SourceTag,
    activity::ActivityBoard,
    api::{Origin, announce_once, methods::value},
    artifacts::{RootState, derived_source::PublishedSource, root_state},
    catalog_types::{
        AssetRowId, CatalogFolderId, ExposureOverride, FileId, FileSignature, FolderChoice,
        FolderProposal, HdrParameters, HeaderMetadata, InspectedInput, MergeAnswer, MergeEstimates,
        MergeId, MergeInspection, MergeKind, MergePlan, MergeProvenance, MergeProvider,
        MergeRecord, MergeState, PlannedInput, ProviderState, SourceAvailability, Targets,
        ViewItem, VolumeId,
        api::{MergeCreate, MergeInspect, MergePlanParams, MergeRerun, MergeTargets},
        jobs::HDR_MERGE,
        match_key,
    },
    editor::now_ms,
    jobs::{JobControl, JobError},
    library::{
        develop,
        development_set::{self, SetCapture},
        journal::{self, Request},
        merge::{
            self, Publication, PublishedMerge, claims, matching, overlap, published_or_picked,
            record, results,
        },
        remove,
    },
    merge::{
        hdr::{
            AlignmentMode, AlignmentOptions, DeghostMode, ExposureOptions, ExposureResolution,
            FrameMetadata, MAX_INPUTS, MIN_INPUTS, Recording, Refusal, SensorLayout,
            frame::capture_order, resolve_exposures,
        },
        job::{self, Developer, Hooks, JobInput, JobSpec, Limits, Merged, SharedDevelopment},
    },
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
#[path = "hdr_merge_end_to_end.rs"]
mod hdr_merge_end_to_end;
#[cfg(test)]
#[path = "hdr_merge_job.rs"]
mod hdr_merge_job;

/// Merges that may wait behind the running one.
pub(super) const MERGE_QUEUE: usize = 1;
/// The folder under the artifact root that holds each request's marker and its job's spool.
const MERGES: &str = "merges";
const MARKER: &str = "request.json";
const MARKER_FORMAT: u32 = 1;
/// The request markers kept: the newest, by when each was made.
const MAX_MARKERS: usize = 64;

/// The merge lane's state on the owner: its worker lane, the requests its jobs run for, the
/// provider and development it merges with and the limits it holds them to. Tests replace the
/// development (no decodable RAW bracket is committed), lower the limits and arrange failures.
pub(super) struct MergeLane {
    pub(super) lane: LibraryLane,
    /// Each queued or running job's request.
    live: HashMap<JobId, Live>,
    /// The HDR provider this build carries, or why there is none.
    provider: Result<MergeProvider, String>,
    developer: Arc<dyn Developer>,
    limits: Limits,
    hooks: Hooks,
    /// Make the next publication's transaction fail with this, after the merge and its result are
    /// written, as a commit refused at its last step would.
    #[cfg(test)]
    publish_fails: Option<Error>,
}

impl MergeLane {
    pub(super) fn new(poster: Poster, board: Arc<ActivityBoard>) -> Self {
        Self {
            lane: LibraryLane::of_kind(LaneKind::Merge, poster, board),
            live: HashMap::new(),
            provider: Ok(job::provider()),
            developer: Arc::new(SharedDevelopment),
            limits: Limits::default(),
            hooks: Hooks::default(),
            #[cfg(test)]
            publish_fails: None,
        }
    }

    fn provider_state(&self) -> ProviderState {
        let built = job::provider();
        match &self.provider {
            Ok(provider) => ProviderState {
                module: provider.module.clone(),
                version: provider.version,
                available: true,
                reason: None,
            },
            Err(reason) => ProviderState {
                module: built.module,
                version: built.version,
                available: false,
                reason: Some(reason.clone()),
            },
        }
    }

    /// The provider a new merge uses, or `not-ready` naming why there is none.
    fn available_provider(&self) -> Result<MergeProvider, Error> {
        self.provider.clone().map_err(|reason| {
            Error::not_ready(format!("HDR merging is not available: {reason}"))
                .with_data(json!({"provider": job::PROVIDER_MODULE, "reason": reason}))
        })
    }
}

/// A queued or running merge: its request's folder, and the exact-match key a new merge's
/// duplicate is found by.
struct Live {
    folder: String,
    key: Option<String>,
}

/// The job already merging a request, answered as its own retry: the same job.
fn same_job(owner: &Owner, job_id: &JobId) -> crate::catalog_types::JobStarted {
    crate::catalog_types::JobStarted {
        job_id: job_id.clone(),
        status: owner
            .jobs
            .read(job_id)
            .map_or(crate::jobs::JobStatus::Queued, |job| job.status),
        deduplicated: true,
    }
}

// Inputs.

/// One input a request names, resolved on the owner from the index, the catalog and one stat.
#[derive(Clone, Debug)]
struct Named {
    path: PathBuf,
    file_id: Option<FileId>,
    /// The file's signature now; none when it is not there.
    signature: Option<FileSignature>,
    volume_id: Option<VolumeId>,
    asset_id: Option<AssetId>,
    /// The catalog folder of the photograph it is.
    folder: Option<CatalogFolderId>,
    header: Option<HeaderMetadata>,
    kind: Option<SourceTag>,
}

/// One item of a request's targets.
enum Item {
    Path(PathBuf),
    File(FileId),
    Asset(AssetId),
    Row(AssetRowId),
}

/// The inputs `targets` names, in the stack's order: files by path, index row or selection, and
/// photographs by identity or selection, each resolved to its original's path. Refused with
/// `validation` for an unknown item or a merged photograph and with a typed refusal for a count
/// outside 2–9.
fn inputs(owner: &Owner, client: ClientId, targets: &Targets) -> Result<Vec<Named>, Error> {
    if let Some(count) = targets.len()
        && !(MIN_INPUTS..=MAX_INPUTS).contains(&count)
    {
        return Err(Refusal::InputCount { count }.into());
    }
    let items: Vec<Item> = match targets {
        Targets::Paths { paths } => paths.iter().cloned().map(Item::Path).collect(),
        Targets::Files { file_ids } => file_ids.iter().copied().map(Item::File).collect(),
        Targets::Assets { asset_ids } => asset_ids.iter().cloned().map(Item::Asset).collect(),
        Targets::Selection => owner
            .catalog
            .views
            .selected(client)?
            .into_iter()
            .map(|item| match item {
                ViewItem::File(id) => Ok(Item::File(id)),
                ViewItem::Photo(row) => Ok(Item::Row(row)),
                // A provenance row is an original an active stack already claims.
                ViewItem::Input(_) => Err(Error::validation(
                    "a selected original belongs to an HDR stack already; remove that merge first",
                )),
            })
            .collect::<Result<_, _>>()?,
    };
    if !(MIN_INPUTS..=MAX_INPUTS).contains(&items.len()) {
        return Err(Refusal::InputCount { count: items.len() }.into());
    }
    let catalog = &owner.service.connection;
    let index = owner.service.index()?;
    let index = index.connection();
    let mut named = Vec::with_capacity(items.len());
    for item in items {
        let (path, file_id, photograph) = match item {
            Item::Path(path) => {
                if !path.is_absolute() {
                    return Err(Error::validation(format!(
                        "{} is not an absolute path",
                        path.display()
                    )));
                }
                let path = path.canonicalize().unwrap_or(path);
                let photograph = photograph_at(catalog, &path)?;
                (path, None, photograph)
            }
            Item::File(id) => {
                let record = crate::index::database::file(index, id)?
                    .ok_or_else(|| Error::validation(format!("unknown file {}", id.0)))?;
                let photograph = photograph_at(catalog, &record.path)?;
                (record.path, Some(id), photograph)
            }
            Item::Asset(id) => {
                let photograph = photograph_by(catalog, "id", &id.to_string())?
                    .ok_or_else(|| Error::validation(format!("unknown asset {id}")))?;
                (photograph.path.clone(), None, Some(photograph))
            }
            Item::Row(row) => {
                let photograph =
                    photograph_by(catalog, "row_id", &row.0.to_string())?.ok_or_else(|| {
                        Error::conflict("a selected photograph is no longer in the catalog")
                    })?;
                (photograph.path.clone(), None, Some(photograph))
            }
        };
        if let Some(photograph) = &photograph
            && photograph.derived
        {
            merge::refuse_generated(catalog, &photograph.asset_id)?;
        }
        let file_id = match file_id {
            Some(id) => Some(id),
            None => index
                .prepare_cached("SELECT id FROM files WHERE path = ?1")?
                .query_row([path.to_string_lossy()], |row| row.get(0))
                .optional()?
                .map(FileId),
        };
        let record = match file_id {
            Some(id) => crate::index::database::file(index, id)?,
            None => None,
        };
        let header = match record.as_ref().map(|record| &record.header) {
            Some(crate::catalog_types::HeaderState::Ok(header)) => Some((**header).clone()),
            _ => match &photograph {
                Some(photograph) => {
                    crate::editor::capture_of(catalog, photograph.row)?.map(|(header, _)| header)
                }
                None => None,
            },
        };
        let signature = path
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| FileSignature::of(&metadata));
        let kind = record
            .as_ref()
            .map(|record| record.kind)
            .or(photograph.as_ref().and_then(|photograph| photograph.kind));
        named.push(Named {
            volume_id: record.as_ref().map(|record| record.volume_id.clone()),
            asset_id: photograph
                .as_ref()
                .map(|photograph| photograph.asset_id.clone()),
            folder: photograph.map(|photograph| photograph.folder),
            path,
            file_id,
            signature,
            header,
            kind,
        });
    }
    let frames: Vec<FrameMetadata> = named.iter().map(planned_frame).collect();
    let order = capture_order(&frames);
    Ok(order.into_iter().map(|at| named[at].clone()).collect())
}

/// A catalog photograph an input is.
struct Photograph {
    asset_id: AssetId,
    row: AssetRowId,
    path: PathBuf,
    folder: CatalogFolderId,
    kind: Option<SourceTag>,
    derived: bool,
}

fn photograph_at(catalog: &Connection, path: &Path) -> Result<Option<Photograph>, Error> {
    photograph_by(catalog, "canonical_locator", &path.to_string_lossy())
}

/// The photograph whose `column` is `value`: one indexed read.
fn photograph_by(
    catalog: &Connection,
    column: &str,
    value: &str,
) -> Result<Option<Photograph>, Error> {
    let row: Option<(String, i64, String, String, String)> = catalog
        .prepare_cached(&format!(
            "SELECT id, row_id, canonical_locator, catalog_folder_id, source_kind FROM assets
             WHERE {column} = ?1"
        ))?
        .query_row([value], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .optional()?;
    row.map(|(id, row, path, folder, kind)| {
        Ok(Photograph {
            asset_id: AssetId::parse(id)?,
            row: AssetRowId(row),
            path: PathBuf::from(path),
            folder: CatalogFolderId::parse(folder)?,
            kind: match kind.as_str() {
                "raw" => Some(SourceTag::Raw),
                "jpeg" => Some(SourceTag::Jpeg),
                _ => None,
            },
            derived: kind == "derived",
        })
    })
    .transpose()
}

/// An input's interpretation from its header alone, for header-level suitability: its kind (the
/// index's or the catalog's, else its extension's), body, upright size, exposure and capture time.
/// A RAW's recording mode is not in its header; its extension stands for it until the job decodes
/// it.
fn planned_frame(named: &Named) -> FrameMetadata {
    let extension = named
        .path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let jpeg = match named.kind {
        Some(kind) => kind == SourceTag::Jpeg,
        None => matches!(extension.as_str(), "jpg" | "jpeg"),
    };
    let header = named.header.clone().unwrap_or_default();
    let orientation = header
        .orientation
        .map_or(1, |orientation| orientation.get());
    let (width, height) = header.dimensions.map_or((0, 0), |size| {
        if (5..=8).contains(&orientation) {
            (size.height, size.width)
        } else {
            (size.width, size.height)
        }
    });
    FrameMetadata {
        identity: named.path.to_string_lossy().into_owned(),
        make: header.camera.as_ref().map(|body| body.make.clone()),
        model: header.camera.as_ref().map(|body| body.model.clone()),
        recording: if jpeg {
            Recording::Jpeg
        } else {
            Recording::Raw {
                mode: format!("file:{extension}"),
                layout: SensorLayout::Bayer,
            }
        },
        width,
        height,
        orientation,
        exposure: job::settings(&header),
        captured_at_ms: header.capture.as_ref().map(|time| time.instant_ms()),
    }
}

/// The header-level frames of `inputs`, every size unknown when any is, so an unread header is
/// not taken for another size.
fn planned_frames(inputs: &[Named]) -> Vec<FrameMetadata> {
    let mut frames: Vec<FrameMetadata> = inputs.iter().map(planned_frame).collect();
    if frames.iter().any(|frame| frame.width == 0) {
        for frame in &mut frames {
            (frame.width, frame.height) = (0, 0);
        }
    }
    frames
}

/// The options a request names, in the stack's positions: the reference and the overrides by
/// path, each refused with `validation` when it names no input.
fn parameters(
    inputs: &[Named],
    reference: Option<&Path>,
    overrides: Option<&[ExposureOverride]>,
    alignment: AlignmentOptions,
) -> Result<HdrParameters, Error> {
    let position = |path: &Path| {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        inputs
            .iter()
            .position(|input| input.path == path)
            .ok_or_else(|| {
                Error::validation(format!(
                    "{} is not one of the merge's inputs",
                    path.display()
                ))
            })
    };
    let reference = reference.map(position).transpose()?;
    let mut offsets = Vec::new();
    if let Some(overrides) = overrides.filter(|overrides| !overrides.is_empty()) {
        offsets = vec![None; inputs.len()];
        for exposure in overrides {
            if !exposure.offset_ev.is_finite() {
                return Err(Error::validation("an exposure override must be finite"));
            }
            let at = position(&exposure.path)?;
            if offsets[at].replace(exposure.offset_ev).is_some() {
                return Err(Error::validation(format!(
                    "{} is overridden twice",
                    exposure.path.display()
                )));
            }
        }
    }
    Ok(HdrParameters {
        exposure: ExposureOptions {
            reference,
            overrides: offsets,
        },
        alignment,
        ..HdrParameters::default()
    })
}

/// A plan's identity: the SHA-256 of its inputs in the stack's order (path, signature and
/// photograph) and its options.
fn plan_identity(inputs: &[Named], parameters: &HdrParameters) -> Result<String, Error> {
    let named: Vec<_> = inputs
        .iter()
        .map(|input| (&input.path, &input.signature, &input.asset_id))
        .collect();
    let text = serde_json::to_vec(&(named, parameters))
        .map_err(|error| Error::internal(format!("cannot encode a merge plan: {error}")))?;
    Ok(format!("{:x}", Sha256::digest(text)))
}

/// The exact-match key of a request, when every input is there to be signed.
fn request_key(
    inputs: &[Named],
    provider: &MergeProvider,
    parameters: &HdrParameters,
) -> Result<Option<String>, Error> {
    let mut signed = Vec::with_capacity(inputs.len());
    for input in inputs {
        let Some(signature) = &input.signature else {
            return Ok(None);
        };
        signed.push((&input.path, signature));
    }
    match_key(MergeKind::Hdr, provider, signed, parameters).map(Some)
}

/// The folder a merge of `inputs` proposes: the folder made from their one event; else the one
/// folder every catalog photograph among them is in; else the reference photograph's. An event
/// with no folder yet answers its new folder's choice instead, which `folder.create` makes: a
/// merge's result goes into an existing folder, since undoing the merge keeps its result.
fn proposal(
    owner: &Owner,
    inputs: &[Named],
    reference: Option<usize>,
) -> Result<(Option<FolderProposal>, Option<FolderChoice>), Error> {
    let catalog = &owner.service.connection;
    let named = |folder_id: &CatalogFolderId, from: &str| -> Result<FolderProposal, Error> {
        Ok(FolderProposal {
            name: crate::library::folders::folder(catalog, folder_id)?.name,
            folder_id: folder_id.clone(),
            from: from.to_owned(),
        })
    };
    let files: Vec<crate::library::targets::NamedFile> = inputs
        .iter()
        .map(|input| crate::library::targets::NamedFile {
            path: input.path.clone(),
            file_id: input.file_id,
            signature: input.signature,
            volume_id: input.volume_id.clone(),
        })
        .collect();
    let mut volumes = HashMap::new();
    let index = owner.service.index()?;
    let plan = develop::plan(catalog, index.connection(), files, &mut volumes)?;
    drop(index);
    let mut suggested = None;
    if let [event] = plan.events.as_slice() {
        match &event.proposal {
            FolderChoice::Existing { folder_id } => {
                return Ok((Some(named(folder_id, "event")?), None));
            }
            choice @ FolderChoice::New { .. } => suggested = Some(choice.clone()),
        }
    }
    let folders: Vec<&CatalogFolderId> = inputs
        .iter()
        .filter_map(|input| input.folder.as_ref())
        .collect();
    if let Some(first) = folders.first()
        && folders.iter().all(|folder| folder == first)
    {
        return Ok((Some(named(first, "inputs")?), None));
    }
    if let Some(folder) = reference
        .and_then(|at| inputs.get(at))
        .and_then(|input| input.folder.as_ref())
    {
        return Ok((Some(named(folder, "reference")?), None));
    }
    Ok((None, suggested))
}

/// The reference the metadata picks, when the headers make a bracket.
fn header_reference(frames: &[FrameMetadata], parameters: &HdrParameters) -> Option<usize> {
    match resolve_exposures(frames, &parameters.exposure) {
        Ok(ExposureResolution::Resolved(exposures)) => Some(exposures.reference),
        Ok(ExposureResolution::NeedsFit(plan)) => Some(plan.reference()),
        Err(_) => parameters.exposure.reference,
    }
}

/// The merges folder under the catalog's artifact root, refused with `incompatible` when the root
/// is another catalog's.
fn merges_root(service: &EditorService) -> Result<PathBuf, Error> {
    let root = service.artifact_root();
    if let RootState::Foreign(detail) = root_state(root, service.catalog_id())? {
        return Err(Error::incompatible(detail));
    }
    Ok(root.join(MERGES))
}

/// What a merge would take, from the headers' size.
fn estimates(
    lane: &MergeLane,
    inputs: &[Named],
    frames: &[FrameMetadata],
    root: &Path,
) -> MergeEstimates {
    let known = frames.first().filter(|frame| frame.width > 0);
    let available = lane.limits.available.or_else(|| {
        root.ancestors()
            .find(|path| path.exists())
            .and_then(|path| luxforge_process::disk::available_bytes(path).ok())
    });
    MergeEstimates {
        width: known.map(|frame| frame.width),
        height: known.map(|frame| frame.height),
        temp_bytes: known.map(|frame| job::temp_bytes(inputs.len(), frame.width, frame.height)),
        source_bytes: known.map(|frame| u64::from(frame.width) * u64::from(frame.height) * 12),
        available_bytes: available,
        temp_ceiling_bytes: lane.limits.temp_ceiling,
        scratch_limit_bytes: lane.limits.scratch,
    }
}

fn alignment(alignment: Option<AlignmentMode>, deghost: Option<DeghostMode>) -> AlignmentOptions {
    AlignmentOptions {
        alignment: alignment.unwrap_or_default(),
        deghost: deghost.unwrap_or_default(),
    }
}

// The methods.

/// `merge.plan`: see [`MergePlan`]. Reads the catalog, the index and one stat an input; refused
/// only for targets that name no bracket of 2–9 known items.
pub(in crate::api) fn merge_plan(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergePlanParams,
) -> Result<Value, Error> {
    let inputs = inputs(owner, call.client, &params.targets)?;
    let parameters = parameters(
        &inputs,
        params.reference.as_deref(),
        params.exposure_overrides.as_deref(),
        alignment(params.alignment, params.deghost),
    )?;
    let frames = planned_frames(&inputs);
    let (suitable, refusal, exposures, needs_fit) =
        match resolve_exposures(&frames, &parameters.exposure) {
            Ok(ExposureResolution::Resolved(exposures)) => (true, None, Some(exposures), false),
            Ok(ExposureResolution::NeedsFit(_)) => (true, None, None, true),
            Err(refusal) => {
                let message = Error::from(refusal.clone()).detail;
                let mut refusal = serde_json::to_value(&refusal).unwrap_or(Value::Null);
                if let Some(object) = refusal.as_object_mut() {
                    object.insert("message".into(), Value::String(message));
                }
                (false, Some(refusal), None, false)
            }
        };
    let reference = header_reference(&frames, &parameters);
    let (folder, suggested_folder) = proposal(owner, &inputs, reference)?;
    let lane = &owner.catalog.merge;
    let provider = lane.provider_state();
    let existing = match &lane.provider {
        Ok(provider) => match request_key(&inputs, provider, &parameters)? {
            Some(key) => matching(&owner.service.connection, &key)?,
            None => None,
        },
        Err(_) => None,
    };
    let claimed = claims(
        &owner.service.connection,
        inputs.iter().map(|input| input.path.clone()),
    )?;
    let root = merges_root(&owner.service)?;
    let answer = MergePlan {
        kind: MergeKind::Hdr,
        plan_id: plan_identity(&inputs, &parameters)?,
        inputs: inputs
            .iter()
            .zip(&frames)
            .enumerate()
            .map(|(ordinal, (input, frame))| PlannedInput {
                ordinal: ordinal as u32,
                file_name: file_name(&input.path),
                path: input.path.clone(),
                file_id: input.file_id,
                asset_id: input.asset_id.clone(),
                available: input.signature.is_some(),
                frame: frame.clone(),
            })
            .collect(),
        suitable,
        refusal,
        exposures,
        needs_fit,
        reference: reference.map(|at| at as u32),
        folder_reason: folder.is_none().then(|| {
            "no catalog folder holds these frames' event or photographs yet: create one with \
             folder.create and name it as folder_id"
                .to_owned()
        }),
        folder,
        suggested_folder,
        estimates: estimates(lane, &inputs, &frames, &root),
        provider,
        existing,
        claimed,
        parameters,
    };
    value(answer)
}

/// `merge.create`: admit the request on the owner (see the module documentation) and start an
/// `hdr-merge` job, answering `{job_id, status, deduplicated}`; its result is a [`MergeAnswer`].
pub(in crate::api) fn merge_create(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergeCreate,
) -> Result<Value, Error> {
    let MergeKind::Hdr = params.kind;
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answered) = retried(owner, call, request)? {
        return value(answered);
    }
    let provider = owner.catalog.merge.available_provider()?;
    // Pick merged: the set the result joins is the one current now, which the client's `set_id`,
    // when named, must be; a set replaced while the job runs is the publication's conflict.
    let pick = params.add_to_development_set.unwrap_or(false);
    let capture = if pick {
        let connection = &owner.service.connection;
        development_set::check_expected(
            development_set::current(connection)?.as_ref(),
            params.set_id.as_ref(),
        )?;
        Some(development_set::capture(connection)?)
    } else {
        None
    };
    let inputs = inputs(owner, call.client, &params.targets)?;
    let parameters = parameters(
        &inputs,
        params.reference.as_deref(),
        params.exposure_overrides.as_deref(),
        alignment(params.alignment, params.deghost),
    )?;
    if let Some(plan_id) = &params.plan_id
        && *plan_id != plan_identity(&inputs, &parameters)?
    {
        return Err(Error::conflict(
            "the inputs or options are no longer what merge.plan planned; plan them again",
        )
        .with_data(json!({"plan_id": plan_id})));
    }
    let frames = planned_frames(&inputs);
    resolve_exposures(&frames, &parameters.exposure)?;
    let signed = signed(&inputs)?;
    let key = request_key(&inputs, &provider, &parameters)?
        .ok_or_else(|| Error::internal("a signed request has no key"))?;
    if let Some((job_id, _)) = owner
        .catalog
        .merge
        .live
        .iter()
        .find(|(_, live)| live.key.as_ref() == Some(&key))
    {
        return value(same_job(owner, job_id));
    }
    if let Some(existing) = matching(&owner.service.connection, &key)? {
        if existing.state == MergeState::Active {
            let mut answer = recorded_answer(&owner.service.connection, &existing.merge_id, true)?;
            if pick {
                answer.change = pick_existing(owner, call, request, &existing.result_asset_id)?;
            }
            return value(LibraryLane::answered(
                &mut owner.jobs,
                &HDR_MERGE,
                None,
                call.origin.clone(),
                encode(&answer)?,
            ));
        }
        return Err(Error::conflict(format!(
            "this exact merge is in Removed as {}; put it back (merge.restore) rather than \
             merging it again",
            file_name_of(&owner.service.connection, &existing.result_asset_id)
        ))
        .with_data(json!({
            "put_back": {"merge_id": existing.merge_id, "result_asset_id": existing.result_asset_id},
        })));
    }
    // Checked after the exact match, which an active stack of its own claims.
    if let Some(claim) = claims(
        &owner.service.connection,
        inputs.iter().map(|input| input.path.clone()),
    )?
    .into_iter()
    .next()
    {
        return Err(overlap(&owner.service.connection, &claim).with_data(json!({
            "stack": claim.merge_id,
            "result_asset_id": claim.result_asset_id,
            "path": claim.path,
        })));
    }
    let reference = header_reference(&frames, &parameters);
    let folder = match params.folder_id {
        Some(folder_id) => {
            crate::library::folders::folder(&owner.service.connection, &folder_id)?;
            folder_id
        }
        None => proposal(owner, &inputs, reference)?
            .0
            .map(|proposal| proposal.folder_id)
            .ok_or_else(|| {
                Error::validation(
                    "no catalog folder holds these frames' event or photographs yet: create one \
                     with folder.create and name it as folder_id",
                )
            })?,
    };
    let job_inputs = inputs
        .iter()
        .zip(signed)
        .zip(frames)
        .map(|((input, signature), planned)| JobInput {
            path: input.path.clone(),
            signature,
            asset_id: input.asset_id.clone(),
            fingerprint: None,
            planned,
        })
        .collect();
    start(
        owner,
        call,
        request,
        Started {
            inputs: job_inputs,
            parameters,
            provider,
            folder,
            replaces: None,
            reuse: None,
            key: Some(key),
            mutation: params.mutation.clone(),
            membership: capture,
        },
    )
}

/// Pick merged of a result that exists: append it to the current set as one library change, or
/// start a one-result set, and answer the change. A result already a member changes nothing.
fn pick_existing(
    owner: &mut Owner,
    call: &Call<'_>,
    request: Request<'_>,
    result: &AssetId,
) -> Result<Option<crate::catalog_types::LibraryChangeSeq>, Error> {
    let connection = &owner.service.connection;
    let current = development_set::current(connection)?;
    if let Some(set) = &current
        && development_set::member(connection, &set.id, result)?.is_some()
    {
        return Ok(None);
    }
    let label = format!(
        "Added {} to the development set",
        file_name_of(connection, result)
    );
    let answer = change(owner, &call.origin, |tx| {
        let set = match current {
            Some(set) => set,
            None => {
                development_set::replace(tx, Some(&[]))?.expect("an empty replacement starts a set")
            }
        };
        journal::apply(
            tx,
            request,
            development_set::additions(&set, std::slice::from_ref(result))?,
            |_| label,
        )
    })?;
    Ok(answer.change)
}

/// `merge.rerun`: merge a recorded merge's exact inputs again, with its recorded options save
/// those named, into a new result; see the module documentation for what it refuses and reuses.
pub(in crate::api) fn merge_rerun(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergeRerun,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answered) = retried(owner, call, request)? {
        return value(answered);
    }
    let recorded = record(&owner.service.connection, &params.merge_id)?;
    let provider = rerun_provider(&owner.catalog.merge, &recorded.provenance)?;
    let provenance = &recorded.provenance;
    for input in &provenance.inputs {
        input_unchanged(&input.path, &input.signature)?;
    }
    let named: Vec<Named> = provenance
        .inputs
        .iter()
        .map(|input| Named {
            path: input.path.clone(),
            file_id: None,
            signature: Some(input.signature),
            volume_id: Some(input.volume_id.clone()),
            asset_id: input.asset_id.clone(),
            folder: None,
            header: None,
            kind: Some(SourceTag::Raw),
        })
        .collect();
    let old = &provenance.parameters;
    let mut parameters = parameters(
        &named,
        params.reference.as_deref(),
        params.exposure_overrides.as_deref(),
        AlignmentOptions {
            alignment: params.alignment.unwrap_or(old.alignment.alignment),
            deghost: params.deghost.unwrap_or(old.alignment.deghost),
        },
    )?;
    if params.reference.is_none() {
        parameters.exposure.reference = old.exposure.reference;
    }
    if params.exposure_overrides.is_none() {
        parameters.exposure.overrides = old.exposure.overrides.clone();
    }
    parameters.fusion = old.fusion;
    let others: Vec<_> = claims(
        &owner.service.connection,
        named.iter().map(|input| input.path.clone()),
    )?
    .into_iter()
    .filter(|claim| claim.merge_id != recorded.merge_id)
    .collect();
    if let Some(claim) = others.first() {
        return Err(overlap(&owner.service.connection, claim).with_data(json!({
            "stack": claim.merge_id,
            "result_asset_id": claim.result_asset_id,
            "path": claim.path,
        })));
    }
    let folder = match params.folder_id {
        Some(folder_id) => {
            crate::library::folders::folder(&owner.service.connection, &folder_id)?;
            folder_id
        }
        None => photograph_by(
            &owner.service.connection,
            "id",
            recorded.result_asset_id.as_str(),
        )?
        .map(|photograph| photograph.folder)
        .ok_or_else(|| Error::validation("name folder_id for the new result"))?,
    };
    // An exact match whose saved source is whole is given to the new result as it is; anything
    // else is merged again.
    let exact = request_key(&named, &provider, &parameters)?.as_deref()
        == Some(provenance.match_key()?.as_str());
    let reuse = if exact
        && owner
            .service
            .derived_source_read(&recorded.result_asset_id)
            .is_ok()
    {
        crate::editor::derived_source::recorded_source(
            &owner.service.connection,
            &recorded.source_id,
        )?
        .map(|source| (provenance.clone(), source))
    } else {
        None
    };
    let inputs = provenance
        .inputs
        .iter()
        .map(|input| JobInput {
            path: input.path.clone(),
            signature: input.signature,
            asset_id: input.asset_id.clone(),
            fingerprint: Some(input.frame.identity.clone()),
            planned: input.frame.clone(),
        })
        .collect();
    start(
        owner,
        call,
        request,
        Started {
            inputs,
            parameters,
            provider,
            folder,
            replaces: Some(recorded.merge_id),
            reuse,
            key: None,
            mutation: params.mutation.clone(),
            membership: None,
        },
    )
}

/// `merge.inspect`: see [`MergeInspection`]. SQL and one stat an object and an input; nothing is
/// read or hashed.
pub(in crate::api) fn merge_inspect(
    owner: &mut Owner,
    _: &Call<'_>,
    params: MergeInspect,
) -> Result<Value, Error> {
    let catalog = &owner.service.connection;
    let merge_id = match (params.merge_id, params.asset_id) {
        (Some(merge_id), None) => merge_id,
        (None, Some(asset_id)) => merge::merge_of(catalog, &asset_id)?.ok_or_else(|| {
            Error::validation(format!("photograph {asset_id} is not a merge's result"))
        })?,
        _ => {
            return Err(Error::validation(
                "name exactly one of merge_id and asset_id",
            ));
        }
    };
    let recorded = record(catalog, &merge_id)?;
    let source = match owner.service.derived_source_read(&recorded.result_asset_id) {
        Ok(_) => SourceAvailability {
            status: "available".into(),
            message: None,
        },
        Err(error) => SourceAvailability {
            status: if error.detail.ends_with("corrupt") {
                "corrupt"
            } else if error.kind == ErrorKind::SourceUnavailable {
                "missing"
            } else {
                "unavailable"
            }
            .into(),
            message: Some(error.detail),
        },
    };
    let inputs: Vec<InspectedInput> = recorded
        .members
        .iter()
        .zip(&recorded.provenance.inputs)
        .map(|(member, input)| InspectedInput {
            file: match input_unchanged(&input.path, &input.signature) {
                Ok(()) => "available",
                Err(error) if error.kind == ErrorKind::Conflict => "changed",
                Err(_) => "missing",
            }
            .into(),
            member: member.clone(),
        })
        .collect();
    let rerun =
        rerun_provider(&owner.catalog.merge, &recorded.provenance).and_then(|_| {
            match inputs.iter().find(|input| input.file != "available") {
                Some(input) => Err(Error::conflict(format!(
                    "{} is {}",
                    file_name(&input.member.path),
                    input.file
                ))),
                None => Ok(()),
            }
        });
    let (low, high) = recorded.provenance.resolved.exposures.range_ev();
    value(MergeInspection {
        merge_id,
        kind: recorded.kind,
        state: recorded.state,
        result_asset_id: recorded.result_asset_id,
        source_id: recorded.source_id,
        created_ms: recorded.created_ms,
        source,
        merged: true,
        exposure_range_ev: [low, high],
        parameters: recorded.provenance.parameters.clone(),
        provider: owner.catalog.merge.provider_state(),
        rerunnable: rerun.is_ok(),
        rerun_reason: rerun.err().map(|error| error.detail),
        inputs,
        provenance: recorded.provenance,
    })
}

/// `merge.remove`: Remove merge, which is Remove of the merges' results
/// (`crate::library::remove`): one library change, each result to Removed with its stack
/// deactivated.
pub(in crate::api) fn merge_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergeTargets,
) -> Result<Value, Error> {
    lifecycle(owner, call, &params, |connection, results| {
        remove::remove(connection, results, now_ms())
    })
}

/// `merge.restore`: Put back merge, which is Put back of the merges' results: one library change,
/// each result visible again with its stack active, refused when another active stack claims an
/// input.
pub(in crate::api) fn merge_restore(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergeTargets,
) -> Result<Value, Error> {
    lifecycle(owner, call, &params, remove::restore)
}

fn lifecycle(
    owner: &mut Owner,
    call: &Call<'_>,
    params: &MergeTargets,
    plan: impl FnOnce(
        &Connection,
        Vec<(AssetId, AssetRowId)>,
    ) -> Result<crate::library::tree::Planned, Error>,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = super::library::retried(owner, request)? {
        return value(answer);
    }
    if params.merge_ids.is_empty() {
        return Err(Error::validation("merge_ids name nothing"));
    }
    let results = results(&owner.service.connection, &params.merge_ids)?;
    let planned = plan(&owner.service.connection, results)?;
    value(change(owner, &call.origin, |tx| {
        planned.apply(tx, request)
    })?)
}

// Admission and the job.

/// A merge admitted on the owner, for [`start`].
struct Started {
    inputs: Vec<JobInput>,
    parameters: HdrParameters,
    provider: MergeProvider,
    folder: CatalogFolderId,
    replaces: Option<MergeId>,
    /// A rerun's exact match: the recorded provenance and source the new result is given.
    reuse: Option<(MergeProvenance, PublishedSource)>,
    /// A new merge's exact-match key, which a later request for the same merge finds it by while
    /// it runs. None for a rerun, which always makes a new result.
    key: Option<String>,
    mutation: MutationRequest,
    /// Pick merged: the set the request was made against, which the result joins when published.
    membership: Option<SetCapture>,
}

/// Every input's signature, refused with `source-unavailable` naming the first that is not there.
fn signed(inputs: &[Named]) -> Result<Vec<FileSignature>, Error> {
    inputs
        .iter()
        .map(|input| {
            input.signature.ok_or_else(|| {
                Error::source_unavailable(format!(
                    "{} is not at {}",
                    file_name(&input.path),
                    input.path.display()
                ))
                .with_data(json!({"path": input.path}))
            })
        })
        .collect()
}

/// Refuse an input that is gone (`source-unavailable`) or changed (`conflict`) since `signature`.
fn input_unchanged(path: &Path, signature: &FileSignature) -> Result<(), Error> {
    let metadata = path
        .metadata()
        .ok()
        .filter(|metadata| metadata.is_file())
        .ok_or_else(|| {
            Error::source_unavailable(format!(
                "{} is no longer at {}",
                file_name(path),
                path.display()
            ))
            .with_data(json!({"path": path}))
        })?;
    if !FileSignature::of(&metadata).unchanged(signature) {
        return Err(
            Error::conflict(format!("{} changed since it was merged", file_name(path)))
                .with_data(json!({"path": path})),
        );
    }
    Ok(())
}

/// The provider a rerun of `provenance` merges with: this build's, which must be the one and the
/// version that made it.
fn rerun_provider(lane: &MergeLane, provenance: &MergeProvenance) -> Result<MergeProvider, Error> {
    let provider = lane.available_provider()?;
    if provider != provenance.provider {
        return Err(Error::incompatible(format!(
            "this merge was made by {} version {}, and this build merges with {} version {}; \
             merge the inputs as a new merge instead",
            provenance.provider.module,
            provenance.provider.version,
            provider.module,
            provider.version
        ))
        .with_data(json!({"recorded": provenance.provider, "available": provider})));
    }
    Ok(provider)
}

/// Queue the admitted merge: refuse it while the lane is full, preflight what the headers say it
/// will spool, record the request's marker and start the job.
fn start(
    owner: &mut Owner,
    call: &Call<'_>,
    request: Request<'_>,
    started: Started,
) -> Result<Value, Error> {
    let lane = &owner.catalog.merge;
    if lane.lane.is_full() {
        return Err(Error::resource_limit(format!(
            "a merge is running and {MERGE_QUEUE} waits; wait for one to end"
        )));
    }
    let root = merges_root(&owner.service)?;
    let key = request_dir(request);
    let spool = root.join(&key);
    let n = started.inputs.len();
    let (width, height) = started
        .inputs
        .first()
        .map_or((0, 0), |input| (input.planned.width, input.planned.height));
    fs::create_dir_all(&spool).map_err(|error| {
        crate::atomic_file::file_error("cannot create the merge's folder", error.kind())
    })?;
    if started.reuse.is_none()
        && width > 0
        && let Err(error) = lane
            .limits
            .preflight(&spool, job::temp_bytes(n, width, height))
    {
        let _ = fs::remove_dir_all(&spool);
        return Err(error);
    }
    write_marker(&spool, request, None)?;
    prune_markers(&root, &lane.live);
    let job_id = JobId::new();
    let spec = JobSpec {
        inputs: started.inputs,
        parameters: started.parameters,
        provider: started.provider,
        spool: spool.clone(),
        writer: owner.service.derived_source_writer(),
        developer: lane.developer.clone(),
        limits: lane.limits,
        hooks: lane.hooks.clone(),
    };
    let publish = Publish {
        folder: started.folder,
        replaces: started.replaces,
        method: call.request.method.clone(),
        mutation: started.mutation,
        origin: call.origin.clone(),
        membership: started.membership,
    };
    let reuse = started.reuse;
    let matched = started.key;
    let detail = format!(
        "{} \u{b7} {n} exposures",
        job::result_name(
            &spec.inputs[spec.parameters.exposure.reference.unwrap_or((n - 1) / 2)].path
        )
    );
    let task = Task {
        job_id: job_id.clone(),
        job: &HDR_MERGE,
        asset_id: None,
        detail: Some(detail),
        origin: call.origin.clone(),
        work: Box::new(move |context: &JobContext<'_>| {
            job::sweep_orphans(&root, &spool);
            let merged = match reuse {
                Some((provenance, source)) => verified(spec, provenance, source, context.control),
                None => spec.run(context.control),
            };
            match merged {
                Ok(merged) => Box::new(move |owner: &mut Owner| publish.commit(owner, merged)),
                Err(error) => Box::new(move |_| Err(error)),
            }
        }),
    };
    let started = owner.catalog.merge.lane.queue(&mut owner.jobs, task);
    match started {
        Ok(started) => {
            owner.catalog.merge.live.insert(
                job_id,
                Live {
                    folder: key,
                    key: matched,
                },
            );
            value(started)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(owner.service.artifact_root().join(MERGES).join(key));
            Err(error)
        }
    }
}

/// A rerun's exact match: every input read once and its SHA-256 checked against the merge's record,
/// with no development and no merging; the new result is given the recorded source.
fn verified(
    spec: JobSpec,
    provenance: MergeProvenance,
    source: PublishedSource,
    control: &JobControl,
) -> Result<Merged, Error> {
    control.set_phase("reading");
    let n = spec.inputs.len();
    let mut volumes: Vec<crate::catalog_types::Volume> = Vec::new();
    for (at, input) in spec.inputs.iter().enumerate() {
        control.checkpoint()?;
        control.set_progress(
            Some(at as f64 / n as f64),
            &format!("Verifying {} of {n}", at + 1),
        );
        input_unchanged(&input.path, &input.signature)?;
        let fingerprint = fingerprint(&input.path, control)?;
        if Some(&fingerprint) != input.fingerprint.as_ref() {
            return Err(Error::conflict(format!(
                "{} is not the file this merge recorded; its bytes differ",
                file_name(&input.path)
            ))
            .with_data(json!({"path": input.path})));
        }
        let volume = crate::index::volume_of(&input.path, now_ms())?;
        if !volumes.iter().any(|known| known.id == volume.id) {
            volumes.push(volume);
        }
    }
    control.set_phase("saving");
    let reference = provenance.reference();
    let header = HeaderMetadata {
        camera: reference
            .frame
            .make
            .clone()
            .zip(reference.frame.model.clone())
            .map(|(make, model)| crate::catalog_types::CameraBody {
                make,
                model,
                serial: None,
            }),
        ..HeaderMetadata::default()
    };
    Ok(Merged {
        file_name: job::result_name(&reference.path),
        header: job::result_header(&header, source.width, source.height),
        place: None,
        volumes,
        provenance: MergeProvenance {
            parameters: spec.parameters,
            ..provenance
        },
        source,
        accounting: job::Accounting::default(),
    })
}

/// The SHA-256 of the file at `path`, streamed in bounded chunks, cancellable between them.
fn fingerprint(path: &Path, control: &JobControl) -> Result<String, Error> {
    use std::io::Read;
    let mut file = fs::File::open(path)
        .map_err(|error| crate::atomic_file::file_error(path.display(), error.kind()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        control.checkpoint()?;
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(crate::atomic_file::file_error(path.display(), error.kind())),
        };
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// The owner's half of a merge: what it publishes the job's result with.
struct Publish {
    folder: CatalogFolderId,
    replaces: Option<MergeId>,
    method: String,
    mutation: MutationRequest,
    origin: Origin,
    membership: Option<SetCapture>,
}

impl Publish {
    /// Publish `merged` in one transaction and one library change, announced as one event, and
    /// answer the [`MergeAnswer`]. Pick merged's development-set membership joins the same change
    /// through `publish_merge`'s `extra`; a set replaced since the request fails the whole
    /// publication with a conflict.
    fn commit(self, owner: &mut Owner, merged: Merged) -> Result<Value, Error> {
        let request = Request::new(&self.method, &self.mutation);
        #[cfg(test)]
        let fails = owner.catalog.merge.publish_fails.take();
        let set_before = development_set::revision(&owner.service.connection)?;
        let published = owner.service.publish_merge(
            request,
            &Publication {
                provenance: &merged.provenance,
                source: &merged.source,
                catalog_folder_id: &self.folder,
                file_name: &merged.file_name,
                header: &merged.header,
                place: merged.place.as_deref(),
                volumes: &merged.volumes,
                replaces: self.replaces.as_ref(),
            },
            |tx, _merge, result| {
                #[cfg(test)]
                if let Some(error) = fails {
                    return Err(error);
                }
                match &self.membership {
                    Some(capture) => {
                        development_set::membership_for_publication(tx, capture, result)
                    }
                    None => Ok(Vec::new()),
                }
            },
        )?;
        announce(owner, &self.origin, &published, set_before);
        let mut answer = recorded_answer(&owner.service.connection, &published.merge_id, false)?;
        answer.change = published.outcome.answer().change;
        answer.deduplicated = published.outcome.announced().is_none();
        answer.replaced = self.replaces.filter(|_| answer.change.is_some());
        answer.accounting = serde_json::to_value(merged.accounting).ok();
        encode(&answer)
    }
}

/// Announce a publication's library change as [`change`] announces one: one event naming its
/// sequence and the result, and the lanes that follow its items told; a change to the development
/// set's members names the set's revision it left, as [`change`] does.
fn announce(owner: &mut Owner, origin: &Origin, published: &PublishedMerge, set_before: u64) {
    if let Some(sequence) = published.outcome.announced() {
        let mut announced = origin
            .clone()
            .library(sequence)
            .changed(published.result.clone(), None);
        if let Ok(set_after) = development_set::revision(&owner.service.connection)
            && set_after != set_before
        {
            announced = announced.development_set(set_after);
        }
        announce_once(&mut owner.announced, &announced);
        owner.library_changed(origin, published.outcome.items());
    }
}

/// A recorded merge as a job answers it.
fn recorded_answer(
    connection: &Connection,
    merge_id: &MergeId,
    reused: bool,
) -> Result<MergeAnswer, Error> {
    let MergeRecord {
        result_asset_id,
        source_id,
        provenance,
        ..
    } = record(connection, merge_id)?;
    let (low, high) = provenance.resolved.exposures.range_ev();
    let photograph = photograph_by(connection, "id", result_asset_id.as_str())?
        .ok_or_else(|| Error::conflict(format!("merge {merge_id}'s result is gone")))?;
    Ok(MergeAnswer {
        merge_id: merge_id.clone(),
        file_name: file_name_of(connection, &result_asset_id),
        result_asset_id,
        source_id,
        catalog_folder_id: photograph.folder,
        merged: true,
        inputs: provenance.inputs.len() as u32,
        exposure_range_ev: [low, high],
        reused,
        replaced: None,
        change: None,
        deduplicated: reused,
        accounting: None,
    })
}

fn file_name_of(connection: &Connection, asset: &AssetId) -> String {
    connection
        .prepare_cached("SELECT file_name FROM assets WHERE id = ?1")
        .and_then(|mut statement| {
            statement
                .query_row([asset.as_str()], |row| row.get::<_, String>(0))
                .optional()
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| asset.to_string())
}

fn encode(answer: &MergeAnswer) -> Result<Value, Error> {
    serde_json::to_value(answer)
        .map_err(|error| Error::internal(format!("cannot encode a merge answer: {error}")))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

// Requests across a restart.

/// A request's marker: who asked for what and when, and, once its job ended without a result,
/// the error it ended with.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    format: u32,
    method: String,
    actor: String,
    request_id: String,
    created_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended: Option<JobError>,
}

/// The folder of `request` under the merges folder: the SHA-256 of its method, actor and identity.
fn request_dir(request: Request<'_>) -> String {
    let text = serde_json::to_vec(&(request.method, request.actor, request.request_id))
        .unwrap_or_default();
    format!("{:x}", Sha256::digest(text))
}

/// Write `request`'s marker in `folder`: staged beside it and renamed over it, never synced, since
/// a marker lost with the machine only makes a retry merge again.
fn write_marker(folder: &Path, request: Request<'_>, ended: Option<JobError>) -> Result<(), Error> {
    let marker = Marker {
        format: MARKER_FORMAT,
        method: request.method.to_owned(),
        actor: request.actor.to_owned(),
        request_id: request.request_id.to_owned(),
        created_ms: now_ms(),
        ended,
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|error| Error::internal(format!("cannot encode a merge marker: {error}")))?;
    let staged = folder.join(format!("{MARKER}.new"));
    fs::write(&staged, bytes)
        .and_then(|()| fs::rename(&staged, folder.join(MARKER)))
        .map_err(|error| {
            crate::atomic_file::file_error("cannot record the merge request", error.kind())
        })
}

fn read_marker(folder: &Path) -> Option<Marker> {
    let bytes = fs::read(folder.join(MARKER)).ok()?;
    serde_json::from_slice::<Marker>(&bytes)
        .ok()
        .filter(|marker| marker.format == MARKER_FORMAT)
}

/// Keep the newest [`MAX_MARKERS`] request folders, and every live job's.
fn prune_markers(root: &Path, live: &HashMap<JobId, Live>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut folders: Vec<(i64, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            !live
                .values()
                .any(|live| Some(&live.folder) == name.as_ref())
        })
        .map(|path| {
            (
                read_marker(&path).map_or(0, |marker| marker.created_ms),
                path,
            )
        })
        .collect();
    if folders.len() <= MAX_MARKERS {
        return;
    }
    folders.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, folder) in folders.into_iter().skip(MAX_MARKERS) {
        let _ = fs::remove_dir_all(folder);
    }
}

/// The answer to a retry of `request`, when it has one: its published merge, from the journal; or
/// its first attempt's end, from its marker — the error it ended with, or `cancelled` with
/// `data.reason` `interrupted` when the process stopped before it ended.
fn retried(
    owner: &mut Owner,
    call: &Call<'_>,
    request: Request<'_>,
) -> Result<Option<crate::catalog_types::JobStarted>, Error> {
    if let Some((published, picked)) = published_or_picked(&owner.service.connection, request)? {
        let mut answer = recorded_answer(&owner.service.connection, &published.merge_id, picked)?;
        answer.change = published.outcome.answer().change;
        answer.deduplicated = true;
        return Ok(Some(LibraryLane::answered(
            &mut owner.jobs,
            &HDR_MERGE,
            None,
            call.origin.clone(),
            encode(&answer)?,
        )));
    }
    let key = request_dir(request);
    if let Some((job_id, _)) = owner
        .catalog
        .merge
        .live
        .iter()
        .find(|(_, live)| live.folder == key)
    {
        return Ok(Some(same_job(owner, job_id)));
    }
    let folder = owner.service.artifact_root().join(MERGES).join(&key);
    let Some(marker) = read_marker(&folder) else {
        return Ok(None);
    };
    let error = match marker.ended {
        Some(ended) => {
            let mut error = Error::new(kind_of(&ended.code), ended.message);
            if let Some(data) = ended.data {
                error = error.with_data(data);
            }
            error
        }
        None => Error::cancelled(format!(
            "request {} was interrupted when Luxforge stopped; nothing was published, and a \
             new request merges again",
            request.request_id
        ))
        .with_data(json!({"reason": "interrupted"})),
    };
    Ok(Some(LibraryLane::ended(
        &mut owner.jobs,
        &HDR_MERGE,
        call.origin.clone(),
        error,
    )))
}

/// The kind a stored error code names.
fn kind_of(code: &str) -> ErrorKind {
    [
        ErrorKind::UnsupportedInput,
        ErrorKind::UnsupportedColor,
        ErrorKind::UnsupportedProfile,
        ErrorKind::FileAccess,
        ErrorKind::Decode,
        ErrorKind::ResourceLimit,
        ErrorKind::Render,
        ErrorKind::Cancelled,
        ErrorKind::Validation,
        ErrorKind::Conflict,
        ErrorKind::Catalog,
        ErrorKind::Incompatible,
        ErrorKind::SourceUnavailable,
        ErrorKind::NotReady,
    ]
    .into_iter()
    .find(|kind| kind.code() == code)
    .unwrap_or(ErrorKind::Internal)
}

/// A merge job ended, however it ended: a published one's marker goes, since the journal answers
/// its retries; any other's records the error it ended with.
pub(super) fn ended(owner: &mut Owner, job_id: &JobId) {
    let Some(live) = owner.catalog.merge.live.remove(job_id) else {
        return;
    };
    let folder = owner
        .service
        .artifact_root()
        .join(MERGES)
        .join(&live.folder);
    let Some(job) = owner.jobs.read(job_id) else {
        return;
    };
    match job.error {
        None => {
            let _ = fs::remove_dir_all(&folder);
        }
        Some(error) => {
            if let Some(marker) = read_marker(&folder) {
                let request = Request {
                    method: &marker.method,
                    actor: &marker.actor,
                    request_id: &marker.request_id,
                };
                let _ = write_marker(&folder, request, Some(error));
            }
        }
    }
}
