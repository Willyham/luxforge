//! The export methods and the export job (`docs/design/export.md`). `export.plan` and
//! `export.jpeg` plan on the catalog owner in `O(layers)`, with short file-system checks on the
//! destination and the original's directory; the job renders, encodes and publishes on the owner's
//! export lane, the shared lane runner's `export` lane: one running and four waiting, never
//! superseded, cancelled only by `export.cancel` or the owner stopping. A client disconnecting
//! leaves its exports running, as it leaves capability jobs.
use super::{Call, Owner};
use crate::{
    AssetId, EntryId, Error, JobId, JobStatus, RenderOptions,
    activity::ActivitySpec,
    api::announce_once,
    api::params::host_params,
    capabilities::jobs::{
        Admission, Cancelled, EXPORT_SUBJECT, JobControl, JobKind, JobRecord, NewJob, Work,
    },
    editor::ExportPlan,
    export::{
        encode::encode_jpeg,
        publish::{self, Destination},
    },
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// The phases an export reports on the activity board, in order.
const RENDERING: &str = "rendering";
const ENCODING: &str = "encoding";
const WRITING: &str = "writing";

/// The reason `export.cancel` gives.
const CANCELLED: &str = "the export was cancelled";

/// Called on the export lane as each phase begins, after `encoding` has staged its temporary file,
/// so a test can hold a running export at a known point.
#[cfg(test)]
pub(super) type Hold = Arc<dyn Fn(&'static str) + Send + Sync>;

host_params! {
    /// `export.plan`.
    pub(in crate::api) struct ExportPlanParams {
        asset_id: AssetId,
        entry_id: Option<EntryId> = "a saved entry of the asset; default its current entry",
    }
}

host_params! {
    /// `export.jpeg`.
    pub(in crate::api) struct ExportJpeg {
        asset_id: AssetId,
        destination: PathBuf,
        mutation: MutationRequest,
        entry_id: Option<EntryId> = "a saved entry of the asset; default its current entry",
        keep_metadata: Option<bool> = "write the original's supported EXIF fields; default false",
    }
}

host_params! {
    /// `export.read` and `export.cancel`.
    pub(in crate::api) struct ExportJobParams {
        job_id: JobId,
    }
}

/// `export.plan`: the output stage from the compiled recipe and a suggested destination beside the
/// original, which costs reading at most 64 names in its directory. Nothing is rendered or
/// prepared.
pub(in crate::api) fn plan(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ExportPlanParams,
) -> Result<Value, Error> {
    let target = owner
        .service
        .export_target(&params.asset_id, params.entry_id.as_ref())?;
    let suggested = match (
        target.original.parent(),
        target.original.file_stem().and_then(|stem| stem.to_str()),
    ) {
        (Some(directory), Some(stem)) => publish::suggest(directory, stem),
        _ => None,
    };
    let identity = target.identity;
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
/// unprepared source is `preparation-required` with the source job the owner queued for it.
pub(in crate::api) fn jpeg(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ExportJpeg,
) -> Result<Value, Error> {
    params.mutation.validate()?;
    let destination = Destination::check(&params.destination)?;
    let plan = owner
        .service
        .export_plan(&params.asset_id, params.entry_id.as_ref())?;
    let keep_metadata = params.keep_metadata.unwrap_or(false);
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
    let record = owner.exports.submit(
        NewJob {
            job_id,
            kind: JobKind::Export,
            module_id: EXPORT_SUBJECT.to_owned(),
            resource_id: None,
            origin: Some(call.origin.clone()),
            grants: Vec::new(),
            admission: Admission::Bounded,
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
    }))
}

/// `export.read`: any client may read any export job the owner still keeps.
pub(in crate::api) fn read(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ExportJobParams,
) -> Result<Value, Error> {
    let record = owner
        .exports
        .read(&params.job_id)
        .ok_or_else(|| unknown(&params.job_id))?;
    Ok(value(&record))
}

/// `export.cancel`: cancels the job for every client. A queued job is removed as `cancelled`; a
/// running one is asked to stop at its next row or block and answers `running` until it has; a
/// finished one is answered as it is.
pub(in crate::api) fn cancel(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ExportJobParams,
) -> Result<Value, Error> {
    let record = match owner
        .exports
        .cancel(&params.job_id, CANCELLED)
        .ok_or_else(|| unknown(&params.job_id))?
    {
        Cancelled::Removed(record) | Cancelled::Requested(record) | Cancelled::Finished(record) => {
            record
        }
    };
    Ok(value(&record))
}

/// The export lane finished a job: record it and start the next. A written file is announced under
/// the request that asked for it, so every client learns of it from the event log.
pub(super) fn finished(owner: &mut Owner, job_id: &JobId, result: Result<Value, Error>) {
    if let Some(done) = owner.exports.complete(job_id, result)
        && done.record.status == JobStatus::Ready
        && let Some(origin) = &done.origin
    {
        announce_once(&mut owner.announced, origin);
    }
}

fn unknown(job_id: &JobId) -> Error {
    Error::validation(format!("unknown export job {job_id}"))
}

/// One export job as `export.read` answers it.
fn value(record: &JobRecord) -> Value {
    let mut value = json!({
        "job_id": record.job_id,
        "status": record.status,
        "progress": record.progress,
    });
    if let Some(result) = &record.result {
        value["result"] = result.clone();
    }
    if let Some(error) = &record.error {
        value["error"] = json!(error);
    }
    value
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
        // what `export.read` answers with; a cancel that arrived meanwhile stops the job there.
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
            source,
            registry,
            context,
            recipe,
            capture,
        } = plan;
        // The frame is the render's own exact frame, inside the evaluated-frame limit; the encoder
        // reads it in place. The source, the recipe and its artifacts are released with the render.
        let frame = crate::render(
            &registry,
            source.input(),
            &recipe,
            RenderOptions::exact(control.render_cancel()),
            &context,
        )?
        .frame(identity.snapshot_id.clone())?;
        drop((source, recipe));
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
        }))
    }
}
