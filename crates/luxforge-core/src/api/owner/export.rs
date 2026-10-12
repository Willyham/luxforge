//! The export methods and the export job (`docs/design/export.md`,
//! `docs/design/export-settings.md`). `export.plan` and `export.write` plan on the catalog owner in
//! `O(layers)`, with short file-system checks on the destination and the export's folder; the job
//! renders, encodes and publishes on the `export`
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
//!
//! Until the new encoders and resized renders are wired in, `export.write` and `batch.export` write
//! what this pipeline can: a full-size, quality-90, 4:4:4 JPEG, streamed without optimisation or a
//! file-size limit, at any metadata level and density ([`writable_now`]). Every other setting is
//! refused by name as not yet available, never ignored; `export.plan` answers for every setting.
//!
//! The `export.presets.*` methods keep the person's export presets beside the preferences
//! ([`crate::export::presets`]).
use super::{Call, Owner};
use crate::{
    AssetId, EntryId, Error, JobId, JobStatus, Renderer, RendererReason,
    activity::ActivitySpec,
    api::announce_once,
    api::params::{NoParams, host_params},
    editor::ExportPlan,
    export::{
        CaptureMetadata, QUALITY,
        encode::{BASELINE_90, encode_jpeg, encode_jpeg_rows},
        presets,
        publish::{self, Destination, Staged},
        settings::{
            Chroma, ExportFolder, ExportFormat, ExportSettings, FOLDER_MISSING, FormatSettings,
            MetadataLevel, Notice, OutputSize,
        },
    },
    jobs::{JobControl, JobKind, NewJob, Work},
    tiles::{BandStream, OutputSpec, TileService, TileStatus},
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

/// The `settings` parameter's notes, which point at the one description in `schema.list`.
pub(in crate::api) const SETTINGS_NOTES: &str =
    "ExportSettings, as schema.list's export_settings describes";

host_params! {
    /// `export.plan`.
    pub(in crate::api) struct ExportPlanParams {
        asset_id: AssetId = asset(),
        settings: ExportSettings = json(SETTINGS_NOTES),
        entry_id: Option<EntryId> = entry().notes("a saved entry of the asset; default its current entry"),
    }
}

host_params! {
    /// `export.write`.
    pub(in crate::api) struct ExportWrite {
        asset_id: AssetId = asset(),
        settings: ExportSettings = json(SETTINGS_NOTES),
        destination: PathBuf = path().notes("an absolute path whose extension matches settings.format (.jpg or .jpeg, .webp, .tif or .tiff), whose parent directory exists and at which nothing exists"),
        mutation: MutationRequest,
        entry_id: Option<EntryId> = entry().notes("a saved entry of the asset; default its current entry"),
        reference: Option<bool> = boolean().default(false).notes("render through the reference renderer, the CPU render every GPU output is measured against, rather than the GPU; the result's renderer then names the reference with the reason requested"),
        acknowledge_stale: Option<crate::StaleAcknowledgement> = json("{entry_id, operations: [correction-…]}: export the entry's stale frozen patches as they render; required, naming exactly the frozen entry and its stale corrections, when it has any (export.plan lists them), and ignored when it has none"),
    }
}

host_params! {
    /// `export.presets.create`.
    pub(in crate::api) struct PresetCreate {
        name: String = string(presets::MAX_NAME).notes("1 to 64 characters, no control character and no leading or trailing space, unique ignoring case across every preset, built-in ones included"),
        settings: ExportSettings = json(SETTINGS_NOTES),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `export.presets.update`.
    pub(in crate::api) struct PresetUpdate {
        preset_id: String = string(presets::MAX_ID).notes("a preset of the person's own, as export.presets.list answers it"),
        mutation: MutationRequest,
        name: Option<String> = string(presets::MAX_NAME).notes("the new name; unchanged when left out"),
        settings: Option<ExportSettings> = json("the new settings, ExportSettings as schema.list's export_settings describes; unchanged when left out"),
    }
}

host_params! {
    /// `export.presets.delete`.
    pub(in crate::api) struct PresetDelete {
        preset_id: String = string(presets::MAX_ID).notes("a preset of the person's own"),
        mutation: MutationRequest,
    }
}

/// `export.plan`: what an export of the entry with `settings` would be, rendering and preparing
/// nothing: the output stage from the compiled recipe and the output size the settings give it,
/// the physical size at their density, the suggested destination in their folder (at most 64 names
/// read), the encoder's held-memory estimate and the notices, and the stale frozen patches an
/// export must acknowledge. A derived photograph has no original file to name a destination after,
/// so nothing is suggested for it.
pub(in crate::api) fn plan(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ExportPlanParams,
) -> Result<Value, Error> {
    let settings = params.settings;
    let target = owner
        .service
        .export_target(&params.asset_id, params.entry_id.as_ref())?;
    let original = target.original.as_deref();
    let identity = target.evaluation.identity()?;
    let stage = (identity.width, identity.height);
    let output = settings.output_size(stage);
    let folder = settings.folder.resolve(original);
    let notices = settings.notices(output, folder.as_deref());
    let suggested = match (
        folder.as_deref().filter(|folder| folder.is_dir()),
        original
            .and_then(std::path::Path::file_stem)
            .and_then(|stem| stem.to_str()),
    ) {
        (Some(directory), Some(stem)) => {
            publish::suggest(directory, stem, &settings.suffix, settings.format())
        }
        _ => None,
    };
    // The frozen patches the planned entry would export stale, which an export must acknowledge.
    let (_, stale) = owner
        .service
        .export_stale(&params.asset_id, Some(&identity.entry_id))?;
    let mut answer = json!({
        "asset_id": identity.asset_id,
        "entry_id": identity.entry_id,
        "snapshot_id": identity.snapshot_id,
        "stage": {"width": stage.0, "height": stage.1},
        "output": {"width": output.0, "height": output.1},
        // A name that is not UTF-8 is not suggested rather than suggested wrongly.
        "suggested": suggested.as_deref().and_then(|path| path.to_str()),
        "estimate": settings.held_bytes(output),
        "notices": notices,
        "stale": stale,
    });
    if let Some(density) = settings.pixels_per_inch {
        answer["physical"] = physical(output, density);
    }
    Ok(answer)
}

/// The physical size of an `output` at `density` pixels per inch, in inches and centimetres, each
/// to a hundredth.
fn physical((width, height): (u32, u32), density: u16) -> Value {
    let inches = |side: u32| f64::from(side) / f64::from(density);
    let hundredths = |value: f64| (value * 100.0).round() / 100.0;
    json!({
        "inches": {"width": hundredths(inches(width)), "height": hundredths(inches(height))},
        "centimetres": {
            "width": hundredths(inches(width) * 2.54),
            "height": hundredths(inches(height) * 2.54),
        },
    })
}

/// Refuse every setting today's pipeline cannot honour yet, by name: it writes a full-size,
/// quality-90, 4:4:4 JPEG, streamed without optimisation or a limit, at any metadata level,
/// density, folder and suffix. Nothing is ever ignored.
pub(super) fn writable_now(settings: &ExportSettings) -> Result<(), Error> {
    let not_yet = |field: &str, what: String| {
        Err(Error::validation(format!(
            "settings.{field} {what} is not yet available for export"
        ))
        .with_data(json!({"field": format!("settings.{field}"), "reason": "not-yet-available"})))
    };
    match settings.format {
        FormatSettings::Jpeg {
            quality,
            chroma,
            limit_bytes,
        } => {
            if quality != QUALITY {
                return not_yet("quality", format!("{quality} (only {QUALITY})"));
            }
            if chroma != Chroma::Full {
                return not_yet("chroma", format!("{} (only 444)", chroma.as_str()));
            }
            if limit_bytes.is_some() {
                return not_yet("limit_bytes", "(a file-size limit)".into());
            }
        }
        FormatSettings::Webp { .. } | FormatSettings::Tiff { .. } => {
            return not_yet(
                "format",
                format!("{} (only jpeg)", settings.format().as_str()),
            );
        }
    }
    if settings.size != OutputSize::Full {
        return not_yet("size", "other than {fit: full}".into());
    }
    if settings.optimise {
        return not_yet("optimise", "true".into());
    }
    Ok(())
}

/// The notice that refuses an export of a `stage` with `settings`, from the photograph's
/// `original`, as `export.plan` shows it. An original's own folder that is missing is left to the
/// source's refusal, which says why (`source-unavailable` for a missing original or an offline
/// volume); a derived photograph, which has no original's folder, is refused here.
pub(super) fn refusing_notice(
    settings: &ExportSettings,
    stage: (u32, u32),
    original: Option<&std::path::Path>,
) -> Option<Notice> {
    let folder = settings.folder.resolve(original);
    let source_reports = settings.folder == ExportFolder::Original && original.is_some();
    settings
        .notices(settings.output_size(stage), folder.as_deref())
        .into_iter()
        .find(|notice| notice.refuses && !(source_reports && notice.code == FOLDER_MISSING))
}

/// `export.write`: check the settings and the destination, freeze the entry and queue one job on the
/// export lane. An obvious refusal — a setting not yet available, a destination's shape or an
/// existing file, an unknown asset or entry, a stack the host cannot evaluate, a notice that
/// refuses the export, a missing original — is answered now and queues nothing; an unprepared
/// source is `preparation-required` with the source job the owner queued for it. The answer echoes
/// the settings and `reference` as the job was accepted with them, and the job streams through the
/// owner's tile service unless `reference` asks for the reference renderer.
pub(in crate::api) fn write_file(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ExportWrite,
) -> Result<Value, Error> {
    let settings = params.settings;
    writable_now(&settings)?;
    let destination = Destination::check(&params.destination, settings.format())?;
    // Stale frozen patches are written only as acknowledged, refused before any job or
    // preparation; an omitted entry is the current one now, so a commit since the plan makes an
    // old acknowledgement name another entry.
    let (entry_id, stale) = owner
        .service
        .export_stale(&params.asset_id, params.entry_id.as_ref())?;
    let acknowledged = crate::editor::acknowledged(
        &entry_id,
        stale,
        params
            .acknowledge_stale
            .as_ref()
            .map(|ack| (&ack.entry_id, ack.operations.as_slice())),
    )?;
    // The notices that refuse an export are the plan's, checked before anything is prepared.
    let target = owner
        .service
        .export_target(&params.asset_id, Some(&entry_id))?;
    let stage = target.evaluation.identity()?;
    if let Some(notice) = refusing_notice(
        &settings,
        (stage.width, stage.height),
        target.original.as_deref(),
    ) {
        return Err(notice.refusal());
    }
    drop(target);
    let plan = owner
        .service
        .export_plan(&params.asset_id, Some(&entry_id))?;
    let reference = params.reference.unwrap_or(false);
    let job_id = JobId::new();
    let control = JobControl::new();
    let identity = plan.identity.clone();
    let job = ExportJob {
        plan,
        acknowledged: acknowledged.clone(),
        destination: destination.clone(),
        options: Options::of(&settings, reference),
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
                label: match settings.format() {
                    ExportFormat::Jpeg => "Exporting JPEG",
                    ExportFormat::Webp => "Exporting WebP",
                    ExportFormat::Tiff => "Exporting TIFF",
                },
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
    // An export of a photograph a client has open carries its image ID in its outcome.
    let usage = &owner.service.usage;
    if let Some(image) = usage
        .on()
        .and_then(|generation| usage.open_image(generation, identity.asset_id.as_str()))
    {
        owner.jobs.set_usage_image(&record.job_id, image);
    }
    Ok(json!({
        "job_id": record.job_id,
        "status": record.status,
        "asset_id": identity.asset_id,
        "entry_id": identity.entry_id,
        "snapshot_id": identity.snapshot_id,
        "destination": destination.path(),
        "width": identity.width,
        "height": identity.height,
        "settings": settings,
        "reference": reference,
        "acknowledged_stale": acknowledged,
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
    /// The stale frozen patches the export was accepted to write, which its result echoes.
    acknowledged: Vec<crate::StaleCorrection>,
    destination: Destination,
    options: Options,
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
    /// Write the frozen entry to its destination ([`write`]), publishing each phase on the board
    /// and naming it in the progress message too, which is what `job.read` answers with.
    fn run(self) -> Result<Value, Error> {
        let Self {
            plan,
            acknowledged,
            destination,
            options,
            tiles,
            control,
            #[cfg(test)]
            hold,
        } = self;
        let phase = |phase: &'static str| {
            control.set_phase(phase);
            control.set_progress(None, phase);
            #[cfg(test)]
            if let Some(hold) = &hold {
                hold(phase);
            }
            control.checkpoint()
        };
        write(
            plan,
            &destination,
            options,
            tiles.as_ref(),
            &control,
            &phase,
            &mut |fraction| control.set_progress(Some(fraction), ENCODING),
        )
        .map(|written| {
            let mut result = written.result;
            result["acknowledged_stale"] = json!(acknowledged);
            result
        })
    }
}

/// A file [`write`] wrote: the answer `export.write`'s job records, and the renderer it names, for
/// `batch.export`'s report.
pub(super) struct Written {
    /// `{path, bytes, width, height, format, quality, optimised, metadata, renderer}`.
    pub(super) result: Value,
    pub(super) renderer: Renderer,
}

/// How one export is written: what of its settings the pipeline reads ([`writable_now`] has
/// refused the rest), and whether it asked for the reference renderer.
#[derive(Clone, Copy, Debug)]
pub(super) struct Options {
    pub(super) metadata: MetadataLevel,
    /// The JFIF density, if any.
    pub(super) pixels_per_inch: Option<u16>,
    /// The export asked for the reference renderer.
    pub(super) reference: bool,
}

impl Options {
    pub(super) fn of(settings: &ExportSettings, reference: bool) -> Self {
        Self {
            metadata: settings.metadata,
            pixels_per_inch: settings.pixels_per_inch,
            reference,
        }
    }
}

/// One export's work once its entry is frozen, shared by `export.write`'s job and each photograph of
/// `batch.export`: render the entry, encode it into a temporary file beside `destination` and
/// publish that under the destination's name without replacing anything, answering `{path, bytes,
/// width, height, format, quality, optimised, metadata, renderer}` with the renderer
/// ([`Written`]).
///
/// Unless the export asked for the reference renderer or the host has no GPU provider, `tiles`
/// renders the output stage in bands that the encoder takes as they arrive; when the service says
/// why it cannot, before the stream or during it, the staged file is dropped and the reference
/// renderer renders the whole frame instead, so one file is never two renderers' pixels.
///
/// `phase` is called as each phase begins (`rendering`, `encoding`, then `writing`; `rendering`
/// again when the reference renders after a stream stopped) and stops the export where it answers
/// an error, as a cancel that arrived meanwhile does; `encoded` is told the fraction of rows
/// encoded. A cancel of `control` stops the stream between tiles, the render within a row or chunk
/// and the encoder within about 1% of the rows; a failure or a cancel before the publish drops the
/// staged file, which removes it.
pub(super) fn write(
    plan: ExportPlan,
    destination: &Destination,
    options: Options,
    tiles: &dyn TileService,
    control: &JobControl,
    phase: &dyn Fn(&'static str) -> Result<(), Error>,
    encoded: &mut dyn FnMut(f64),
) -> Result<Written, Error> {
    phase(RENDERING)?;
    let ExportPlan {
        identity,
        evaluation,
        capture,
    } = plan;
    let capture = capture.at_level(options.metadata);
    let encoding = Encoding {
        control,
        capture: &capture,
        metadata: options.metadata,
        pixels_per_inch: options.pixels_per_inch,
    };
    // Which renderer renders the file, and why the reference does: the reference when the export
    // asked for it or the host has no GPU provider, whose only renderer it is; the GPU otherwise,
    // unless the service says why it cannot.
    let reason = if options.reference {
        Some(RendererReason::Requested)
    } else if tiles.status() == TileStatus::Reference(None) {
        None
    } else {
        let output = OutputSpec::full(identity.width, identity.height);
        match tiles.stream(&evaluation, output, control.render_cancel()) {
            Ok(stream) => {
                let size = (identity.width, identity.height);
                match streamed(stream, size, destination, &encoding, phase, encoded)? {
                    Streamed::Written(staged, renderer) => {
                        drop(evaluation);
                        phase(WRITING)?;
                        return written(destination, staged, size, &encoding, renderer);
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
        &BASELINE_90,
        exif.as_deref(),
        options.pixels_per_inch,
        encoded,
        &|| control.checkpoint(),
    )?;
    drop(frame);
    phase(WRITING)?;
    written(destination, staged, size, &encoding, renderer)
}

/// What every encode of one export shares: its control, the original's metadata at the export's
/// level and its options.
struct Encoding<'a> {
    control: &'a JobControl,
    /// The fields the export's metadata level keeps.
    capture: &'a CaptureMetadata,
    metadata: MetadataLevel,
    pixels_per_inch: Option<u16>,
}

impl Encoding<'_> {
    /// The EXIF payload of a `size` output: none at level none, and otherwise the level's fields
    /// with the structural ones.
    fn exif(&self, (width, height): (u32, u32)) -> Option<Vec<u8>> {
        self.metadata
            .writes_exif()
            .then(|| self.capture.exif_payload(width, height))
    }
}

/// Encode `stream`'s bands of the `size` output stage into a new temporary file as they arrive, in
/// the phase `encoding`, `encoded` told the fraction of the stage's rows encoded and the cancel
/// checked before every strip. A cancellation, the encoder's own failure and an error of the
/// stream's own end the export with it; a stream its provider stopped drawing for a reason the
/// reference renders instead is dropped with the temporary file, and that reason is answered.
fn streamed(
    mut stream: BandStream,
    size: (u32, u32),
    destination: &Destination,
    encoding: &Encoding<'_>,
    phase: &dyn Fn(&'static str) -> Result<(), Error>,
    encoded: &mut dyn FnMut(f64),
) -> Result<Streamed, Error> {
    let control = encoding.control;
    let mut staged = destination.stage()?;
    phase(ENCODING)?;
    let exif = encoding.exif(size);
    let bands = stream
        .by_ref()
        .map(|band| band.and_then(|band| Ok((band.rows, band.into_rgba8()?))));
    let result = encode_jpeg_rows(
        &mut staged,
        size,
        bands,
        &BASELINE_90,
        exif.as_deref(),
        encoding.pixels_per_inch,
        encoded,
        &|| control.checkpoint(),
    );
    match result {
        Ok(()) => Ok(Streamed::Written(staged, stream.answered().into())),
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
/// written file: its path, length, size, format, the quality used, whether it was optimised, the
/// EXIF fields kept and the renderer that rendered it.
fn written(
    destination: &Destination,
    staged: Staged,
    (width, height): (u32, u32),
    encoding: &Encoding<'_>,
    renderer: Renderer,
) -> Result<Written, Error> {
    let bytes = staged.publish()?;
    // The level's fields: none at level none, whose capture holds none.
    let metadata = encoding.capture.field_names();
    Ok(Written {
        result: json!({
            "path": destination.path(),
            "bytes": bytes,
            "width": width,
            "height": height,
            "format": ExportFormat::Jpeg.as_str(),
            "quality": QUALITY,
            "optimised": false,
            "metadata": metadata,
            "renderer": renderer,
        }),
        renderer,
    })
}

/// `export.presets.list`: the built-in presets, then the person's by name, each `{preset_id, name,
/// builtin, settings}`.
pub(in crate::api) fn presets_list(
    owner: &mut Owner,
    _: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    let presets = owner.host.preferences.export_presets().list()?;
    Ok(json!({ "presets": presets }))
}

/// `export.presets.create`: one stored preset, announced.
pub(in crate::api) fn presets_create(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PresetCreate,
) -> Result<Value, Error> {
    let preset = owner
        .host
        .preferences
        .export_presets()
        .create(&params.name, params.settings)?;
    announce_once(&mut owner.announced, &call.origin);
    Ok(json!({ "preset": preset }))
}

/// `export.presets.update`: a preset of the person's own renamed, given new settings, or both,
/// announced. A built-in preset is refused with `validation`.
pub(in crate::api) fn presets_update(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PresetUpdate,
) -> Result<Value, Error> {
    let preset = owner.host.preferences.export_presets().update(
        &params.preset_id,
        params.name.as_deref(),
        params.settings,
    )?;
    announce_once(&mut owner.announced, &call.origin);
    Ok(json!({ "preset": preset }))
}

/// `export.presets.delete`: a preset of the person's own removed, announced when it was there. A
/// built-in preset is refused with `validation`.
pub(in crate::api) fn presets_delete(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PresetDelete,
) -> Result<Value, Error> {
    let deleted = owner
        .host
        .preferences
        .export_presets()
        .delete(&params.preset_id)?;
    if deleted {
        announce_once(&mut owner.announced, &call.origin);
    }
    let outcome = if deleted {
        crate::MutationOutcome::Applied
    } else {
        crate::MutationOutcome::NoOp
    };
    Ok(json!({"outcome": outcome, "deleted": deleted}))
}
