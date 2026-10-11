//! The HDR merge job (`docs/design/hdr-merge.md`, "Jobs, commands and client state" and "Bounds,
//! recovery and regeneration"): what the merge lane's worker runs for one `merge.create` or
//! `merge.rerun`, off the catalog owner and off the source worker, composing the shared verified
//! RAW development, the numerical provider ([`crate::merge::hdr`]) and durable derived storage
//! ([`DerivedSourceWriter`]). The owner commits what it answers ([`Merged`]) in one publication
//! transaction (`crate::library::merge`).
//!
//! In order, cancellable between every step:
//!
//! 1. **Reading.** Each input, the planned reference first, is checked against the signature the
//!    request captured, read once (its SHA-256 streamed) and developed through the shared
//!    verified RAW path at the reference's as-shot white balance, with its required embedded
//!    corrections, into float32 linear sRGB/D65 ([`Developer`]). Its rows are reduced to its
//!    [`AnalysisProxy`] and written, with each pixel's [`SensorLevel`], to the job's spool file
//!    one band at a time, and then the development is dropped: one development is held at a
//!    time, never N.
//! 2. **Aligning.** The decoded bracket is validated, its exposures resolved (fitting them from
//!    aligned proxy samples when the metadata cannot), registered, its common coverage computed
//!    and its moving regions found.
//! 3. **Merging.** The result is written chunk by chunk ([`DerivedSourceWriter::write_with`]): for
//!    each 1024 × 1024 chunk of the coverage, every input's bounded source window is read back
//!    from its spool, resampled into reference coordinates ([`resample_rect`]), and fused with
//!    the chunk's motion selection ([`fuse_rect`]) straight into the writer's chunk buffer.
//! 4. **Saving.** The writer stages, hashes, syncs and renames every chunk and the manifest; the
//!    inputs' signatures are checked once more, and the owner's publication follows.
//!
//! **Bounds.** Scratch is accounted explicitly ([`Accounting`]) against [`SCRATCH_LIMIT`] (256
//! MiB), by phase: the analysis phase holds the proxies, their aligned copies, the fit samples and
//! the motion map; the merging phase the motion map and one chunk's worth of every input's aligned
//! samples, one source window, the chunk's motion selection and the writer's chunk and encoding.
//! A bracket whose phase would exceed the limit is refused with `resource-limit` before the
//! allocation. Beside it the job holds one admitted development (the RAW mosaic and its float
//! planes), which it does not take from the source worker's plane gate: the merge never waits on
//! a lease the active editor holds, and the editor never waits on the merge. The spool holds
//! [`SPOOL_PIXEL`] bytes a pixel an input; with the result's objects it is preflighted against the
//! per-job temporary-disk ceiling ([`TEMP_CEILING`], 16 GiB) and the volume's free space before the
//! first byte is spooled. Only this job's own spool files are removed, on every exit.
//!
//! Progress names the phases `reading`, `aligning`, `merging` and `saving` on the job's activity,
//! and its partial result reports the stage, the scratch it holds and its peak, the temporary
//! bytes spooled and the development's bytes.
use crate::{
    AssetId, Error, ErrorKind, LinearImage,
    artifacts::derived_source::{ChunkRect, DerivedSourceWriter, PublishedSource, ReferenceCrop},
    atomic_file::file_error,
    catalog_types::{
        FileSignature, HdrDevelopment, HdrParameters, HdrResolution, HeaderMetadata,
        MERGE_PROVENANCE_FORMAT, MergeInput, MergeKind, MergeProvenance, MergeProvider, Volume,
    },
    jobs::JobControl,
    merge::hdr::{
        AlignedProxies, AlignedRect, AnalysisProxy, ExposureResolution, ExposureSettings,
        FitSamples, FrameMetadata, FrameRect, FusedRect, FusionDiagnostics, MotionDiagnostics,
        MotionMap, MotionRect, PixelRect, ProxyBuilder, Recording, Refusal, Registration,
        ResolvedExposures, SensorClipModel, SensorLayout, SensorLevel, SourceWindow,
        common_coverage, detect_motion, fuse_rect, register, resample_rect, resolve_exposures,
        source_window, validate_bracket,
    },
};
use serde::Serialize;
use serde_json::json;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const MIB: u64 = 1024 * 1024;
/// The scratch one merge may hold beside its one development.
pub(crate) const SCRATCH_LIMIT: u64 = 256 * MIB;
/// The provisional per-job temporary-disk ceiling: nine maximum-size float inputs plus the result.
pub(crate) const TEMP_CEILING: u64 = 16 * 1024 * MIB;
/// One spooled pixel: its three little-endian binary32 samples, then its little-endian
/// [`SensorLevel`].
pub(crate) const SPOOL_PIXEL: u64 = 14;
/// One stored result pixel: three binary32 samples.
const RESULT_PIXEL: u64 = 12;
/// Rows a development is spooled and reduced in at a time.
const BAND_ROWS: u32 = 16;

/// The HDR provider this build carries: its module and the algorithm version every result it
/// writes records, and a rerun must match.
pub(crate) const PROVIDER_MODULE: &str = "luxforge.hdr";
pub(crate) const PROVIDER_VERSION: u32 = 1;

pub(crate) fn provider() -> MergeProvider {
    MergeProvider {
        module: PROVIDER_MODULE.to_owned(),
        version: PROVIDER_VERSION,
    }
}

/// Where a job stands, as a test may hold it ([`Hooks`]) and its partial result reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "stage", content = "at", rename_all = "kebab-case")]
pub(crate) enum Stage {
    /// About to read and develop the input at this stack position.
    Reading(usize),
    /// That input is spooled and its development released.
    Spooled(usize),
    Aligning,
    /// About to fuse and write this output chunk.
    Merging(usize),
    /// The result's source is durable, before the owner publishes it.
    Saved,
}

/// What a test may arrange for one job: hold it at each [`Stage`], make the writer fail at a step,
/// or make spooling an input fail as a full disk does. Nothing outside tests.
#[derive(Clone, Default)]
pub(crate) struct Hooks {
    #[cfg(test)]
    pub stage: Option<Arc<dyn Fn(Stage) + Send + Sync>>,
    #[cfg(test)]
    pub writer_fails: Option<crate::artifacts::derived_source::Step>,
    #[cfg(test)]
    pub spool_full: Option<usize>,
}

impl Hooks {
    fn at(&self, _stage: Stage) {
        #[cfg(test)]
        if let Some(hold) = &self.stage {
            hold(_stage);
        }
    }

    fn spool_full(&self, _input: usize) -> bool {
        #[cfg(test)]
        return self.spool_full == Some(_input);
        #[cfg(not(test))]
        false
    }
}

/// The limits a job is held to; only a test lowers them or names the free space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    pub scratch: u64,
    pub temp_ceiling: u64,
    /// The free space to assume instead of asking the volume.
    pub available: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            scratch: SCRATCH_LIMIT,
            temp_ceiling: TEMP_CEILING,
            available: None,
        }
    }
}

impl Limits {
    /// Refuse `bytes` of temporary storage under `directory` beyond the ceiling or the volume's
    /// free space.
    pub(crate) fn preflight(&self, directory: &Path, bytes: u64) -> Result<(), Error> {
        if bytes > self.temp_ceiling {
            return Err(Error::resource_limit(format!(
                "the merge needs {} of temporary storage, above the {} a merge may use",
                gib(bytes),
                gib(self.temp_ceiling)
            ))
            .with_data(json!({"temp_bytes": bytes, "ceiling_bytes": self.temp_ceiling})));
        }
        let available = match self.available {
            Some(available) => available,
            None => luxforge_process::disk::available_bytes(directory)
                .map_err(|error| file_error("cannot read the free disk space", error.kind()))?,
        };
        if bytes > available {
            return Err(Error::resource_limit(format!(
                "the merge needs {} of temporary storage and the disk has {} free",
                gib(bytes),
                gib(available)
            ))
            .with_data(json!({"temp_bytes": bytes, "available_bytes": available})));
        }
        Ok(())
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * MIB as f64))
}

/// The temporary storage a merge of `inputs` frames of `width × height` needs: every input's
/// spool and the result's objects.
pub(crate) fn temp_bytes(inputs: usize, width: u32, height: u32) -> u64 {
    let pixels = u64::from(width) * u64::from(height);
    inputs as u64 * pixels * SPOOL_PIXEL + pixels * RESULT_PIXEL
}

/// One input as the request captured it, in the stack's order.
#[derive(Clone, Debug)]
pub(crate) struct JobInput {
    pub path: PathBuf,
    pub signature: FileSignature,
    pub asset_id: Option<AssetId>,
    /// The fingerprint a rerun requires of it: its merge's recorded identity. None for a new
    /// merge.
    pub fingerprint: Option<String>,
    /// What the index or the catalog said of it when it was requested, which picks the reference
    /// developed first.
    pub planned: FrameMetadata,
}

/// One input read and developed ([`Developer`]): what it is, how it was developed, and its
/// upright linear frame with each pixel's clipping model. A JPEG has no frame.
pub(crate) struct Developed {
    /// Its canonical path.
    pub path: PathBuf,
    /// The SHA-256 of the bytes it was developed from.
    pub fingerprint: String,
    pub header: HeaderMetadata,
    pub place: Option<String>,
    pub volume: Volume,
    pub frame: FrameMetadata,
    pub development: Option<Development>,
}

/// A RAW input's development.
pub(crate) struct Development {
    pub gains: [f32; 3],
    pub cam_xyz: [[f32; 3]; 4],
    pub image: LinearImage,
    pub clip: SensorClipModel,
}

/// Reads and develops one input. The merge's is [`SharedDevelopment`]; a test supplies a
/// synthetic camera, since no decodable RAW bracket is committed.
pub(crate) trait Developer: Send + Sync {
    /// Read the file at `path` once and develop it at `gains`, or at its own as-shot gains when
    /// none are named.
    fn develop(
        &self,
        path: &Path,
        gains: Option<[f32; 3]>,
        control: &JobControl,
    ) -> Result<Developed, Error>;
}

/// The shared verified development every RAW preparation uses: one bounded read that streams the
/// file's SHA-256 with its signature held throughout (`crate::library::develop::read`), the RAW
/// decoder's unpacking with its embedded corrections, and [`crate::source::RawPrepared::develop`]
/// at the named gains, calibrated to linear sRGB/D65 and cropped and turned upright.
pub(crate) struct SharedDevelopment;

impl Developer for SharedDevelopment {
    fn develop(
        &self,
        path: &Path,
        gains: Option<[f32; 3]>,
        control: &JobControl,
    ) -> Result<Developed, Error> {
        use crate::{editor::ReadContent, source::RawPrepared};
        let read = crate::library::develop::read(path, true, control, &|_| {})?;
        let capture_ms = read.header.capture.as_ref().map(|time| time.instant_ms());
        let settings = settings(&read.header);
        let camera = read.header.camera.clone();
        let Some(ReadContent::Raw { sensor, capture }) = read.kept else {
            return Ok(Developed {
                frame: FrameMetadata {
                    identity: read.fingerprint.clone(),
                    make: camera.as_ref().map(|body| body.make.clone()),
                    model: camera.as_ref().map(|body| body.model.clone()),
                    recording: Recording::Jpeg,
                    width: read.width,
                    height: read.height,
                    orientation: 1,
                    exposure: settings,
                    captured_at_ms: capture_ms,
                },
                path: read.path,
                fingerprint: read.fingerprint,
                header: read.header,
                place: read.place,
                volume: read.volume,
                development: None,
            });
        };
        let metadata = sensor.metadata().clone();
        let gains = gains.unwrap_or(metadata.as_shot_gains);
        let layout = SensorLayout::of_raw(&metadata).ok_or_else(|| {
            Error::unsupported_input("HDR merging does not know this colour filter array")
        })?;
        let clip = SensorClipModel::of_raw(&metadata, gains)?;
        let prepared = RawPrepared::develop(
            sensor,
            capture,
            read.fingerprint.clone(),
            gains,
            control.flag(),
        )
        .map_err(|error| {
            if control.is_cancelled() {
                control.cancelled_error()
            } else {
                error
            }
        })?;
        let image = prepared
            .linear
            .ok_or_else(|| Error::internal("a RAW development held no frame"))?;
        Ok(Developed {
            frame: FrameMetadata {
                identity: read.fingerprint.clone(),
                make: Some(
                    camera
                        .as_ref()
                        .map_or(metadata.make.clone(), |body| body.make.clone()),
                ),
                model: Some(
                    camera
                        .as_ref()
                        .map_or(metadata.model.clone(), |body| body.model.clone()),
                ),
                recording: Recording::Raw {
                    mode: metadata.mode.id().to_owned(),
                    layout,
                },
                width: image.width(),
                height: image.height(),
                orientation: metadata.exif_orientation,
                exposure: settings,
                captured_at_ms: capture_ms,
            },
            path: read.path,
            fingerprint: read.fingerprint,
            header: read.header,
            place: read.place,
            volume: read.volume,
            development: Some(Development {
                gains,
                cam_xyz: metadata.cam_xyz,
                image,
                clip,
            }),
        })
    }
}

/// A header's exposure as the merge reads it.
pub(crate) fn settings(header: &HeaderMetadata) -> ExposureSettings {
    let exposure = &header.exposure;
    ExposureSettings {
        shutter_s: exposure.time_s.map(f64::from),
        f_number: exposure.f_number.map(f64::from),
        iso: exposure.iso,
        bias_ev: exposure.bias_ev.map(f64::from),
        focal_mm: exposure.focal_mm.map(f64::from),
    }
}

/// Everything one job needs, moved to the worker.
pub(crate) struct JobSpec {
    /// The inputs in the stack's order; every index in `parameters` is a position here.
    pub inputs: Vec<JobInput>,
    pub parameters: HdrParameters,
    pub provider: MergeProvider,
    /// The job's own spool directory, which it creates and empties.
    pub spool: PathBuf,
    pub writer: DerivedSourceWriter,
    pub developer: Arc<dyn Developer>,
    pub limits: Limits,
    pub hooks: Hooks,
}

/// What a job answers for the owner to publish: the provenance, the durable source, and the
/// reference's header, place and the inputs' volumes, the result's name and what the job held.
#[derive(Debug)]
pub(crate) struct Merged {
    pub provenance: MergeProvenance,
    pub source: PublishedSource,
    pub header: HeaderMetadata,
    pub place: Option<String>,
    pub volumes: Vec<Volume>,
    pub file_name: String,
    pub accounting: Accounting,
}

/// The job's explicit accounting: the scratch it holds now and at most, the temporary bytes it
/// spooled and the largest development it held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Accounting {
    pub scratch_bytes: u64,
    pub scratch_peak_bytes: u64,
    pub scratch_limit_bytes: u64,
    pub temp_bytes: u64,
    pub development_bytes: u64,
}

impl Accounting {
    /// Take `bytes` of scratch for `what`, refused with `resource-limit` past the limit before
    /// anything is allocated.
    fn take(&mut self, bytes: u64, what: &str) -> Result<(), Error> {
        let total = self.scratch_bytes.saturating_add(bytes);
        if total > self.scratch_limit_bytes {
            return Err(Error::resource_limit(format!(
                "merging this bracket needs {} MiB of scratch for {what}, above the {} MiB a \
                 merge may hold",
                total.div_ceil(MIB),
                self.scratch_limit_bytes / MIB
            ))
            .with_data(json!({
                "scratch_bytes": total,
                "scratch_limit_bytes": self.scratch_limit_bytes,
            })));
        }
        self.scratch_bytes = total;
        self.scratch_peak_bytes = self.scratch_peak_bytes.max(total);
        Ok(())
    }

    fn give(&mut self, bytes: u64) {
        self.scratch_bytes = self.scratch_bytes.saturating_sub(bytes);
    }
}

/// The job's spool files, removed as it ends however it ends. The directory and anything else in
/// it (the owner's request marker) are not this job's to remove.
struct Spool {
    files: Vec<PathBuf>,
}

impl Spool {
    fn new(directory: &Path, inputs: usize) -> Result<Self, Error> {
        fs::create_dir_all(directory).map_err(|error| {
            file_error("cannot create the merge's working folder", error.kind())
        })?;
        Ok(Self {
            files: (0..inputs)
                .map(|input| directory.join(spool_name(input)))
                .collect(),
        })
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
    }
}

/// The spool file of the input at stack position `input`.
pub(crate) fn spool_name(input: usize) -> String {
    format!("input-{input}.spool")
}

/// Whether `name` is a spool file's: what an orphan sweep removes.
pub(crate) fn is_spool(name: &str) -> bool {
    name.starts_with("input-") && name.ends_with(".spool")
}

/// One input's analysis results, kept after its development is dropped.
struct Analysed {
    developed: Developed,
    cam_xyz: [[f32; 3]; 4],
    proxy: AnalysisProxy,
}

impl JobSpec {
    /// Run the job: see the module documentation.
    pub(crate) fn run(self, control: &JobControl) -> Result<Merged, Error> {
        let n = self.inputs.len();
        let spool = Spool::new(&self.spool, n)?;
        let mut accounting = Accounting {
            scratch_limit_bytes: self.limits.scratch,
            ..Accounting::default()
        };
        let cancel = control.render_cancel();

        // Reading: the planned reference first, so its as-shot white balance develops the rest.
        control.set_phase("reading");
        let first = self.planned_reference();
        let mut order: Vec<usize> = (0..n).collect();
        order.retain(|&input| input != first);
        order.insert(0, first);
        let mut analysed: Vec<Option<Analysed>> = (0..n).map(|_| None).collect();
        let mut gains = None;
        let mut proxy_bytes = 0;
        for (done, &input) in order.iter().enumerate() {
            control.checkpoint()?;
            report(control, Stage::Reading(input), &accounting);
            self.hooks.at(Stage::Reading(input));
            control.set_progress(
                Some(0.5 * done as f64 / n as f64),
                &format!("Reading {} of {n}", done + 1),
            );
            let item = &self.inputs[input];
            self.check_signature(item)?;
            let mut developed = self.developer.develop(&item.path, gains, control)?;
            if let Some(expected) = &item.fingerprint
                && *expected != developed.fingerprint
            {
                return Err(Error::conflict(format!(
                    "{} is not the file this merge recorded; its bytes differ",
                    file_name(&item.path)
                ))
                .with_data(json!({"input": input, "path": item.path})));
            }
            let Some(development) = developed.development.take() else {
                return Err(Refusal::RawOnly { input }.into());
            };
            gains.get_or_insert(development.gains);
            let (width, height) = (development.image.width(), development.image.height());
            if done == 0 {
                // The bracket's size is known now: preflight its temporary storage and its
                // scratch before the first byte is spooled.
                self.limits
                    .preflight(&self.spool, temp_bytes(n, width, height))?;
                proxy_bytes = AnalysisProxy::bytes_for(width, height) as u64;
                accounting.take(n as u64 * proxy_bytes, "the analysis proxies")?;
                analysis_fits(&accounting, n, proxy_bytes)?;
                fusion_fits(&accounting, n, width, height)?;
            }
            accounting.development_bytes = accounting
                .development_bytes
                .max(development.image.plane_bytes());
            let band = band_bytes(width);
            accounting.take(band, "a development band")?;
            report(control, Stage::Reading(input), &accounting);
            let proxy = self
                .spool_input(
                    &spool.files[input],
                    input,
                    &development,
                    control,
                    &mut accounting,
                )
                .inspect_err(|_| accounting.give(band))?;
            accounting.give(band);
            let cam_xyz = development.cam_xyz;
            drop(development);
            report(control, Stage::Spooled(input), &accounting);
            self.hooks.at(Stage::Spooled(input));
            analysed[input] = Some(Analysed {
                developed,
                cam_xyz,
                proxy,
            });
        }
        let analysed: Vec<Analysed> = analysed.into_iter().flatten().collect();
        let gains = gains.ok_or_else(|| Error::internal("a merge developed no input"))?;

        // Aligning: validate the decoded bracket, resolve its exposures, register, cover, and
        // find what moved.
        control.checkpoint()?;
        control.set_phase("aligning");
        report(control, Stage::Aligning, &accounting);
        self.hooks.at(Stage::Aligning);
        control.set_progress(Some(0.5), "Aligning");
        let frames: Vec<FrameMetadata> = analysed
            .iter()
            .map(|input| input.developed.frame.clone())
            .collect();
        validate_bracket(&frames)?;
        let (width, height) = (frames[0].width, frames[0].height);
        let proxies: Vec<AnalysisProxy> =
            analysed.iter().map(|input| input.proxy.clone()).collect();
        let mode = self.parameters.alignment.alignment;
        let (exposures, registration) = match resolve_exposures(&frames, &self.parameters.exposure)?
        {
            ExposureResolution::Resolved(exposures) => {
                let registration = register(&frames, &proxies, exposures.reference, mode, cancel)?;
                (exposures, registration)
            }
            ExposureResolution::NeedsFit(plan) => {
                let registration = register(&frames, &proxies, plan.reference(), mode, cancel)?;
                accounting.take(n as u64 * proxy_bytes, "the aligned proxies")?;
                let aligned = AlignedProxies::new(&registration, &proxies, cancel)?;
                let samples_bytes = (crate::merge::hdr::exposure::MAX_FIT_SAMPLES
                    * n
                    * (3 * size_of::<f32>() + size_of::<SensorLevel>()))
                    as u64;
                accounting.take(samples_bytes, "the exposure fit")?;
                let mut samples = FitSamples::new(n);
                aligned.gather_fit_samples(&mut samples)?;
                drop(aligned);
                let exposures = plan.complete(&samples)?;
                drop(samples);
                accounting.give(samples_bytes + n as u64 * proxy_bytes);
                let registration = if exposures.reference == registration.reference {
                    registration
                } else {
                    // The fit moved a provisional reference: align on the final one.
                    control.checkpoint()?;
                    register(&frames, &proxies, exposures.reference, mode, cancel)?
                };
                (exposures, registration)
            }
        };
        let transforms = registration.transforms();
        let coverage = common_coverage(width, height, &transforms, cancel)?;
        accounting.take(n as u64 * proxy_bytes, "the aligned proxies")?;
        let aligned = AlignedProxies::new(&registration, &proxies, cancel)?;
        // The proxies are not needed once aligned on the final reference.
        drop(proxies);
        let cam_xyz = analysed[exposures.reference].cam_xyz;
        let analysed: Vec<Developed> = analysed.into_iter().map(|input| input.developed).collect();
        accounting.give(n as u64 * proxy_bytes);
        let motion_bytes = proxy_bytes;
        accounting.take(motion_bytes, "the motion map")?;
        let motion = detect_motion(
            &aligned,
            &exposures,
            self.parameters.alignment.deghost,
            cancel,
        )?;
        drop(aligned);
        accounting.give(n as u64 * proxy_bytes);
        report(control, Stage::Aligning, &accounting);

        // Merging: one chunk of the coverage at a time, every input's window read back from its
        // spool.
        control.checkpoint()?;
        control.set_phase("merging");
        let result = self.fuse(
            &spool,
            &exposures,
            &registration,
            coverage.rect,
            (width, height),
            motion.as_ref(),
            control,
            &mut accounting,
        )?;
        let (source, fusion) = result;
        accounting.give(motion_bytes);

        // Saving: the source is durable; the inputs must still be the files that were read.
        control.checkpoint()?;
        for input in &self.inputs {
            self.check_signature(input)?;
        }
        report(control, Stage::Saved, &accounting);
        self.hooks.at(Stage::Saved);
        control.set_progress(Some(1.0), "Saving");
        drop(spool);
        accounting.temp_bytes = 0;
        let reference = exposures.reference;
        let reference_read = &analysed[reference];
        let provenance = MergeProvenance {
            format: MERGE_PROVENANCE_FORMAT,
            kind: MergeKind::Hdr,
            provider: self.provider.clone(),
            inputs: self
                .inputs
                .iter()
                .zip(&analysed)
                .map(|(input, developed)| MergeInput {
                    path: input.path.clone(),
                    volume_id: developed.volume.id.clone(),
                    signature: input.signature,
                    asset_id: input.asset_id.clone(),
                    frame: developed.frame.clone(),
                })
                .collect(),
            parameters: self.parameters.clone(),
            resolved: HdrResolution {
                development: HdrDevelopment {
                    white_balance: gains,
                    cam_xyz,
                },
                exposures,
                registration,
                coverage,
                motion: motion
                    .as_ref()
                    .map_or_else(MotionDiagnostics::default, MotionMap::diagnostics),
                fusion,
            },
        };
        provenance.validate()?;
        let mut volumes: Vec<Volume> = Vec::with_capacity(n);
        for developed in &analysed {
            if !volumes
                .iter()
                .any(|volume| volume.id == developed.volume.id)
            {
                volumes.push(developed.volume.clone());
            }
        }
        Ok(Merged {
            file_name: result_name(&reference_read.path),
            header: result_header(&reference_read.header, source.width, source.height),
            place: reference_read.place.clone(),
            volumes,
            provenance,
            source,
            accounting,
        })
    }

    /// The reference the request's own metadata picks: the requested one, else the one the
    /// header-level resolution picks, else the middle input. Developed first, its white balance is
    /// the common one.
    fn planned_reference(&self) -> usize {
        let n = self.inputs.len();
        if let Some(reference) = self.parameters.exposure.reference.filter(|&at| at < n) {
            return reference;
        }
        let planned: Vec<FrameMetadata> = self
            .inputs
            .iter()
            .map(|input| input.planned.clone())
            .collect();
        match resolve_exposures(&planned, &self.parameters.exposure) {
            Ok(ExposureResolution::Resolved(exposures)) => exposures.reference,
            Ok(ExposureResolution::NeedsFit(plan)) => plan.reference(),
            Err(_) => (n - 1) / 2,
        }
    }

    /// Refuse an input that is gone (`source-unavailable`) or not the file the request captured
    /// (`conflict`): one stat.
    fn check_signature(&self, input: &JobInput) -> Result<(), Error> {
        let name = file_name(&input.path);
        let metadata = input
            .path
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .ok_or_else(|| {
                Error::source_unavailable(format!(
                    "{name} is no longer at {}; nothing was merged",
                    input.path.display()
                ))
                .with_data(json!({"path": input.path}))
            })?;
        if !FileSignature::of(&metadata).unchanged(&input.signature) {
            return Err(Error::conflict(format!(
                "{name} changed since the merge was requested; nothing was merged"
            ))
            .with_data(json!({"path": input.path})));
        }
        Ok(())
    }

    /// Write `development`'s upright rows to `path`, [`SPOOL_PIXEL`] bytes a pixel, a band at a
    /// time, reducing each band into the input's proxy as it goes. Cancellable per band.
    fn spool_input(
        &self,
        path: &Path,
        input: usize,
        development: &Development,
        control: &JobControl,
        accounting: &mut Accounting,
    ) -> Result<AnalysisProxy, Error> {
        let (width, height) = (development.image.width(), development.image.height());
        let mut builder = ProxyBuilder::new(width, height)?;
        let reader = development.image.reader();
        let w = width as usize;
        let rows = BAND_ROWS as usize;
        let mut planes = [
            vec![0.0_f32; w * rows],
            vec![0.0_f32; w * rows],
            vec![0.0_f32; w * rows],
        ];
        let mut levels = vec![SensorLevel(0); w * rows];
        let mut bytes = Vec::with_capacity(w * rows * SPOOL_PIXEL as usize);
        let mut file = File::create(path).map_err(|error| spool_error(error.kind()))?;
        for top in (0..height).step_by(rows) {
            control.checkpoint()?;
            let band = (height - top).min(BAND_ROWS) as usize;
            let walk = reader
                .walk((0, top), (1, 0), (0, 1), width, band as u32)
                .ok_or_else(|| Error::internal("a development band lies outside its view"))?;
            reader.visit(walk, |i, rgb| {
                planes[0][i] = rgb[0];
                planes[1][i] = rgb[1];
                planes[2][i] = rgb[2];
                levels[i] = development.clip.level(rgb);
                Ok::<(), Error>(())
            })?;
            bytes.clear();
            for i in 0..band * w {
                for plane in &planes {
                    bytes.extend_from_slice(&plane[i].to_le_bytes());
                }
                bytes.extend_from_slice(&levels[i].0.to_le_bytes());
            }
            builder.push_rows(&FrameRect {
                width: w,
                height: band,
                stride: w,
                rgb: [&planes[0], &planes[1], &planes[2]],
                levels: &levels,
            })?;
            if self.hooks.spool_full(input) {
                return Err(spool_error(std::io::ErrorKind::StorageFull));
            }
            file.write_all(&bytes)
                .map_err(|error| spool_error(error.kind()))?;
            accounting.temp_bytes += bytes.len() as u64;
        }
        file.flush().map_err(|error| spool_error(error.kind()))?;
        builder.finish()
    }

    /// Fuse the coverage chunk by chunk into a durable source. Answers its record and the fusion's
    /// diagnostics.
    #[allow(clippy::too_many_arguments)]
    fn fuse(
        &self,
        spool: &Spool,
        exposures: &ResolvedExposures,
        registration: &Registration,
        coverage: PixelRect,
        (width, height): (u32, u32),
        motion: Option<&MotionMap>,
        control: &JobControl,
        accounting: &mut Accounting,
    ) -> Result<(PublishedSource, FusionDiagnostics), Error> {
        let n = self.inputs.len();
        let transforms = registration.transforms();
        let grid = crate::artifacts::derived_source::chunk_grid(coverage.width, coverage.height)?;
        let in_reference = |chunk: ChunkRect| PixelRect {
            x: coverage.x + chunk.x,
            y: coverage.y + chunk.y,
            width: chunk.width,
            height: chunk.height,
        };
        // The largest window any input's chunk reads, for the one window buffer.
        let mut window_pixels = 0_u64;
        for chunk in &grid {
            for transform in &transforms {
                let window = source_window(transform, in_reference(*chunk), width, height)
                    .ok_or_else(|| Error::internal("an empty merge chunk"))?;
                window_pixels = window_pixels.max(window.pixels());
            }
        }
        let chunk_pixels = grid
            .first()
            .map_or(0, |chunk| u64::from(chunk.width) * u64::from(chunk.height));
        let held = chunk_bytes(n, chunk_pixels, window_pixels);
        accounting.take(held, "one chunk of every input")?;
        report(control, Stage::Merging(0), accounting);
        let mut files = Vec::with_capacity(n);
        for path in &spool.files {
            files.push(File::open(path).map_err(|error| spool_error(error.kind()))?);
        }
        let mut window = Window::default();
        let mut aligned: Vec<AlignedRect> = (0..n).map(|_| AlignedRect::default()).collect();
        let mut selection = MotionRect::default();
        let mut diagnostics = FusionDiagnostics::default();
        let chunks = grid.len();
        let mut done = 0;
        let cancel = control.render_cancel();
        #[cfg(test)]
        if let Some(step) = self.hooks.writer_fails {
            crate::artifacts::derived_source::fail::at(step);
        }
        let crop = Some(ReferenceCrop {
            reference_width: width,
            reference_height: height,
            x: coverage.x,
            y: coverage.y,
        });
        let crop = crop.filter(|_| (coverage.width, coverage.height) != (width, height));
        let written = self.writer.write_with(
            coverage.width,
            coverage.height,
            crop,
            control.flag(),
            |chunk, planes| {
                control.checkpoint()?;
                report(control, Stage::Merging(done), accounting);
                self.hooks.at(Stage::Merging(done));
                let rect = in_reference(chunk);
                for (input, file) in files.iter_mut().enumerate() {
                    let bounds = source_window(&transforms[input], rect, width, height)
                        .ok_or_else(|| Error::internal("an empty merge chunk"))?;
                    window.read(file, width, bounds)?;
                    resample_rect(
                        &transforms[input],
                        &window.source(bounds),
                        width,
                        height,
                        rect,
                        &mut aligned[input],
                        cancel,
                    )?;
                }
                if let Some(motion) = motion {
                    motion.render(rect, &mut selection)?;
                }
                let pixels = chunk.width as usize * chunk.height as usize;
                let (red, rest) = planes.split_at_mut(pixels);
                let (green, blue) = rest.split_at_mut(pixels);
                let views: Vec<FrameRect<'_>> = aligned.iter().map(AlignedRect::view).collect();
                let selected = selection.view();
                let part = fuse_rect(
                    exposures,
                    &self.parameters.fusion,
                    &views,
                    motion.map(|_| &selected),
                    &mut FusedRect {
                        width: chunk.width as usize,
                        height: chunk.height as usize,
                        stride: chunk.width as usize,
                        rgb: [red, green, blue],
                        limits: None,
                    },
                )?;
                diagnostics.add(&part);
                done += 1;
                control.set_progress(
                    Some(0.6 + 0.35 * done as f64 / chunks as f64),
                    &format!("Merging {done} of {chunks}"),
                );
                if done == chunks {
                    control.set_phase("saving");
                }
                Ok(())
            },
        );
        accounting.give(held);
        let source = written?;
        Ok((source, diagnostics))
    }
}

/// Every input's bytes a band of `width` pixels holds while it is spooled: its planes, levels and
/// encoding.
fn band_bytes(width: u32) -> u64 {
    u64::from(width) * u64::from(BAND_ROWS) * (12 + 2 + SPOOL_PIXEL)
}

/// What the merging phase holds: every input's aligned chunk, one source window, the chunk's
/// motion selection and the writer's chunk samples and encoding.
fn chunk_bytes(inputs: usize, chunk_pixels: u64, window_pixels: u64) -> u64 {
    inputs as u64 * chunk_pixels * SPOOL_PIXEL
        + window_pixels * (SPOOL_PIXEL + SPOOL_PIXEL)
        + chunk_pixels * 5
        + chunk_pixels * 2 * RESULT_PIXEL
}

/// Refuse a bracket whose analysis phase would pass the limit: the proxies, their aligned copies,
/// the exposure fit's samples and the motion map.
fn analysis_fits(accounting: &Accounting, inputs: usize, proxy_bytes: u64) -> Result<(), Error> {
    let fit = (crate::merge::hdr::exposure::MAX_FIT_SAMPLES
        * inputs
        * (3 * size_of::<f32>() + size_of::<SensorLevel>())) as u64;
    let mut probe = Accounting {
        scratch_bytes: 0,
        ..*accounting
    };
    probe.take(
        2 * inputs as u64 * proxy_bytes + fit + proxy_bytes,
        "aligning the bracket",
    )
}

/// Refuse a bracket whose merging phase would pass the limit, before anything is spooled: a full
/// chunk of every input and a window of it widened by the largest rotation alignment accepts.
fn fusion_fits(
    accounting: &Accounting,
    inputs: usize,
    width: u32,
    height: u32,
) -> Result<(), Error> {
    let side = |length: u32| u64::from(length.min(crate::artifacts::derived_source::CHUNK_SIDE));
    let chunk = side(width) * side(height);
    // A 1024-pixel chunk turned by 3° and shifted reads at most about 1080 pixels a side.
    let window = (side(width) + 64) * (side(height) + 64);
    let mut probe = Accounting {
        scratch_bytes: 0,
        ..*accounting
    };
    probe.take(
        chunk_bytes(inputs, chunk, window) + AnalysisProxy::bytes_for(width, height) as u64,
        "merging the bracket",
    )
}

/// The one source window buffer: an input's window read back from its spool into planes.
#[derive(Default)]
struct Window {
    rgb: [Vec<f32>; 3],
    levels: Vec<SensorLevel>,
    row: Vec<u8>,
}

impl Window {
    /// Read `bounds` of a `width`-pixel-wide spooled frame from `file`.
    fn read(&mut self, file: &mut File, width: u32, bounds: PixelRect) -> Result<(), Error> {
        let (w, h) = (bounds.width as usize, bounds.height as usize);
        for plane in &mut self.rgb {
            plane.clear();
            plane.resize(w * h, 0.0);
        }
        self.levels.clear();
        self.levels.resize(w * h, SensorLevel(0));
        self.row.resize(w * SPOOL_PIXEL as usize, 0);
        for y in 0..h {
            let offset = ((u64::from(bounds.y) + y as u64) * u64::from(width)
                + u64::from(bounds.x))
                * SPOOL_PIXEL;
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.read_exact(&mut self.row))
                .map_err(|error| spool_error(error.kind()))?;
            for (x, pixel) in self.row.chunks_exact(SPOOL_PIXEL as usize).enumerate() {
                let i = y * w + x;
                let sample = |at: usize| {
                    f32::from_le_bytes([pixel[at], pixel[at + 1], pixel[at + 2], pixel[at + 3]])
                };
                self.rgb[0][i] = sample(0);
                self.rgb[1][i] = sample(4);
                self.rgb[2][i] = sample(8);
                self.levels[i] = SensorLevel(u16::from_le_bytes([pixel[12], pixel[13]]));
            }
        }
        Ok(())
    }

    fn source(&self, bounds: PixelRect) -> SourceWindow<'_> {
        SourceWindow {
            x: bounds.x,
            y: bounds.y,
            frame: FrameRect {
                width: bounds.width as usize,
                height: bounds.height as usize,
                stride: bounds.width as usize,
                rgb: [&self.rgb[0], &self.rgb[1], &self.rgb[2]],
                levels: &self.levels,
            },
        }
    }
}

fn spool_error(kind: std::io::ErrorKind) -> Error {
    let error = file_error("cannot use the merge's working data", kind);
    if error.kind == ErrorKind::ResourceLimit {
        Error::resource_limit("the disk holding the merge's working data is full")
    } else {
        error
    }
}

/// Publish the job's progress: its stage and what it holds, as `job.read` reports a running job's
/// result.
fn report(control: &JobControl, stage: Stage, accounting: &Accounting) {
    control.update_partial(|partial| {
        *partial = json!({
            "stage": stage,
            "accounting": accounting,
        });
    });
}

/// The result's name: the reference file's stem with "HDR".
pub(crate) fn result_name(reference: &Path) -> String {
    let stem = reference.file_stem().map_or_else(
        || "Merge".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    format!("{stem} HDR")
}

/// The result's header: the reference's capture time, place, camera, lens, aperture, ISO and focal
/// length, its own upright size, and no shutter time or bias, which belong to one capture.
pub(crate) fn result_header(reference: &HeaderMetadata, width: u32, height: u32) -> HeaderMetadata {
    HeaderMetadata {
        exposure: crate::catalog_types::Exposure {
            time_s: None,
            bias_ev: None,
            ..reference.exposure
        },
        dimensions: Some(crate::catalog_types::Dimensions { width, height }),
        orientation: crate::catalog_types::ExifOrientation::new(1),
        thumbnail: None,
        ..reference.clone()
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Remove every spool file under `root` (the merges' working folders) but `keep`'s, which a job
/// that ended without removing its own (a crash, a stopped process) left: what each job does as
/// it starts. Only the files [`is_spool`] names are removed; request markers stay.
pub(crate) fn sweep_orphans(root: &Path, keep: &Path) -> usize {
    let Ok(folders) = fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for folder in folders.flatten() {
        let path = folder.path();
        if path == keep || !path.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if is_spool(&name.to_string_lossy()) && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}
