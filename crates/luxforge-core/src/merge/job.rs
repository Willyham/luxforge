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
//!    from its spool in contiguous bands of whole rows, resampled into reference coordinates ([`resample_rect`]), and fused with
//!    the chunk's motion selection ([`fuse_rect`]) straight into the writer's chunk buffer.
//! 4. **Saving.** The writer stages, hashes, syncs and renames every chunk and the manifest; the
//!    inputs' signatures are checked once more, and the owner's publication follows.
//!
//! **Bounds.** Scratch is accounted explicitly ([`Accounting`]) against HDR's scratch limit
//! ([`MergeKind::scratch_limit`], 256 MiB), by phase: the analysis phase holds the proxies, their aligned copies, the fit samples and
//! the motion map; the merging phase the motion map and one chunk's worth of every input's aligned
//! samples, one source window and the band buffer it is read back through (at most 8 MiB, or one
//! frame row: [`READ_BAND_BYTES`]), the chunk's motion selection and the writer's chunk and
//! encoding.
//! A bracket whose phase would exceed the limit is refused with `resource-limit` before the
//! allocation. Beside it the job holds one admitted development (the RAW mosaic and its float
//! planes), which it does not take from the source worker's plane gate: the merge never waits on
//! a lease the active editor holds, and the editor never waits on the merge. The spool holds
//! [`SPOOL_PIXEL`] bytes a pixel an input; with the result's objects it is preflighted against the
//! per-job temporary-disk ceiling ([`MergeKind::temp_ceiling`], 16 GiB for HDR) and the volume's
//! free space before the first byte is spooled. Only this job's own spool files are removed, on every exit.
//!
//! Progress names the phases `reading`, `aligning`, `merging` and `saving` on the job's activity,
//! and its partial result reports the stage, the scratch it holds and its peak, the temporary
//! bytes spooled and the development's bytes.
//!
//! **Stages.** Reading and spooling ([`JobSpec::read`]), aligning ([`analyse`]) and merging and
//! saving ([`JobSpec::merge`]) are separate stages a merge preview shares
//! (`crate::merge::preview`): a merge without a preview runs all three ([`JobSpec::run`]); a
//! preview reads, with a [`ColourProxy`] built beside each input's analysis proxy, aligns and draws
//! its picture, and keeps the [`Prepared`] bracket; a merge given that preview runs only the last
//! stage on it ([`JobSpec::run_prepared`]), reading and aligning nothing again unless its options
//! are not the ones the preview last aligned. For the same options and the same development the
//! stages compute the same values in the same order, so the result is the same, bit for bit.
use crate::{
    AssetId, Error, ErrorKind, LinearImage,
    artifacts::derived_source::{ChunkRect, DerivedSourceWriter, PublishedSource, ReferenceCrop},
    atomic_file::file_error,
    catalog_types::{
        FileSignature, HdrDevelopment, HdrParameters, HdrResolution, HeaderMetadata,
        MERGE_PROVENANCE_FORMAT, MergeInput, MergeKind, MergeParameters, MergeProvenance,
        MergeProvider, MergeResolution, Volume,
    },
    jobs::JobControl,
    merge::hdr::{
        AlignedProxies, AlignedRect, AnalysisProxy, ColourProxy, ColourProxyBuilder, Coverage,
        ExposureResolution, ExposureSettings, FitSamples, FrameMetadata, FrameRect, FusedRect,
        FusionDiagnostics, MotionDiagnostics, MotionMap, MotionRect, PixelRect, ProxyBuilder,
        Recording, Refusal, Registration, ResolvedExposures, SensorClipModel, SensorLayout,
        SensorLevel, SourceWindow, common_coverage, detect_motion, fuse_rect, register,
        resample_rect, resolve_exposures, source_window, validate_bracket,
    },
};
use serde::Serialize;
use serde_json::json;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const MIB: u64 = 1024 * 1024;
/// One spooled pixel: its three little-endian binary32 samples, then its little-endian
/// [`SensorLevel`].
pub(crate) const SPOOL_PIXEL: u64 = 14;
/// One stored result pixel: three binary32 samples.
pub(crate) const RESULT_PIXEL: u64 = 12;
/// Rows a development is spooled and reduced in at a time.
const BAND_ROWS: u32 = 16;
/// The most bytes one spool read takes: a window is read back in bands of whole rows, each band
/// one contiguous span from its first row's first window pixel to its last row's last, so the
/// columns outside the window between its rows are read and skipped rather than sought past. A
/// band holds as many rows as fit (at least one, whose span is at most a frame row).
const READ_BAND_BYTES: u64 = 8 * MIB;

/// Where a job stands, as a test may hold it ([`Hooks`]) and its partial result reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "stage", content = "at", rename_all = "kebab-case")]
pub(crate) enum Stage {
    /// About to read and develop the input at this stack position.
    Reading(usize),
    /// That input is spooled and its development released.
    Spooled(usize),
    Aligning,
    /// A panorama is about to match and register its frames, and plan its stitch.
    Matching,
    /// A preview is about to draw its picture.
    Drawing,
    /// About to fuse and write this output chunk.
    Merging(usize),
    /// About to stitch and write this output chunk of a panorama.
    Stitching(usize),
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
    pub(crate) fn at(&self, _stage: Stage) {
        #[cfg(test)]
        if let Some(hold) = &self.stage {
            hold(_stage);
        }
    }

    pub(crate) fn spool_full(&self, _input: usize) -> bool {
        #[cfg(test)]
        return self.spool_full == Some(_input);
        #[cfg(not(test))]
        false
    }
}

/// The limits a job is held to, its kind's ([`Limits::of`]); only a test lowers them or names the
/// free space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    pub scratch: u64,
    pub temp_ceiling: u64,
    /// The free space to assume instead of asking the volume.
    pub available: Option<u64>,
}

impl Limits {
    /// The limits of a merge of `kind`: its scratch limit and temporary-disk ceiling.
    pub(crate) const fn of(kind: MergeKind) -> Self {
        Self {
            scratch: kind.scratch_limit(),
            temp_ceiling: kind.temp_ceiling(),
            available: None,
        }
    }

    /// Refuse `bytes` of temporary storage under `directory` beyond the ceiling, beside the `held`
    /// bytes open merge previews keep spooled on the same lane, or beyond the volume's free space.
    pub(crate) fn preflight(&self, directory: &Path, bytes: u64, held: u64) -> Result<(), Error> {
        if bytes.saturating_add(held) > self.temp_ceiling {
            let beside = if held > 0 {
                format!(" beside the {} open merge previews keep", gib(held))
            } else {
                String::new()
            };
            return Err(Error::resource_limit(format!(
                "the merge needs {} of temporary storage{beside}, above the {} a merge may use",
                gib(bytes),
                gib(self.temp_ceiling)
            ))
            .with_data(json!({
                "temp_bytes": bytes,
                "held_bytes": held,
                "ceiling_bytes": self.temp_ceiling,
            })));
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

    /// Refuse `more` bytes of temporary storage under `directory` beside the `spooled` bytes a job
    /// already holds there: all of it, with the `held` bytes of open previews, beyond the ceiling,
    /// or `more` beyond the volume's free space.
    pub(crate) fn preflight_more(
        &self,
        directory: &Path,
        spooled: u64,
        more: u64,
        held: u64,
    ) -> Result<(), Error> {
        let total = spooled.saturating_add(more);
        if total.saturating_add(held) > self.temp_ceiling {
            return Err(Error::resource_limit(format!(
                "the merge needs {} of temporary storage with its result, above the {} a merge \
                 may use",
                gib(total.saturating_add(held)),
                gib(self.temp_ceiling)
            ))
            .with_data(json!({
                "temp_bytes": total,
                "held_bytes": held,
                "ceiling_bytes": self.temp_ceiling,
            })));
        }
        let available = match self.available {
            Some(available) => available.saturating_sub(spooled),
            None => luxforge_process::disk::available_bytes(directory)
                .map_err(|error| file_error("cannot read the free disk space", error.kind()))?,
        };
        if more > available {
            return Err(Error::resource_limit(format!(
                "the merge's result needs {} and the disk has {} free",
                gib(more),
                gib(available)
            ))
            .with_data(json!({"temp_bytes": more, "available_bytes": available})));
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
        develop_read(
            crate::library::develop::read(path, true, control, &|_| {})?,
            gains,
            control,
        )
    }
}

/// Develop a file the shared bounded read has just read, keeping what it read: a RAW through
/// [`crate::source::RawPrepared::develop`] at `gains`, or its own as-shot gains when none are
/// named; anything else, which kept no sensor, as a JPEG with no development. What
/// [`SharedDevelopment`] does after reading, and what a panorama frame does after its own read
/// (`crate::merge::panorama::frames`).
pub(crate) fn develop_read(
    read: crate::library::develop::ReadFile,
    gains: Option<[f32; 3]>,
    control: &JobControl,
) -> Result<Developed, Error> {
    use crate::{editor::ReadContent, source::RawPrepared};
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
    /// The temporary bytes open merge previews keep spooled on the lane, which count against the
    /// same ceiling.
    pub held: Arc<AtomicU64>,
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
    /// An empty account held to `limits`' scratch limit.
    pub(crate) fn of(limits: &Limits) -> Self {
        Self {
            scratch_limit_bytes: limits.scratch,
            ..Self::default()
        }
    }

    /// Take `bytes` of scratch for `what`, refused with `resource-limit` past the limit before
    /// anything is allocated.
    pub(crate) fn take(&mut self, bytes: u64, what: &str) -> Result<(), Error> {
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

    pub(crate) fn give(&mut self, bytes: u64) {
        self.scratch_bytes = self.scratch_bytes.saturating_sub(bytes);
    }
}

/// The job's spool files, removed as they are dropped, however the job or the preview holding them
/// ends. The directory and anything else in it (the owner's request marker) are not the spool's to
/// remove, unless it is a preview's own folder ([`Spool::owned`]).
pub(crate) struct Spool {
    pub(crate) files: Vec<PathBuf>,
    /// The folder to remove with the files: a preview's, which holds nothing else.
    folder: Option<PathBuf>,
}

impl Spool {
    pub(crate) fn new(directory: &Path, inputs: usize) -> Result<Self, Error> {
        fs::create_dir_all(directory).map_err(|error| {
            file_error("cannot create the merge's working folder", error.kind())
        })?;
        Ok(Self {
            files: (0..inputs)
                .map(|input| directory.join(spool_name(input)))
                .collect(),
            folder: None,
        })
    }

    /// A spool whose folder is its own, removed with it.
    pub(crate) fn owned(directory: &Path, inputs: usize) -> Result<Self, Error> {
        let mut spool = Self::new(directory, inputs)?;
        spool.folder = Some(directory.to_path_buf());
        Ok(spool)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
        if let Some(folder) = &self.folder {
            let _ = fs::remove_dir(folder);
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

/// One input read and spooled, its development dropped: what it is and its colour interpretation.
pub(crate) struct SpooledInput {
    pub developed: Developed,
    pub cam_xyz: [[f32; 3]; 4],
}

/// What reading leaves ([`JobSpec::read`]): the bracket spooled in the stack's order, each input's
/// analysis proxy (and, for a preview, its [`ColourProxy`]), and the white balance every input was
/// developed at. A preview's session, its updates and the merge that takes it over share it; its
/// spool files are removed with its last holder.
pub(crate) struct Prepared {
    spool: Spool,
    pub inputs: Vec<SpooledInput>,
    pub proxies: Vec<AnalysisProxy>,
    /// Empty unless a preview read the bracket.
    pub colour: Vec<ColourProxy>,
    pub gains: [f32; 3],
    pub width: u32,
    pub height: u32,
    /// The bytes spooled.
    pub temp_bytes: u64,
}

impl Prepared {
    /// The decoded inputs' frames, in the stack's order.
    pub(crate) fn frames(&self) -> Vec<FrameMetadata> {
        self.inputs
            .iter()
            .map(|input| input.developed.frame.clone())
            .collect()
    }

    /// One analysis proxy's bytes.
    pub(crate) fn proxy_bytes(&self) -> u64 {
        AnalysisProxy::bytes_for(self.width, self.height) as u64
    }

    /// The scratch the proxies it still holds take.
    pub(crate) fn proxies_bytes(&self) -> u64 {
        let colour = ColourProxy::bytes_for(self.width, self.height) as u64;
        self.proxies.len() as u64 * self.proxy_bytes() + self.colour.len() as u64 * colour
    }

    /// Drop the proxies, which a merge no longer needs once it is aligned.
    fn release_proxies(&mut self) {
        self.proxies = Vec::new();
        self.colour = Vec::new();
    }
}

/// What aligning leaves ([`analyse`]): the options it ran with, the resolved exposures, the
/// registration, the common coverage and, with deghosting, the motion map.
#[derive(Clone, Debug)]
pub(crate) struct Analysis {
    pub parameters: HdrParameters,
    pub exposures: ResolvedExposures,
    pub registration: Registration,
    pub coverage: Coverage,
    pub motion: Option<MotionMap>,
}

/// Validate the decoded bracket, resolve its exposures (fitting them from aligned proxy samples
/// when the metadata cannot), register it, compute its coverage and find what moved: the aligning
/// stage, on the proxies alone. Takes the scratch its aligned copies and samples need and gives it
/// back, keeping the motion map's (one analysis proxy's bytes) taken.
pub(crate) fn analyse(
    frames: &[FrameMetadata],
    proxies: &[AnalysisProxy],
    parameters: &HdrParameters,
    control: &JobControl,
    accounting: &mut Accounting,
) -> Result<Analysis, Error> {
    let cancel = control.render_cancel();
    validate_bracket(frames)?;
    let n = frames.len();
    let (width, height) = (frames[0].width, frames[0].height);
    let proxy_bytes = AnalysisProxy::bytes_for(width, height) as u64;
    let mode = parameters.alignment.alignment;
    let (exposures, registration) = match resolve_exposures(frames, &parameters.exposure)? {
        ExposureResolution::Resolved(exposures) => {
            let registration = register(frames, proxies, exposures.reference, mode, cancel)?;
            (exposures, registration)
        }
        ExposureResolution::NeedsFit(plan) => {
            let registration = register(frames, proxies, plan.reference(), mode, cancel)?;
            accounting.take(n as u64 * proxy_bytes, "the aligned proxies")?;
            let aligned = AlignedProxies::new(&registration, proxies, cancel)?;
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
                register(frames, proxies, exposures.reference, mode, cancel)?
            };
            (exposures, registration)
        }
    };
    let transforms = registration.transforms();
    let coverage = common_coverage(width, height, &transforms, cancel)?;
    let motion = motion(
        proxies,
        &exposures,
        &registration,
        parameters,
        control,
        accounting,
    )?;
    Ok(Analysis {
        parameters: parameters.clone(),
        exposures,
        registration,
        coverage,
        motion,
    })
}

/// Find what moved, on the proxies aligned by `registration`: the motion map of `parameters`'
/// deghosting. The map's scratch (one analysis proxy's bytes) stays taken.
pub(crate) fn motion(
    proxies: &[AnalysisProxy],
    exposures: &ResolvedExposures,
    registration: &Registration,
    parameters: &HdrParameters,
    control: &JobControl,
    accounting: &mut Accounting,
) -> Result<Option<MotionMap>, Error> {
    let cancel = control.render_cancel();
    let n = proxies.len() as u64;
    let proxy_bytes = proxies.first().map_or(0, |proxy| {
        let (width, height) = proxy.frame_dimensions();
        AnalysisProxy::bytes_for(width, height) as u64
    });
    accounting.take(n * proxy_bytes, "the aligned proxies")?;
    let aligned = AlignedProxies::new(registration, proxies, cancel)
        .inspect_err(|_| accounting.give(n * proxy_bytes))?;
    let found = accounting
        .take(proxy_bytes, "the motion map")
        .and_then(|()| {
            detect_motion(&aligned, exposures, parameters.alignment.deghost, cancel)
                .inspect_err(|_| accounting.give(proxy_bytes))
        });
    drop(aligned);
    accounting.give(n * proxy_bytes);
    found
}

impl JobSpec {
    /// Run the job without a preview: read, align and merge. See the module documentation.
    pub(crate) fn run(self, control: &JobControl) -> Result<Merged, Error> {
        let mut accounting = Accounting::of(&self.limits);
        let mut prepared = self.read(control, &mut accounting, false)?;
        let analysis = self.align(&prepared, control, &mut accounting)?;
        // The proxies are not needed once aligned on the final reference.
        let held = prepared.proxies_bytes();
        prepared.release_proxies();
        accounting.give(held);
        self.merge(Arc::new(prepared), &analysis, control, accounting)
    }

    /// Run the job on a preview's bracket: merge what the preview read, with the preview's
    /// alignment when it was aligned with these options and on its proxies otherwise, reading no
    /// frame again. The white balance is the preview's.
    pub(crate) fn run_prepared(
        self,
        mut prepared: Arc<Prepared>,
        analysis: Option<Arc<Analysis>>,
        control: &JobControl,
    ) -> Result<Merged, Error> {
        let mut accounting = Accounting::of(&self.limits);
        accounting.temp_bytes = prepared.temp_bytes;
        if prepared.inputs.len() != self.inputs.len()
            || prepared
                .inputs
                .iter()
                .zip(&self.inputs)
                .any(|(spooled, input)| spooled.developed.path != input.path)
        {
            return Err(Error::internal(
                "a merge preview's inputs are not its merge's",
            ));
        }
        accounting.take(prepared.proxies_bytes(), "the preview's proxies")?;
        let analysis = match analysis.filter(|analysis| analysis.parameters == self.parameters) {
            Some(analysis) => {
                accounting.take(prepared.proxy_bytes(), "the motion map")?;
                analysis
            }
            None => Arc::new(self.align(&prepared, control, &mut accounting)?),
        };
        // The proxies are released when this merge holds the preview's bracket alone.
        let held = prepared.proxies_bytes();
        if let Some(owned) = Arc::get_mut(&mut prepared) {
            owned.release_proxies();
            accounting.give(held);
        }
        // The result's objects, beside what previews keep, before anything is written.
        let pixels = u64::from(prepared.width) * u64::from(prepared.height);
        self.limits.preflight(
            &self.spool,
            prepared.temp_bytes + pixels * RESULT_PIXEL,
            self.held.load(Ordering::SeqCst),
        )?;
        self.merge(prepared, &analysis, control, accounting)
    }

    /// Read and spool: each input, the planned reference first, so its as-shot white balance
    /// develops the rest, checked against its signature, developed, reduced to its proxies (with
    /// `colour`, its [`ColourProxy`] too) and spooled, one development at a time. See the module
    /// documentation.
    pub(crate) fn read(
        &self,
        control: &JobControl,
        accounting: &mut Accounting,
        colour: bool,
    ) -> Result<Prepared, Error> {
        let n = self.inputs.len();
        let spool = if colour {
            Spool::owned(&self.spool, n)?
        } else {
            Spool::new(&self.spool, n)?
        };
        control.set_phase("reading");
        let first = self.planned_reference();
        let mut order: Vec<usize> = (0..n).collect();
        order.retain(|&input| input != first);
        order.insert(0, first);
        type Read = (SpooledInput, AnalysisProxy, Option<ColourProxy>);
        let mut read: Vec<Option<Read>> = (0..n).map(|_| None).collect();
        let mut gains = None;
        let mut size = (0, 0);
        // How far through the job's progress reading takes it.
        let share = if colour { 0.8 } else { 0.5 };
        for (done, &input) in order.iter().enumerate() {
            control.checkpoint()?;
            report(control, Stage::Reading(input), accounting);
            control.set_progress(
                Some(share * done as f64 / n as f64),
                &format!("Reading {} of {n}", done + 1),
            );
            self.hooks.at(Stage::Reading(input));
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
                size = (width, height);
                self.limits.preflight(
                    &self.spool,
                    temp_bytes(n, width, height),
                    self.held.load(Ordering::SeqCst),
                )?;
                let proxy_bytes = AnalysisProxy::bytes_for(width, height) as u64;
                accounting.take(n as u64 * proxy_bytes, "the analysis proxies")?;
                if colour {
                    let colour_bytes = ColourProxy::bytes_for(width, height) as u64;
                    accounting.take(n as u64 * colour_bytes, "the preview proxies")?;
                    crate::merge::preview::fits(accounting, n, width, height)?;
                }
                analysis_fits(accounting, n, proxy_bytes)?;
                fusion_fits(accounting, n, width, height)?;
            }
            accounting.development_bytes = accounting
                .development_bytes
                .max(development.image.plane_bytes());
            let band = band_bytes(width);
            accounting.take(band, "a development band")?;
            report(control, Stage::Reading(input), accounting);
            let (proxy, colour_proxy) = self
                .spool_input(
                    &spool.files[input],
                    input,
                    &development,
                    colour,
                    control,
                    accounting,
                )
                .inspect_err(|_| accounting.give(band))?;
            accounting.give(band);
            let cam_xyz = development.cam_xyz;
            drop(development);
            report(control, Stage::Spooled(input), accounting);
            self.hooks.at(Stage::Spooled(input));
            read[input] = Some((SpooledInput { developed, cam_xyz }, proxy, colour_proxy));
        }
        let gains = gains.ok_or_else(|| Error::internal("a merge developed no input"))?;
        let mut prepared = Prepared {
            spool,
            inputs: Vec::with_capacity(n),
            proxies: Vec::with_capacity(n),
            colour: Vec::with_capacity(if colour { n } else { 0 }),
            gains,
            width: size.0,
            height: size.1,
            temp_bytes: accounting.temp_bytes,
        };
        for (input, proxy, colour) in read.into_iter().flatten() {
            prepared.inputs.push(input);
            prepared.proxies.push(proxy);
            prepared.colour.extend(colour);
        }
        Ok(prepared)
    }

    /// The aligning stage of a merge or a preview: [`analyse`] with its progress, stage and hold.
    pub(crate) fn align(
        &self,
        prepared: &Prepared,
        control: &JobControl,
        accounting: &mut Accounting,
    ) -> Result<Analysis, Error> {
        control.checkpoint()?;
        control.set_phase("aligning");
        report(control, Stage::Aligning, accounting);
        self.hooks.at(Stage::Aligning);
        let share = if prepared.colour.is_empty() { 0.5 } else { 0.8 };
        control.set_progress(Some(share), "Aligning");
        let analysis = analyse(
            &prepared.frames(),
            &prepared.proxies,
            &self.parameters,
            control,
            accounting,
        )?;
        report(control, Stage::Aligning, accounting);
        Ok(analysis)
    }

    /// The merging and saving stage: fuse the aligned bracket chunk by chunk from its spool into a
    /// durable source, check the inputs' signatures again, and answer what to publish. The motion
    /// map's scratch, which aligning left taken, is given back here.
    pub(crate) fn merge(
        self,
        prepared: Arc<Prepared>,
        analysis: &Analysis,
        control: &JobControl,
        mut accounting: Accounting,
    ) -> Result<Merged, Error> {
        let n = self.inputs.len();
        let (width, height) = (prepared.width, prepared.height);
        let motion_bytes = prepared.proxy_bytes();
        // Merging: one chunk of the coverage at a time, every input's window read back from its
        // spool.
        control.checkpoint()?;
        control.set_phase("merging");
        let (source, fusion) = self.fuse(
            &prepared.spool,
            &analysis.exposures,
            &analysis.registration,
            analysis.coverage.rect,
            (width, height),
            analysis.motion.as_ref(),
            control,
            &mut accounting,
        )?;
        accounting.give(motion_bytes);

        // Saving: the source is durable; the inputs must still be the files that were read.
        control.checkpoint()?;
        for input in &self.inputs {
            self.check_signature(input)?;
        }
        report(control, Stage::Saved, &accounting);
        self.hooks.at(Stage::Saved);
        control.set_progress(Some(1.0), "Saving");
        accounting.temp_bytes = 0;
        let exposures = analysis.exposures.clone();
        let reference = exposures.reference;
        let developed: Vec<&Developed> = prepared
            .inputs
            .iter()
            .map(|input| &input.developed)
            .collect();
        let cam_xyz = prepared.inputs[reference].cam_xyz;
        let provenance = MergeProvenance {
            format: MERGE_PROVENANCE_FORMAT,
            kind: MergeKind::Hdr,
            provider: self.provider.clone(),
            inputs: self
                .inputs
                .iter()
                .zip(&developed)
                .map(|(input, developed)| MergeInput {
                    path: input.path.clone(),
                    volume_id: developed.volume.id.clone(),
                    signature: input.signature,
                    asset_id: input.asset_id.clone(),
                    frame: developed.frame.clone(),
                })
                .collect(),
            parameters: MergeParameters::Hdr(self.parameters.clone()),
            resolved: MergeResolution::Hdr(HdrResolution {
                development: HdrDevelopment {
                    white_balance: prepared.gains,
                    cam_xyz,
                },
                exposures,
                registration: analysis.registration.clone(),
                coverage: analysis.coverage,
                motion: analysis
                    .motion
                    .as_ref()
                    .map_or_else(MotionDiagnostics::default, MotionMap::diagnostics),
                fusion,
            }),
        };
        provenance.validate()?;
        let mut volumes: Vec<Volume> = Vec::with_capacity(n);
        for developed in &developed {
            if !volumes
                .iter()
                .any(|volume| volume.id == developed.volume.id)
            {
                volumes.push(developed.volume.clone());
            }
        }
        let reference_read = developed[reference];
        let merged = Merged {
            file_name: result_name(&reference_read.path),
            header: result_header(&reference_read.header, source.width, source.height),
            place: reference_read.place.clone(),
            volumes,
            provenance,
            source,
            accounting,
        };
        // The spool goes now, unless a preview's cancelled update still holds it.
        drop(developed);
        drop(prepared);
        Ok(merged)
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
        check_signature(input)
    }
    /// Write `development`'s upright rows to `path`, [`SPOOL_PIXEL`] bytes a pixel, a band at a
    /// time, reducing each band into the input's proxy as it goes. Cancellable per band.
    fn spool_input(
        &self,
        path: &Path,
        input: usize,
        development: &Development,
        colour: bool,
        control: &JobControl,
        accounting: &mut Accounting,
    ) -> Result<(AnalysisProxy, Option<ColourProxy>), Error> {
        let (width, height) = (development.image.width(), development.image.height());
        let mut builder = ProxyBuilder::new(width, height)?;
        let mut colour = colour
            .then(|| ColourProxyBuilder::new(width, height))
            .transpose()?;
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
            let spooled = FrameRect {
                width: w,
                height: band,
                stride: w,
                rgb: [&planes[0], &planes[1], &planes[2]],
                levels: &levels,
            };
            builder.push_rows(&spooled)?;
            if let Some(colour) = &mut colour {
                colour.push_rows(&spooled)?;
            }
            if self.hooks.spool_full(input) {
                return Err(spool_error(std::io::ErrorKind::StorageFull));
            }
            file.write_all(&bytes)
                .map_err(|error| spool_error(error.kind()))?;
            accounting.temp_bytes += bytes.len() as u64;
        }
        file.flush().map_err(|error| spool_error(error.kind()))?;
        Ok((
            builder.finish()?,
            colour.map(ColourProxyBuilder::finish).transpose()?,
        ))
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
        let held = chunk_bytes(n, chunk_pixels, window_pixels, width);
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

/// The bytes of a spool span covering `rows` rows of a `window`-pixel-wide window in a
/// `width`-pixel-wide frame: from the first row's first window pixel to the last row's last.
fn band_span(width: u32, window: u32, rows: u32) -> u64 {
    (u64::from(rows.max(1) - 1) * u64::from(width) + u64::from(window)) * SPOOL_PIXEL
}

/// The rows of `bounds` one read takes in a `width`-pixel-wide frame: as many as keep the span
/// within `limit`, at least one, at most the window's.
fn band_rows(width: u32, bounds: PixelRect, limit: u64) -> usize {
    let row = u64::from(width) * SPOOL_PIXEL;
    let first = u64::from(bounds.width) * SPOOL_PIXEL;
    let rows = if limit <= first || row == 0 {
        1
    } else {
        1 + (limit - first) / row
    };
    rows.min(u64::from(bounds.height).max(1)) as usize
}

/// The most the window's band buffer holds when reading a `width`-pixel-wide frame:
/// [`READ_BAND_BYTES`], or one frame row when that is larger.
pub(crate) fn read_band_bytes(width: u32) -> u64 {
    READ_BAND_BYTES.max(u64::from(width) * SPOOL_PIXEL)
}

/// What the merging phase holds: every input's aligned chunk, one source window and the band
/// buffer it is read through from a `width`-pixel-wide spool, the chunk's motion selection and the
/// writer's chunk samples and encoding.
fn chunk_bytes(inputs: usize, chunk_pixels: u64, window_pixels: u64, width: u32) -> u64 {
    inputs as u64 * chunk_pixels * SPOOL_PIXEL
        + window_pixels * SPOOL_PIXEL
        + read_band_bytes(width)
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
        chunk_bytes(inputs, chunk, window, width) + AnalysisProxy::bytes_for(width, height) as u64,
        "merging the bracket",
    )
}

/// The one source window buffer: an input's window read back from its spool into planes, through
/// one bounded band buffer.
#[derive(Default)]
pub(crate) struct Window {
    rgb: [Vec<f32>; 3],
    levels: Vec<SensorLevel>,
    band: Vec<u8>,
}

impl Window {
    /// Read `bounds` of a `width`-pixel-wide spooled frame from `file`: see [`READ_BAND_BYTES`].
    pub(crate) fn read(
        &mut self,
        file: &mut File,
        width: u32,
        bounds: PixelRect,
    ) -> Result<(), Error> {
        self.read_in_bands(file, width, bounds, READ_BAND_BYTES)
    }

    /// [`Self::read`] with at most `limit` bytes a band (and at least one row's span).
    fn read_in_bands(
        &mut self,
        file: &mut File,
        width: u32,
        bounds: PixelRect,
        limit: u64,
    ) -> Result<(), Error> {
        let (w, h) = (bounds.width as usize, bounds.height as usize);
        for plane in &mut self.rgb {
            plane.clear();
            plane.resize(w * h, 0.0);
        }
        self.levels.clear();
        self.levels.resize(w * h, SensorLevel(0));
        let pixel = SPOOL_PIXEL as usize;
        let rows = band_rows(width, bounds, limit);
        let mut y = 0;
        while y < h {
            let k = rows.min(h - y);
            let span = band_span(width, bounds.width, k as u32) as usize;
            let offset = ((u64::from(bounds.y) + y as u64) * u64::from(width)
                + u64::from(bounds.x))
                * SPOOL_PIXEL;
            self.band.resize(span, 0);
            file.seek(SeekFrom::Start(offset))
                .and_then(|_| file.read_exact(&mut self.band[..span]))
                .map_err(|error| spool_error(error.kind()))?;
            for row in 0..k {
                let start = row * width as usize * pixel;
                let bytes = &self.band[start..start + w * pixel];
                for (x, pixel) in bytes.chunks_exact(pixel).enumerate() {
                    let i = (y + row) * w + x;
                    let sample = |at: usize| {
                        f32::from_le_bytes([pixel[at], pixel[at + 1], pixel[at + 2], pixel[at + 3]])
                    };
                    self.rgb[0][i] = sample(0);
                    self.rgb[1][i] = sample(4);
                    self.rgb[2][i] = sample(8);
                    self.levels[i] = SensorLevel(u16::from_le_bytes([pixel[12], pixel[13]]));
                }
            }
            y += k;
        }
        Ok(())
    }

    pub(crate) fn source(&self, bounds: PixelRect) -> SourceWindow<'_> {
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

pub(crate) fn spool_error(kind: std::io::ErrorKind) -> Error {
    let error = file_error("cannot use the merge's working data", kind);
    if error.kind == ErrorKind::ResourceLimit {
        Error::resource_limit("the disk holding the merge's working data is full")
    } else {
        error
    }
}

/// Publish the job's progress: its stage and what it holds, as `job.read` reports a running job's
/// result.
pub(crate) fn report(control: &JobControl, stage: Stage, accounting: &Accounting) {
    control.update_partial(|partial| {
        *partial = json!({
            "stage": stage,
            "accounting": accounting,
        });
    });
}

/// The result's name: the reference file's stem with "HDR".
pub(crate) fn result_name(reference: &Path) -> String {
    named_result(reference, MergeKind::Hdr)
}

/// A result's name: the reference file's stem with "HDR" or "Panorama".
pub(crate) fn named_result(reference: &Path, kind: MergeKind) -> String {
    let stem = reference.file_stem().map_or_else(
        || "Merge".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    match kind {
        MergeKind::Hdr => format!("{stem} HDR"),
        MergeKind::Panorama => format!("{stem} Panorama"),
    }
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

/// Refuse an input that is gone (`source-unavailable`) or not the file the request captured
/// (`conflict`): one stat.
pub(crate) fn check_signature(input: &JobInput) -> Result<(), Error> {
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

pub(crate) fn file_name(path: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Cancel,
        merge::hdr::{
            ExposureOrigin, FusionParameters, MetadataSource, ReferenceOrigin, ResolvedExposure,
            RigidTransform,
        },
    };
    use luxforge_testbase::paths::temp_dir;

    /// A deterministic spooled frame: every pixel's samples (signed, some above one) and level
    /// distinct, so a misplaced byte shows.
    fn spool_frame(path: &Path, width: u32, height: u32, seed: u32) {
        let mut bytes = Vec::with_capacity((width * height) as usize * SPOOL_PIXEL as usize);
        let mut state = 0x9e37_79b9_u32 ^ seed;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for y in 0..height {
            for x in 0..width {
                let base = (x as f32 + 0.5) / width as f32 + (y as f32) / height as f32;
                for c in 0..3 {
                    let noise = (next() % 1000) as f32 / 4000.0 - 0.05;
                    let value = base * (0.4 + 0.3 * c as f32) + noise;
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                let level = (next() % 40_000) as u16;
                bytes.extend_from_slice(&level.to_le_bytes());
            }
        }
        fs::write(path, bytes).expect("spool written");
    }

    /// The unbanded read: one seek and one read a window row.
    fn read_by_rows(file: &mut File, width: u32, bounds: PixelRect) -> Window {
        let (w, h) = (bounds.width as usize, bounds.height as usize);
        let mut window = Window {
            rgb: [vec![0.0; w * h], vec![0.0; w * h], vec![0.0; w * h]],
            levels: vec![SensorLevel(0); w * h],
            band: Vec::new(),
        };
        let mut row = vec![0_u8; w * SPOOL_PIXEL as usize];
        for y in 0..h {
            let offset = ((u64::from(bounds.y) + y as u64) * u64::from(width)
                + u64::from(bounds.x))
                * SPOOL_PIXEL;
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.read_exact(&mut row).unwrap();
            for (x, pixel) in row.chunks_exact(SPOOL_PIXEL as usize).enumerate() {
                let i = y * w + x;
                for (c, plane) in window.rgb.iter_mut().enumerate() {
                    let at = 4 * c;
                    plane[i] = f32::from_le_bytes([
                        pixel[at],
                        pixel[at + 1],
                        pixel[at + 2],
                        pixel[at + 3],
                    ]);
                }
                window.levels[i] = SensorLevel(u16::from_le_bytes([pixel[12], pixel[13]]));
            }
        }
        window
    }

    fn same_bits(a: &Window, b: &Window) -> bool {
        a.levels == b.levels
            && (0..3).all(|c| {
                a.rgb[c].len() == b.rgb[c].len()
                    && a.rgb[c]
                        .iter()
                        .zip(&b.rgb[c])
                        .all(|(x, y)| x.to_bits() == y.to_bits())
            })
    }

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn hdr_merge_spool_band_reads_equal_row_reads_bit_for_bit() {
        let directory = temp_dir("hdr-merge-spool-bands");
        let path = directory.join(spool_name(0));
        let (width, height) = (97_u32, 61_u32);
        spool_frame(&path, width, height, 1);
        let mut file = File::open(&path).unwrap();
        let windows = [
            rect(0, 0, width, height),
            rect(13, 7, 40, 33),
            rect(96, 60, 1, 1),
            rect(0, 59, width, 2),
            rect(50, 0, 47, 61),
        ];
        let row = u64::from(width) * SPOOL_PIXEL;
        // One row a band (a limit under a window row), a few rows, exactly whole rows, just under
        // whole rows, and the production bound.
        let limits = [1, 3 * row + 5, 10 * row, 7 * row - 1, READ_BAND_BYTES];
        let mut window = Window::default();
        for bounds in windows {
            let expected = read_by_rows(&mut file, width, bounds);
            for limit in limits {
                window
                    .read_in_bands(&mut file, width, bounds, limit)
                    .expect("band read");
                assert!(
                    same_bits(&window, &expected),
                    "{bounds:?} at {limit} bytes a band"
                );
                let rows = band_rows(width, bounds, limit);
                assert!(rows >= 1 && rows <= bounds.height as usize);
                assert!(
                    window.band.len() as u64 <= limit.max(u64::from(bounds.width) * SPOOL_PIXEL),
                    "the band buffer stays within its bound"
                );
            }
        }
        // A window running past the spool is an error, not a short read.
        assert!(
            window
                .read(&mut file, width, rect(0, 60, width, 2))
                .is_err()
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn hdr_merge_spool_band_reads_fuse_bit_identically() {
        let directory = temp_dir("hdr-merge-spool-fusion");
        let (width, height) = (180_u32, 120_u32);
        let transforms = [
            RigidTransform::identity(width, height),
            RigidTransform::new(width, height, 1.5_f64.to_radians(), 2.25, -1.5),
        ];
        let mut files = Vec::new();
        for input in 0..2 {
            let path = directory.join(spool_name(input));
            spool_frame(&path, width, height, input as u32 + 7);
            files.push(File::open(&path).unwrap());
        }
        let exposures = ResolvedExposures {
            reference: 0,
            reference_origin: ReferenceOrigin::Requested,
            inputs: [0.0_f64, -2.0]
                .into_iter()
                .enumerate()
                .map(|(i, offset_ev)| ResolvedExposure {
                    offset_ev,
                    scale: (-offset_ev).exp2(),
                    origin: if i == 0 {
                        ExposureOrigin::Reference
                    } else {
                        ExposureOrigin::Metadata {
                            source: MetadataSource::Settings,
                        }
                    },
                })
                .collect(),
        };
        let chunk = rect(20, 15, 120, 80);
        let cancel = Cancel::never();
        let mut fused = Vec::new();
        for banded in [false, true] {
            let mut aligned = [AlignedRect::default(), AlignedRect::default()];
            for (input, file) in files.iter_mut().enumerate() {
                let bounds = source_window(&transforms[input], chunk, width, height).unwrap();
                let window = if banded {
                    let mut window = Window::default();
                    // A small bound, so the window takes several bands.
                    let limit = 9 * u64::from(width) * SPOOL_PIXEL;
                    window.read_in_bands(file, width, bounds, limit).unwrap();
                    window
                } else {
                    read_by_rows(file, width, bounds)
                };
                resample_rect(
                    &transforms[input],
                    &window.source(bounds),
                    width,
                    height,
                    chunk,
                    &mut aligned[input],
                    &cancel,
                )
                .unwrap();
            }
            let pixels = chunk.pixels() as usize;
            let (mut red, mut green, mut blue) =
                (vec![0.0_f32; pixels], vec![0.0; pixels], vec![0.0; pixels]);
            let views: Vec<FrameRect<'_>> = aligned.iter().map(AlignedRect::view).collect();
            let diagnostics = fuse_rect(
                &exposures,
                &FusionParameters::default(),
                &views,
                None,
                &mut FusedRect {
                    width: chunk.width as usize,
                    height: chunk.height as usize,
                    stride: chunk.width as usize,
                    rgb: [&mut red, &mut green, &mut blue],
                    limits: None,
                },
            )
            .unwrap();
            let bits: Vec<u32> = [red, green, blue]
                .iter()
                .flatten()
                .map(|value| value.to_bits())
                .collect();
            fused.push((bits, diagnostics));
        }
        assert_eq!(fused[0], fused[1]);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn hdr_merge_spool_band_buffer_is_accounted() {
        // The band buffer is the bound, or one frame row past it, and the chunk counts it.
        assert_eq!(read_band_bytes(6048), READ_BAND_BYTES);
        assert_eq!(read_band_bytes(1 << 20), (1 << 20) * SPOOL_PIXEL);
        let without = chunk_bytes(9, 1 << 20, 1080 * 1080, 1) - read_band_bytes(1);
        assert_eq!(
            chunk_bytes(9, 1 << 20, 1080 * 1080, 6048),
            without + READ_BAND_BYTES
        );
        // A 6048-pixel frame reads a 1080-pixel window 99 rows a band.
        let bounds = rect(100, 0, 1080, 1080);
        let rows = band_rows(6048, bounds, READ_BAND_BYTES);
        assert_eq!(rows, 99);
        assert!(band_span(6048, 1080, rows as u32) <= READ_BAND_BYTES);
        assert!(band_span(6048, 1080, rows as u32 + 1) > READ_BAND_BYTES);
    }
}
