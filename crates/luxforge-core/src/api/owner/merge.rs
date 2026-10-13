//! The merge lane and `merge.plan`, `merge.create`, `merge.inspect`, `merge.remove`,
//! `merge.restore` and `merge.rerun` on the owner (`docs/design/hdr-merge.md`, "Jobs, commands and
//! client state"; `docs/design/panorama-merge.md`, "Generalizing the merge boundary"). The
//! worker's job is `crate::merge::job`; the catalog side, publication and the stack's lifecycle,
//! is `crate::library::merge`.
//!
//! - **Kinds.** `merge.plan` and `merge.create` name the [`MergeKind`] they merge, and
//!   `merge.rerun` may; each kind's options travel under its own name (`hdr`, `panorama`). A kind
//!   this build registers no provider for (`crate::merge::providers`) is `not-ready` naming that
//!   provider, answered before the request is parsed ([`kinded`]) so a schema that enumerates only
//!   the registered kinds does not answer it as `validation`; another kind's options are
//!   `validation`. A panorama merges only while the lane holds a stitch
//!   (`crate::merge::panorama::stitching`); without one it is `not-ready` at admission.
//! - **The lane** is a library lane of its own ([`LaneKind::Merge`]) that every kind shares: one
//!   worker thread, started on the first merge, running one merge while at most [`MERGE_QUEUE`]
//!   waits; a further request is `resource-limit`, never dropped. Each kind holds its merges to its
//!   own limits ([`Limits::of`]). A merge job belongs to no client: no selection, view or
//!   workspace change and no disconnect cancels it, only `job.cancel`.
//! - **Planning and admission** read only the catalog, the index and one stat an input: the inputs
//!   in capture order (the request's breaking ties), which is an HDR stack's order, their
//!   header-level suitability and exposures, the folder, the estimates, an exact recorded match and
//!   the active stacks that claim an input. Everything that needs pixels is the job's; a
//!   panorama's stack order is the frames' left-to-right order, which registration decides.
//! - **Publication** is the job's answer, committed on the owner when the job ends: one
//!   transaction ([`EditorService::publish_merge`](crate::EditorService::publish_merge)) and one
//!   library change, announced as one event, unless `job.cancel` reached the owner first, when
//!   nothing is published and the durable objects the job wrote are unreferenced, for a later
//!   collection. A cancel after it finds the job `ready`.
//! - **An exact active match** is reused as it is: `merge.create` answers its result at once,
//!   `reused`, and records no library change.
//! - **Previews** (`merge.preview`, `merge.preview-update`, `merge.preview-close`, and
//!   `merge.create`'s `preview_id`) are each client's open session beside the lane
//!   ([`preview::Sessions`], `merge_preview.rs`): preparing occupies the lane, a ready preview's
//!   spool counts against the kind's ceiling, and a merge of it takes its bracket over.
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
        AssetRowId, CaptureTime, CatalogFolderId, EventSpan, FileId, FileSignature, FolderChoice,
        FolderProposal, HdrOptions, HdrParameters, HeaderMetadata, InspectedInput, LocalDay,
        MergeAnswer, MergeEstimates, MergeId, MergeInspection, MergeKind, MergeParameters,
        MergePlan, MergeProvenance, MergeProvider, MergeRecord, MergeState, PanoramaOptions,
        PanoramaParameters, PlannedInput, ProviderState, SourceAvailability, Targets, ViewItem,
        VolumeId,
        api::{MergeCreate, MergeInspect, MergePlanParams, MergeRerun, MergeTargets},
        jobs::CatalogJob,
        match_key,
    },
    editor::now_ms,
    jobs::{JobControl, JobError},
    library::{
        develop::{self, Destination},
        journal::Request,
        merge::{
            self, Publication, PublishedMerge, claims, matching, overlap, published_by, record,
            results,
        },
        remove,
    },
    merge::{
        hdr::{
            AlignmentOptions, ExposureOptions, ExposureResolution, FrameMetadata, Recording,
            Refusal, SensorLayout, frame::capture_order, resolve_exposures,
        },
        job::{self, Developer, Hooks, JobInput, JobSpec, Limits, Merged, SharedDevelopment},
        panorama::{
            frames::{FrameDeveloper, SharedFrames},
            job::{PanoramaSpec, focal_prior, spool_bytes},
            stitching::{self, Stitcher},
            suitability,
        },
        providers,
    },
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(any(test, feature = "test-holds"))]
#[path = "merge_frames.rs"]
mod frames;
#[cfg(test)]
#[path = "hdr_merge_end_to_end.rs"]
mod hdr_merge_end_to_end;
#[cfg(test)]
#[path = "hdr_merge_job.rs"]
mod hdr_merge_job;
#[cfg(test)]
#[path = "panorama_merge_tests.rs"]
mod panorama_merge;
#[cfg(any(test, feature = "test-holds"))]
pub use frames::SyntheticFrame;
#[path = "merge_preview.rs"]
mod preview;
pub(super) use preview::{
    PREVIEW_QUEUE, disconnect as disconnect_previews, expire as expire_previews,
};
pub(in crate::api) use preview::{merge_preview, merge_preview_close, merge_preview_update};

/// Merges that may wait behind the running one.
pub(super) const MERGE_QUEUE: usize = 1;
/// The folder under the artifact root that holds each request's marker and its job's spool.
const MERGES: &str = "merges";
const MARKER: &str = "request.json";
const MARKER_FORMAT: u32 = 2;
/// The request markers kept: the newest, by when each was made.
const MAX_MARKERS: usize = 64;

/// The merge lane's state on the owner, shared by every kind: its worker lane, the requests its
/// jobs run for, each kind's provider and limits, and the development it merges with. Tests
/// replace a provider and the development (no decodable RAW bracket is committed), lower the
/// limits and arrange failures.
pub(super) struct MergeLane {
    pub(super) lane: LibraryLane,
    /// The merge preview lane, which draws open previews again ([`preview`]).
    pub(super) updates: LibraryLane,
    /// Each client's open merge preview.
    pub(super) previews: preview::Sessions,
    /// Each queued or running job's request.
    live: HashMap<JobId, Live>,
    /// Each kind's provider in this build ([`providers::provider`]), or why there is none.
    providers: BTreeMap<MergeKind, Result<MergeProvider, String>>,
    developer: Arc<dyn Developer>,
    /// How a panorama's frames are read and developed: RAW or JPEG, a lens profile baked.
    frames: Arc<dyn FrameDeveloper>,
    /// The panorama stitch ([`stitching::production`]), or none in a build without one.
    stitcher: Option<Arc<dyn Stitcher>>,
    /// Each kind's limits ([`Limits::of`]).
    limits: BTreeMap<MergeKind, Limits>,
    hooks: Hooks,
    /// Make the next publication's transaction fail with this, after the merge and its result are
    /// written, as a commit refused at its last step would.
    #[cfg(test)]
    publish_fails: Option<Error>,
}

impl MergeLane {
    pub(super) fn new(poster: Poster, board: Arc<ActivityBoard>) -> Self {
        Self {
            lane: LibraryLane::of_kind(LaneKind::Merge, poster.clone(), board.clone()),
            updates: LibraryLane::of_kind(LaneKind::Preview, poster, board),
            previews: preview::Sessions::default(),
            live: HashMap::new(),
            providers: MergeKind::ALL
                .into_iter()
                .map(|kind| {
                    let provider = match kind {
                        MergeKind::Panorama if stitching::production().is_none() => {
                            Err(stitching::UNWIRED.to_owned())
                        }
                        _ => providers::provider(kind).ok_or_else(|| providers::missing(kind)),
                    };
                    (kind, provider)
                })
                .collect(),
            developer: Arc::new(SharedDevelopment),
            frames: Arc::new(SharedFrames),
            stitcher: stitching::production(),
            limits: MergeKind::ALL
                .into_iter()
                .map(|kind| (kind, Limits::of(kind)))
                .collect(),
            hooks: Hooks::default(),
            #[cfg(test)]
            publish_fails: None,
        }
    }

    fn provider(&self, kind: MergeKind) -> Result<MergeProvider, String> {
        self.providers
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| Err(providers::missing(kind)))
    }

    /// The limits a merge of `kind` is held to.
    fn limits(&self, kind: MergeKind) -> Limits {
        self.limits.get(&kind).copied().unwrap_or(Limits::of(kind))
    }

    /// Replace `kind`'s provider, as a build without it or with another version would carry.
    #[cfg(test)]
    pub(super) fn set_provider(
        &mut self,
        kind: MergeKind,
        provider: Result<MergeProvider, String>,
    ) {
        self.providers.insert(kind, provider);
    }

    /// Stitch panoramas with `stitcher` and read their frames with `frames`, as a build with its
    /// panorama stitch does.
    #[cfg(test)]
    pub(super) fn set_panorama(
        &mut self,
        stitcher: Arc<dyn Stitcher>,
        frames: Option<Arc<dyn FrameDeveloper>>,
    ) {
        self.stitcher = Some(stitcher);
        if let Some(frames) = frames {
            self.frames = frames;
        }
        self.providers.insert(
            MergeKind::Panorama,
            providers::provider(MergeKind::Panorama)
                .ok_or_else(|| providers::missing(MergeKind::Panorama)),
        );
    }

    /// Remove the panorama stitch, as a build without one would carry: panoramas plan, and are
    /// `not-ready` to merge or preview.
    #[cfg(test)]
    pub(super) fn remove_panorama(&mut self) {
        self.stitcher = None;
        self.providers
            .insert(MergeKind::Panorama, Err(stitching::UNWIRED.to_owned()));
    }

    /// The stitch a panorama merges with, or `not-ready` while this build has none.
    fn stitcher(&self) -> Result<Arc<dyn Stitcher>, Error> {
        self.stitcher
            .clone()
            .ok_or_else(|| providers::not_ready(MergeKind::Panorama, stitching::UNWIRED))
    }

    #[cfg(test)]
    pub(super) fn limits_mut(&mut self, kind: MergeKind) -> &mut Limits {
        self.limits.entry(kind).or_insert(Limits::of(kind))
    }

    fn provider_state(&self, kind: MergeKind) -> ProviderState {
        match self.provider(kind) {
            Ok(provider) => ProviderState {
                module: provider.module,
                version: provider.version,
                available: true,
                reason: None,
            },
            Err(reason) => ProviderState {
                module: kind.module().to_owned(),
                version: providers::provider(kind).map_or(0, |built| built.version),
                available: false,
                reason: Some(reason),
            },
        }
    }

    /// The provider a new merge of `kind` uses, or `not-ready` naming why there is none.
    fn available_provider(&self, kind: MergeKind) -> Result<MergeProvider, Error> {
        self.provider(kind)
            .map_err(|reason| providers::not_ready(kind, &reason))
    }
}

/// Refuse a merge request of a kind this build registers no provider for (`not-ready`, naming it),
/// or that carries another kind's options (`validation`, naming the field and the kind), before
/// its parameters are parsed: the schema enumerates only the registered kinds, so the parse would
/// answer an unregistered one as `validation`. A `kind` that is no kind at all, or none, is left to
/// the parse. HDR's options that do not parse are refused naming `hdr`.
pub(in crate::api) fn kinded(params: &Value) -> Result<(), Error> {
    let Some(kind) = params
        .get("kind")
        .and_then(Value::as_str)
        .and_then(MergeKind::parse)
    else {
        return Ok(());
    };
    for other in MergeKind::ALL.into_iter().filter(|other| *other != kind) {
        let field = other.as_str();
        if params.get(field).is_some_and(|value| !value.is_null()) {
            return Err(Error::validation(format!(
                "{field} holds {} options, and this is {} merge",
                other.noun(),
                article(kind)
            ))
            .with_data(json!({"kind": kind, "field": field})));
        }
    }
    if providers::provider(kind).is_none() {
        return Err(unregistered(kind));
    }
    if let Some(options) = params
        .get(kind.as_str())
        .filter(|options| !options.is_null())
    {
        let parsed = match kind {
            MergeKind::Hdr => HdrOptions::deserialize(options).map(|_| ()),
            MergeKind::Panorama => PanoramaOptions::deserialize(options).map(|_| ()),
        };
        parsed.map_err(|error| {
            Error::validation(format!("{}: {error}", kind.as_str()))
                .with_data(json!({"kind": kind, "field": kind.as_str()}))
        })?;
    }
    Ok(())
}

/// "an HDR", "a panorama".
fn article(kind: MergeKind) -> String {
    match kind {
        MergeKind::Hdr => "an HDR".to_owned(),
        MergeKind::Panorama => "a panorama".to_owned(),
    }
}

/// The refusal of a merge of `kind`, which this build registers no provider for.
fn unregistered(kind: MergeKind) -> Error {
    providers::not_ready(kind, &providers::missing(kind))
}

/// The job a merge of `kind` runs as.
fn job_of(kind: MergeKind) -> Result<&'static CatalogJob, Error> {
    Ok(kind.job())
}

/// A request's options, of its kind.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Options {
    Hdr(HdrOptions),
    Panorama(PanoramaOptions),
}

/// The options a request of `kind` names: its `hdr` or `panorama` options, defaulted when left
/// out; the other kind's are `validation`, naming the field. A kind with no provider is
/// `not-ready`.
pub(super) fn options(
    kind: MergeKind,
    hdr: Option<HdrOptions>,
    panorama: Option<PanoramaOptions>,
) -> Result<Options, Error> {
    if providers::provider(kind).is_none() {
        return Err(unregistered(kind));
    }
    let other = match kind {
        MergeKind::Hdr => panorama.is_some().then_some(MergeKind::Panorama),
        MergeKind::Panorama => hdr.is_some().then_some(MergeKind::Hdr),
    };
    if let Some(other) = other {
        let field = other.as_str();
        return Err(Error::validation(format!(
            "{field} holds {} options, and this is {} merge",
            other.noun(),
            article(kind)
        ))
        .with_data(json!({"kind": kind, "field": field})));
    }
    Ok(match kind {
        MergeKind::Hdr => Options::Hdr(hdr.unwrap_or_default()),
        MergeKind::Panorama => Options::Panorama(panorama.unwrap_or_default()),
    })
}

impl Options {
    /// The options as a preview revision answers them: every default named.
    pub(super) fn resolved(&self) -> crate::catalog_types::MergeOptions {
        match self {
            Self::Hdr(options) => crate::catalog_types::MergeOptions::Hdr(HdrOptions {
                reference: options.reference.clone(),
                exposure_overrides: options
                    .exposure_overrides
                    .clone()
                    .filter(|overrides| !overrides.is_empty()),
                alignment: Some(options.alignment.unwrap_or_default()),
                deghost: Some(options.deghost.unwrap_or_default()),
            }),
            Self::Panorama(options) => {
                crate::catalog_types::MergeOptions::Panorama(PanoramaOptions {
                    reference: options.reference.clone(),
                    projection: Some(options.projection.unwrap_or_default()),
                    boundary_warp: Some(options.boundary_warp.unwrap_or(0)),
                })
            }
        }
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

/// The refusal of `count` inputs for a merge of `kind`, outside its bounds: HDR's typed refusal,
/// or a validation naming the kind; either names the kind in its data.
fn input_count(kind: MergeKind, count: usize) -> Error {
    match kind {
        MergeKind::Hdr => Refusal::InputCount { count }.into(),
        MergeKind::Panorama => {
            let bounds = kind.inputs();
            Error::validation(format!(
                "a panorama merge takes {} to {} photos, not {count}",
                bounds.start(),
                bounds.end()
            ))
            .with_data(json!({"reason": "input-count", "kind": kind, "count": count}))
        }
    }
}

/// The inputs `targets` names for a merge of `kind`, in the stack's order: files by path, index
/// row or selection, and photographs by identity or selection, each resolved to its original's
/// path. Refused with `validation` for an unknown item or a merged photograph and with a typed
/// refusal for a count outside the kind's bounds.
fn inputs(
    owner: &Owner,
    client: ClientId,
    kind: MergeKind,
    targets: &Targets,
) -> Result<Vec<Named>, Error> {
    if let Some(count) = targets.len()
        && !kind.inputs().contains(&count)
    {
        return Err(input_count(kind, count));
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
                    "a selected original belongs to a merge's stack already; remove that merge first",
                )),
            })
            .collect::<Result<_, _>>()?,
    };
    if !kind.inputs().contains(&items.len()) {
        return Err(input_count(kind, items.len()));
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

/// The HDR options a request names, in the stack's positions: the reference and the overrides by
/// path, each refused with `validation` when it names no input.
fn parameters(
    inputs: &[Named],
    options: &HdrOptions,
    alignment: AlignmentOptions,
) -> Result<HdrParameters, Error> {
    let reference = options.reference.as_deref();
    let overrides = options.exposure_overrides.as_deref();
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

/// The position among `inputs` of the input at `path`, refused with `validation` when it names
/// none.
fn position(inputs: &[Named], path: &Path) -> Result<usize, Error> {
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
}

/// The panorama options a request names, in the inputs' positions, each left out the recorded
/// one's (a rerun's) or the default: the reference by path, the projection and the Boundary
/// Warp, refused with `validation` past 100.
fn panorama_parameters(
    inputs: &[Named],
    options: &PanoramaOptions,
    recorded: Option<&PanoramaParameters>,
) -> Result<PanoramaParameters, Error> {
    let boundary_warp = options
        .boundary_warp
        .or(recorded.map(|recorded| recorded.boundary_warp))
        .unwrap_or(0);
    if boundary_warp > crate::catalog_types::MAX_BOUNDARY_WARP {
        return Err(Error::validation(format!(
            "boundary_warp is {boundary_warp}; it takes 0 to 100"
        ))
        .with_data(json!({"kind": MergeKind::Panorama, "field": "boundary_warp"})));
    }
    Ok(PanoramaParameters {
        projection: options
            .projection
            .or(recorded.map(|recorded| recorded.projection))
            .unwrap_or_default(),
        boundary_warp,
        reference: match &options.reference {
            Some(path) => Some(position(inputs, path)?),
            None => recorded.and_then(|recorded| recorded.reference),
        },
    })
}

/// The kind-tagged parameters `options` name for `inputs`.
fn requested(inputs: &[Named], options: &Options) -> Result<MergeParameters, Error> {
    Ok(match options {
        Options::Hdr(options) => {
            MergeParameters::Hdr(parameters(inputs, options, alignment(options))?)
        }
        Options::Panorama(options) => {
            MergeParameters::Panorama(panorama_parameters(inputs, options, None)?)
        }
    })
}

/// The header-level refusal a request answers at once, from its planned frames: an HDR bracket's
/// exposures that do not resolve, or frames that cannot be a panorama, named by file.
fn header_check(
    inputs: &[Named],
    frames: &[FrameMetadata],
    parameters: &MergeParameters,
) -> Result<(), Error> {
    match parameters {
        MergeParameters::Hdr(parameters) => {
            resolve_exposures(frames, &parameters.exposure)?;
            Ok(())
        }
        MergeParameters::Panorama(_) => suitability::check(frames).map_err(|refusal| {
            refusal.error(
                &inputs
                    .iter()
                    .map(|input| file_name(&input.path))
                    .collect::<Vec<_>>(),
            )
        }),
    }
}

/// The reference the request's metadata picks before any pixel is read: HDR's from the headers'
/// exposures; a panorama's the named one (registration picks it otherwise).
fn planned_reference(frames: &[FrameMetadata], parameters: &MergeParameters) -> Option<usize> {
    match parameters {
        MergeParameters::Hdr(parameters) => header_reference(frames, parameters),
        MergeParameters::Panorama(parameters) => parameters.reference,
    }
}

/// A plan's identity: the SHA-256 of its inputs in the stack's order (path, signature and
/// photograph) and its kind-tagged options.
fn plan_identity(inputs: &[Named], parameters: &MergeParameters) -> Result<String, Error> {
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
    parameters: &MergeParameters,
) -> Result<Option<String>, Error> {
    let mut signed = Vec::with_capacity(inputs.len());
    for input in inputs {
        let Some(signature) = &input.signature else {
            return Ok(None);
        };
        signed.push((&input.path, signature));
    }
    match_key(parameters.kind(), provider, signed, parameters).map(Some)
}

/// The folder a merge proposes: the folder choice a Develop would file its reference frame by.
struct Proposed {
    /// The proposal of the reference's event ([`develop::plan`]).
    choice: FolderChoice,
    /// That event's span and identity, which a place folder the merge makes records.
    span: Option<EventSpan>,
    /// The reference's camera-local capture day, by its header.
    day: Option<LocalDay>,
}

/// What a merge of `inputs` proposes for the reference frame `reference` (the middle input when
/// none): the folder choice a Develop of the inputs would make for its event, for its capture day.
fn proposal(owner: &Owner, inputs: &[Named], reference: Option<usize>) -> Result<Proposed, Error> {
    let catalog = &owner.service.connection;
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
    let at = reference
        .filter(|&at| at < inputs.len())
        .unwrap_or((inputs.len().max(1) - 1) / 2);
    let reference = &inputs[at];
    let event = plan
        .files
        .iter()
        .find(|file| file.path == reference.path)
        .map_or(0, |file| file.event);
    let event = plan
        .events
        .get(event)
        .ok_or_else(|| Error::internal("a merge's inputs were planned in no event"))?;
    Ok(Proposed {
        choice: event.proposal.clone(),
        span: develop::made_from(std::slice::from_ref(&event.span)),
        day: reference
            .header
            .as_ref()
            .and_then(|header| header.capture.as_ref())
            .map(CaptureTime::local_day),
    })
}

/// What `merge.plan` answers of the folder: the folder `proposed` files the result in when it is
/// in the catalog already — `existing` when the choice names a folder, `place` when the year, place
/// and day folders exist — or else the choice itself, whose folders `merge.create` makes.
fn proposed_folder(
    catalog: &Connection,
    proposed: Proposed,
) -> Result<(Option<FolderProposal>, Option<FolderChoice>), Error> {
    let from = match proposed.choice {
        FolderChoice::Existing { .. } => "existing",
        FolderChoice::Place { .. } => "place",
        FolderChoice::New { .. } => "new",
    };
    let destination = Destination::chosen(catalog, &proposed.choice, None, now_ms())?;
    Ok(match destination.found(catalog, proposed.day)? {
        Some(folder_id) => (
            Some(FolderProposal {
                name: crate::library::folders::folder(catalog, &folder_id)?.name,
                folder_id,
                from: from.to_owned(),
            }),
            None,
        ),
        None => (None, Some(proposed.choice)),
    })
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

/// What a merge of `kind` would take, from the headers' size: an HDR result is its reference's
/// size; a panorama's is estimated ([`panorama_size`]) and provisional.
fn estimates(
    lane: &MergeLane,
    kind: MergeKind,
    inputs: &[Named],
    frames: &[FrameMetadata],
    root: &Path,
) -> MergeEstimates {
    let limits = lane.limits(kind);
    let known = frames.first().filter(|frame| frame.width > 0);
    let available = limits.available.or_else(|| {
        root.ancestors()
            .find(|path| path.exists())
            .and_then(|path| luxforge_process::disk::available_bytes(path).ok())
    });
    let (size, temp_bytes, scale) = match (kind, known) {
        (MergeKind::Hdr, Some(frame)) => (
            Some((frame.width, frame.height)),
            Some(job::temp_bytes(inputs.len(), frame.width, frame.height)),
            None,
        ),
        (MergeKind::Panorama, Some(frame)) => {
            let header = inputs.first().and_then(|input| input.header.as_ref());
            let (width, height, scale) = panorama_size(frame, header, inputs.len());
            (
                Some((width, height)),
                Some(
                    spool_bytes(inputs.len(), frame.width, frame.height)
                        + crate::merge::panorama::job::result_bytes(width, height),
                ),
                (scale < 1.0).then_some(scale),
            )
        }
        (_, None) => (None, None, None),
    };
    MergeEstimates {
        width: size.map(|size| size.0),
        height: size.map(|size| size.1),
        temp_bytes,
        source_bytes: size.map(|(width, height)| u64::from(width) * u64::from(height) * 12),
        available_bytes: available,
        temp_ceiling_bytes: limits.temp_ceiling,
        scratch_limit_bytes: limits.scratch,
        scale,
        provisional: kind == MergeKind::Panorama,
    }
}

/// A panorama's size from its frames' headers alone, before registration decides it: `count`
/// frames of `frame`'s size in one row overlapping by a third, laid on a cylinder of the focal
/// length their focal length and sensor size give (the 35 mm equivalent, or the focal length
/// times the camera's crop factor from the lens index, when it is loaded), or edge to edge in
/// pixels without one; at most 360° around, and scaled to the large-source limits. Answers the
/// width, height and scale.
fn panorama_size(
    frame: &FrameMetadata,
    header: Option<&HeaderMetadata>,
    count: usize,
) -> (u32, u32, f64) {
    use crate::merge::panorama::stitching::OutputLimits;
    let (w, h) = (f64::from(frame.width), f64::from(frame.height));
    let index = crate::modules::lens::index::shared().ok();
    let focal = header
        .and_then(|header| focal_prior(header, index.as_deref()))
        .and_then(|prior| prior.focal_px(frame.width, frame.height));
    let advance = 2.0 / 3.0;
    let width = match focal {
        Some(focal) => {
            let frame_angle = 2.0 * (w / 2.0 / focal).atan();
            let angle = (frame_angle * (1.0 + advance * (count as f64 - 1.0)))
                .min(2.0 * std::f64::consts::PI);
            focal * angle
        }
        None => w * (1.0 + advance * (count as f64 - 1.0)),
    };
    let limits = OutputLimits::DERIVED;
    let pixels = width * h;
    let scale = (f64::from(limits.max_side) / width.max(h))
        .min((limits.max_pixels as f64 / pixels).sqrt())
        .min(1.0);
    (
        (width * scale).round().max(1.0) as u32,
        (h * scale).round().max(1.0) as u32,
        scale,
    )
}

fn alignment(options: &HdrOptions) -> AlignmentOptions {
    AlignmentOptions {
        alignment: options.alignment.unwrap_or_default(),
        deghost: options.deghost.unwrap_or_default(),
    }
}

// The methods.

/// `merge.plan`: see [`MergePlan`]. Reads the catalog, the index and one stat an input; refused
/// only for a kind with no provider and targets that name no set of known items the kind takes
/// (HDR: a bracket of 2–9; panorama: 2–32 frames).
pub(in crate::api) fn merge_plan(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergePlanParams,
) -> Result<Value, Error> {
    let kind = params.kind;
    let options = options(kind, params.hdr, params.panorama)?;
    let inputs = inputs(owner, call.client, kind, &params.targets)?;
    let tagged = requested(&inputs, &options)?;
    let frames = planned_frames(&inputs);
    let typed = |error: Error| {
        let mut refusal = error.data.as_deref().cloned().unwrap_or(Value::Null);
        if let Some(object) = refusal.as_object_mut() {
            object.insert("message".into(), Value::String(error.detail.clone()));
        }
        refusal
    };
    let (suitable, refusal, exposures, needs_fit) = match &tagged {
        MergeParameters::Hdr(parameters) => {
            match resolve_exposures(&frames, &parameters.exposure) {
                Ok(ExposureResolution::Resolved(exposures)) => (true, None, Some(exposures), false),
                Ok(ExposureResolution::NeedsFit(_)) => (true, None, None, true),
                Err(refusal) => (false, Some(typed(refusal.into())), None, false),
            }
        }
        MergeParameters::Panorama(_) => match header_check(&inputs, &frames, &tagged) {
            Ok(()) => (true, None, None, false),
            Err(error) => (false, Some(typed(error)), None, false),
        },
    };
    let reference = planned_reference(&frames, &tagged);
    let (folder, suggested_folder) = proposed_folder(
        &owner.service.connection,
        proposal(owner, &inputs, reference)?,
    )?;
    let lane = &owner.catalog.merge;
    let provider = lane.provider_state(kind);
    let existing = match lane.provider(kind) {
        Ok(provider) => match request_key(&inputs, &provider, &tagged)? {
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
        kind,
        plan_id: plan_identity(&inputs, &tagged)?,
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
        folder,
        suggested_folder,
        estimates: estimates(lane, kind, &inputs, &frames, &root),
        provider,
        existing,
        claimed,
        parameters: tagged,
    };
    value(answer)
}

/// `merge.create`: admit the request on the owner (see the module documentation) and start the
/// kind's job (`hdr-merge`), answering `{job_id, status, deduplicated}`; its result is a
/// [`MergeAnswer`].
pub(in crate::api) fn merge_create(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergeCreate,
) -> Result<Value, Error> {
    let kind = params.kind;
    let options = options(kind, params.hdr.clone(), params.panorama.clone())?;
    let job = job_of(kind)?;
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answered) = retried(owner, call, request)? {
        return value(answered);
    }
    let provider = owner.catalog.merge.available_provider(kind)?;
    let inputs = inputs(owner, call.client, kind, &params.targets)?;
    let tagged = requested(&inputs, &options)?;
    if let Some(plan_id) = &params.plan_id
        && *plan_id != plan_identity(&inputs, &tagged)?
    {
        return Err(Error::conflict(
            "the inputs or options are no longer what merge.plan planned; plan them again",
        )
        .with_data(json!({"plan_id": plan_id})));
    }
    let frames = planned_frames(&inputs);
    header_check(&inputs, &frames, &tagged)?;
    let signed = signed(&inputs)?;
    let key = request_key(&inputs, &provider, &tagged)?
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
            let answer = recorded_answer(&owner.service.connection, &existing.merge_id, true)?;
            return value(LibraryLane::answered(
                &mut owner.jobs,
                job,
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
    let reference = planned_reference(&frames, &tagged);
    let proposed = proposal(owner, &inputs, reference)?;
    let choice = params.folder.unwrap_or(proposed.choice);
    let folder = Destination::chosen(&owner.service.connection, &choice, proposed.span, now_ms())?;
    // A merge of a preview takes its bracket over: the preview's inputs, as it read them.
    let continuation = match &params.preview_id {
        Some(preview_id) => Some(preview::continuation(
            owner,
            call.client,
            preview_id,
            kind,
            &inputs,
            &tagged,
        )?),
        None => None,
    };
    let mut job_inputs: Vec<JobInput> = inputs
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
    if let Some(continuation) = &continuation
        && let Some(previewed) = preview::preview_inputs(owner, &continuation.preview_id)
    {
        for (input, previewed) in job_inputs.iter_mut().zip(previewed) {
            input.signature = previewed.signature;
            input.planned = previewed.planned;
        }
    }
    start(
        owner,
        call,
        request,
        Started {
            kind,
            inputs: job_inputs,
            parameters: tagged,
            provider,
            folder,
            replaces: None,
            reuse: None,
            key: Some(key),
            mutation: params.mutation.clone(),
            preview: continuation,
        },
    )
}

/// `merge.rerun`: merge a recorded merge's exact inputs again, with its recorded options save
/// those named, into a new result; see the module documentation for what it refuses and reuses.
/// A `kind` it names must be the merge's.
pub(in crate::api) fn merge_rerun(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergeRerun,
) -> Result<Value, Error> {
    if let Some(kind) = params.kind {
        options(kind, None, None)?;
    }
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answered) = retried(owner, call, request)? {
        return value(answered);
    }
    let recorded = record(&owner.service.connection, &params.merge_id)?;
    if let Some(kind) = params.kind
        && kind != recorded.kind
    {
        return Err(Error::validation(format!(
            "merge {} is {} merge, not {} one",
            recorded.merge_id,
            article(recorded.kind),
            article(kind)
        ))
        .with_data(json!({"kind": recorded.kind, "requested": kind})));
    }
    let options = options(recorded.kind, params.hdr, params.panorama)?;
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
            header: None,
            kind: Some(SourceTag::Raw),
        })
        .collect();
    let parameters = match (&provenance.parameters, &options) {
        (MergeParameters::Hdr(old), Options::Hdr(options)) => {
            let mut parameters = parameters(
                &named,
                options,
                AlignmentOptions {
                    alignment: options.alignment.unwrap_or(old.alignment.alignment),
                    deghost: options.deghost.unwrap_or(old.alignment.deghost),
                },
            )?;
            if options.reference.is_none() {
                parameters.exposure.reference = old.exposure.reference;
            }
            if options.exposure_overrides.is_none() {
                parameters.exposure.overrides = old.exposure.overrides.clone();
            }
            parameters.fusion = old.fusion;
            MergeParameters::Hdr(parameters)
        }
        (MergeParameters::Panorama(old), Options::Panorama(options)) => {
            MergeParameters::Panorama(panorama_parameters(&named, options, Some(old))?)
        }
        _ => {
            return Err(Error::incompatible(format!(
                "merge {} records options of another kind than its own",
                recorded.merge_id
            )));
        }
    };
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
    let folder_id = match params.folder_id {
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
    let folder = Destination::Existing {
        folder_id,
        days: false,
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
            kind: recorded.kind,
            inputs,
            parameters,
            provider,
            folder,
            replaces: Some(recorded.merge_id),
            reuse,
            key: None,
            mutation: params.mutation.clone(),
            preview: None,
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
    value(MergeInspection {
        merge_id,
        kind: recorded.kind,
        state: recorded.state,
        result_asset_id: recorded.result_asset_id,
        source_id: recorded.source_id,
        created_ms: recorded.created_ms,
        source,
        merged: true,
        exposure_range_ev: recorded.provenance.resolved.exposure_range_ev(),
        projection: recorded
            .provenance
            .resolved
            .panorama_view()
            .map(|(projection, _)| projection),
        field_of_view_deg: recorded
            .provenance
            .resolved
            .panorama_view()
            .map(|(_, fov)| fov),
        optics: recorded.provenance.optics_ledger(),
        parameters: recorded.provenance.parameters.clone(),
        provider: owner.catalog.merge.provider_state(recorded.kind),
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
    kind: MergeKind,
    inputs: Vec<JobInput>,
    parameters: MergeParameters,
    provider: MergeProvider,
    /// The folder the result is filed by.
    folder: Destination,
    replaces: Option<MergeId>,
    /// A rerun's exact match: the recorded provenance and source the new result is given.
    reuse: Option<(MergeProvenance, PublishedSource)>,
    /// A new merge's exact-match key, which a later request for the same merge finds it by while
    /// it runs. None for a rerun, which always makes a new result.
    key: Option<String>,
    mutation: MutationRequest,
    /// The preview whose bracket the merge takes over.
    preview: Option<preview::Continuation>,
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

/// The provider a rerun of `provenance` merges with: this build's for its kind, which must be the
/// one and the version that made it.
fn rerun_provider(lane: &MergeLane, provenance: &MergeProvenance) -> Result<MergeProvider, Error> {
    let provider = lane.available_provider(provenance.kind)?;
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

/// Queue the admitted merge: refuse it while the lane, which every kind shares, is full, preflight
/// what the headers say it will spool against its kind's limits, record the request's marker and
/// start the kind's job.
fn start(
    owner: &mut Owner,
    call: &Call<'_>,
    request: Request<'_>,
    started: Started,
) -> Result<Value, Error> {
    let kind = started.kind;
    let job_kind = job_of(kind)?;
    let lane = &owner.catalog.merge;
    let limits = lane.limits(kind);
    // A merge continuing a preview still preparing waits behind it, outside the bound: it carries
    // on the preview's work rather than adding its own.
    let behind = started
        .preview
        .as_ref()
        .and_then(|continuation| continuation.after.clone());
    if behind.is_none() && lane.lane.is_full() {
        return Err(Error::resource_limit(format!(
            "a merge is running and {MERGE_QUEUE} waits; wait for one to end"
        )));
    }
    let root = merges_root(&owner.service)?;
    let previews_root = preview::previews_root(owner)?;
    let pictures_root = preview::pictures_root(owner);
    let kept = lane.previews.kept();
    let held = lane.previews.held.clone();
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
    // What the headers say the merge spools: an HDR bracket's spool and result; a panorama's
    // frames' spool, since registration decides its result's size.
    let planned = match kind {
        MergeKind::Hdr => job::temp_bytes(n, width, height),
        MergeKind::Panorama => spool_bytes(n, width, height),
    };
    if started.reuse.is_none()
        && started.preview.is_none()
        && width > 0
        && let Err(error) = limits.preflight(
            &spool,
            planned,
            held.load(std::sync::atomic::Ordering::SeqCst),
        )
    {
        let _ = fs::remove_dir_all(&spool);
        return Err(error);
    }
    write_marker(&spool, kind, request, None)?;
    prune_markers(&root, &lane.live);
    let job_id = JobId::new();
    let reference = planned_reference(
        &started
            .inputs
            .iter()
            .map(|input| input.planned.clone())
            .collect::<Vec<_>>(),
        &started.parameters,
    )
    .unwrap_or((n - 1) / 2);
    let detail = match kind {
        MergeKind::Hdr => format!(
            "{} \u{b7} {n} exposures",
            job::result_name(&started.inputs[reference].path)
        ),
        MergeKind::Panorama => format!(
            "{} \u{b7} {n} photos",
            job::named_result(&started.inputs[reference].path, kind)
        ),
    };
    let spec = Spec::new(
        lane,
        started.inputs,
        started.parameters,
        started.provider,
        spool.clone(),
        owner.service.derived_source_writer(),
        limits,
        held,
        owner.service.auto_lens_profile(),
    )?;
    let publish = Publish {
        folder: started.folder,
        replaces: started.replaces,
        method: call.request.method.clone(),
        mutation: started.mutation,
        origin: call.origin.clone(),
    };
    let reuse = started.reuse;
    let matched = started.key;
    let continuation = started.preview;
    let slot = continuation
        .as_ref()
        .map(|continuation| continuation.slot.clone());
    let task = Task {
        job_id: job_id.clone(),
        job: job_kind,
        asset_id: None,
        detail: Some(detail),
        origin: call.origin.clone(),
        work: Box::new(move |context: &JobContext<'_>| {
            job::sweep_orphans(&root, &spool);
            preview::sweep(&previews_root, &pictures_root, &kept);
            let merged = match (reuse, slot) {
                (Some((provenance, source)), _) => {
                    verified(spec, provenance, source, context.control)
                }
                (None, Some(slot)) => preview::take(&slot)
                    .and_then(|retained| spec.run_prepared(retained, context.control)),
                (None, None) => spec.run(context.control),
            };
            match merged {
                Ok(merged) => Box::new(move |owner: &mut Owner| publish.commit(owner, merged)),
                Err(error) => Box::new(move |_| Err(error)),
            }
        }),
    };
    // The preview is the merge's from now: its spool no longer counts as a preview's, so the
    // merge's own preflight counts it once, and an update it had is stale.
    if let Some(continuation) = &continuation {
        preview::merging(owner, &continuation.preview_id, &job_id);
    }
    let started = match behind {
        Some(after) => Ok(owner
            .catalog
            .merge
            .lane
            .queue_after(&mut owner.jobs, task, &after)),
        None => owner.catalog.merge.lane.queue(&mut owner.jobs, task),
    };
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

/// A merge's job of its kind, moved to the worker.
enum Spec {
    Hdr(JobSpec),
    Panorama(PanoramaSpec),
}

impl Spec {
    /// The job of `parameters`' kind, with the lane's development, stitch and hooks.
    #[allow(clippy::too_many_arguments)]
    fn new(
        lane: &MergeLane,
        inputs: Vec<JobInput>,
        parameters: MergeParameters,
        provider: MergeProvider,
        spool: PathBuf,
        writer: crate::artifacts::derived_source::DerivedSourceWriter,
        limits: Limits,
        held: Arc<std::sync::atomic::AtomicU64>,
        detect_lens: bool,
    ) -> Result<Self, Error> {
        Ok(match parameters {
            MergeParameters::Hdr(parameters) => Self::Hdr(JobSpec {
                inputs,
                parameters,
                provider,
                spool,
                writer,
                developer: lane.developer.clone(),
                limits,
                hooks: lane.hooks.clone(),
                held,
            }),
            MergeParameters::Panorama(parameters) => Self::Panorama(PanoramaSpec {
                inputs,
                parameters,
                provider,
                spool,
                writer,
                frames: lane.frames.clone(),
                stitcher: lane.stitcher()?,
                limits,
                hooks: lane.hooks.clone(),
                held,
                detect_lens,
            }),
        })
    }

    fn run(self, control: &JobControl) -> Result<Merged, Error> {
        match self {
            Self::Hdr(spec) => spec.run(control),
            Self::Panorama(spec) => spec.run(control),
        }
    }

    /// Merge what a preview of the same kind read.
    fn run_prepared(
        self,
        retained: preview::Retained,
        control: &JobControl,
    ) -> Result<Merged, Error> {
        match (self, retained) {
            (Self::Hdr(spec), preview::Retained::Hdr { prepared, analysis }) => {
                spec.run_prepared(prepared, analysis, control)
            }
            (Self::Panorama(spec), preview::Retained::Panorama { prepared, analysis }) => {
                spec.run_prepared(prepared, analysis, control)
            }
            _ => Err(Error::internal(
                "a merge preview of another kind than its merge",
            )),
        }
    }

    /// The inputs and the kind-tagged parameters.
    fn into_parts(self) -> (Vec<JobInput>, MergeParameters) {
        match self {
            Self::Hdr(spec) => (spec.inputs, MergeParameters::Hdr(spec.parameters)),
            Self::Panorama(spec) => (spec.inputs, MergeParameters::Panorama(spec.parameters)),
        }
    }
}

/// A rerun's exact match: every input read once and its SHA-256 checked against the merge's record,
/// with no development and no merging; the new result is given the recorded source.
fn verified(
    spec: Spec,
    provenance: MergeProvenance,
    source: PublishedSource,
    control: &JobControl,
) -> Result<Merged, Error> {
    control.set_phase("reading");
    let (inputs, parameters) = spec.into_parts();
    let n = inputs.len();
    let mut volumes: Vec<crate::catalog_types::Volume> = Vec::new();
    for (at, input) in inputs.iter().enumerate() {
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
    let reference = provenance.reference_input();
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
        file_name: job::named_result(&reference.path, provenance.kind),
        header: job::result_header(&header, source.width, source.height),
        place: None,
        volumes,
        provenance: MergeProvenance {
            parameters,
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
    folder: Destination,
    replaces: Option<MergeId>,
    method: String,
    mutation: MutationRequest,
    origin: Origin,
}

impl Publish {
    /// Publish `merged` in one transaction and one library change, announced as one event, and
    /// answer the [`MergeAnswer`].
    fn commit(self, owner: &mut Owner, merged: Merged) -> Result<Value, Error> {
        let request = Request::new(&self.method, &self.mutation);
        #[cfg(test)]
        let fails = owner.catalog.merge.publish_fails.take();
        let published = owner.service.publish_merge(
            request,
            &Publication {
                provenance: &merged.provenance,
                source: &merged.source,
                folder: &self.folder,
                file_name: &merged.file_name,
                header: &merged.header,
                place: merged.place.as_deref(),
                volumes: &merged.volumes,
                replaces: self.replaces.as_ref(),
            },
            |_, _, _| {
                #[cfg(test)]
                if let Some(error) = fails {
                    return Err(error);
                }
                Ok(Vec::new())
            },
        )?;
        announce(owner, &self.origin, &published);
        let mut answer = recorded_answer(&owner.service.connection, &published.merge_id, false)?;
        answer.change = published.outcome.answer().change;
        answer.deduplicated = published.outcome.announced().is_none();
        answer.replaced = self.replaces.filter(|_| answer.change.is_some());
        answer.accounting = serde_json::to_value(merged.accounting).ok();
        encode(&answer)
    }
}

/// Announce a publication's library change as [`change`] announces one: one event naming its
/// sequence and the result, and the lanes that follow its items told; the change that made its
/// folders, when it made any, first, as one event of its own.
fn announce(owner: &mut Owner, origin: &Origin, published: &PublishedMerge) {
    if let Some(folders) = &published.folders
        && let Some(sequence) = folders.announced()
    {
        announce_once(&mut owner.announced, &origin.clone().library(sequence));
        owner.library_changed(origin, folders.items());
    }
    if let Some(sequence) = published.outcome.announced() {
        let announced = origin
            .clone()
            .library(sequence)
            .changed(published.result.clone(), None);
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
        kind,
        result_asset_id,
        source_id,
        provenance,
        ..
    } = record(connection, merge_id)?;
    let photograph = photograph_by(connection, "id", result_asset_id.as_str())?
        .ok_or_else(|| Error::conflict(format!("merge {merge_id}'s result is gone")))?;
    Ok(MergeAnswer {
        merge_id: merge_id.clone(),
        kind,
        file_name: file_name_of(connection, &result_asset_id),
        result_asset_id,
        source_id,
        catalog_folder_id: photograph.folder,
        merged: true,
        inputs: provenance.inputs.len() as u32,
        exposure_range_ev: provenance.resolved.exposure_range_ev(),
        projection: provenance
            .resolved
            .panorama_view()
            .map(|(projection, _)| projection),
        field_of_view_deg: provenance.resolved.panorama_view().map(|(_, fov)| fov),
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

/// A request's marker: who asked for what kind of merge and when, and, once its job ended without
/// a result, the error it ended with.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    format: u32,
    kind: MergeKind,
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
fn write_marker(
    folder: &Path,
    kind: MergeKind,
    request: Request<'_>,
    ended: Option<JobError>,
) -> Result<(), Error> {
    let marker = Marker {
        format: MARKER_FORMAT,
        kind,
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
    if let Some(published) = published_by(&owner.service.connection, request)? {
        let mut answer = recorded_answer(&owner.service.connection, &published.merge_id, false)?;
        answer.change = published.outcome.answer().change;
        answer.deduplicated = true;
        return Ok(Some(LibraryLane::answered(
            &mut owner.jobs,
            job_of(answer.kind)?,
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
        job_of(marker.kind)?,
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

/// A merge or merge preview job ended, however it ended: the preview it prepared, updated or was
/// merged from hears of it (`merge_preview.rs`); a published merge's marker goes, since the journal
/// answers its retries; any other's records the error it ended with.
pub(super) fn ended(owner: &mut Owner, job_id: &JobId) {
    preview::ended(owner, job_id);
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
                let _ = write_marker(&folder, marker.kind, request, Some(error));
            }
        }
    }
}
