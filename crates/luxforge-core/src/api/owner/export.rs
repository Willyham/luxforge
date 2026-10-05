//! The export methods and the export job (`docs/design/export.md`). `export.plan` and
//! `export.jpeg` plan on the catalog owner in `O(layers)`, with short file-system checks on the
//! destination and the original's directory; the job renders, encodes and publishes on the `export`
//! lane of the owner's one job table: one running and four waiting, never superseded, read with
//! `job.read` and cancelled only by `job.cancel` or the owner stopping. A client disconnecting
//! leaves its exports running, as it leaves capability jobs.
//!
//! Every export renders through the reference renderer, and a written file's result names it.
//! `reference: true` asks for that renderer explicitly, which changes nothing until the GPU renders
//! exports (`docs/design/gpu-first.md`, stage 4).
use super::{Call, Owner};
use crate::{
    AssetId, EntryId, Error, JobId, JobStatus, Renderer,
    activity::ActivitySpec,
    api::announce_once,
    api::params::host_params,
    editor::ExportPlan,
    export::{
        encode::encode_jpeg,
        publish::{self, Destination},
    },
    jobs::{JobControl, JobKind, NewJob, Work},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// The phases an export reports on the activity board, in order.
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
        reference: Option<bool> = boolean().default(false).notes("render through the reference renderer rather than the GPU; today every export is the reference renderer's, so the file is the same either way"),
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
/// answer echoes `keep_metadata` and `reference` as the job was accepted with them; the job reads
/// no `reference`, because the reference renderer renders every export.
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
    let reference = params.reference.unwrap_or(false);
    let job_id = JobId::new();
    let control = JobControl::new();
    let identity = plan.identity.clone();
    let job = ExportJob {
        plan,
        destination: destination.clone(),
        keep_metadata,
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
    control: Arc<JobControl>,
    #[cfg(test)]
    hold: Option<Hold>,
}

impl ExportJob {
    /// Render the frozen entry exactly, encode it into a temporary file beside the destination and
    /// publish that under the destination's name without replacing anything. A cancel stops the
    /// render within a row or chunk and the encoder within about 1% of the rows; a failure or a
    /// cancel before the publish drops the staged file, which removes it.
    fn run(self) -> Result<Value, Error> {
        let Self {
            plan,
            destination,
            keep_metadata,
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
        // The frame is the render's own exact frame, from the compilation the plan made, inside the
        // evaluated-frame limit; the encoder reads it in place. The source, the recipe and its
        // artifacts are released with the evaluation.
        let frame = evaluation
            .exact(control.render_cancel())?
            .frame(identity.snapshot_id.clone())?;
        drop(evaluation);
        let mut staged = destination.stage()?;
        phase(ENCODING)?;
        let exif = keep_metadata.then(|| capture.exif_payload(frame.width, frame.height));
        encode_jpeg(
            &mut staged,
            &frame,
            exif.as_deref(),
            &mut |fraction| control.set_progress(Some(fraction), ENCODING),
            &|| control.checkpoint(),
        )?;
        let (width, height) = (frame.width, frame.height);
        drop(frame);
        phase(WRITING)?;
        let bytes = staged.publish()?;
        let metadata = if keep_metadata {
            capture.field_names()
        } else {
            Vec::new()
        };
        Ok(json!({
            "path": destination.path(),
            "bytes": bytes,
            "width": width,
            "height": height,
            "metadata": metadata,
            // The renderer that rendered the file: the reference renderer, with no reason because
            // it is the only renderer an export has until the GPU renders exports.
            "renderer": Renderer::headless(),
        }))
    }
}
