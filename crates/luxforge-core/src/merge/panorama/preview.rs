//! A panorama's merge preview (`docs/design/panorama-merge.md`, "The merge preview"): what a
//! `merge-preview` job of kind `panorama` runs off the owner, composing the job's own stages
//! ([`super::job`]) with the stitch's preview ([`Stitcher::preview`]).
//!
//! - **Preparing** ([`PanoramaSpec::preview`]) reads and spools the frames exactly as a merge does,
//!   into the preview's own folder, registers them, plans the stitch and draws the revision. The
//!   [`PanoramaPrepared`] frames and the [`PanoramaAnalysis`] are kept for the session.
//! - **Updating** ([`PanoramaUpdate`]) reads no frame: a projection or Boundary Warp change plans
//!   the stitch again on the retained registration (a redraw); a reference change registers again
//!   on the retained features. Then it draws.
//! - **Drawing** renders the whole stitch at preview scale (at most [`PREVIEW_SIDE`] a side) from
//!   the colour proxies, encodes the display-referred JPEG for that revision, and reports the
//!   resolved projection, field of view and crop loss, the overlays (covered region, crop
//!   rectangle and frame outlines numbered in the stack's order) and the output the merge saves.
//!
//! A refusal the frames earn is the revision's answer ([`Drawn::Refused`]), so the session stays
//! open for another setting.
use super::{
    job::{PanoramaAnalysis, PanoramaPrepared, PanoramaSpec, analyse, stack_order},
    stitching::{PreviewStitch, Stitcher},
};
use crate::{
    Error,
    catalog_types::{
        MergeOutput, MergePreviewDiagnostics, MergePreviewOverlays, PanoramaParameters,
        PanoramaPreviewDiagnostics,
    },
    jobs::JobControl,
    merge::{
        hdr::{FusionDiagnostics, PixelRect, PreviewPicture, encode_preview_jpeg},
        job::{Accounting, Hooks, Limits, RESULT_PIXEL, Stage, report},
        preview::{Drawn, Picture, Pictures, refusal},
    },
};
use std::sync::Arc;

/// The preview picture's longest side.
pub(crate) const PREVIEW_SIDE: u32 = 1536;

/// What a panorama preview's first job leaves.
pub(crate) struct PanoramaPreviewed {
    pub prepared: Option<Arc<PanoramaPrepared>>,
    pub analysis: Option<Arc<PanoramaAnalysis>>,
    pub drawn: Drawn,
}

impl PanoramaSpec {
    /// A preview's first job: read and spool into the preview's folder, register, plan and draw
    /// `revision`. A refusal is the answer, not the failure; any other error fails the job.
    pub(crate) fn preview(
        &self,
        pictures: &Pictures,
        revision: u32,
        control: &JobControl,
    ) -> Result<PanoramaPreviewed, Error> {
        let mut accounting = Accounting::of(&self.limits);
        let prepared = match self.read_into(control, &mut accounting, true) {
            Ok(prepared) => Arc::new(prepared),
            Err(error) => {
                let refused = refusal(&error).ok_or(error)?;
                return Ok(PanoramaPreviewed {
                    prepared: None,
                    analysis: None,
                    drawn: Drawn::Refused(refused),
                });
            }
        };
        let analysis = match self.analyse(&prepared, None, control, &mut accounting) {
            Ok(analysis) => Arc::new(analysis),
            Err(error) => {
                let refused = refusal(&error).ok_or(error)?;
                return Ok(PanoramaPreviewed {
                    prepared: Some(prepared),
                    analysis: None,
                    drawn: Drawn::Refused(refused),
                });
            }
        };
        let picture = draw(
            &prepared,
            &analysis,
            self.stitcher.as_ref(),
            pictures,
            revision,
            &self.hooks,
            control,
            &mut accounting,
        )?;
        Ok(PanoramaPreviewed {
            prepared: Some(prepared),
            analysis: Some(analysis),
            drawn: Drawn::Picture(picture),
        })
    }
}

/// What a panorama update is given: the session's frames and its last analysis, the new options
/// and where its picture goes.
pub(crate) struct PanoramaUpdate {
    pub prepared: Arc<PanoramaPrepared>,
    pub previous: Option<Arc<PanoramaAnalysis>>,
    pub parameters: PanoramaParameters,
    pub stitcher: Arc<dyn Stitcher>,
    pub limits: Limits,
    pub hooks: Hooks,
    pub pictures: Pictures,
    pub revision: u32,
}

impl PanoramaUpdate {
    /// Draw the new revision from the retained features and proxies, reading no frame. Answers the
    /// revision's analysis (none when refused) and its answer.
    pub(crate) fn run(
        self,
        control: &JobControl,
    ) -> Result<(Option<Arc<PanoramaAnalysis>>, Drawn), Error> {
        let mut accounting = Accounting::of(&self.limits);
        accounting.temp_bytes = self.prepared.temp_bytes;
        accounting.take(self.prepared.held_bytes(), "the preview's proxies")?;
        let analysis = match analyse(
            &self.prepared,
            &self.parameters,
            self.previous.as_deref(),
            self.stitcher.as_ref(),
            &self.hooks,
            control,
            &mut accounting,
        ) {
            Ok(analysis) => Arc::new(analysis),
            Err(error) => {
                let refused = refusal(&error).ok_or(error)?;
                return Ok((None, Drawn::Refused(refused)));
            }
        };
        let picture = draw(
            &self.prepared,
            &analysis,
            self.stitcher.as_ref(),
            &self.pictures,
            self.revision,
            &self.hooks,
            control,
            &mut accounting,
        )?;
        Ok((Some(analysis), Drawn::Picture(picture)))
    }
}

/// Draw `analysis`'s stitch of `prepared` as `revision`: see the module documentation.
#[allow(clippy::too_many_arguments)]
fn draw(
    prepared: &PanoramaPrepared,
    analysis: &PanoramaAnalysis,
    stitcher: &dyn Stitcher,
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
    // The picture, and its display-referred encoding beside it.
    let held = 2 * PreviewStitch::bytes_for(PREVIEW_SIDE);
    accounting.take(held, "the preview's picture")?;
    let drawn = (|| {
        let stitch = stitcher.preview(&analysis.plan, &prepared.colour, PREVIEW_SIDE, cancel)?;
        let picture = PreviewPicture {
            rect: PixelRect {
                x: 0,
                y: 0,
                width: stitch.width as u32,
                height: stitch.height as u32,
            },
            factor: 1,
            width: stitch.width,
            height: stitch.height,
            rgb: stitch.rgb,
            diagnostics: FusionDiagnostics::default(),
        };
        let jpeg = encode_preview_jpeg(&picture, cancel)?;
        control.checkpoint()?;
        let path = pictures.write(revision, &jpeg)?;
        Ok::<_, Error>((picture.width, picture.height, stitch.overlays, path))
    })();
    accounting.give(held);
    let (width, height, mut overlays, path) = drawn?;
    let order = stack_order(&analysis.registration);
    let mut at = vec![0; order.len()];
    for (position, &frame) in order.iter().enumerate() {
        at[frame] = position;
    }
    for outline in &mut overlays.frames {
        if let Some(&position) = at.get(outline.frame as usize) {
            outline.frame = position as u32;
        }
    }
    overlays.frames.sort_by_key(|outline| outline.frame);
    let record = &analysis.plan.record;
    let proxy_side = prepared.colour.first().map_or(0, |proxy| {
        let (w, h) = proxy.dimensions();
        w.max(h) as u32
    });
    report(control, Stage::Drawing, accounting);
    Ok(Picture {
        path,
        width: width as u32,
        height: height as u32,
        diagnostics: MergePreviewDiagnostics::Panorama(PanoramaPreviewDiagnostics {
            proxy_side,
            projection: record.projection,
            field_of_view_deg: record.field_of_view_deg,
            crop_loss: record.crop_loss,
            reference: at[analysis.registration.reference] as u32,
            max_residual_px: analysis.registration.max_residual_px,
            frames: order
                .iter()
                .filter_map(|&frame| prepared.inputs.get(frame))
                .map(|input| input.path.clone())
                .collect(),
        }),
        overlays: MergePreviewOverlays::Panorama(overlays),
        output: MergeOutput {
            width: analysis.plan.width,
            height: analysis.plan.height,
            scale: record.scale,
            source_bytes: u64::from(analysis.plan.width)
                * u64::from(analysis.plan.height)
                * RESULT_PIXEL,
        },
    })
}
