//! The merge preview's work (`docs/design/panorama-merge.md`, "The merge preview";
//! `docs/design/hdr-merge.md`, "The merge preview"): what a `merge-preview` job runs off the
//! owner, composing the merge job's own stages ([`crate::merge::job`]) with the preview's numerics
//! ([`crate::merge::hdr::preview`]). The owner keeps the session (`crate::api` `merge.preview*`).
//!
//! - **Preparing** ([`JobSpec::preview`]) reads and spools the bracket exactly as a merge does,
//!   building each input's [`ColourProxy`] beside its analysis proxy, aligns it, and draws the
//!   revision's picture. The [`Prepared`] bracket and the [`Analysis`] are kept for the session.
//! - **Updating** ([`update`]) reads no frame: a change of deghosting alone finds what moved again
//!   on the retained proxies (a redraw); any other change aligns them again. Then it draws.
//! - **Drawing** ([`draw`]) fuses the colour proxies with the resolved exposures, registration and
//!   motion, encodes the display-referred JPEG to the preview's picture for that revision (one file
//!   a revision, the preview's older ones removed), and reports the diagnostics, the deghost
//!   overlay and the output the merge will save.
//!
//! A refusal the bracket earns (a typed HDR refusal: `reason`, the inputs it names, `kind`) is the
//! revision's answer rather than the job's failure, so its session stays open for another setting
//! ([`Drawn::Refused`]).
//!
//! **Bounds.** A preview is held to its kind's scratch limit as a merge is ([`fits`]): beside the
//! merge's own phases it holds every input's colour proxy (14 bytes a proxy pixel), and while it
//! draws the picture (12 bytes a proxy pixel), one band of fusion and the overlay's labels (4
//! bytes a proxy pixel).
use crate::{
    Error, ErrorKind,
    atomic_file::file_error,
    catalog_types::{
        HdrParameters, HdrPreviewDiagnostics, HdrPreviewOverlays, MergeOutput,
        MergePreviewDiagnostics, MergePreviewOverlays,
    },
    jobs::JobControl,
    merge::{
        hdr::{
            AnalysisProxy, ColourProxy, PreviewPicture, SensorLevel, encode_preview_jpeg,
            fuse_preview, max_residual_px, motion_overlays, preview_proxy_side,
            preview_scratch_bytes,
        },
        job::{
            self, Accounting, Analysis, Hooks, JobSpec, Limits, Prepared, Stage, analyse, report,
        },
    },
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

/// One result pixel's stored bytes: what the output estimate counts.
const RESULT_PIXEL: u64 = 12;

/// What a preview's first job leaves.
pub(crate) struct Previewed {
    /// The bracket read, kept for updates and the merge; none when reading refused it.
    pub prepared: Option<Arc<Prepared>>,
    /// The revision's alignment; none when it was refused.
    pub analysis: Option<Arc<Analysis>>,
    pub drawn: Drawn,
}

/// A revision's answer: its picture and data, or the refusal the bracket earned with its options.
pub(crate) enum Drawn {
    Picture(Picture),
    Refused(Value),
}

/// A drawn revision: its JPEG, size, diagnostics, overlays and output estimate, of its kind.
pub(crate) struct Picture {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub diagnostics: MergePreviewDiagnostics,
    pub overlays: MergePreviewOverlays,
    pub output: MergeOutput,
}

/// Where one preview's pictures go: `<preview_id>-<revision>.jpg` in the preview cache's
/// merges folder.
#[derive(Clone, Debug)]
pub(crate) struct Pictures {
    pub folder: PathBuf,
    pub id: String,
}

impl Pictures {
    /// The picture of `revision`.
    pub(crate) fn path(&self, revision: u32) -> PathBuf {
        self.folder.join(format!("{}-{revision}.jpg", self.id))
    }

    /// Whether `name` is one of this preview's pictures (or a picture being written).
    fn holds(&self, name: &str) -> bool {
        name.strip_prefix(&self.id)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|rest| rest.ends_with(".jpg") || rest.ends_with(".jpg.tmp"))
    }

    /// Write `revision`'s picture through a temporary file and a rename, so a reader sees the
    /// whole file or none, and remove every other picture of this preview. Never flushed: a
    /// picture is disposable.
    pub(crate) fn write(&self, revision: u32, jpeg: &[u8]) -> Result<PathBuf, Error> {
        let path = self.path(revision);
        fs::create_dir_all(&self.folder)
            .map_err(|error| file_error(self.folder.display(), error.kind()))?;
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        if let Err(error) = fs::write(&temporary, jpeg) {
            let _ = fs::remove_file(&temporary);
            return Err(file_error(temporary.display(), error.kind()));
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(file_error(path.display(), error.kind()));
        }
        self.remove(Some(&path));
        Ok(path)
    }

    /// Remove this preview's pictures but `keep`.
    pub(crate) fn remove(&self, keep: Option<&Path>) {
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if Some(path.as_path()) != keep && self.holds(&entry.file_name().to_string_lossy()) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// The typed refusal `error` carries, as a preview answers it: its data (`reason`, the inputs it
/// names, `kind`) with its `message`. None for any other error.
pub(crate) fn refusal(error: &Error) -> Option<Value> {
    if !matches!(
        error.kind,
        ErrorKind::Validation | ErrorKind::UnsupportedInput
    ) {
        return None;
    }
    let data = error.data.as_deref()?;
    data.get("reason")?.as_str()?;
    let mut refusal = data.clone();
    refusal
        .as_object_mut()?
        .insert("message".into(), Value::String(error.detail.clone()));
    Some(refusal)
}

/// Refuse a preview of a `width × height` bracket of `inputs` frames whose phases would pass the
/// scratch limit, before anything is spooled: aligning with every colour proxy held, and drawing
/// with every proxy, the motion map, the picture, a band of fusion and the overlay's labels.
pub(crate) fn fits(
    accounting: &Accounting,
    inputs: usize,
    width: u32,
    height: u32,
) -> Result<(), Error> {
    let n = inputs as u64;
    let proxy = AnalysisProxy::bytes_for(width, height) as u64;
    let colour = ColourProxy::bytes_for(width, height) as u64;
    let fit = (crate::merge::hdr::exposure::MAX_FIT_SAMPLES
        * inputs
        * (3 * size_of::<f32>() + size_of::<SensorLevel>())) as u64;
    let aligning = n * colour + 2 * n * proxy + fit + proxy;
    let drawing = n * (proxy + colour) + proxy + drawing_bytes(inputs, width, height);
    let mut probe = Accounting {
        scratch_bytes: 0,
        ..*accounting
    };
    probe.take(aligning.max(drawing), "previewing the bracket")
}

/// What drawing holds beside the proxies and the motion map: the picture, one band of fusion and
/// the overlay's labels.
fn drawing_bytes(inputs: usize, width: u32, height: u32) -> u64 {
    let (pw, ph) = AnalysisProxy::dimensions_for(width, height);
    PreviewPicture::bytes_for(width, height) as u64
        + preview_scratch_bytes(inputs, pw) as u64
        + (pw * ph * size_of::<u32>()) as u64
}

impl JobSpec {
    /// A preview's first job: read and spool with colour proxies, align and draw `revision`. A
    /// refusal is the answer, not the failure; any other error fails the job, its spool removed.
    pub(crate) fn preview(
        &self,
        pictures: &Pictures,
        revision: u32,
        control: &JobControl,
    ) -> Result<Previewed, Error> {
        let mut accounting = Accounting::of(&self.limits);
        let prepared = match self.read(control, &mut accounting, true) {
            Ok(prepared) => Arc::new(prepared),
            Err(error) => {
                let refused = refusal(&error).ok_or(error)?;
                return Ok(Previewed {
                    prepared: None,
                    analysis: None,
                    drawn: Drawn::Refused(refused),
                });
            }
        };
        let analysis = match self.align(&prepared, control, &mut accounting) {
            Ok(analysis) => Arc::new(analysis),
            Err(error) => {
                let refused = refusal(&error).ok_or(error)?;
                return Ok(Previewed {
                    prepared: Some(prepared),
                    analysis: None,
                    drawn: Drawn::Refused(refused),
                });
            }
        };
        let picture = draw(
            &prepared,
            &analysis,
            pictures,
            revision,
            &self.hooks,
            control,
            &mut accounting,
        )?;
        Ok(Previewed {
            prepared: Some(prepared),
            analysis: Some(analysis),
            drawn: Drawn::Picture(picture),
        })
    }
}

/// What an update is given: the session's bracket and its last alignment, the new options and
/// where its picture goes.
pub(crate) struct Update {
    pub prepared: Arc<Prepared>,
    pub previous: Option<Arc<Analysis>>,
    pub parameters: HdrParameters,
    pub limits: Limits,
    pub hooks: Hooks,
    pub pictures: Pictures,
    pub revision: u32,
}

impl Update {
    /// Draw the new revision from the retained proxies, reading no frame: see the module
    /// documentation. Answers the revision's alignment (none when refused) and its answer.
    pub(crate) fn run(self, control: &JobControl) -> Result<(Option<Arc<Analysis>>, Drawn), Error> {
        let mut accounting = Accounting::of(&self.limits);
        accounting.temp_bytes = self.prepared.temp_bytes;
        accounting.take(self.prepared.proxies_bytes(), "the preview's proxies")?;
        let prepared = &self.prepared;
        let parameters = &self.parameters;
        control.set_phase("aligning");
        let aligned = match self.previous.as_deref() {
            // Only deghosting changed: the alignment stands, and what moved is found again.
            Some(previous)
                if previous.parameters.exposure == parameters.exposure
                    && previous.parameters.alignment.alignment
                        == parameters.alignment.alignment
                    && previous.parameters.fusion == parameters.fusion =>
            {
                control.set_progress(Some(0.5), "Drawing");
                job::motion(
                    &prepared.proxies,
                    &previous.exposures,
                    &previous.registration,
                    parameters,
                    control,
                    &mut accounting,
                )
                .map(|motion| Analysis {
                    parameters: parameters.clone(),
                    motion,
                    ..previous.clone()
                })
            }
            _ => {
                report(control, Stage::Aligning, &accounting);
                self.hooks.at(Stage::Aligning);
                control.set_progress(Some(0.2), "Aligning");
                analyse(
                    &prepared.frames(),
                    &prepared.proxies,
                    parameters,
                    control,
                    &mut accounting,
                )
            }
        };
        let analysis = match aligned {
            Ok(analysis) => Arc::new(analysis),
            Err(error) => {
                let refused = refusal(&error).ok_or(error)?;
                return Ok((None, Drawn::Refused(refused)));
            }
        };
        let picture = draw(
            prepared,
            &analysis,
            &self.pictures,
            self.revision,
            &self.hooks,
            control,
            &mut accounting,
        )?;
        Ok((Some(analysis), Drawn::Picture(picture)))
    }
}

/// Draw `analysis`'s picture of `prepared` as `revision` and report what the sheet shows beside
/// it: see the module documentation.
pub(crate) fn draw(
    prepared: &Prepared,
    analysis: &Analysis,
    pictures: &Pictures,
    revision: u32,
    hooks: &Hooks,
    control: &JobControl,
    accounting: &mut Accounting,
) -> Result<Picture, Error> {
    control.checkpoint()?;
    report(control, Stage::Drawing, accounting);
    hooks.at(Stage::Drawing);
    control.set_progress(Some(0.9), "Drawing");
    let cancel = control.render_cancel();
    let (width, height) = (prepared.width, prepared.height);
    let n = prepared.inputs.len();
    let held = drawing_bytes(n, width, height);
    accounting.take(held, "the preview's picture")?;
    let drawn = (|| {
        let picture = fuse_preview(
            &prepared.colour,
            &analysis.exposures,
            &analysis.registration,
            &analysis.coverage,
            analysis.motion.as_ref(),
            &analysis.parameters.fusion,
            cancel,
        )?;
        let motion = match &analysis.motion {
            Some(motion) => motion_overlays(motion, &analysis.exposures, picture.rect, cancel)?,
            None => Vec::new(),
        };
        let jpeg = encode_preview_jpeg(&picture, cancel)?;
        control.checkpoint()?;
        let path = pictures.write(revision, &jpeg)?;
        Ok::<_, Error>((picture, motion, path))
    })();
    accounting.give(held);
    let (picture, motion, path) = drawn?;
    let cover = analysis.coverage.rect;
    report(control, Stage::Drawing, accounting);
    Ok(Picture {
        path,
        width: picture.width as u32,
        height: picture.height as u32,
        diagnostics: MergePreviewDiagnostics::Hdr(HdrPreviewDiagnostics {
            proxy_side: preview_proxy_side(width, height),
            alignment: analysis.parameters.alignment.alignment,
            max_residual_px: max_residual_px(&analysis.registration),
            deghost: analysis.parameters.alignment.deghost,
            highlights_extended: picture.highlights_extended(),
            shadows_extended: picture.shadows_extended(),
            exposures: analysis
                .exposures
                .inputs
                .iter()
                .map(|input| input.offset_ev)
                .collect(),
            reference: analysis.exposures.reference as u32,
        }),
        overlays: MergePreviewOverlays::Hdr(HdrPreviewOverlays { motion }),
        output: MergeOutput {
            width: cover.width,
            height: cover.height,
            scale: 1.0,
            source_bytes: u64::from(cover.width) * u64::from(cover.height) * RESULT_PIXEL,
        },
    })
}

/// Remove what open previews no longer account for: every folder under `root` (the previews'
/// spools) and every picture in `pictures` whose preview `keep` does not name. What each merge or
/// preview job does as it starts, so a preview a stopped process left goes with the next job.
pub(crate) fn sweep(root: &Path, pictures: &Path, keep: &dyn Fn(&str) -> bool) -> usize {
    let mut removed = 0;
    if let Ok(folders) = fs::read_dir(root) {
        for folder in folders.flatten() {
            let name = folder.file_name().to_string_lossy().into_owned();
            if !keep(&name) && fs::remove_dir_all(folder.path()).is_ok() {
                removed += 1;
            }
        }
    }
    if let Ok(files) = fs::read_dir(pictures) {
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let preview = name.rsplit_once('-').map_or(name.as_str(), |(id, _)| id);
            if !keep(preview) && fs::remove_file(file.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}
