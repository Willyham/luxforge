//! The export methods and the export job (`docs/design/export.md`). `export.plan` and
//! `export.jpeg` plan on the catalog owner in `O(layers)`, with short file-system checks on the
//! destination and the original's directory; the job renders, encodes and publishes on the `export`
//! lane of the owner's one job table: one running and four waiting, never superseded, read with
//! `job.read` and cancelled only by `job.cancel` or the owner stopping. A client disconnecting
//! leaves its exports running, as it leaves capability jobs.
//!
//! An export streams through the owner's tile service (`docs/design/gpu-first.md`, stage 4): the
//! host's GPU provider renders the output stage in full-resolution tiles on its own thread, and the
//! encoder takes them as bands, in order, as they arrive. The reference renderer renders the whole
//! frame instead when the export asks for it (`reference: true`), when the host has no GPU provider
//! (`luxforge-json`), and when the provider says why it cannot, at once or part-way, in which case
//! the export starts again from nothing: GPU and reference pixels are never mixed in one file. A
//! written file's result names the renderer that rendered it, and why the reference did.
use super::{Call, Owner};
use crate::{
    AssetId, EntryId, Error, JobId, JobStatus, Renderer, RendererReason, RendererRecord,
    activity::ActivitySpec,
    api::announce_once,
    api::params::host_params,
    editor::ExportPlan,
    export::{
        CaptureMetadata,
        encode::{encode_jpeg, encode_jpeg_rows},
        publish::{self, Destination, Staged},
    },
    jobs::{JobControl, JobKind, NewJob, Work},
    tiles::{BandStream, TileService, TileStatus},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// The phases an export reports on the activity board, in order. An export the reference renders
/// again after its stream stopped begins `rendering` again.
const RENDERING: &str = "rendering";
const ENCODING: &str = "encoding";
const WRITING: &str = "writing";

/// The reason `job.cancel` gives an export.
pub(super) const CANCELLED: &str = "the export was cancelled";

/// Called on the export lane as each phase begins, after `encoding` has staged its temporary file,
/// so a test can hold a running export at a known point.
#[cfg(test)]
pub(super) type Hold = Arc<dyn Fn(&'static str) + Send + Sync>;

host_params! {
    /// `export.plan`.
    pub(in crate::api) struct ExportPlanParams {
        asset_id: AssetId = asset(),
        entry_id: Option<EntryId> = entry().notes("a saved entry of the asset; default its current entry"),
    }
}

host_params! {
    /// `export.jpeg`.
    pub(in crate::api) struct ExportJpeg {
        asset_id: AssetId = asset(),
        destination: PathBuf = path().notes("an absolute path ending .jpg or .jpeg whose parent directory exists and at which nothing exists"),
        mutation: MutationRequest,
        entry_id: Option<EntryId> = entry().notes("a saved entry of the asset; default its current entry"),
        keep_metadata: Option<bool> = boolean().default(false).notes("write the original's supported EXIF fields"),
        pixels_per_inch: Option<u16> = integer(1, 65535).notes("the JFIF header's density, which sizes the file in viewers such as Preview and in print; default none, a unitless 1:1 aspect ratio"),
        reference: Option<bool> = boolean().default(false).notes("render through the reference renderer, the CPU render every GPU output is measured against, rather than the GPU; the result's renderer then names the reference with the reason requested"),
    }
}

/// `export.plan`: the output stage from the compiled recipe and a suggested destination in the
/// remembered export folder while it exists, or beside the original, which costs reading the small
/// preferences file and at most 64 names in that folder. Nothing is rendered or prepared. A
/// preferences file that cannot be read suggests beside the original; the desktop reports that
/// failure when it reads the preferences itself.
pub(in crate::api) fn plan(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ExportPlanParams,
) -> Result<Value, Error> {
    let target = owner
        .service
        .export_target(&params.asset_id, params.entry_id.as_ref())?;
    let remembered = owner
        .host
        .preferences
        .read()
        .ok()
        .and_then(|preferences| preferences.export_folder)
        .filter(|folder| folder.is_dir());
    let suggested = match (
        remembered.as_deref().or(target.original.parent()),
        target.original.file_stem().and_then(|stem| stem.to_str()),
    ) {
        (Some(directory), Some(stem)) => publish::suggest(directory, stem),
        _ => None,
    };
    let identity = target.evaluation.identity()?;
    Ok(json!({
        "asset_id": identity.asset_id,
        "entry_id": identity.entry_id,
        "snapshot_id": identity.snapshot_id,
        "width": identity.width,
        "height": identity.height,
        // A name that is not UTF-8 is not suggested rather than suggested wrongly.
        "suggested": suggested.as_deref().and_then(|path| path.to_str()),
    }))
}

/// `export.jpeg`: check the destination, freeze the entry and queue one job on the export lane.
/// An obvious refusal — a destination's shape or an existing file, an unknown asset or entry, a
/// stack the host cannot evaluate, a missing original — is answered now and queues nothing; an
/// unprepared source is `preparation-required` with the source job the owner queued for it. The
/// answer echoes `keep_metadata` and `reference` as the job was accepted with them, and the job
/// streams through the owner's tile service unless `reference` asks for the reference renderer.
pub(in crate::api) fn jpeg(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ExportJpeg,
) -> Result<Value, Error> {
    let destination = Destination::check(&params.destination)?;
    let plan = owner
        .service
        .export_plan(&params.asset_id, params.entry_id.as_ref())?;
    let keep_metadata = params.keep_metadata.unwrap_or(false);
    let pixels_per_inch = params.pixels_per_inch;
    let reference = params.reference.unwrap_or(false);
    let job_id = JobId::new();
    let control = JobControl::new();
    let identity = plan.identity.clone();
    let job = ExportJob {
        plan,
        destination: destination.clone(),
        keep_metadata,
        pixels_per_inch,
        reference,
        tiles: Arc::clone(&owner.tiles),
        control: control.clone(),
        #[cfg(test)]
        hold: owner.export_hold.clone(),
    };
    let work: Work = Box::new(move || job.run());
    let record = owner.jobs.submit(
        NewJob {
            job_id,
            kind: JobKind::Export,
            module_id: None,
            resource_id: None,
            asset_id: Some(identity.asset_id.clone()),
            origin: Some(call.origin.clone()),
            grants: Vec::new(),
            activity: Some(ActivitySpec {
                kind: "export",
                label: "Exporting JPEG",
                detail: destination
                    .path()
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                asset_id: Some(identity.asset_id.clone()),
                job_id: None,
            }),
        },
        control,
        work,
    )?;
    Ok(json!({
        "job_id": record.job_id,
        "status": record.status,
        "asset_id": identity.asset_id,
        "entry_id": identity.entry_id,
        "snapshot_id": identity.snapshot_id,
        "destination": destination.path(),
        "width": identity.width,
        "height": identity.height,
        "keep_metadata": keep_metadata,
        "pixels_per_inch": pixels_per_inch,
        "reference": reference,
    }))
}

/// The export lane finished a job: record it and start the next. A written file is announced under
/// the request that asked for it, so every client learns of it from the event log.
pub(super) fn finished(owner: &mut Owner, job_id: &JobId, result: Result<Value, Error>) {
    if let Some(done) = owner.jobs.complete(job_id, result)
        && done.record.status == JobStatus::Ready
        && let Some(origin) = &done.origin
    {
        announce_once(&mut owner.announced, origin);
    }
}

/// Everything one export needs on the lane, frozen when it was accepted.
struct ExportJob {
    plan: ExportPlan,
    destination: Destination,
    keep_metadata: bool,
    pixels_per_inch: Option<u16>,
    /// The export asked for the reference renderer.
    reference: bool,
    /// The owner's tile service, which renders the export's bands on the GPU.
    tiles: Arc<dyn TileService>,
    control: Arc<JobControl>,
    #[cfg(test)]
    hold: Option<Hold>,
}

/// What a stream through the tile service left: the file it wrote, staged, with the renderer that
/// drew its bands; or why the reference is to render the export instead, nothing of the stream
/// kept.
enum Streamed {
    Written(Staged, Renderer),
    Stopped(RendererReason),
}

impl ExportJob {
    /// Render the frozen entry, encode it into a temporary file beside the destination and publish
    /// that under the destination's name without replacing anything. Unless the export asked for
    /// the reference renderer or the host has no GPU provider, the tile service renders the output
    /// stage in bands that the encoder takes as they arrive; when the service says why it cannot,
    /// before the stream or during it, the staged file is dropped and the reference renderer
    /// renders the whole frame instead, so one file is never two renderers' pixels. A cancel stops
    /// the stream between tiles, the render within a row or chunk and the encoder within about 1%
    /// of the rows; a failure or a cancel before the publish drops the staged file, which removes
    /// it.
    fn run(self) -> Result<Value, Error> {
        let Self {
            plan,
            destination,
            keep_metadata,
            pixels_per_inch,
            reference,
            tiles,
            control,
            #[cfg(test)]
            hold,
        } = self;
        // Each phase is published on the board and named by the progress message too, which is
        // what `job.read` answers with; a cancel that arrived meanwhile stops the job there.
        let phase = |phase: &'static str| {
            control.set_phase(phase);
            control.set_progress(None, phase);
            #[cfg(test)]
            if let Some(hold) = &hold {
                hold(phase);
            }
            control.checkpoint()
        };
        phase(RENDERING)?;
        let ExportPlan {
            identity,
            evaluation,
            capture,
        } = plan;
        let encoding = Encoding {
            control: &control,
            capture: &capture,
            keep_metadata,
            pixels_per_inch,
        };
        // Which renderer renders the file, and why the reference does: the reference when the
        // export asked for it or the host has no GPU provider, whose only renderer it is; the GPU
        // otherwise, unless the service says why it cannot.
        let reason = if reference {
            Some(RendererReason::Requested)
        } else if tiles.status() == TileStatus::Reference(None) {
            None
        } else {
            match tiles.stream(&evaluation, control.render_cancel()) {
                Ok(stream) => {
                    let size = (identity.width, identity.height);
                    match streamed(stream, size, &destination, &encoding, &phase)? {
                        Streamed::Written(staged, renderer) => {
                            drop(evaluation);
                            phase(WRITING)?;
                            return written(&destination, staged, size, &encoding, renderer);
                        }
                        // Rendered again from nothing by the reference, which begins with its own
                        // render.
                        Streamed::Stopped(reason) => {
                            phase(RENDERING)?;
                            Some(reason)
                        }
                    }
                }
                Err(fallback) => Some(RendererReason::from(&fallback)),
            }
        };
        let renderer = reason.map_or(Renderer::headless(), Renderer::reference);
        // The frame is the render's own exact frame, from the compilation the plan made, inside the
        // evaluated-frame limit; the encoder reads it in place. The source, the recipe and its
        // artifacts are released with the evaluation.
        let frame = evaluation
            .exact(control.render_cancel())?
            .frame(identity.snapshot_id.clone())?;
        drop(evaluation);
        let mut staged = destination.stage()?;
        phase(ENCODING)?;
        let size = (frame.width, frame.height);
        let exif = encoding.exif(size);
        encode_jpeg(
            &mut staged,
            &frame,
            exif.as_deref(),
            pixels_per_inch,
            &mut |fraction| control.set_progress(Some(fraction), ENCODING),
            &|| control.checkpoint(),
        )?;
        drop(frame);
        phase(WRITING)?;
        written(&destination, staged, size, &encoding, renderer)
    }
}

/// What every encode of one export shares: its control, the original's metadata and its options.
struct Encoding<'a> {
    control: &'a JobControl,
    capture: &'a CaptureMetadata,
    keep_metadata: bool,
    pixels_per_inch: Option<u16>,
}

impl Encoding<'_> {
    /// The EXIF payload of a `size` output, with Keep metadata.
    fn exif(&self, (width, height): (u32, u32)) -> Option<Vec<u8>> {
        self.keep_metadata
            .then(|| self.capture.exif_payload(width, height))
    }
}

/// Encode `stream`'s bands of the `size` output stage into a new temporary file as they arrive, in
/// the phase `encoding`, progress as the rows encoded over the stage's height and the cancel
/// checked before every strip. A cancellation, the encoder's own failure and an error of the
/// stream's own end the export with it; a stream its provider stopped drawing for a reason the
/// reference renders instead is dropped with the temporary file, and that reason is answered.
fn streamed(
    mut stream: BandStream,
    size: (u32, u32),
    destination: &Destination,
    encoding: &Encoding<'_>,
    phase: &dyn Fn(&'static str) -> Result<(), Error>,
) -> Result<Streamed, Error> {
    let control = encoding.control;
    let mut staged = destination.stage()?;
    phase(ENCODING)?;
    let exif = encoding.exif(size);
    let bands = stream
        .by_ref()
        .map(|band| band.map(|band| (band.rows, band.rgba)));
    let encoded = encode_jpeg_rows(
        &mut staged,
        size,
        bands,
        exif.as_deref(),
        encoding.pixels_per_inch,
        &mut |fraction| control.set_progress(Some(fraction), ENCODING),
        &|| control.checkpoint(),
    );
    match encoded {
        Ok(()) => {
            let answered = stream.answered();
            let renderer = match answered.record {
                RendererRecord::Gpu => Renderer::gpu(),
                RendererRecord::Reference => answered
                    .reason
                    .as_ref()
                    .map_or(Renderer::headless(), |reason| {
                        Renderer::reference(reason.into())
                    }),
            };
            Ok(Streamed::Written(staged, renderer))
        }
        Err(error) => {
            // Nothing of the stream is kept: the temporary file goes now, and the provider stops
            // drawing once the stream is dropped.
            drop(staged);
            control.checkpoint()?;
            let reason = stream.fallback().map(RendererReason::from);
            drop(stream);
            reason.map(Streamed::Stopped).ok_or(error)
        }
    }
}

/// Publish `staged`, the encoded `size` output, under the destination's name, and answer the
/// written file: its path, length, size, the EXIF fields kept and the renderer that rendered it.
fn written(
    destination: &Destination,
    staged: Staged,
    (width, height): (u32, u32),
    encoding: &Encoding<'_>,
    renderer: Renderer,
) -> Result<Value, Error> {
    let bytes = staged.publish()?;
    let metadata = if encoding.keep_metadata {
        encoding.capture.field_names()
    } else {
        Vec::new()
    };
    Ok(json!({
        "path": destination.path(),
        "bytes": bytes,
        "width": width,
        "height": height,
        "metadata": metadata,
        "renderer": renderer,
    }))
}
