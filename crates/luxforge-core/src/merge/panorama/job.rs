//! The panorama merge job (`docs/design/panorama-merge.md`, "Generalizing the merge boundary",
//! "Bounds, recovery and regeneration"): what the merge lane's worker runs for one `merge.create`
//! or `merge.rerun` of kind `panorama`, off the catalog owner and off the source worker, composing
//! the frame path ([`super::frames`]), registration ([`super::registration`]), the stitch
//! ([`Stitcher`]) and durable derived storage. The owner publishes what it answers ([`Merged`])
//! in HDR's one publication transaction.
//!
//! In order, cancellable between every step and within each:
//!
//! 1. **Reading** ("Reading 2 of 7"). Each frame, the planned reference first (the one named, or
//!    the middle of the capture order) so its white balance and lens profile develop the rest, is
//!    checked against the signature the request captured and read once through the frame path into
//!    its spool file, with its registration proxy and colour proxy. Its features are detected on
//!    the registration proxy, which is then dropped. One development is held at a time. The
//!    decoded frames are held to the header rules again as each is read ([`suitability::check`]).
//! 2. **Matching.** The frames are registered from their features; a refusal names the frames by
//!    file name. The stitch is planned from the registration and the colour proxies, which
//!    resolves the projection, the field of view, the scale and the crop, and may refuse. The
//!    result's size is then known: its objects are preflighted against the temporary ceiling and
//!    the free space before any is written.
//! 3. **Stitching** ("Stitching 3 of 12"). The result is written chunk by chunk: the windows each
//!    chunk reads are read back from the frames' spools in bounded bands and rendered by the stitch
//!    into the writer's chunk buffer, resident or paged by its size.
//! 4. **Saving.** The frames' signatures are checked once more, and the provenance is written in
//!    the stack's order: the frames left to right in the result (top to bottom, then left to right,
//!    for several rows).
//!
//! **Bounds.** Scratch is accounted against the kind's limit (512 MiB) by phase and preflighted
//! after the first frame is read ([`fits`]): reading holds every frame's colour proxy and features,
//! one registration proxy and its detection; matching adds registration's bound
//! ([`super::registration::scratch_bytes_for`]); stitching holds the plan, one chunk's frame
//! windows, the spool's band buffer and the writer's chunk. The spool is preflighted against the
//! temporary ceiling (64 GiB) and the free space once the frames' size is known, and the result's
//! objects once the plan has sized them.
use super::{
    FocalPrior, FrameFeatures, PanoramaProxy, PanoramaRegistration, RadialMode,
    RegistrationOptions, RegistrationStage,
    frames::{FrameDeveloper, FrameRequest, LensBake, LensChoice, read_frame},
    proxy::MAX_PROXY_SIDE,
    register_with_progress,
    stitching::{
        ChunkStats, FrameWindows, MAX_WINDOW_PIXELS, OutputLimits, StitchPlan, StitchRequest,
        Stitcher,
    },
    suitability,
};
use crate::{
    Error,
    artifacts::derived_source::{CHUNK_SIDE, DerivedSourceWriter, PublishedSource},
    catalog_types::{
        HdrDevelopment, MERGE_PROVENANCE_FORMAT, MergeInput, MergeKind, MergeParameters,
        MergeProvenance, MergeProvider, MergeResolution, PanoramaLensResolution,
        PanoramaParameters, PanoramaResolution, Volume,
    },
    jobs::JobControl,
    merge::{
        hdr::{ColourProxy, FrameMetadata, PixelRect, frame::capture_order},
        job::{
            Accounting, Developed, Hooks, JobInput, Limits, Merged, RESULT_PIXEL, SPOOL_PIXEL,
            Spool, Stage, Window, check_signature, file_name, named_result, read_band_bytes,
            report, result_header, spool_error,
        },
        panorama::Levelling,
    },
};
use serde_json::json;
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Everything one panorama job needs, moved to the worker.
pub(crate) struct PanoramaSpec {
    /// The frames in the request's order (capture order); every index in `parameters` is a
    /// position here.
    pub inputs: Vec<JobInput>,
    pub parameters: PanoramaParameters,
    pub provider: MergeProvider,
    /// The job's own spool directory.
    pub spool: PathBuf,
    pub writer: DerivedSourceWriter,
    pub frames: Arc<dyn FrameDeveloper>,
    pub stitcher: Arc<dyn Stitcher>,
    pub limits: Limits,
    pub hooks: Hooks,
    /// The temporary bytes open merge previews keep spooled on the lane.
    pub held: Arc<AtomicU64>,
    /// Whether the reference frame's detected lens profile is baked into RAW frames: the
    /// person's preference for new RAW photos' lens profiles.
    pub detect_lens: bool,
}

/// What reading leaves: the frames spooled in the request's order with their features and colour
/// proxies, the lens profile baked into them, the common development and their size. A preview's
/// session, its updates and the merge that takes it over share it; its spool files go with its
/// last holder.
pub(crate) struct PanoramaPrepared {
    spool: Spool,
    /// Each frame read, its development dropped.
    pub inputs: Vec<Developed>,
    pub features: Vec<FrameFeatures>,
    pub colour: Vec<ColourProxy>,
    pub lens: Option<LensBake>,
    pub development: Option<HdrDevelopment>,
    pub width: u32,
    pub height: u32,
    pub raw: bool,
    /// The bytes spooled.
    pub temp_bytes: u64,
}

impl PanoramaPrepared {
    /// The decoded frames, in the request's order.
    pub(crate) fn frames(&self) -> Vec<FrameMetadata> {
        self.inputs
            .iter()
            .map(|input| input.frame.clone())
            .collect()
    }

    /// The frames' file names, in the request's order.
    pub(crate) fn names(&self) -> Vec<String> {
        self.inputs
            .iter()
            .map(|input| file_name(&input.path))
            .collect()
    }

    /// The scratch its proxies and features hold.
    pub(crate) fn held_bytes(&self) -> u64 {
        self.colour.len() as u64 * ColourProxy::bytes_for(self.width, self.height) as u64
            + self
                .features
                .iter()
                .map(|features| features.bytes() as u64)
                .sum::<u64>()
    }
}

/// What matching leaves: the options it ran with, the registration and the stitch plan.
#[derive(Clone, Debug)]
pub(crate) struct PanoramaAnalysis {
    pub parameters: PanoramaParameters,
    pub registration: PanoramaRegistration,
    pub exposure_scales: Vec<f64>,
    pub plan: StitchPlan,
}

/// The temporary storage `frames` frames of `width × height` spool.
pub(crate) fn spool_bytes(frames: usize, width: u32, height: u32) -> u64 {
    frames as u64 * u64::from(width) * u64::from(height) * SPOOL_PIXEL
}

/// The temporary storage a `width × height` result's objects take: its level 0 and, past the
/// resident limits, the overview levels above it (at most a third more).
pub(crate) fn result_bytes(width: u32, height: u32) -> u64 {
    let level = u64::from(width) * u64::from(height) * RESULT_PIXEL;
    match crate::artifacts::derived_source::SourceClass::of(width, height) {
        Ok(crate::artifacts::derived_source::SourceClass::Paged) => level + level / 3,
        _ => level,
    }
}

/// The most bytes one frame's features hold.
fn features_bytes() -> u64 {
    (super::MAX_FEATURES * (size_of::<super::Feature>() + size_of::<[u64; 4]>())) as u64
}

/// Refuse a panorama of `inputs` frames of `width × height` whose phases would pass the scratch
/// limit, before anything is spooled: reading (every colour proxy and the features, one
/// registration proxy and its detection), matching (registration's bound beside them) and
/// stitching (one chunk's frame windows, the band buffer and the writer's chunk).
pub(crate) fn fits(
    accounting: &Accounting,
    inputs: usize,
    width: u32,
    height: u32,
) -> Result<(), Error> {
    let n = inputs as u64;
    let colour = n * ColourProxy::bytes_for(width, height) as u64;
    let features = n * features_bytes();
    let factor = PanoramaProxy::factor_for(width, height, MAX_PROXY_SIDE);
    let (pw, ph) = ((width / factor) as usize, (height / factor) as usize);
    let detection = PanoramaProxy::bytes_for(width, height, MAX_PROXY_SIDE) as u64
        + FrameFeatures::scratch_bytes_for(pw, ph) as u64;
    let reading = colour + features + detection + super::frames::band_bytes(width);
    let matching = colour + features + super::registration::scratch_bytes_for(inputs) as u64;
    let stitching = MAX_WINDOW_PIXELS * SPOOL_PIXEL
        + read_band_bytes(width)
        + u64::from(CHUNK_SIDE) * u64::from(CHUNK_SIDE) * 2 * RESULT_PIXEL;
    let mut probe = Accounting {
        scratch_bytes: 0,
        ..*accounting
    };
    probe.take(
        reading.max(matching).max(stitching),
        "stitching the panorama",
    )
}

/// The focal length prior a frame's header gives: its 35 mm equivalent, or its focal length times
/// its camera's crop factor in the shared lens profile index (`index`, when loaded).
pub(crate) fn focal_prior(
    header: &crate::catalog_types::HeaderMetadata,
    index: Option<&crate::modules::lens::index::LensIndex>,
) -> Option<FocalPrior> {
    let exposure = &header.exposure;
    if let Some(focal) = exposure
        .focal_35mm_mm
        .map(f64::from)
        .filter(|focal| focal.is_finite() && *focal > 0.0)
    {
        return Some(FocalPrior::Equivalent35 { focal_mm: focal });
    }
    let focal = exposure
        .focal_mm
        .map(f64::from)
        .filter(|focal| focal.is_finite() && *focal > 0.0)?;
    let body = header.camera.as_ref()?;
    let normalize = crate::modules::lens::resolve::normalize;
    let (make, model) = (normalize(&body.make), normalize(&body.model));
    let crop = index?
        .cameras
        .iter()
        .find(|camera| normalize(&camera.maker) == make && normalize(&camera.model) == model)
        .map(|camera| camera.crop_factor)
        .filter(|crop| crop.is_finite() && *crop > 0.0)?;
    Some(FocalPrior::Equivalent35 {
        focal_mm: focal * crop,
    })
}

/// The stack's order of registered frames: by row from the top, rows told apart by their pitch,
/// then left to right by yaw within each row. `order[k]` is the request position of the frame at
/// stack position `k`.
pub(crate) fn stack_order(registration: &PanoramaRegistration) -> Vec<usize> {
    let lens = registration.lens;
    let half_height = (f64::from(lens.height) / 2.0 / lens.focal_px).atan();
    let place: Vec<(f64, f64)> = registration
        .rotations
        .iter()
        .map(|rotation| {
            let [x, y, z] = rotation.forward();
            (x.atan2(z), y.clamp(-1.0, 1.0).asin())
        })
        .collect();
    let mut by_pitch: Vec<usize> = (0..place.len()).collect();
    by_pitch.sort_by(|&a, &b| place[a].1.total_cmp(&place[b].1).then(a.cmp(&b)));
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut top = f64::NEG_INFINITY;
    for frame in by_pitch {
        if rows.is_empty() || place[frame].1 - top > half_height {
            top = place[frame].1;
            rows.push(Vec::new());
        }
        if let Some(row) = rows.last_mut() {
            row.push(frame);
        }
    }
    let mut order = Vec::with_capacity(place.len());
    for mut row in rows {
        row.sort_by(|&a, &b| place[a].0.total_cmp(&place[b].0).then(a.cmp(&b)));
        order.extend(row);
    }
    order
}

/// Every frame's window of one chunk, read back from the spools.
/// The frames' spools as a stitch reads them back: one window at a time into one buffer.
struct ChunkReader {
    files: Vec<File>,
    window: Window,
    width: u32,
    height: u32,
}

impl FrameWindows for ChunkReader {
    fn read(
        &mut self,
        frame: usize,
        bounds: PixelRect,
    ) -> Result<crate::merge::hdr::SourceWindow<'_>, Error> {
        let file = self
            .files
            .get_mut(frame)
            .ok_or_else(|| Error::internal("a stitch named an unknown frame"))?;
        if bounds.is_empty()
            || bounds.pixels() > MAX_WINDOW_PIXELS
            || u64::from(bounds.x) + u64::from(bounds.width) > u64::from(self.width)
            || u64::from(bounds.y) + u64::from(bounds.height) > u64::from(self.height)
        {
            return Err(Error::internal(
                "a stitch asked for a window outside its frame or past the bound",
            ));
        }
        self.window.read(file, self.width, bounds)?;
        Ok(self.window.source(bounds))
    }
}

impl PanoramaSpec {
    /// Run the job without a preview: read, match and stitch. See the module documentation.
    pub(crate) fn run(self, control: &JobControl) -> Result<Merged, Error> {
        let mut accounting = Accounting::of(&self.limits);
        let mut prepared = self.read(control, &mut accounting)?;
        let analysis = self.analyse(&prepared, None, control, &mut accounting)?;
        // The stitch is planned: the proxies and features are not needed any more.
        let held = prepared.held_bytes();
        prepared.colour = Vec::new();
        prepared.features = Vec::new();
        accounting.give(held);
        self.stitch(Arc::new(prepared), &analysis, control, accounting)
    }

    /// Run the job on a preview's frames: stitch what the preview read, with the preview's
    /// registration and plan when its options were these, re-planning (and re-registering for
    /// another reference) on the retained features otherwise, reading no frame again.
    pub(crate) fn run_prepared(
        self,
        mut prepared: Arc<PanoramaPrepared>,
        analysis: Option<Arc<PanoramaAnalysis>>,
        control: &JobControl,
    ) -> Result<Merged, Error> {
        let mut accounting = Accounting::of(&self.limits);
        accounting.temp_bytes = prepared.temp_bytes;
        if prepared.inputs.len() != self.inputs.len()
            || prepared
                .inputs
                .iter()
                .zip(&self.inputs)
                .any(|(read, input)| read.path != input.path)
        {
            return Err(Error::internal(
                "a merge preview's frames are not its merge's",
            ));
        }
        accounting.take(prepared.held_bytes(), "the preview's proxies")?;
        let analysis = match analysis {
            Some(analysis) if analysis.parameters == self.parameters => (*analysis).clone(),
            previous => self.analyse(&prepared, previous.as_deref(), control, &mut accounting)?,
        };
        let held = prepared.held_bytes();
        if let Some(owned) = Arc::get_mut(&mut prepared) {
            owned.colour = Vec::new();
            owned.features = Vec::new();
            accounting.give(held);
        }
        self.stitch(prepared, &analysis, control, accounting)
    }

    /// The reference read first: the named one, else the middle of the capture order.
    fn planned_reference(&self) -> usize {
        let n = self.inputs.len();
        self.parameters
            .reference
            .filter(|&at| at < n)
            .unwrap_or_else(|| {
                let planned: Vec<FrameMetadata> = self
                    .inputs
                    .iter()
                    .map(|input| input.planned.clone())
                    .collect();
                capture_order(&planned)[(n - 1) / 2]
            })
    }

    /// Read and spool every frame, the planned reference first, detecting each one's features as
    /// it goes. See the module documentation.
    pub(crate) fn read(
        &self,
        control: &JobControl,
        accounting: &mut Accounting,
    ) -> Result<PanoramaPrepared, Error> {
        self.read_into(control, accounting, false)
    }

    /// [`Self::read`] into a spool of its own folder (a preview's), or the request's.
    pub(crate) fn read_into(
        &self,
        control: &JobControl,
        accounting: &mut Accounting,
        owned: bool,
    ) -> Result<PanoramaPrepared, Error> {
        let n = self.inputs.len();
        let spool = if owned {
            Spool::owned(&self.spool, n)?
        } else {
            Spool::new(&self.spool, n)?
        };
        control.set_phase("reading");
        let first = self.planned_reference();
        let mut order: Vec<usize> = (0..n).collect();
        order.retain(|&input| input != first);
        order.insert(0, first);
        let names: Vec<String> = self
            .inputs
            .iter()
            .map(|input| file_name(&input.path))
            .collect();
        type Read = (Developed, FrameFeatures, ColourProxy);
        let mut read: Vec<Option<Read>> = (0..n).map(|_| None).collect();
        let mut gains = None;
        let mut development = None;
        let mut lens: Option<LensBake> = None;
        let mut size = (0, 0);
        let cancel = control.render_cancel();
        for (done, &input) in order.iter().enumerate() {
            control.checkpoint()?;
            report(control, Stage::Reading(input), accounting);
            control.set_progress(
                Some(0.4 * done as f64 / n as f64),
                &format!("Reading {} of {n}", done + 1),
            );
            self.hooks.at(Stage::Reading(input));
            let item = &self.inputs[input];
            check_signature(item)?;
            if self.hooks.spool_full(input) {
                return Err(spool_error(std::io::ErrorKind::StorageFull));
            }
            let choice = if done == 0 && self.detect_lens {
                LensChoice::Detect
            } else {
                LensChoice::Use(lens.as_ref())
            };
            let frame = read_frame(
                self.frames.as_ref(),
                &FrameRequest {
                    path: &item.path,
                    gains,
                    lens: choice,
                    exposure_scale: 1.0,
                    colour: true,
                },
                &spool.files[input],
                control,
                accounting,
            )?;
            if let Some(expected) = &item.fingerprint
                && *expected != frame.developed.fingerprint
            {
                return Err(Error::conflict(format!(
                    "{} is not the file this merge recorded; its bytes differ",
                    names[input]
                ))
                .with_data(json!({"input": input, "path": item.path})));
            }
            if done == 0 {
                gains = frame.gains;
                development =
                    frame
                        .gains
                        .zip(frame.cam_xyz)
                        .map(|(gains, cam_xyz)| HdrDevelopment {
                            white_balance: gains,
                            cam_xyz,
                        });
                lens = frame.lens.clone();
                let (width, height) = (frame.spooled.width, frame.spooled.height);
                size = (width, height);
                // The frames' size is known now: preflight their spool and the phases' scratch
                // before another frame is read.
                self.limits.preflight(
                    &self.spool,
                    spool_bytes(n, width, height),
                    self.held.load(Ordering::SeqCst),
                )?;
                fits(accounting, n, width, height)?;
            }
            // The decoded frames read so far, in the request's order, are held to the header
            // rules as each comes.
            if done >= 1 {
                let mut stack: Vec<(usize, FrameMetadata)> = order[..done]
                    .iter()
                    .filter_map(|&at| {
                        read[at]
                            .as_ref()
                            .map(|(developed, ..)| (at, developed.frame.clone()))
                    })
                    .collect();
                stack.push((input, frame.developed.frame.clone()));
                stack.sort_by_key(|(at, _)| *at);
                let frames: Vec<FrameMetadata> =
                    stack.iter().map(|(_, frame)| frame.clone()).collect();
                let stack_names: Vec<String> =
                    stack.iter().map(|(at, _)| names[*at].clone()).collect();
                match suitability::check(&frames) {
                    Ok(()) | Err(suitability::Unsuitable::InputCount { .. }) => {}
                    Err(refusal) => return Err(refusal.error(&stack_names)),
                }
            }
            let colour_bytes = ColourProxy::bytes_for(size.0, size.1) as u64;
            accounting.take(colour_bytes, "a frame's colour proxy")?;
            let proxy_bytes = frame.spooled.proxy.bytes() as u64;
            let (pw, ph) = frame.spooled.proxy.dimensions();
            let detection = FrameFeatures::scratch_bytes_for(pw, ph) as u64;
            accounting.take(proxy_bytes + detection, "detecting a frame's features")?;
            let features = FrameFeatures::detect(&frame.spooled.proxy, cancel);
            accounting.give(proxy_bytes + detection);
            let features = features?;
            accounting.take(features.bytes() as u64, "a frame's features")?;
            let colour = frame
                .spooled
                .colour
                .ok_or_else(|| Error::internal("a panorama frame was read without its colour"))?;
            report(control, Stage::Spooled(input), accounting);
            self.hooks.at(Stage::Spooled(input));
            read[input] = Some((frame.developed, features, colour));
        }
        let mut prepared = PanoramaPrepared {
            spool,
            inputs: Vec::with_capacity(n),
            features: Vec::with_capacity(n),
            colour: Vec::with_capacity(n),
            lens,
            development,
            width: size.0,
            height: size.1,
            raw: false,
            temp_bytes: accounting.temp_bytes,
        };
        for (developed, features, colour) in read.into_iter().flatten() {
            prepared.inputs.push(developed);
            prepared.features.push(features);
            prepared.colour.push(colour);
        }
        prepared.raw = prepared.inputs.iter().all(|input| {
            matches!(
                input.frame.recording,
                crate::merge::hdr::Recording::Raw { .. }
            )
        });
        Ok(prepared)
    }

    /// The matching stage of a merge or a preview: register the frames (or keep `previous`'s
    /// registration when only the stitch's options changed) and plan the stitch, every refusal
    /// worded by file name.
    pub(crate) fn analyse(
        &self,
        prepared: &PanoramaPrepared,
        previous: Option<&PanoramaAnalysis>,
        control: &JobControl,
        accounting: &mut Accounting,
    ) -> Result<PanoramaAnalysis, Error> {
        analyse(
            prepared,
            &self.parameters,
            previous,
            self.stitcher.as_ref(),
            &self.hooks,
            control,
            accounting,
        )
    }

    /// The stitching and saving stage: render the result chunk by chunk from the spools into a
    /// durable source, check the frames' signatures again and answer what to publish, in the
    /// stack's order.
    pub(crate) fn stitch(
        self,
        prepared: Arc<PanoramaPrepared>,
        analysis: &PanoramaAnalysis,
        control: &JobControl,
        mut accounting: Accounting,
    ) -> Result<Merged, Error> {
        let plan = &analysis.plan;
        let (width, height) = (plan.width, plan.height);
        // The result's size is known: its objects beside the spool, before any is written.
        self.limits.preflight_more(
            &self.spool,
            prepared.temp_bytes,
            result_bytes(width, height),
            self.held.load(Ordering::SeqCst),
        )?;
        control.checkpoint()?;
        control.set_phase("stitching");
        accounting.take(plan.plan_bytes, "the stitch's plan")?;
        let source = self.write(&prepared, plan, control, &mut accounting);
        accounting.give(plan.plan_bytes);
        let (source, stats) = source?;

        // Saving: the source is durable; the frames must still be the files that were read.
        control.checkpoint()?;
        for input in &self.inputs {
            check_signature(input)?;
        }
        report(control, Stage::Saved, &accounting);
        self.hooks.at(Stage::Saved);
        control.set_progress(Some(1.0), "Saving");
        accounting.temp_bytes = 0;
        let order = stack_order(&analysis.registration);
        let provenance = self.provenance(&prepared, analysis, &order, stats)?;
        let mut volumes: Vec<Volume> = Vec::new();
        for developed in &prepared.inputs {
            if !volumes
                .iter()
                .any(|volume| volume.id == developed.volume.id)
            {
                volumes.push(developed.volume.clone());
            }
        }
        let reference = &prepared.inputs[analysis.registration.reference];
        let merged = Merged {
            file_name: named_result(&reference.path, MergeKind::Panorama),
            header: result_header(&reference.header, source.width, source.height),
            place: reference.place.clone(),
            volumes,
            provenance,
            source,
            accounting,
        };
        drop(prepared);
        Ok(merged)
    }

    /// Write the result chunk by chunk; answer it and what rendering measured.
    fn write(
        &self,
        prepared: &PanoramaPrepared,
        plan: &StitchPlan,
        control: &JobControl,
        accounting: &mut Accounting,
    ) -> Result<(PublishedSource, ChunkStats), Error> {
        let cancel = control.render_cancel();
        let mut reader = ChunkReader {
            files: Vec::with_capacity(prepared.inputs.len()),
            window: Window::default(),
            width: prepared.width,
            height: prepared.height,
        };
        for path in &prepared.spool.files {
            reader
                .files
                .push(File::open(path).map_err(|error| spool_error(error.kind()))?);
        }
        let chunks = crate::artifacts::derived_source::grid_of(plan.width, plan.height).len();
        // The spool's band buffer, the one frame window, the stitch's own and the writer's chunk.
        let fixed = read_band_bytes(prepared.width)
            + MAX_WINDOW_PIXELS * SPOOL_PIXEL
            + plan.chunk_bytes
            + u64::from(CHUNK_SIDE) * u64::from(CHUNK_SIDE) * 2 * RESULT_PIXEL;
        accounting.take(fixed, "one chunk of the panorama")?;
        report(control, Stage::Stitching(0), accounting);
        let mut done = 0;
        let mut stats = ChunkStats::default();
        #[cfg(test)]
        if let Some(step) = self.hooks.writer_fails {
            crate::artifacts::derived_source::fail::at(step);
        }
        let written = self.writer.write_with(
            plan.width,
            plan.height,
            None,
            control.flag(),
            |chunk, planes| {
                control.checkpoint()?;
                report(control, Stage::Stitching(done), accounting);
                self.hooks.at(Stage::Stitching(done));
                let rect = PixelRect {
                    x: chunk.x,
                    y: chunk.y,
                    width: chunk.width,
                    height: chunk.height,
                };
                let measured =
                    self.stitcher
                        .render_chunk(plan, rect, &mut reader, planes, cancel)?;
                stats = stats.and(measured);
                if let Some(sample) = planes.iter().find(|sample| !sample.is_finite()) {
                    return Err(Error::render(format!(
                        "the stitch rendered a sample that is not finite ({sample})"
                    )));
                }
                done += 1;
                control.set_progress(
                    Some(0.5 + 0.45 * done as f64 / chunks as f64),
                    &format!("Stitching {done} of {chunks}"),
                );
                if done == chunks {
                    control.set_phase("saving");
                }
                Ok(())
            },
        );
        accounting.give(fixed);
        // The window is dropped with the reader.
        drop(reader);
        written.map(|source| (source, stats))
    }

    /// The merge's provenance in the stack's order (`order[k]` the request position at `k`), with
    /// what rendering measured.
    fn provenance(
        &self,
        prepared: &PanoramaPrepared,
        analysis: &PanoramaAnalysis,
        order: &[usize],
        stats: ChunkStats,
    ) -> Result<MergeProvenance, Error> {
        let n = order.len();
        let mut at = vec![0; n];
        for (position, &input) in order.iter().enumerate() {
            at[input] = position;
        }
        let registration = &analysis.registration;
        let mut pairs: Vec<_> = registration
            .pairs
            .iter()
            .map(|pair| {
                let (a, b) = (at[pair.a], at[pair.b]);
                super::PairEvidence {
                    a: a.min(b),
                    b: a.max(b),
                    ..pair.clone()
                }
            })
            .collect();
        pairs.sort_by_key(|pair| (pair.a, pair.b));
        let lens = registration.lens;
        let provenance = MergeProvenance {
            format: MERGE_PROVENANCE_FORMAT,
            kind: MergeKind::Panorama,
            provider: self.provider.clone(),
            inputs: order
                .iter()
                .map(|&input| {
                    let item = &self.inputs[input];
                    let developed = &prepared.inputs[input];
                    MergeInput {
                        path: item.path.clone(),
                        volume_id: developed.volume.id.clone(),
                        signature: item.signature,
                        asset_id: item.asset_id.clone(),
                        frame: developed.frame.clone(),
                    }
                })
                .collect(),
            parameters: MergeParameters::Panorama(PanoramaParameters {
                reference: analysis.parameters.reference.map(|reference| at[reference]),
                ..analysis.parameters.clone()
            }),
            resolved: MergeResolution::Panorama(Box::new(PanoramaResolution {
                reference: at[registration.reference],
                reference_origin: registration.reference_origin,
                development: prepared.development.clone(),
                exposure_scales: order
                    .iter()
                    .map(|&input| analysis.exposure_scales[input])
                    .collect(),
                lens: PanoramaLensResolution {
                    profile: prepared.lens.as_ref().map(LensBake::record),
                    width: lens.width,
                    height: lens.height,
                    focal_px: lens.focal_px,
                    focal_origin: registration.focal_origin,
                    radial: lens.radial,
                    radial_origin: registration.radial_origin,
                },
                rotations: order
                    .iter()
                    .map(|&input| registration.rotations[input])
                    .collect(),
                frames: order
                    .iter()
                    .map(|&input| registration.frames[input].clone())
                    .collect(),
                pairs,
                levelling: match registration.levelling {
                    Levelling::Contradicted { frame, roll_deg } => Levelling::Contradicted {
                        frame: at[frame],
                        roll_deg,
                    },
                    levelled => levelled,
                },
                rms_residual_px: registration.rms_residual_px,
                max_residual_px: registration.max_residual_px,
                stitch: {
                    let mut stitch = analysis.plan.record.permuted(order);
                    stitch.seams.clipped_px = stats.clipped_pixels;
                    stitch.seams.max_overshoot = f64::from(stats.max_overshoot);
                    stitch
                },
            })),
        };
        provenance.validate()?;
        Ok(provenance)
    }
}

/// Register `prepared`'s frames with `parameters` (keeping `previous`'s registration when its
/// reference was the same) and plan the stitch: the matching stage, shared by a merge, a preview
/// and its updates. Every refusal is worded by the frames' file names.
pub(crate) fn analyse(
    prepared: &PanoramaPrepared,
    parameters: &PanoramaParameters,
    previous: Option<&PanoramaAnalysis>,
    stitcher: &dyn Stitcher,
    hooks: &Hooks,
    control: &JobControl,
    accounting: &mut Accounting,
) -> Result<PanoramaAnalysis, Error> {
    let names = prepared.names();
    let cancel = control.render_cancel();
    control.checkpoint()?;
    control.set_phase("matching");
    report(control, Stage::Matching, accounting);
    control.set_progress(Some(0.4), "Matching");
    hooks.at(Stage::Matching);
    let registration =
        match previous.filter(|previous| previous.parameters.reference == parameters.reference) {
            Some(previous) => previous.registration.clone(),
            None => {
                let n = prepared.inputs.len();
                let bound = super::registration::scratch_bytes_for(n) as u64;
                accounting.take(bound, "registering the frames")?;
                let frames = prepared.frames();
                let index = crate::modules::lens::index::shared().ok();
                let options = RegistrationOptions {
                    reference: parameters.reference,
                    radial: if prepared.lens.is_some() {
                        RadialMode::Fixed { radial: 0.0 }
                    } else {
                        RadialMode::Fit
                    },
                    focal: prepared
                        .inputs
                        .first()
                        .and_then(|input| focal_prior(&input.header, index.as_deref())),
                    capture_order: Some(capture_order(&frames)),
                };
                let registered =
                    register_with_progress(&prepared.features, &options, cancel, &mut |stage| {
                        control.set_progress(
                            Some(0.4),
                            match stage {
                                RegistrationStage::Matching => "Matching",
                                _ => "Registering",
                            },
                        );
                    });
                accounting.give(bound);
                registered.map_err(|error| suitability::named(error, &names))?
            }
        };
    control.checkpoint()?;
    let exposure_scales = suitability::exposure_scales(&prepared.frames(), registration.reference);
    let plan = stitcher
        .plan(
            &StitchRequest {
                registration: &registration,
                colour: &prepared.colour,
                raw: prepared.raw,
                exposure_scales: &exposure_scales,
                projection: parameters.projection,
                boundary_warp: parameters.boundary_warp,
                limits: OutputLimits::DERIVED,
            },
            cancel,
        )
        .map_err(|error| suitability::named(error, &names))?;
    if !OutputLimits::DERIVED.fits(plan.width, plan.height) || plan.width == 0 || plan.height == 0 {
        return Err(suitability::named(
            Error::validation(format!(
                "the panorama would be {} × {}, beyond what a derived source may hold",
                plan.width, plan.height
            ))
            .with_data(json!({
                "kind": "panorama",
                "reason": "output-too-large",
                "frames": (0..prepared.inputs.len()).collect::<Vec<_>>(),
            })),
            &names,
        ));
    }
    report(control, Stage::Matching, accounting);
    Ok(PanoramaAnalysis {
        parameters: parameters.clone(),
        registration,
        exposure_scales,
        plan,
    })
}
