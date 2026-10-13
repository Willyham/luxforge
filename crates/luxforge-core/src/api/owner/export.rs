//! The export methods and the export job (`docs/design/export.md`,
//! `docs/design/export-settings.md`). `export.plan` and `export.write` plan on the catalog owner in
//! `O(layers)`, with short file-system checks on the destination and the export's folder; the job
//! renders, encodes and publishes on the `export`
//! lane of the owner's one job table: one running and four waiting, never superseded, read with
//! `job.read` and cancelled only by `job.cancel` or the owner stopping. A client disconnecting
//! leaves its exports running, as it leaves capability jobs.
//!
//! An export renders its settings' output (`ExportSettings::output_spec`: the output stage, or a
//! smaller size, at 8 bits or a TIFF's 16) through the owner's tile service (`docs/design/gpu-first.md`,
//! stage 4): the host's GPU provider renders it in bands on its own thread, resampling a smaller
//! output in its last sweep. The reference renderer renders the whole output instead
//! ([`reference_output_phased`]) when the export asks for it (`reference: true`), when the host has
//! no GPU provider (`luxforge-json`), and when the provider says why it cannot, at once or
//! part-way, in which case the export starts again from nothing: GPU and reference pixels are never
//! mixed in one file. A written file's result names the renderer that rendered it, and why the
//! reference did. A paged derived source has no whole frame: its reference is the banded mode
//! ([`banded`]), which renders every output the settings ask for band by band, each band the
//! whole-frame reference's rows, so neither the stage nor the output is held whole except where a
//! held format holds its output frame.
//!
//! Each format is encoded as its encoder allows ([`Route`]): a JPEG without a size limit, optimised
//! or not, and a TIFF at 8 or 16 bits, are encoded as the bands arrive, so no frame is held (an
//! optimised JPEG's coefficients are libjpeg's own); a size-limited JPEG, a WebP and a size-limited
//! WebP hold the 8-bit output frame, assembled once from the bands or read in place from the
//! reference's frame, and encode it whole. Progress is determinate over the whole export, in whole
//! percent and never decreasing, through the phases [`Phase`] names.
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
        CaptureMetadata,
        encode::{self, JpegOptions, JpegProgress},
        limit::{self, LimitProgress, MAX_ATTEMPTS},
        presets,
        publish::{self, Destination, Staged},
        settings::{
            ExportFolder, ExportFormat, ExportSettings, FOLDER_MISSING, FormatSettings, Notice,
        },
        tiff::{TiffOptions, TiffWriter},
        webp::{self, WebpOptions},
    },
    jobs::{JobControl, JobKind, NewJob, Work},
    render::{bands::Banded, bands_output::output_bands},
    tiles::{
        BandStream, OutputSpec, PixelRows, Pixels, TileService, TileStatus,
        output::{OutputFrame, reference_output_phased},
    },
};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf, sync::Arc};

/// The reason `job.cancel` gives an export.
pub(super) const CANCELLED: &str = "the export was cancelled";

/// Called on the export lane as each phase begins (with the phase's name, after a streamed
/// `encoding` has staged its temporary file) and each time its progress changes (with
/// `"progress"`, after `job.read` would answer the change), and with `"written"` once its file is
/// published and before the job is recorded as finished, so a test can hold a running export at a
/// known point or read it there.
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
/// export lane. An obvious refusal — a destination's shape or an existing file, an unknown asset or entry, a stack the host cannot evaluate, a notice that
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
        settings: settings.clone(),
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
    settings: ExportSettings,
    /// The export asked for the reference renderer.
    reference: bool,
    /// The owner's tile service, which renders the export's bands on the GPU.
    tiles: Arc<dyn TileService>,
    control: Arc<JobControl>,
    #[cfg(test)]
    hold: Option<Hold>,
}

impl ExportJob {
    /// Write the frozen entry to its destination ([`write`]), publishing each phase on the board
    /// and the whole export's progress, with the phase as its message, which is what `job.read`
    /// answers with.
    fn run(self) -> Result<Value, Error> {
        let Self {
            plan,
            acknowledged,
            destination,
            settings,
            reference,
            tiles,
            control,
            #[cfg(test)]
            hold,
        } = self;
        let phase = |phase: &'static str| {
            control.set_phase(phase);
            #[cfg(test)]
            if let Some(hold) = &hold {
                hold(phase);
            }
            control.checkpoint()
        };
        let mut progress = |fraction: f64, message: &str| {
            control.set_progress(Some(fraction), message);
            #[cfg(test)]
            if let Some(hold) = &hold {
                hold("progress");
            }
        };
        write(
            plan,
            &destination,
            &settings,
            reference,
            tiles.as_ref(),
            &control,
            &phase,
            &mut progress,
        )
        .map(|written| {
            // Published, its last progress reported, and not yet recorded as finished.
            #[cfg(test)]
            if let Some(hold) = &hold {
                hold("written");
            }
            let mut result = written.result;
            result["acknowledged_stale"] = json!(acknowledged);
            result
        })
    }
}

/// A file [`write`] wrote: the answer `export.write`'s job records, and the renderer and quality it
/// names, for `batch.export`'s report.
pub(super) struct Written {
    /// `{path, bytes, width, height, format, quality?, optimised, metadata, renderer}`.
    pub(super) result: Value,
    pub(super) renderer: Renderer,
    /// The quality the file was encoded at: the settings' own, or the one a size limit chose. None
    /// for a TIFF.
    pub(super) quality: Option<u8>,
}

/// The phases an export reports, in order: on the activity board as its phase, and in `job.read`'s
/// progress message. A GPU export resamples a smaller output in its stream's last sweep, so it
/// reports no `resampling` of its own: that time is inside `encoding` for a streamed format and
/// `rendering` for a held one. An export the reference renders again after its stream stopped
/// begins `rendering` again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The render: the reference's whole frame, or a held format's bands as they arrive. A
    /// streamed format's bands are rendered while `encoding`.
    Rendering,
    /// The reference resampling its frame to a smaller output.
    Resampling,
    /// The rows encoded as they arrive, or a held frame encoded once.
    Encoding,
    /// An optimised JPEG's passes after its rows.
    Optimising,
    /// A size limit's encodes, each named with its attempt (`limiting 3/8`).
    Limiting,
    /// The file published, after a held format's bytes are written to it.
    Writing,
}

impl Phase {
    const COUNT: usize = 6;

    fn name(self) -> &'static str {
        match self {
            Self::Rendering => "rendering",
            Self::Resampling => "resampling",
            Self::Encoding => "encoding",
            Self::Optimising => "optimising",
            Self::Limiting => "limiting",
            Self::Writing => "writing",
        }
    }
}

/// How an export's settings are encoded.
#[derive(Clone, Copy, Debug)]
enum Route {
    /// Encoded as the bands arrive, holding no frame.
    Streamed(Streaming),
    /// The 8-bit output frame held and encoded whole.
    Held(Held),
}

#[derive(Clone, Copy, Debug)]
enum Streaming {
    /// A JPEG without a size limit: baseline, or optimised with libjpeg holding its coefficients.
    Jpeg(JpegOptions),
    /// A TIFF at 8 or 16 bits, by strips.
    Tiff(TiffOptions),
}

#[derive(Clone, Copy, Debug)]
enum Held {
    /// A JPEG under a size limit, by [`encode::encode_jpeg_limited`].
    Jpeg { options: JpegOptions, limit: u64 },
    /// A WebP, under a size limit by [`limit::search`] when it has one.
    Webp {
        options: WebpOptions,
        limit: Option<u64>,
    },
}

impl Route {
    fn of(settings: &ExportSettings) -> Self {
        let optimise = settings.optimise;
        match settings.format {
            FormatSettings::Jpeg {
                quality,
                chroma,
                limit_bytes,
            } => {
                let options = JpegOptions {
                    quality,
                    chroma,
                    optimise,
                };
                match limit_bytes {
                    None => Self::Streamed(Streaming::Jpeg(options)),
                    Some(limit) => Self::Held(Held::Jpeg { options, limit }),
                }
            }
            FormatSettings::Webp {
                quality,
                limit_bytes,
            } => Self::Held(Held::Webp {
                options: WebpOptions { quality, optimise },
                limit: limit_bytes,
            }),
            FormatSettings::Tiff { bit_depth } => Self::Streamed(Streaming::Tiff(TiffOptions {
                depth: bit_depth,
                compress: optimise,
                pixels_per_inch: settings.pixels_per_inch,
            })),
        }
    }

    /// Each phase's share of the whole export's progress, in the order [`Phase`] lists them, for
    /// an export whose output arrives `in_bands` (the GPU's stream, or the reference's banded mode
    /// on a paged source) or is rendered whole by the reference, resampled to a smaller output or
    /// not: what each phase costs, roughly, measured on the M4 (`docs/design/export-settings.md`,
    /// progress). A banded render runs inside a streamed format's `encoding` and a held format's
    /// `rendering`, resampling included; the whole-frame reference renders first and resamples
    /// after. An optimised JPEG's passes outweigh its rows; a size limit's attempts are almost the
    /// whole of a held export. Writing a file is the last 5%.
    fn weights(&self, in_bands: bool, resampled: bool) -> [u32; Phase::COUNT] {
        let rendering = match (in_bands, self) {
            (true, Self::Streamed(_)) => 0,
            (true, Self::Held(_)) => 20,
            (false, _) => 30,
        };
        let resampling = if !in_bands && resampled { 10 } else { 0 };
        let (encoding, optimising, limiting) = match self {
            Self::Streamed(Streaming::Jpeg(options)) if options.optimise => {
                if in_bands {
                    (30, 65, 0)
                } else {
                    (15, 40, 0)
                }
            }
            Self::Streamed(_) => (if in_bands { 95 } else { 55 }, 0, 0),
            Self::Held(Held::Jpeg { .. } | Held::Webp { limit: Some(_), .. }) => (0, 0, 70),
            Self::Held(Held::Webp { limit: None, .. }) => (70, 0, 0),
        };
        [rendering, resampling, encoding, optimising, limiting, 5]
    }
}

/// The whole export's progress as its phases go: each phase a span of the whole by its weight,
/// reported in whole percent, never decreasing, with the phase as its message. When the reference
/// renders again after a stream stopped, its phases share what is left from where the stream got
/// to.
struct Report<'a> {
    phase: &'a dyn Fn(&'static str) -> Result<(), Error>,
    progress: &'a mut dyn FnMut(f64, &str),
    /// Each phase's start and end, from 0 to 1 of what is left after `base`.
    spans: [(f64, f64); Phase::COUNT],
    base: f64,
    /// The whole percent and the message last reported.
    percent: u32,
    message: String,
    /// The optimising phase has begun.
    optimising: bool,
}

impl<'a> Report<'a> {
    fn new(
        phase: &'a dyn Fn(&'static str) -> Result<(), Error>,
        progress: &'a mut dyn FnMut(f64, &str),
        weights: [u32; Phase::COUNT],
    ) -> Self {
        let mut report = Self {
            phase,
            progress,
            spans: [(0.0, 0.0); Phase::COUNT],
            base: 0.0,
            percent: 0,
            message: String::new(),
            optimising: false,
        };
        report.plan(weights);
        report
    }

    fn plan(&mut self, weights: [u32; Phase::COUNT]) {
        let total = f64::from(weights.iter().sum::<u32>().max(1));
        let mut start = 0;
        for (span, weight) in self.spans.iter_mut().zip(weights) {
            *span = (f64::from(start) / total, f64::from(start + weight) / total);
            start += weight;
        }
    }

    /// Start again from nothing, as the reference does after a stream stopped, with its own
    /// weights over what is left.
    fn restart(&mut self, weights: [u32; Phase::COUNT]) {
        self.base = f64::from(self.percent) / 100.0;
        self.optimising = false;
        self.plan(weights);
    }

    /// Begin `phase`: on the board, where a cancel that arrived stops the export, then at its
    /// start in the progress.
    fn enter(&mut self, phase: Phase) -> Result<(), Error> {
        (self.phase)(phase.name())?;
        self.at(phase, 0.0);
        Ok(())
    }

    /// `within` of `phase` done, named by the phase.
    fn at(&mut self, phase: Phase, within: f64) {
        self.at_saying(phase, within, phase.name());
    }

    fn at_saying(&mut self, phase: Phase, within: f64, message: &str) {
        let (start, end) = self.spans[phase as usize];
        let fraction =
            self.base + (1.0 - self.base) * (start + within.clamp(0.0, 1.0) * (end - start));
        let percent = ((fraction * 100.0).floor() as u32).clamp(self.percent, 99);
        self.report(percent, message);
    }

    fn report(&mut self, percent: u32, message: &str) {
        if percent != self.percent || message != self.message {
            self.percent = percent;
            message.clone_into(&mut self.message);
            (self.progress)(f64::from(percent) / 100.0, message);
        }
    }

    /// An encode of a JPEG: its rows, then an optimised encode's passes, which begin the
    /// `optimising` phase on the board. A cancel there is the encoder's to see, at its next check.
    fn jpeg(&mut self, at: JpegProgress) {
        match at {
            JpegProgress::Rows(fraction) => self.at(Phase::Encoding, fraction),
            JpegProgress::Optimising(fraction) => {
                if !self.optimising {
                    self.optimising = true;
                    let _ = (self.phase)(Phase::Optimising.name());
                }
                self.at(Phase::Optimising, fraction);
            }
        }
    }

    /// A size limit's search, its attempt in the message.
    fn limiting(&mut self, at: LimitProgress) {
        let message = format!("{} {}/{MAX_ATTEMPTS}", Phase::Limiting.name(), at.attempt);
        self.at_saying(Phase::Limiting, at.fraction, &message);
    }

    /// The file is published: the whole export is done.
    fn done(&mut self) {
        let message = self.message.clone();
        self.report(100, &message);
    }
}

/// What every encode of one export shares: its control, the original's metadata at the export's
/// level and its settings.
struct Encoding<'a> {
    control: &'a JobControl,
    /// The fields the export's metadata level keeps.
    capture: &'a CaptureMetadata,
    settings: &'a ExportSettings,
}

impl Encoding<'_> {
    /// The EXIF payload of a `size` output, JPEG's APP1 and WebP's EXIF chunk: none at level
    /// none, and otherwise the level's fields with the structural ones.
    fn exif(&self, (width, height): (u32, u32)) -> Option<Vec<u8>> {
        self.settings
            .metadata
            .writes_exif()
            .then(|| self.capture.exif_payload(width, height))
    }

    /// A TIFF's metadata for its directories: none at level none.
    fn tiff_metadata(&self) -> Option<&CaptureMetadata> {
        self.settings.metadata.writes_exif().then_some(self.capture)
    }

    fn cancel(&self) -> Result<(), Error> {
        self.control.checkpoint()
    }
}

/// An encoded export before it is published: its temporary file, streamed into as it was encoded,
/// or a held format's bytes, which are written to one when the export is `writing`.
enum Encoded {
    Staged(Staged),
    Bytes(Vec<u8>),
}

/// What a stream through the tile service left: the export encoded (a streamed format) or its
/// output frame (a held one), with the renderer that drew its bands; or why the reference is to
/// render the export instead, nothing of the stream kept.
enum Streamed {
    Staged(Staged, Renderer),
    Frame(Vec<u8>, Renderer),
    /// The service would not stream the export at all.
    Refused(RendererReason),
    /// The stream stopped part-way.
    Stopped(RendererReason),
}

/// One export's work once its entry is frozen, shared by `export.write`'s job and each photograph of
/// `batch.export`: render the entry's output with `settings`, encode it, write it into a temporary
/// file beside `destination` and publish that under the destination's name without replacing
/// anything, answering `{path, bytes, width, height, format, quality?, optimised, metadata,
/// renderer}` with the renderer and the quality used ([`Written`]).
///
/// Unless the export asked for the reference renderer (`reference`) or the host has no GPU
/// provider, `tiles` renders the output in bands: a streamed format encodes them as they arrive and
/// a held one assembles its frame from them. When the service says why it cannot, before the stream
/// or during it, whatever the stream left is dropped and the reference renderer renders the whole
/// output instead, so one file is never two renderers' pixels.
///
/// `phase` is called as each [`Phase`] begins and stops the export where it answers an error, as a
/// cancel that arrived meanwhile does; `progress` is told the whole export's fraction in whole
/// percent, never decreasing, with the phase as its message. A cancel of `control` stops the stream
/// between tiles, the render within a row or chunk, a JPEG or TIFF encoder within about 1% of the
/// rows, an optimised JPEG between and within its passes, WebP at libwebp's reports and a limit
/// search between its attempts; a failure or a cancel before the publish drops the temporary file,
/// which removes it, and a held format stages none until it is writing.
#[allow(clippy::too_many_arguments)]
pub(super) fn write(
    plan: ExportPlan,
    destination: &Destination,
    settings: &ExportSettings,
    reference: bool,
    tiles: &dyn TileService,
    control: &JobControl,
    phase: &dyn Fn(&'static str) -> Result<(), Error>,
    progress: &mut dyn FnMut(f64, &str),
) -> Result<Written, Error> {
    let ExportPlan {
        identity,
        evaluation,
        capture,
    } = plan;
    let capture = capture.at_level(settings.metadata);
    let output = settings.output_spec((identity.width, identity.height));
    // The reference's resampling phase also sharpens a full-size output.
    let resampled = !output.is_full(identity.width, identity.height) || output.sharpen.is_some();
    let route = Route::of(settings);
    let encoding = Encoding {
        control,
        capture: &capture,
        settings,
    };
    let size = (output.width, output.height);
    // Which renderer renders the file, and why the reference does: the reference when the export
    // asked for it or the host has no GPU provider, whose only renderer it is; the GPU otherwise,
    // unless the service says why it cannot.
    let gpu = !reference && tiles.status() != TileStatus::Reference(None);
    // A paged source has no whole frame: the reference renders it in bands, as the GPU streams it.
    let paged = evaluation.source().paged().is_some();
    let mut report = Report::new(phase, progress, route.weights(gpu || paged, resampled));
    report.enter(Phase::Rendering)?;
    let reason = if reference {
        Some(RendererReason::Requested)
    } else if !gpu {
        None
    } else {
        let streamed = match tiles.stream(&evaluation, output, control.render_cancel()) {
            Ok(stream) => streamed(stream, &route, destination, &encoding, &mut report)?,
            Err(fallback) => {
                report.restart(route.weights(paged, resampled));
                Streamed::Refused(RendererReason::from(&fallback))
            }
        };
        match streamed {
            Streamed::Staged(staged, renderer) => {
                drop(evaluation);
                let encoded = (Encoded::Staged(staged), settings.quality());
                return finish(destination, encoded, size, &encoding, renderer, &mut report);
            }
            Streamed::Frame(frame, renderer) => {
                drop(evaluation);
                let Route::Held(held) = &route else {
                    return Err(Error::internal("a streamed export assembled a frame"));
                };
                let encoded = encode_held(&frame, size, held, &encoding, &mut report)?;
                drop(frame);
                return finish(destination, encoded, size, &encoding, renderer, &mut report);
            }
            Streamed::Refused(reason) => Some(reason),
            // Rendered again from nothing by the reference, which begins with its own render.
            Streamed::Stopped(reason) => {
                report.restart(route.weights(paged, resampled));
                report.enter(Phase::Rendering)?;
                Some(reason)
            }
        }
    };
    if paged {
        return banded(
            evaluation,
            output,
            &route,
            reason,
            destination,
            &encoding,
            &mut report,
        );
    }
    let renderer = reason.map_or(Renderer::headless(), Renderer::reference);
    // The reference's output, from the compilation the plan made, inside the evaluated-frame
    // limit; a full-size 8-bit output is the render's own frame, which the encoder reads in place.
    // The source, the recipe and its artifacts are released with the evaluation.
    let frame = reference_output_phased(
        &evaluation,
        output,
        identity.snapshot_id.clone(),
        control.render_cancel(),
        &mut || report.enter(Phase::Resampling),
    )?;
    drop(evaluation);
    let encoded = encode_frame(&frame, &route, destination, &encoding, &mut report)?;
    drop(frame);
    finish(destination, encoded, size, &encoding, renderer, &mut report)
}

/// A paged source's export by the reference (`docs/design/panorama-merge.md`, "The reference on
/// paged sources"), when the export asked for it, the host has no GPU provider or the GPU's stream
/// said why it could not draw it (`reason`, as for a whole frame): the output in full-width bands
/// from windows of the source's level 0 ([`Banded`]), after a first pass over the stages a
/// whole-stage statistic or a repair reads. A streamed format encodes each band as it is rendered,
/// so neither the source nor the output is ever held whole; a held format assembles its output
/// frame from the bands, as it does from the GPU's. Phases, progress, cancellation and the staged
/// file behave as they do for a whole frame's export.
#[allow(clippy::too_many_arguments)]
fn banded(
    evaluation: crate::Evaluation,
    output: OutputSpec,
    route: &Route,
    reason: Option<RendererReason>,
    destination: &Destination,
    encoding: &Encoding<'_>,
    report: &mut Report<'_>,
) -> Result<Written, Error> {
    let renderer = reason.map_or(Renderer::headless(), Renderer::reference);
    let cancel = encoding.control.render_cancel();
    let paged = evaluation
        .source()
        .paged()
        .ok_or_else(|| Error::internal("a banded export of a source that is not paged"))?;
    let banded = Banded::new(
        paged.source().as_ref(),
        evaluation.compiled()?,
        evaluation.context(),
        cancel,
    )?;
    let size = (output.width, output.height);
    let bands = output_bands(&banded, output, cancel)?;
    let encoded = match route {
        Route::Streamed(how) => {
            let staged = encode_streamed(bands, size, how, destination, encoding, report)?;
            (Encoded::Staged(staged), encoding.settings.quality())
        }
        Route::Held(held) => {
            let frame = assemble(bands, size, encoding, report)?;
            encode_held(&frame, size, held, encoding, report)?
        }
    };
    drop(banded);
    drop(evaluation);
    finish(destination, encoded, size, encoding, renderer, report)
}

/// The rows of an output, borrowed as an encoder reads them: a GPU band's own pixels, or the
/// reference's whole frame.
trait Rows {
    fn rows(&self) -> PixelRows<'_>;
}

impl Rows for Pixels {
    fn rows(&self) -> PixelRows<'_> {
        Pixels::rows(self)
    }
}

impl Rows for PixelRows<'_> {
    fn rows(&self) -> PixelRows<'_> {
        *self
    }
}

/// 8-bit rows as JPEG's encoder reads them, refused before it if they are not.
struct Eight<P>(P);

impl<P: Rows> AsRef<[u8]> for Eight<P> {
    fn as_ref(&self) -> &[u8] {
        match self.0.rows() {
            PixelRows::Eight(rgba) => rgba,
            PixelRows::Sixteen(_) => &[],
        }
    }
}

/// Encode a streamed format's `bands` of the `size` output into a new temporary file as they
/// arrive, in the phase `encoding` (and `optimising` for an optimised JPEG's passes).
fn encode_streamed<P: Rows>(
    bands: impl Iterator<Item = Result<(u32, P), Error>>,
    size: (u32, u32),
    how: &Streaming,
    destination: &Destination,
    encoding: &Encoding<'_>,
    report: &mut Report<'_>,
) -> Result<Staged, Error> {
    let mut staged = destination.stage()?;
    report.enter(Phase::Encoding)?;
    let cancel = || encoding.cancel();
    match how {
        Streaming::Jpeg(options) => {
            let exif = encoding.exif(size);
            let bands = bands.map(|band| {
                band.and_then(|(rows, pixels)| match pixels.rows() {
                    PixelRows::Eight(_) => Ok((rows, Eight(pixels))),
                    PixelRows::Sixteen(_) => {
                        Err(Error::internal("a 16-bit band reached the JPEG encoder"))
                    }
                })
            });
            encode::encode_jpeg_stages(
                &mut staged,
                size,
                bands,
                options,
                exif.as_deref(),
                encoding.settings.pixels_per_inch,
                &mut |at| report.jpeg(at),
                &cancel,
            )?;
        }
        Streaming::Tiff(options) => {
            let mut writer = TiffWriter::start(
                &mut staged,
                size.0,
                size.1,
                options,
                encode::srgb_profile(),
                encoding.tiff_metadata(),
            )?;
            for band in bands {
                let (rows, pixels) = band?;
                writer.write_rows(
                    rows,
                    pixels.rows(),
                    &mut |fraction| report.at(Phase::Encoding, fraction),
                    &cancel,
                )?;
            }
            writer.finish()?;
        }
    }
    Ok(staged)
}

/// Encode a held format's 8-bit `rgba` frame of the `size` output into memory: a size-limited
/// JPEG's search, in the phase `limiting`, a WebP in `encoding`, a size-limited WebP's search in
/// `limiting`. Answers the file and the quality it was encoded at.
fn encode_held(
    rgba: &[u8],
    size: (u32, u32),
    held: &Held,
    encoding: &Encoding<'_>,
    report: &mut Report<'_>,
) -> Result<(Encoded, Option<u8>), Error> {
    let cancel = || encoding.cancel();
    let exif = encoding.exif(size);
    let (bytes, quality) = match *held {
        Held::Jpeg { options, limit } => {
            report.enter(Phase::Limiting)?;
            let limited = encode::encode_jpeg_limited(
                size,
                rgba,
                &options,
                limit,
                exif.as_deref(),
                encoding.settings.pixels_per_inch,
                &mut |at| report.limiting(at),
                &cancel,
            )?;
            (limited.bytes, limited.quality)
        }
        Held::Webp {
            options,
            limit: None,
        } => {
            report.enter(Phase::Encoding)?;
            let bytes = webp::encode_webp(
                size.0,
                size.1,
                rgba,
                &options,
                encode::srgb_profile(),
                exif.as_deref(),
                &mut |fraction| report.at(Phase::Encoding, fraction),
                &cancel,
            )?;
            (bytes, options.quality)
        }
        Held::Webp {
            options,
            limit: Some(limit),
        } => {
            report.enter(Phase::Limiting)?;
            let mut attempt = |quality: u8, fraction: &mut dyn FnMut(f64)| {
                let options = WebpOptions { quality, ..options };
                let bytes = webp::encode_webp(
                    size.0,
                    size.1,
                    rgba,
                    &options,
                    encode::srgb_profile(),
                    exif.as_deref(),
                    fraction,
                    &cancel,
                )?;
                // A file past the limit is dropped now, before the next attempt.
                Ok((bytes.len() as u64 <= limit).then_some(bytes))
            };
            let limited = limit::search(
                limit,
                options.quality,
                &mut attempt,
                &mut |at| report.limiting(at),
                &cancel,
            )?;
            (limited.bytes, limited.quality)
        }
    };
    Ok((Encoded::Bytes(bytes), Some(quality)))
}

/// Encode the reference's whole output `frame`: streamed formats as one band, read in place, and
/// held ones from its 8-bit rows, also in place.
fn encode_frame(
    frame: &OutputFrame,
    route: &Route,
    destination: &Destination,
    encoding: &Encoding<'_>,
    report: &mut Report<'_>,
) -> Result<(Encoded, Option<u8>), Error> {
    let size = (frame.width, frame.height);
    match route {
        Route::Streamed(how) => {
            let whole = std::iter::once(Ok((frame.height, frame.rows())));
            let staged = encode_streamed(whole, size, how, destination, encoding, report)?;
            Ok((Encoded::Staged(staged), encoding.settings.quality()))
        }
        Route::Held(held) => {
            let PixelRows::Eight(rgba) = frame.rows() else {
                return Err(Error::internal("a 16-bit frame reached an 8-bit encoder"));
            };
            encode_held(rgba, size, held, encoding, report)
        }
    }
}

/// Take `stream`'s bands of the output as they arrive: a streamed format encodes them into a new
/// temporary file; a held one assembles its 8-bit frame from them, in the phase `rendering`, each
/// band copied once into the frame and released. A cancellation, an encoder's own failure and an
/// error of the stream's own end the export with it; a stream its provider stopped drawing for a
/// reason the reference renders instead is dropped with whatever it left, the temporary file
/// included, and that reason is answered.
fn streamed(
    mut stream: BandStream,
    route: &Route,
    destination: &Destination,
    encoding: &Encoding<'_>,
    report: &mut Report<'_>,
) -> Result<Streamed, Error> {
    let output = stream.output();
    let size = (output.width, output.height);
    let result = match route {
        Route::Streamed(how) => {
            let bands = stream
                .by_ref()
                .map(|band| band.map(|band| (band.rows, band.pixels)));
            encode_streamed(bands, size, how, destination, encoding, report).map(Ok)
        }
        Route::Held(_) => {
            let bands = stream
                .by_ref()
                .map(|band| band.map(|band| (band.rows, band.pixels)));
            assemble(bands, size, encoding, report).map(Err)
        }
    };
    match result {
        Ok(Ok(staged)) => Ok(Streamed::Staged(staged, stream.answered().into())),
        Ok(Err(frame)) => Ok(Streamed::Frame(frame, stream.answered().into())),
        Err(error) => {
            // Nothing of the stream is kept: the provider stops drawing once it is dropped.
            encoding.cancel()?;
            let reason = stream.fallback().map(RendererReason::from);
            drop(stream);
            reason.map(Streamed::Stopped).ok_or(error)
        }
    }
}

/// A held format's 8-bit `size` frame from `bands` of its output, the GPU's or the banded
/// reference's, allocated once at its size. It has no cap (owner, 2026-10-08 and 2026-10-13):
/// `export.plan` reports the memory in advance and warns above 24 MP, and an allocation the host
/// cannot make is refused as `resource-limit`, never an abort.
fn assemble(
    bands: impl Iterator<Item = Result<(u32, Pixels), Error>>,
    (width, height): (u32, u32),
    encoding: &Encoding<'_>,
    report: &mut Report<'_>,
) -> Result<Vec<u8>, Error> {
    let length = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| Error::resource_limit("the export's frame is not addressable"))?;
    let mut frame = Vec::new();
    frame.try_reserve_exact(length).map_err(|_| {
        Error::resource_limit(format!(
            "the export's {width} \u{d7} {height} frame, {} MB, could not be allocated",
            length.div_ceil(1_000_000)
        ))
    })?;
    for band in bands {
        let (_, pixels) = band?;
        encoding.cancel()?;
        let Pixels::Eight(rgba) = pixels else {
            return Err(Error::internal(
                "a 16-bit band reached a reader of 8-bit codes",
            ));
        };
        frame.extend_from_slice(&rgba);
        report.at(Phase::Rendering, frame.len() as f64 / length.max(1) as f64);
    }
    Ok(frame)
}

/// Publish the encoded `size` output under the destination's name, in the phase `writing`, writing
/// a held format's bytes to a new temporary file first, and answer the written file: its path,
/// length, size, format, the quality used, whether it was optimised, the EXIF fields kept and the
/// renderer that rendered it.
fn finish(
    destination: &Destination,
    (encoded, quality): (Encoded, Option<u8>),
    (width, height): (u32, u32),
    encoding: &Encoding<'_>,
    renderer: Renderer,
    report: &mut Report<'_>,
) -> Result<Written, Error> {
    report.enter(Phase::Writing)?;
    let staged = match encoded {
        Encoded::Staged(staged) => staged,
        Encoded::Bytes(bytes) => {
            let mut staged = destination.stage()?;
            staged.write_all(&bytes).map_err(|error| {
                Error::file_access(format!(
                    "writing {} failed: {error}",
                    destination.path().display()
                ))
            })?;
            staged
        }
    };
    let bytes = staged.publish()?;
    report.done();
    let settings = encoding.settings;
    // The level's fields: none at level none, whose capture holds none.
    let mut result = json!({
        "path": destination.path(),
        "bytes": bytes,
        "width": width,
        "height": height,
        "format": settings.format().as_str(),
        "optimised": settings.optimise,
        "metadata": encoding.capture.field_names(),
        "renderer": renderer,
    });
    if let Some(quality) = quality {
        result["quality"] = json!(quality);
    }
    Ok(Written {
        result,
        renderer,
        quality,
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
