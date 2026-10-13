//! The seam between the panorama job and the stitch (`docs/design/panorama-merge.md`, "Projection
//! and output size", "Exposure, colour and seams", "Boundary Warp and the crop"): what the job asks
//! of a [`Stitcher`] once registration has placed the frames, and nothing of how it answers.
//!
//! 1. **Plan** ([`Stitcher::plan`]) from the registration and the frames' colour proxies alone:
//!    the resolved projection and field of view, the output scale (1 at the reference's centre
//!    pixel, below 1 only to fit [`OutputLimits`]), the bounding rectangle and the crop the result
//!    is, the gains, seams and Boundary Warp mesh ([`PanoramaStitch`]), and what rendering holds.
//!    A stitch refusal (`ExcessiveFieldOfView`, `OutputTooLarge`, `InsufficientCoverage`) is a
//!    typed error naming frames by request position (`data.kind` `panorama`), as registration's
//!    are; the job words it by file name.
//! 2. **Render a chunk** ([`Stitcher::render_chunk`]) of the result straight into the writer's
//!    planar chunk buffer, reading the frames' full-resolution pixels back from the spool through
//!    [`FrameWindows`]: one window at a time, each at most [`MAX_WINDOW_PIXELS`]. Output is the
//!    same for any split into chunks. It answers what it measured ([`ChunkStats`]): the job
//!    records the clipped pixels and the blend's largest overshoot.
//! 3. **Preview** ([`Stitcher::preview`]): the whole stitch at preview scale from the colour
//!    proxies, with the overlays' data in the picture's pixels.
//!
//! Frames are by request position throughout; the job turns them into the stack's order.
//!
//! This build's production stitch is [`production`]: [`PanoramaStitcher`], the stitch of
//! [`super::stitch`]. A test supplies its own, or none to refuse panoramas `not-ready`.
pub(crate) use super::stitch::{ChunkStats, FrameWindows, MAX_WINDOW_PIXELS};
use super::{
    GainMode, Projection, ProjectionChoice, Refusal, StitchOptions, layout,
    stitch::{self, plan_with_exposure},
};
use crate::{
    Cancel, Error,
    catalog_types::{
        PanoramaFrameOutline, PanoramaPreviewOverlays, PanoramaProjection, PanoramaSeams,
        PanoramaStitch, PanoramaWarpMesh,
    },
    merge::{
        hdr::{ColourProxy, PixelRect},
        panorama::PanoramaRegistration,
    },
};
use std::{any::Any, sync::Arc};

#[cfg(test)]
#[path = "stitching_simple.rs"]
pub(crate) mod simple;

/// The largest output a stitch may plan: the paged derived-source limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutputLimits {
    pub max_side: u32,
    pub max_pixels: u64,
}

impl OutputLimits {
    /// A derived source's limits (`crate::source::PAGED_DERIVED_SOURCE`).
    pub(crate) const DERIVED: Self = Self {
        max_side: crate::source::PAGED_DERIVED_SOURCE.max_side,
        max_pixels: crate::source::PAGED_DERIVED_SOURCE.max_pixels,
    };

    /// Whether a `width × height` output is within them.
    pub(crate) fn fits(&self, width: u32, height: u32) -> bool {
        width.max(height) <= self.max_side
            && u64::from(width) * u64::from(height) <= self.max_pixels
    }
}

/// What a stitch is planned from, every frame by request position.
pub(crate) struct StitchRequest<'a> {
    pub registration: &'a PanoramaRegistration,
    /// Each frame's colour proxy (`crate::merge::hdr::ColourProxy`): its mean linear samples and
    /// largest clipping levels on a grid of at most 1,024 a side.
    pub colour: &'a [ColourProxy],
    /// The frames are RAW (one gain a frame) rather than JPEG (one gain a channel).
    pub raw: bool,
    /// Each frame's scale to the reference's exposure from its settings: the photometric fit's
    /// starting point (1 for JPEG frames).
    pub exposure_scales: &'a [f64],
    pub projection: PanoramaProjection,
    /// 0 to 100.
    pub boundary_warp: u8,
    pub limits: OutputLimits,
}

/// A planned stitch: the result's size, what the provenance records of it, what rendering holds,
/// and the stitcher's own state.
#[derive(Clone)]
pub(crate) struct StitchPlan {
    /// The result: the crop, at the plan's scale.
    pub width: u32,
    pub height: u32,
    /// Per frame by request position.
    pub record: PanoramaStitch,
    /// What the plan holds while it is kept: seam maps, the warp mesh.
    pub plan_bytes: u64,
    /// What rendering one chunk holds beside the frame window it reads and the chunk it writes.
    pub chunk_bytes: u64,
    pub inner: Arc<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for StitchPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StitchPlan")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("record", &self.record)
            .finish_non_exhaustive()
    }
}

/// The preview's picture of the whole stitch, linear, and its overlays in its pixels (frames by
/// request position).
pub(crate) struct PreviewStitch {
    pub width: usize,
    pub height: usize,
    /// Planar linear sRGB; uncovered pixels are 0.
    pub rgb: [Vec<f32>; 3],
    pub overlays: PanoramaPreviewOverlays,
}

impl PreviewStitch {
    /// The bytes a preview picture of at most `side` pixels a side holds.
    pub(crate) fn bytes_for(side: u32) -> u64 {
        u64::from(side) * u64::from(side) * 12
    }
}

/// The stitch: see the module documentation.
pub(crate) trait Stitcher: Send + Sync {
    /// Plan the stitch of the registered frames.
    fn plan(&self, request: &StitchRequest<'_>, cancel: &Cancel) -> Result<StitchPlan, Error>;

    /// Render `chunk` (in the result's pixels) into `out`, three planes of `chunk`'s pixels, red
    /// then green then blue, reading the frames (by request position) through `windows`, one
    /// window of at most [`MAX_WINDOW_PIXELS`] at a time.
    fn render_chunk(
        &self,
        plan: &StitchPlan,
        chunk: PixelRect,
        windows: &mut dyn FrameWindows,
        out: &mut [f32],
        cancel: &Cancel,
    ) -> Result<ChunkStats, Error>;

    /// The preview's picture of the stitch, at most `side` pixels a side, from the colour proxies.
    fn preview(
        &self,
        plan: &StitchPlan,
        colour: &[ColourProxy],
        side: u32,
        cancel: &Cancel,
    ) -> Result<PreviewStitch, Error>;
}

/// This build's panorama stitch: [`PanoramaStitcher`].
pub(crate) fn production() -> Option<Arc<dyn Stitcher>> {
    Some(Arc::new(PanoramaStitcher))
}

/// Why a build without a panorama stitch cannot merge panoramas.
pub(crate) const UNWIRED: &str = "this build has no panorama stitch";

/// The production stitch ([`super::stitch`]) behind the job's seam.
pub(crate) struct PanoramaStitcher;

impl PanoramaStitcher {
    fn inner(plan: &StitchPlan) -> Result<&stitch::StitchPlan, Error> {
        plan.inner
            .downcast_ref::<stitch::StitchPlan>()
            .ok_or_else(|| Error::internal("a stitch plan of another stitcher"))
    }
}

impl Stitcher for PanoramaStitcher {
    fn plan(&self, request: &StitchRequest<'_>, cancel: &Cancel) -> Result<StitchPlan, Error> {
        let n = request.colour.len();
        let options = StitchOptions {
            projection: match request.projection {
                PanoramaProjection::Auto => ProjectionChoice::Auto,
                PanoramaProjection::Perspective => ProjectionChoice::Perspective,
                PanoramaProjection::Cylindrical => ProjectionChoice::Cylindrical,
                PanoramaProjection::Spherical => ProjectionChoice::Spherical,
            },
            boundary_warp: request.boundary_warp,
            gains: if request.raw {
                GainMode::PerFrame
            } else {
                GainMode::PerChannel
            },
        };
        let limits = layout::OutputLimits {
            max_side: request.limits.max_side,
            max_pixels: request.limits.max_pixels,
            max_bytes: request.limits.max_pixels.saturating_mul(12),
        };
        let plan = plan_with_exposure(
            request.registration,
            request.colour,
            &options,
            request.exposure_scales,
            &limits,
            cancel,
            &mut |_| {},
        )
        .map_err(|error| naming_every_frame(error, n))?;
        let record = record(&plan);
        let (width, height) = plan.output_size();
        Ok(StitchPlan {
            width,
            height,
            record,
            plan_bytes: plan.bytes() as u64,
            chunk_bytes: stitch::tile_scratch_bytes() as u64,
            inner: Arc::new(plan),
        })
    }

    fn render_chunk(
        &self,
        plan: &StitchPlan,
        chunk: PixelRect,
        windows: &mut dyn FrameWindows,
        out: &mut [f32],
        cancel: &Cancel,
    ) -> Result<ChunkStats, Error> {
        Self::inner(plan)?.render_into(chunk, windows, out, cancel)
    }

    fn preview(
        &self,
        plan: &StitchPlan,
        colour: &[ColourProxy],
        side: u32,
        cancel: &Cancel,
    ) -> Result<PreviewStitch, Error> {
        let preview = Self::inner(plan)?.preview(colour, side, cancel)?;
        let overlays = preview.overlays;
        Ok(PreviewStitch {
            width: preview.width,
            height: preview.height,
            rgb: preview.rgb,
            overlays: PanoramaPreviewOverlays {
                covered: overlays.covered,
                crop: overlays.crop,
                frames: overlays
                    .frames
                    .into_iter()
                    .map(|outline| PanoramaFrameOutline {
                        frame: outline.frame as u32,
                        outline: outline.outline,
                    })
                    .collect(),
            },
        })
    }
}

/// A stitch refusal naming every frame, as the job words refusals by the frames' names.
fn naming_every_frame(error: Error, frames: usize) -> Error {
    if !matches!(
        Refusal::of(&error),
        Some(
            Refusal::ExcessiveFieldOfView { .. }
                | Refusal::OutputTooLarge { .. }
                | Refusal::InsufficientCoverage { .. }
        )
    ) {
        return error;
    }
    let mut data = error.data.as_deref().cloned().unwrap_or_default();
    if let Some(object) = data.as_object_mut() {
        object.insert("frames".into(), (0..frames).collect::<Vec<_>>().into());
    }
    error.with_data(data)
}

/// What the provenance records of a plan, every frame by request position.
fn record(plan: &stitch::StitchPlan) -> PanoramaStitch {
    let diagnostics = plan.diagnostics();
    let n = plan.gains().len();
    let mut seam_px = vec![0_u64; n];
    for seam in &diagnostics.seams {
        for frame in [seam.a, seam.b] {
            if let Some(length) = seam_px.get_mut(frame) {
                *length += seam.length_px;
            }
        }
    }
    let mut contributing_px = vec![0_u64; n];
    for contribution in &diagnostics.contributions {
        if let Some(pixels) = contributing_px.get_mut(contribution.frame) {
            *pixels += contribution.pixels;
        }
    }
    let amount = f64::from(diagnostics.boundary_warp) / 100.0;
    let mesh = plan.mesh();
    PanoramaStitch {
        projection: match diagnostics.projection {
            Projection::Perspective => PanoramaProjection::Perspective,
            Projection::Cylindrical => PanoramaProjection::Cylindrical,
            Projection::Spherical => PanoramaProjection::Spherical,
        },
        field_of_view_deg: [
            diagnostics.field.horizontal_deg,
            diagnostics.field.vertical_deg,
        ],
        scale: diagnostics.scale,
        bounds_width: plan.geometry().width,
        bounds_height: plan.geometry().height,
        crop: plan.crop(),
        crop_loss: diagnostics.crop_loss,
        gains: plan.gains().iter().map(|gain| gain.gain).collect(),
        seams: PanoramaSeams {
            seam_px,
            contributing_px,
            clipped_px: diagnostics.clipped_pixels_estimate,
            max_overshoot: 0.0,
        },
        warp: (diagnostics.boundary_warp > 0).then(|| PanoramaWarpMesh {
            columns: mesh.columns,
            rows: mesh.rows,
            displacements: mesh
                .displacement
                .iter()
                .map(|d| [(d[0] * amount) as f32, (d[1] * amount) as f32])
                .collect(),
        }),
    }
}
