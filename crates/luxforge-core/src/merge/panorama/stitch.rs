//! Stitching registered frames (`docs/design/panorama-merge.md`, "Projection and output size",
//! "Exposure, colour and seams", "Boundary Warp and the crop").
//!
//! A merge stitches in two steps:
//!
//! 1. [`plan_stitch`] lays out the output from the registration and the frames' colour proxies,
//!    with bounded memory and without reading a frame: the projection ([`super::projection`]),
//!    the bounding rectangle and scale ([`super::layout`]), the plan grid, coverage, gains
//!    ([`super::photometric`]), seams ([`super::seams`]), the blend's correction field
//!    ([`super::blend`]), the warp mesh ([`super::warp`]), the crop, the stack order and the
//!    diagnostics. [`StitchPlan::with_boundary_warp`] replans only the warp and crop, and
//!    [`StitchPlan::preview`] draws the stitch from the colour proxies with its overlays.
//! 2. [`StitchPlan::render_chunk`] draws any rectangle of the output from windows of the frames'
//!    full-resolution pixels, which a caller reads on request. Each output pixel depends only on
//!    its own position and the plan, so the output is bit-identical for any split into chunks.
//!
//! **The plan grid** covers the bounding rectangle with square cells of `g` output pixels: the
//! reference's colour-proxy factor at the output's scale (at least 1), grown until the grid is at
//! most [`PLAN_MAX_SIDE`] a side and [`PLAN_MAX_CELLS`] cells. Cell `(i, j)` holds output pixels
//! `[i·g, (i + 1)·g) × [j·g, (j + 1)·g)` and its centre is `(i·g + (g − 1)/2, j·g + (g − 1)/2)`.
//! Its corner pixels are its four outermost pixels within the bounding rectangle.
//!
//! **Coverage.** A frame covers a cell when every corner pixel of the cell lies inside it; a
//! cell is covered when every corner pixel lies inside some frame. The crop at any Boundary Warp
//! amount is the largest rectangle of covered cells (largest area, then the smallest top row,
//! bottom row and left column) of the output grid, each corner pixel taken through the warp, in
//! output pixels clipped to the bounding rectangle. A crop under a quarter of the bounding
//! rectangle on a side is refused ([`Refusal::InsufficientCoverage`]).
//!
//! **An output pixel.** Result pixel `(X, Y)` is bounding-rectangle point `b = crop origin +
//! (X, Y)`, shown by stitch point `p = b + a·D(b)` ([`super::warp`]) and world direction `d` of `p`
//! ([`OutputGeometry::direction`]). Its seam weights are the bilinear weights of the four plan
//! cells around `p` (cell coordinates `(p − (g − 1)/2)/g`, clamped to the grid) gathered by label;
//! a labelled frame that does not cover `p` is dropped and the rest renormalized. With none left,
//! the covering frame `p` lies deepest inside takes it whole; with none covering, the frame it lies
//! nearest is sampled at its clamped edge and the pixel counted uncovered. Each frame is sampled
//! bilinearly (clamped taps) at `d`'s position, times its gain, and the pixel is
//! `Σ wᵢ·gᵢ·Fᵢ + C(p)`, accumulated in `f64` in ascending frame order and rounded once to `f32`.
//! Below full scale each pixel averages an `n × n` grid of such points, `n = ⌈1/k⌉` at most
//! [`MAX_SUPERSAMPLE`], at offsets `((s + ½)/n − ½)` of a pixel.

use super::blend::{self, BLEND_SIGMA_CELLS};
use super::camera::{Lens, Rotation};
use super::layout::{
    self, Footprints, OutputGeometry, OutputLimits, depth, frame_position, inside,
};
use super::linalg::V3;
use super::photometric::{
    FrameGain, GAIN_FLOOR, GAIN_LATTICE_CELLS, GAIN_MAX_GRADIENT, GainMode, GainSamples,
};
use super::projection::{Field, Projection, ProjectionChoice};
use super::refusal::Refusal;
use super::registration::{MAX_FRAMES, MIN_FRAMES, PanoramaRegistration};
use super::seams::{self, NONE, SeamGrid, SeamSample};
use super::warp::{self, WarpMesh};
use crate::merge::hdr::{ColourProxy, FrameRect, PixelRect, SensorLevel, SourceWindow};
use crate::{Cancel, Error};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// The plan grid's longest side, in cells.
pub const PLAN_MAX_SIDE: usize = 4096;
/// The plan grid's most cells.
pub const PLAN_MAX_CELLS: usize = 1 << 20;
/// The most points a side an output pixel averages below full scale.
pub const MAX_SUPERSAMPLE: u32 = 4;
/// Output points prepared at once while rendering.
pub const MAX_TILE_POINTS: usize = 1 << 18;
/// The most pixels of one frame's window a render reads at once.
pub const MAX_WINDOW_PIXELS: u64 = 8 << 20;
/// The preview picture's default longest side.
pub const PREVIEW_MAX_SIDE: u32 = 2048;
/// Points an edge of each frame's outline in the preview's overlay.
pub const OUTLINE_SAMPLES: usize = 16;
/// A window's bytes a pixel: three `f32` samples and a [`SensorLevel`].
pub const WINDOW_PIXEL_BYTES: u64 = 14;

/// What a merge asks of the stitch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StitchOptions {
    pub projection: ProjectionChoice,
    /// 0–100.
    pub boundary_warp: u8,
    /// One gain a frame (RAW) or a channel (JPEG).
    pub gains: GainMode,
}

/// The stitch plan's stages, in order, as [`plan_stitch_with_progress`] reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StitchStage {
    Layout,
    Gains,
    Seams,
    Blend,
    Warp,
}

/// The plan grid. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanGrid {
    /// Output pixels a cell side.
    pub cell: u32,
    pub width: u32,
    pub height: u32,
}

impl PlanGrid {
    /// Cell `(x, y)`'s centre in the bounding rectangle.
    fn centre(&self, x: usize, y: usize) -> [f64; 2] {
        let g = f64::from(self.cell);
        [
            x as f64 * g + (g - 1.0) / 2.0,
            y as f64 * g + (g - 1.0) / 2.0,
        ]
    }

    /// Cell `(x, y)`'s corner pixels within a `width × height` bounding rectangle.
    fn corners(&self, x: usize, y: usize, width: u32, height: u32) -> [[f64; 2]; 4] {
        let g = self.cell as usize;
        let (x0, y0) = (x * g, y * g);
        let x1 = (x0 + g - 1).min(width as usize - 1);
        let y1 = (y0 + g - 1).min(height as usize - 1);
        let (x0, y0, x1, y1) = (x0 as f64, y0 as f64, x1 as f64, y1 as f64);
        [[x0, y0], [x1, y0], [x0, y1], [x1, y1]]
    }

    /// The four cells around point `p` and their bilinear weights.
    #[inline]
    fn around(&self, p: [f64; 2]) -> ([usize; 4], [f64; 4]) {
        let g = f64::from(self.cell);
        let axis = |v: f64, count: u32| -> (usize, usize, f64) {
            let c = (v - (g - 1.0) / 2.0) / g;
            let floor = c.floor();
            let last = f64::from(count - 1);
            let i0 = floor.clamp(0.0, last) as usize;
            let i1 = (floor + 1.0).clamp(0.0, last) as usize;
            (i0, i1, (c - floor).clamp(0.0, 1.0))
        };
        let (x0, x1, fx) = axis(p[0], self.width);
        let (y0, y1, fy) = axis(p[1], self.height);
        let w = self.width as usize;
        (
            [y0 * w + x0, y0 * w + x1, y1 * w + x0, y1 * w + x1],
            [
                (1.0 - fx) * (1.0 - fy),
                fx * (1.0 - fy),
                (1.0 - fx) * fy,
                fx * fy,
            ],
        )
    }

    /// The cell holding point `p`, clamped to the grid.
    fn cell_of(&self, p: [f64; 2]) -> (usize, usize) {
        let g = f64::from(self.cell);
        let clamp = |v: f64, count: u32| (v / g).floor().clamp(0.0, f64::from(count - 1)) as usize;
        (clamp(p[0], self.width), clamp(p[1], self.height))
    }
}

/// Where a render reads the frames' full-resolution pixels: one window at a time, each a
/// rectangle of one frame of at most [`MAX_WINDOW_PIXELS`] (planar linear RGB and each pixel's
/// [`SensorLevel`]). A read may replace the last window read.
pub trait FrameWindows {
    /// `rect` of frame `frame` (by request position), exactly.
    fn read(&mut self, frame: usize, rect: PixelRect) -> Result<SourceWindow<'_>, Error>;
}

/// A rectangle of one frame's full-resolution pixels, owned: planar linear RGB and each pixel's
/// [`SensorLevel`], row-major over `rect`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameWindow {
    pub rect: PixelRect,
    pub rgb: [Vec<f32>; 3],
    pub levels: Vec<SensorLevel>,
}

impl FrameWindow {
    pub fn source(&self) -> SourceWindow<'_> {
        SourceWindow {
            x: self.rect.x,
            y: self.rect.y,
            frame: FrameRect {
                width: self.rect.width as usize,
                height: self.rect.height as usize,
                stride: self.rect.width as usize,
                rgb: [&self.rgb[0], &self.rgb[1], &self.rgb[2]],
                levels: &self.levels,
            },
        }
    }
}

/// [`FrameWindows`] from a function answering each window as a [`FrameWindow`], holding the last.
pub struct WindowReader<F> {
    read: F,
    last: FrameWindow,
}

impl<F: FnMut(usize, PixelRect) -> Result<FrameWindow, Error>> WindowReader<F> {
    pub fn new(read: F) -> Self {
        Self {
            read,
            last: FrameWindow::default(),
        }
    }
}

impl<F: FnMut(usize, PixelRect) -> Result<FrameWindow, Error>> FrameWindows for WindowReader<F> {
    fn read(&mut self, frame: usize, rect: PixelRect) -> Result<SourceWindow<'_>, Error> {
        self.last = (self.read)(frame, rect)?;
        Ok(self.last.source())
    }
}

/// What rendering a chunk measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChunkStats {
    /// Pixels any of whose weighted taps was clipped.
    pub clipped_pixels: u64,
    /// Pixels no frame covered, drawn from the nearest frame's edge.
    pub uncovered_pixels: u64,
    /// The largest amount by which a pixel left the range of the samples it blends, linear.
    pub max_overshoot: f32,
}

impl ChunkStats {
    /// Both chunks' measures together.
    pub fn and(self, other: Self) -> Self {
        Self {
            clipped_pixels: self.clipped_pixels + other.clipped_pixels,
            uncovered_pixels: self.uncovered_pixels + other.uncovered_pixels,
            max_overshoot: self.max_overshoot.max(other.max_overshoot),
        }
    }
}

/// One rendered rectangle of the output.
#[derive(Clone, Debug, PartialEq)]
pub struct StitchedChunk {
    pub rect: PixelRect,
    /// Planar linear RGB, row-major over `rect`.
    pub rgb: [Vec<f32>; 3],
    pub stats: ChunkStats,
}

/// The seam between two frames, by length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeamLength {
    pub a: usize,
    pub b: usize,
    /// Neighbouring plan cells taken from the two frames, in output pixels.
    pub length_px: u64,
}

/// What one frame contributes, by the seam map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    pub frame: usize,
    /// Output pixels of the bounding rectangle taken from it.
    pub pixels: u64,
    /// Their bounds in the bounding rectangle.
    pub bounds: PixelRect,
}

/// The stitch's diagnostics, as provenance and the preview report them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StitchDiagnostics {
    pub projection: Projection,
    pub projection_choice: ProjectionChoice,
    pub field: Field,
    /// The output's scale: output pixels per input pixel at the reference's centre, at most 1.
    pub scale: f64,
    /// The bounding rectangle at full scale.
    pub full_width: u64,
    pub full_height: u64,
    pub boundary_warp: u8,
    /// The crop in the bounding rectangle.
    pub crop: PixelRect,
    /// Output pixels of the bounding rectangle the warped frames cover (whole cells).
    pub covered_pixels: u64,
    /// Covered pixels outside the crop.
    pub pixels_lost: u64,
    /// `pixels_lost / covered_pixels`.
    pub crop_loss: f64,
    pub gains: Vec<FrameGain>,
    pub seams: Vec<SeamLength>,
    pub contributions: Vec<Contribution>,
    /// Output pixels whose seam frame's proxy sample is clipped, from the plan grid.
    pub clipped_pixels_estimate: u64,
    pub max_registration_residual_px: f64,
    pub grid: PlanGrid,
    pub mesh_columns: u32,
    pub mesh_rows: u32,
    /// The low band's Gaussian, in output pixels.
    pub blend_sigma_px: f64,
    pub supersample: u32,
}

/// A frame's outline in the preview picture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameOutline {
    /// The frame, in request order.
    pub frame: usize,
    /// Its position in the stack, from 1.
    pub position: usize,
    /// Its closed outline, [`OUTLINE_SAMPLES`] points an edge clockwise from its top-left corner,
    /// in picture coordinates; a frame across a full circle's ends continues past the picture.
    pub outline: Vec<[f64; 2]>,
}

/// The preview's overlay data, in picture coordinates: picture pixel `(i, j)` covers
/// `[i, i + 1) × [j, j + 1)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanoramaOverlays {
    /// The covered region's outline: closed polygons along picture pixels' edges, each corner
    /// once, clockwise on screen around covered pixels (holes run the other way).
    pub covered: Vec<Vec<[f64; 2]>>,
    /// `[x, y, width, height]`.
    pub crop: [f64; 4],
    /// In stack order.
    pub frames: Vec<FrameOutline>,
    pub crop_loss: f64,
}

/// The preview picture: the whole warped bounding rectangle drawn from the colour proxies, linear
/// float32 sRGB/D65 at the reference's brightness, black where nothing is covered.
#[derive(Clone, Debug, PartialEq)]
pub struct PanoramaPreview {
    pub width: usize,
    pub height: usize,
    /// Output pixels a picture pixel side: picture pixel `(i, j)` shows bounding-rectangle point
    /// `(i·c + (c − 1)/2, j·c + (c − 1)/2)`.
    pub cell: u32,
    pub rgb: [Vec<f32>; 3],
    pub covered: Vec<bool>,
    pub overlays: PanoramaOverlays,
}

/// One output point's plan: its direction, the frames it blends with their weights and
/// positions, and the correction field there.
#[derive(Clone, Copy, Debug, Default)]
struct PointPlan {
    count: u8,
    frames: [u8; 4],
    weights: [f64; 4],
    positions: [[f64; 2]; 4],
    correction: [f64; 3],
    uncovered: bool,
}

/// A chunk being drawn: its rectangle, its three planes and what it measured.
struct Chunk<'a> {
    rect: PixelRect,
    out: &'a mut [f32],
    stats: ChunkStats,
}

/// A stitch laid out and ready to render. See the module documentation.
#[derive(Clone, Debug)]
pub struct StitchPlan {
    options: StitchOptions,
    lens: Lens,
    rotations: Vec<Rotation>,
    reference: usize,
    geometry: OutputGeometry,
    grid: PlanGrid,
    /// Per frame, the cells `[x₀, y₀, x₁, y₁)` it may cover.
    bounds: Vec<[usize; 4]>,
    /// Per cell, whether the frames cover it unwarped.
    covered: Vec<bool>,
    labels: Vec<u8>,
    correction: [Vec<f32>; 3],
    gains: Vec<FrameGain>,
    mesh: WarpMesh,
    amount: f64,
    crop: PixelRect,
    order: Vec<usize>,
    supersample: u32,
    /// The most pixels a frame window may hold: [`MAX_WINDOW_PIXELS`], smaller in tests.
    window_pixels: u64,
    diagnostics: StitchDiagnostics,
}

/// Plan the stitch of `registration`'s frames from their colour proxies, in request order. See the
/// module documentation.
pub fn plan_stitch(
    registration: &PanoramaRegistration,
    proxies: &[ColourProxy],
    options: &StitchOptions,
    cancel: &Cancel,
) -> Result<StitchPlan, Error> {
    plan_stitch_with_progress(registration, proxies, options, cancel, &mut |_| {})
}

/// [`plan_stitch`], calling `progress` as each stage starts.
pub fn plan_stitch_with_progress(
    registration: &PanoramaRegistration,
    proxies: &[ColourProxy],
    options: &StitchOptions,
    cancel: &Cancel,
    progress: &mut dyn FnMut(StitchStage),
) -> Result<StitchPlan, Error> {
    plan_with_limits(
        registration,
        proxies,
        options,
        &OutputLimits::paged(),
        cancel,
        progress,
    )
}

/// [`plan_stitch_with_progress`] within `limits`.
pub(crate) fn plan_with_limits(
    registration: &PanoramaRegistration,
    proxies: &[ColourProxy],
    options: &StitchOptions,
    limits: &OutputLimits,
    cancel: &Cancel,
    progress: &mut dyn FnMut(StitchStage),
) -> Result<StitchPlan, Error> {
    plan_with_exposure(
        registration,
        proxies,
        options,
        &[],
        limits,
        cancel,
        progress,
    )
}

/// [`plan_with_limits`] starting the photometric fit from each frame's scale to the reference's
/// exposure from its settings (`exposure`, one a frame; empty for none): the fit measures what the
/// overlaps say beyond it, and a frame whose overlaps give too few samples keeps it.
pub(crate) fn plan_with_exposure(
    registration: &PanoramaRegistration,
    proxies: &[ColourProxy],
    options: &StitchOptions,
    exposure: &[f64],
    limits: &OutputLimits,
    cancel: &Cancel,
    progress: &mut dyn FnMut(StitchStage),
) -> Result<StitchPlan, Error> {
    let lens = registration.lens;
    let rotations = &registration.rotations;
    let n = rotations.len();
    let reference = registration.reference;
    if !(MIN_FRAMES..=MAX_FRAMES).contains(&n) || proxies.len() != n || reference >= n {
        return Err(Error::validation(format!(
            "a panorama stitches {MIN_FRAMES} to {MAX_FRAMES} registered frames with one colour \
             proxy each"
        )));
    }
    if !exposure.is_empty()
        && (exposure.len() != n || exposure.iter().any(|e| !e.is_finite() || *e <= 0.0))
    {
        return Err(Error::validation(
            "a panorama's exposure scales are one positive scale a frame",
        ));
    }
    // Each frame's starting gain, relative to the reference's.
    let start: Vec<f64> = (0..n)
        .map(|f| {
            if exposure.is_empty() {
                1.0
            } else {
                exposure[f] / exposure[reference]
            }
        })
        .collect();
    if options.boundary_warp > 100 {
        return Err(Error::validation("Boundary Warp is 0 to 100"));
    }
    if proxies
        .iter()
        .any(|p| p.frame_dimensions() != (lens.width, lens.height))
    {
        return Err(Error::validation(
            "every colour proxy must reduce a frame of the registered size",
        ));
    }
    progress(StitchStage::Layout);
    cancel.check()?;
    let footprints = Footprints::of(&lens, rotations);
    let field = footprints.extent.field;
    let projection = options.projection.fixed().unwrap_or_else(|| field.auto());
    let refuse_field = || -> Error {
        Refusal::ExcessiveFieldOfView {
            projection,
            horizontal_deg: field.horizontal_deg.round() as u32,
            vertical_deg: field.vertical_deg.round() as u32,
        }
        .into()
    };
    match projection {
        Projection::Perspective if !field.fits_perspective() => return Err(refuse_field()),
        Projection::Cylindrical if !field.fits_cylindrical() => return Err(refuse_field()),
        _ => {}
    }
    let full = layout::full_scale(projection, &lens, &rotations[reference]);
    let (extent, wrap) = footprints.plane_extent(projection);
    let (full_width, full_height) = layout::dimensions(extent, wrap, full);
    let scale = layout::fit_scale(extent, wrap, full, limits)?;
    let plane_scale = full * scale;
    let (width, height) = layout::dimensions(extent, wrap, plane_scale);
    let geometry = OutputGeometry {
        projection,
        plane_scale,
        scale,
        origin: [extent[0], extent[1]],
        width: width as u32,
        height: height as u32,
        wrap,
    };
    // The plan grid.
    let factor = f64::from(proxies[reference].factor());
    let mut g = ((factor * scale).floor() as u64).max(1);
    while width.div_ceil(g) as usize > PLAN_MAX_SIDE
        || height.div_ceil(g) as usize > PLAN_MAX_SIDE
        || (width.div_ceil(g) * height.div_ceil(g)) as usize > PLAN_MAX_CELLS
    {
        g += 1;
    }
    let grid = PlanGrid {
        cell: g as u32,
        width: width.div_ceil(g) as u32,
        height: height.div_ceil(g) as u32,
    };
    let (gw, gh) = (grid.width as usize, grid.height as usize);
    let bounds: Vec<[usize; 4]> = (0..n)
        .map(|frame| {
            let e = footprints.frame_extent(frame, projection);
            let to_cell = |v: f64, origin: f64, count: usize, end: bool| -> usize {
                let px = (v - origin) * plane_scale;
                if !px.is_finite() {
                    return if end { count } else { 0 };
                }
                let c = (px / g as f64).floor() + if end { 2.0 } else { -1.0 };
                c.clamp(0.0, count as f64) as usize
            };
            let span = 2.0 * std::f64::consts::PI;
            let full_width =
                wrap && (e[0] < geometry.origin[0] || e[2] > geometry.origin[0] + span);
            let (x0, x1) = if full_width || !e[0].is_finite() || !e[2].is_finite() {
                (0, gw)
            } else {
                (
                    to_cell(e[0], geometry.origin[0], gw, false),
                    to_cell(e[2], geometry.origin[0], gw, true),
                )
            };
            [
                x0,
                to_cell(e[1], geometry.origin[1], gh, false),
                x1,
                to_cell(e[3], geometry.origin[1], gh, true),
            ]
        })
        .collect();
    let mut plan = StitchPlan {
        options: *options,
        lens,
        rotations: rotations.clone(),
        reference,
        geometry,
        grid,
        bounds,
        covered: Vec::new(),
        labels: Vec::new(),
        correction: [Vec::new(), Vec::new(), Vec::new()],
        gains: Vec::new(),
        mesh: WarpMesh::identity(geometry.width, geometry.height),
        amount: f64::from(options.boundary_warp) / 100.0,
        crop: PixelRect::default(),
        order: Vec::new(),
        supersample: ((1.0 / scale - 1e-9).ceil() as u32).clamp(1, MAX_SUPERSAMPLE),
        window_pixels: MAX_WINDOW_PIXELS,
        diagnostics: StitchDiagnostics {
            projection,
            projection_choice: options.projection,
            field,
            scale,
            full_width,
            full_height,
            boundary_warp: options.boundary_warp,
            crop: PixelRect::default(),
            covered_pixels: 0,
            pixels_lost: 0,
            crop_loss: 0.0,
            gains: Vec::new(),
            seams: Vec::new(),
            contributions: Vec::new(),
            clipped_pixels_estimate: 0,
            max_registration_residual_px: registration.max_residual_px,
            grid,
            mesh_columns: 2,
            mesh_rows: 2,
            blend_sigma_px: f64::from(BLEND_SIGMA_CELLS) * g as f64,
            supersample: 1,
        },
    };
    plan.diagnostics.supersample = plan.supersample;
    // Coverage of the unwarped stitch.
    let coverage: Vec<(u32, bool)> = (0..gh)
        .into_par_iter()
        .map(|y| {
            cancel.check()?;
            Ok((0..gw)
                .map(|x| plan.cell_coverage(x, y))
                .collect::<Vec<_>>())
        })
        .collect::<Result<Vec<_>, Error>>()?
        .into_iter()
        .flatten()
        .collect();
    let cover: Vec<u32> = coverage.iter().map(|c| c.0).collect();
    plan.covered = coverage.iter().map(|c| c.1).collect();
    drop(coverage);
    let eligible: Vec<u32> = (0..gw * gh)
        .map(|i| {
            let (x, y) = (i % gw, i / gw);
            if x == 0 || y == 0 || x + 1 == gw || y + 1 == gh {
                return 0;
            }
            let mut bits = u32::MAX;
            for yy in y - 1..=y + 1 {
                for xx in x - 1..=x + 1 {
                    bits &= cover[yy * gw + xx];
                }
            }
            bits
        })
        .collect();
    drop(cover);

    // Gains.
    progress(StitchStage::Gains);
    cancel.check()?;
    let stride = ((gw * gh) as f64 / GAIN_LATTICE_CELLS as f64)
        .sqrt()
        .ceil()
        .max(1.0) as usize;
    let lattice: Vec<usize> = (0..gh)
        .step_by(stride)
        .flat_map(|y| (0..gw).step_by(stride).map(move |x| y * gw + x))
        .filter(|&i| eligible[i].count_ones() >= 2)
        .collect();
    let cell_samples: Vec<Vec<(usize, [f32; 3])>> = lattice
        .par_iter()
        .map(|&cell| {
            let centre = plan.grid.centre(cell % gw, cell / gw);
            let d = plan.geometry.direction(centre);
            bits(eligible[cell])
                .filter_map(|frame| {
                    let sample = plan.proxy_sample(&proxies[frame], frame, d)?;
                    usable_for_gain(&sample).then(|| {
                        (
                            frame,
                            sample.rgb.map(|v| (f64::from(v) * start[frame]) as f32),
                        )
                    })
                })
                .collect()
        })
        .collect();
    let mut samples = GainSamples::new(n, options.gains);
    for cell in &cell_samples {
        samples.add(cell);
    }
    drop(cell_samples);
    plan.gains = samples.solve(reference);
    for (gain, start) in plan.gains.iter_mut().zip(&start) {
        gain.gain = gain.gain.map(|g| g * start);
    }
    let gain_of = |frame: usize| plan.gains[frame].gain.map(|g| g as f32);

    // Seams.
    progress(StitchStage::Seams);
    cancel.check()?;
    let seam_grid = SeamGrid {
        width: gw,
        height: gh,
        eligible: &eligible,
        bounds: &plan.bounds,
    };
    let order = seams::frame_order(&seam_grid, n, reference);
    let gained = |frame: usize, cell: usize| -> Option<ProxySample> {
        let centre = plan.grid.centre(cell % gw, cell / gw);
        let d = plan.geometry.direction(centre);
        let sample = plan.proxy_sample(&proxies[frame], frame, d)?;
        let gain = gain_of(frame);
        Some(ProxySample {
            rgb: std::array::from_fn(|c| sample.rgb[c] * gain[c]),
            ..sample
        })
    };
    let mut labels = seams::seam_labels(
        &seam_grid,
        &order,
        &|frame, cell| gained(frame, cell).map(|s| SeamSample::of(s.rgb, s.clipped)),
        cancel,
    )?;
    // Covered cells no frame is eligible at take the frame their centre lies deepest inside.
    let fills: Vec<(usize, u8)> = (0..gw * gh)
        .into_par_iter()
        .filter(|&i| labels[i] == NONE && plan.covered[i])
        .filter_map(|i| {
            let d = plan.geometry.direction(plan.grid.centre(i % gw, i / gw));
            plan.deepest(i % gw, i / gw, d, true)
                .map(|(frame, _)| (i, frame as u8))
        })
        .collect();
    for (i, frame) in fills {
        labels[i] = frame;
    }

    // The blend's correction field.
    progress(StitchStage::Blend);
    cancel.check()?;
    let correction = blend::correction_field(
        gw,
        gh,
        &labels,
        &eligible,
        &plan.bounds,
        &|frame, cell| gained(frame, cell).map(|s| s.rgb),
        cancel,
    )?;
    let clipped = (0..gw * gh)
        .into_par_iter()
        .filter(|&i| {
            let label = labels[i];
            label != NONE
                && eligible[i] & (1 << label) != 0
                && gained(usize::from(label), i).is_some_and(|s| s.clipped)
        })
        .count() as u64
        * g
        * g;
    drop(eligible);
    plan.labels = labels;
    plan.correction = correction;
    plan.diagnostics.clipped_pixels_estimate = clipped;

    // The warp mesh, the crop and the stack order.
    progress(StitchStage::Warp);
    cancel.check()?;
    plan.mesh = warp::solve(
        &plan.covered,
        gw,
        gh,
        grid.cell,
        geometry.width,
        geometry.height,
        cancel,
    )?;
    plan.diagnostics.mesh_columns = plan.mesh.columns;
    plan.diagnostics.mesh_rows = plan.mesh.rows;
    plan.order = plan.order_frames();
    plan.diagnostics.gains = plan.gains.clone();
    plan.diagnostics.seams = plan.seam_lengths();
    plan.diagnostics.contributions = plan.contributions();
    plan.recrop(cancel)?;
    Ok(plan)
}

/// A colour-proxy sample: its linear RGB, whether any weighted tap was clipped, and its
/// luminance gradient in stops a proxy pixel.
#[derive(Clone, Copy, Debug)]
struct ProxySample {
    rgb: [f32; 3],
    clipped: bool,
    gradient: f32,
}

fn usable_for_gain(sample: &ProxySample) -> bool {
    !sample.clipped
        && sample.rgb.iter().all(|&v| v > GAIN_FLOOR)
        && sample.gradient <= GAIN_MAX_GRADIENT
}

/// The frames whose bits are set, ascending.
fn bits(mut mask: u32) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        (mask != 0).then(|| {
            let frame = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            frame
        })
    })
}

/// The bilinear sample of `view` at `q` in its own pixels, taps clamped to the view: linear RGB in
/// `f64` and whether a tap with weight was clipped.
#[inline]
fn bilinear(view: &FrameRect<'_>, q: [f64; 2]) -> ([f64; 3], bool) {
    let axis = |v: f64, count: usize| -> (usize, usize, f64) {
        let v = v.clamp(0.0, (count - 1) as f64);
        let i0 = v.floor() as usize;
        let i1 = (i0 + 1).min(count - 1);
        (i0, i1, v - i0 as f64)
    };
    let (x0, x1, fx) = axis(q[0], view.width);
    let (y0, y1, fy) = axis(q[1], view.height);
    let taps = [
        (x0, y0, (1.0 - fx) * (1.0 - fy)),
        (x1, y0, fx * (1.0 - fy)),
        (x0, y1, (1.0 - fx) * fy),
        (x1, y1, fx * fy),
    ];
    let mut rgb = [0.0_f64; 3];
    let mut clipped = false;
    for (x, y, w) in taps {
        let (sample, level) = view.at(x, y);
        for c in 0..3 {
            rgb[c] += w * f64::from(sample[c]);
        }
        clipped |= w > 0.0 && level.is_clipped();
    }
    (rgb, clipped)
}

impl StitchPlan {
    /// The frames covering every corner pixel of cell `(x, y)`, one bit a frame, and whether every
    /// corner pixel lies inside some frame.
    fn cell_coverage(&self, x: usize, y: usize) -> (u32, bool) {
        let corners = self
            .grid
            .corners(x, y, self.geometry.width, self.geometry.height);
        let mut all = u32::MAX;
        let mut union = true;
        for corner in corners {
            let d = self.geometry.direction(corner);
            let mut mask = 0_u32;
            for frame in 0..self.rotations.len() {
                let [x0, y0, x1, y1] = self.bounds[frame];
                if x < x0 || x >= x1 || y < y0 || y >= y1 {
                    continue;
                }
                if frame_position(&self.lens, &self.rotations[frame], d)
                    .is_some_and(|q| inside(q, self.lens.width, self.lens.height))
                {
                    mask |= 1 << frame;
                }
            }
            all &= mask;
            union &= mask != 0;
        }
        (all, union)
    }

    /// Whether stitch point `p` lies inside some frame.
    fn covered_point(&self, p: [f64; 2]) -> bool {
        let (cx, cy) = self.grid.cell_of(p);
        let d = self.geometry.direction(p);
        (0..self.rotations.len()).any(|frame| {
            let [x0, y0, x1, y1] = self.bounds[frame];
            cx >= x0
                && cx < x1
                && cy >= y0
                && cy < y1
                && frame_position(&self.lens, &self.rotations[frame], d)
                    .is_some_and(|q| inside(q, self.lens.width, self.lens.height))
        })
    }

    /// The frame direction `d` lies deepest inside among those that may cover cell `(x, y)`, with
    /// its position; only covering frames when `covering`.
    fn deepest(&self, x: usize, y: usize, d: V3, covering: bool) -> Option<(usize, [f64; 2])> {
        let mut best: Option<(f64, usize, [f64; 2])> = None;
        for frame in 0..self.rotations.len() {
            let [x0, y0, x1, y1] = self.bounds[frame];
            if x < x0 || x >= x1 || y < y0 || y >= y1 {
                continue;
            }
            let Some(q) = frame_position(&self.lens, &self.rotations[frame], d) else {
                continue;
            };
            let depth = depth(q, self.lens.width, self.lens.height);
            if covering && depth < 0.0 {
                continue;
            }
            if best.is_none_or(|(b, _, _)| depth > b) {
                best = Some((depth, frame, q));
            }
        }
        best.map(|(_, frame, q)| (frame, q))
    }

    /// Frame `frame`'s colour-proxy sample of direction `d`, with its luminance gradient.
    fn proxy_sample(&self, proxy: &ColourProxy, frame: usize, d: V3) -> Option<ProxySample> {
        let q = frame_position(&self.lens, &self.rotations[frame], d)?;
        if !inside(q, self.lens.width, self.lens.height) {
            return None;
        }
        let view = proxy.view();
        let f = f64::from(proxy.factor());
        let local = [(q[0] - (f - 1.0) / 2.0) / f, (q[1] - (f - 1.0) / 2.0) / f];
        let (rgb, clipped) = bilinear(&view, local);
        let luma = |p: [f64; 2]| {
            let (rgb, _) = bilinear(&view, p);
            (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2])
                .max(1e-12)
                .log2()
        };
        let gx = (luma([local[0] + 1.0, local[1]]) - luma([local[0] - 1.0, local[1]])) / 2.0;
        let gy = (luma([local[0], local[1] + 1.0]) - luma([local[0], local[1] - 1.0])) / 2.0;
        Some(ProxySample {
            rgb: rgb.map(|v| v as f32),
            clipped,
            gradient: gx.hypot(gy) as f32,
        })
    }

    /// The stitch point bounding-rectangle point `b` shows.
    #[inline]
    fn warped(&self, b: [f64; 2]) -> [f64; 2] {
        if self.amount == 0.0 {
            return b;
        }
        let d = self.mesh.at(b);
        [b[0] + self.amount * d[0], b[1] + self.amount * d[1]]
    }

    /// Bounding-rectangle point `b`'s plan. See the module documentation.
    fn prepare(&self, b: [f64; 2]) -> PointPlan {
        let p = self.warped(b);
        let d = self.geometry.direction(p);
        let (cells, weights) = self.grid.around(p);
        let mut plan = PointPlan::default();
        let mut correction = [0.0_f64; 3];
        let mut candidates: [(u8, f64); 4] = [(NONE, 0.0); 4];
        let mut count = 0;
        for (&cell, &w) in cells.iter().zip(&weights) {
            for (c, plane) in self.correction.iter().enumerate() {
                correction[c] += w * f64::from(plane[cell]);
            }
            let label = self.labels[cell];
            if label == NONE || w <= 0.0 {
                continue;
            }
            match candidates[..count].iter_mut().find(|(f, _)| *f == label) {
                Some(entry) => entry.1 += w,
                None => {
                    candidates[count] = (label, w);
                    count += 1;
                }
            }
        }
        candidates[..count].sort_by_key(|&(frame, _)| frame);
        plan.correction = correction;
        let mut total = 0.0;
        for &(frame, w) in &candidates[..count] {
            let f = usize::from(frame);
            if let Some(q) = frame_position(&self.lens, &self.rotations[f], d)
                && inside(q, self.lens.width, self.lens.height)
            {
                let k = usize::from(plan.count);
                plan.frames[k] = frame;
                plan.weights[k] = w;
                plan.positions[k] = q;
                plan.count += 1;
                total += w;
            }
        }
        if plan.count > 0 {
            for w in &mut plan.weights[..usize::from(plan.count)] {
                *w /= total;
            }
            return plan;
        }
        let (cx, cy) = self.grid.cell_of(p);
        let fallback = self.deepest(cx, cy, d, true).or_else(|| {
            plan.uncovered = true;
            self.deepest(cx, cy, d, false)
        });
        if let Some((frame, q)) = fallback {
            plan.frames[0] = frame as u8;
            plan.weights[0] = 1.0;
            plan.positions[0] = q;
            plan.count = 1;
        }
        plan
    }

    /// Re-solve the crop for the current amount.
    fn recrop(&mut self, cancel: &Cancel) -> Result<(), Error> {
        let (gw, gh) = (self.grid.width as usize, self.grid.height as usize);
        let covered: Vec<bool> = if self.amount == 0.0 {
            self.covered.clone()
        } else {
            (0..gh)
                .into_par_iter()
                .map(|y| {
                    cancel.check()?;
                    Ok((0..gw)
                        .map(|x| {
                            self.grid
                                .corners(x, y, self.geometry.width, self.geometry.height)
                                .iter()
                                .all(|&b| self.covered_point(self.warped(b)))
                        })
                        .collect::<Vec<bool>>())
                })
                .collect::<Result<Vec<_>, Error>>()?
                .into_iter()
                .flatten()
                .collect()
        };
        cancel.check()?;
        let [x, y, w, h] = layout::largest_rectangle(&covered, gw, gh);
        let g = self.grid.cell;
        let (bw, bh) = (self.geometry.width, self.geometry.height);
        let crop = if w == 0 {
            PixelRect::default()
        } else {
            let (x0, y0) = (x as u32 * g, y as u32 * g);
            PixelRect {
                x: x0,
                y: y0,
                width: ((x + w) as u32 * g).min(bw) - x0,
                height: ((y + h) as u32 * g).min(bh) - y0,
            }
        };
        if u64::from(crop.width) * 4 < u64::from(bw) || u64::from(crop.height) * 4 < u64::from(bh) {
            return Err(Refusal::InsufficientCoverage {
                width: crop.width,
                height: crop.height,
                bounding_width: bw,
                bounding_height: bh,
            }
            .into());
        }
        let covered_pixels: u64 = (0..gw * gh)
            .filter(|&i| covered[i])
            .map(|i| {
                let (x, y) = (i % gw, i / gw);
                let cw = (((x + 1) as u32 * g).min(bw) - x as u32 * g) as u64;
                let ch = (((y + 1) as u32 * g).min(bh) - y as u32 * g) as u64;
                cw * ch
            })
            .sum();
        self.crop = crop;
        self.diagnostics.crop = crop;
        self.diagnostics.boundary_warp = self.options.boundary_warp;
        self.diagnostics.covered_pixels = covered_pixels;
        self.diagnostics.pixels_lost = covered_pixels.saturating_sub(crop.pixels());
        self.diagnostics.crop_loss = if covered_pixels > 0 {
            self.diagnostics.pixels_lost as f64 / covered_pixels as f64
        } else {
            0.0
        };
        Ok(())
    }

    /// This plan at another Boundary Warp amount: the same layout, gains, seams and mesh, with
    /// the crop solved again.
    pub fn with_boundary_warp(&self, amount: u8, cancel: &Cancel) -> Result<Self, Error> {
        if amount > 100 {
            return Err(Error::validation("Boundary Warp is 0 to 100"));
        }
        let mut plan = self.clone();
        plan.options.boundary_warp = amount;
        plan.amount = f64::from(amount) / 100.0;
        plan.recrop(cancel)?;
        Ok(plan)
    }

    fn order_frames(&self) -> Vec<usize> {
        let near = self.geometry.origin[0]
            + f64::from(self.geometry.width) / 2.0 / self.geometry.plane_scale;
        let centres: Vec<[f64; 2]> = self
            .rotations
            .iter()
            .map(|r| {
                self.geometry
                    .point(r.forward(), near)
                    .unwrap_or([f64::INFINITY; 2])
            })
            .collect();
        let gap =
            0.5 * self.geometry.plane_scale * f64::from(self.lens.height) / self.lens.focal_px;
        layout::stack_order(&centres, gap)
    }

    fn seam_lengths(&self) -> Vec<SeamLength> {
        let (gw, gh) = (self.grid.width as usize, self.grid.height as usize);
        let n = self.rotations.len();
        let mut counts = vec![0_u64; n * n];
        for y in 0..gh {
            for x in 0..gw {
                let a = self.labels[y * gw + x];
                for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                    if nx >= gw || ny >= gh {
                        continue;
                    }
                    let b = self.labels[ny * gw + nx];
                    if a != NONE && b != NONE && a != b {
                        let (a, b) = (usize::from(a.min(b)), usize::from(a.max(b)));
                        counts[a * n + b] += 1;
                    }
                }
            }
        }
        let mut seams = Vec::new();
        for a in 0..n {
            for b in a + 1..n {
                if counts[a * n + b] > 0 {
                    seams.push(SeamLength {
                        a,
                        b,
                        length_px: counts[a * n + b] * u64::from(self.grid.cell),
                    });
                }
            }
        }
        seams
    }

    fn contributions(&self) -> Vec<Contribution> {
        let (gw, gh) = (self.grid.width as usize, self.grid.height as usize);
        let g = self.grid.cell as usize;
        let (bw, bh) = (self.geometry.width as usize, self.geometry.height as usize);
        (0..self.rotations.len())
            .map(|frame| {
                let mut pixels = 0_u64;
                let mut r = [usize::MAX, usize::MAX, 0, 0];
                for y in 0..gh {
                    for x in 0..gw {
                        if usize::from(self.labels[y * gw + x]) != frame {
                            continue;
                        }
                        let (x0, y0) = (x * g, y * g);
                        let (x1, y1) = ((x0 + g).min(bw), (y0 + g).min(bh));
                        pixels += ((x1 - x0) * (y1 - y0)) as u64;
                        r = [r[0].min(x0), r[1].min(y0), r[2].max(x1), r[3].max(y1)];
                    }
                }
                Contribution {
                    frame,
                    pixels,
                    bounds: if pixels == 0 {
                        PixelRect::default()
                    } else {
                        PixelRect {
                            x: r[0] as u32,
                            y: r[1] as u32,
                            width: (r[2] - r[0]) as u32,
                            height: (r[3] - r[1]) as u32,
                        }
                    },
                }
            })
            .collect()
    }

    /// The output's dimensions: the crop's.
    pub fn output_size(&self) -> (u32, u32) {
        (self.crop.width, self.crop.height)
    }

    /// The crop in the bounding rectangle.
    pub fn crop(&self) -> PixelRect {
        self.crop
    }

    pub fn options(&self) -> &StitchOptions {
        &self.options
    }

    /// The registration's reference frame: the gains' anchor and the output scale's.
    pub fn reference(&self) -> usize {
        self.reference
    }

    pub fn geometry(&self) -> &OutputGeometry {
        &self.geometry
    }

    pub fn grid(&self) -> PlanGrid {
        self.grid
    }

    pub fn diagnostics(&self) -> &StitchDiagnostics {
        &self.diagnostics
    }

    pub fn gains(&self) -> &[FrameGain] {
        &self.gains
    }

    /// The warp's mesh at amount 100.
    pub fn mesh(&self) -> &WarpMesh {
        &self.mesh
    }

    /// The frames in stack order: rows top to bottom, each left to right.
    pub fn stack_order(&self) -> &[usize] {
        &self.order
    }

    /// The seam map's frame per plan cell, [`u8::MAX`] where none.
    pub fn seam_map(&self) -> &[u8] {
        &self.labels
    }

    /// Below full scale, the points a side each output pixel averages.
    pub fn supersample(&self) -> u32 {
        self.supersample
    }

    /// The plan with frame windows of at most `pixels`, so tests split tiles.
    #[cfg(test)]
    pub(crate) fn with_window_pixels(mut self, pixels: u64) -> Self {
        self.window_pixels = pixels;
        self
    }

    /// The bytes the plan holds.
    pub fn bytes(&self) -> usize {
        self.labels.len()
            + self.covered.len()
            + self.correction.iter().map(|c| c.len() * 4).sum::<usize>()
            + self.mesh.bytes()
            + self.bounds.len() * 32
    }

    /// The world direction result pixel `(x, y)` shows (at its centre).
    pub fn direction(&self, pixel: [f64; 2]) -> V3 {
        let b = [
            f64::from(self.crop.x) + pixel[0],
            f64::from(self.crop.y) + pixel[1],
        ];
        self.geometry.direction(self.warped(b))
    }

    /// Where result pixel `(x, y)` (at its centre) lies in frame `frame`, inside it or not.
    pub fn frame_position(&self, frame: usize, pixel: [f64; 2]) -> Option<[f64; 2]> {
        frame_position(
            &self.lens,
            self.rotations.get(frame)?,
            self.direction(pixel),
        )
    }

    /// Draw `rect` of the result as a [`StitchedChunk`]: see [`Self::render_into`].
    pub fn render_chunk(
        &self,
        rect: PixelRect,
        windows: &mut dyn FrameWindows,
        cancel: &Cancel,
    ) -> Result<StitchedChunk, Error> {
        let pixels = rect.pixels() as usize;
        let mut out = vec![0.0; 3 * pixels];
        let stats = self.render_into(rect, windows, &mut out, cancel)?;
        let blue = out.split_off(2 * pixels);
        let green = out.split_off(pixels);
        Ok(StitchedChunk {
            rect,
            rgb: [out, green, blue],
            stats,
        })
    }

    /// Draw `rect` of the result into `out`, three planes of `rect`'s pixels (red, green, blue),
    /// reading each frame's full-resolution pixels through `windows`. Windows are at most
    /// [`MAX_WINDOW_PIXELS`] and read one at a time; `rect` is drawn in tiles of at most
    /// [`MAX_TILE_POINTS`] points, smaller where a window would be larger. Checks `cancel` per tile
    /// and per window.
    pub fn render_into(
        &self,
        rect: PixelRect,
        windows: &mut dyn FrameWindows,
        out: &mut [f32],
        cancel: &Cancel,
    ) -> Result<ChunkStats, Error> {
        if rect.is_empty()
            || u64::from(rect.x) + u64::from(rect.width) > u64::from(self.crop.width)
            || u64::from(rect.y) + u64::from(rect.height) > u64::from(self.crop.height)
        {
            return Err(Error::validation(
                "a stitched chunk must be a nonempty rectangle of the output",
            ));
        }
        let pixels = rect.pixels() as usize;
        if out.len() < 3 * pixels {
            return Err(Error::internal("a chunk buffer is shorter than its chunk"));
        }
        let mut chunk = Chunk {
            rect,
            out,
            stats: ChunkStats::default(),
        };
        let n = self.supersample as usize;
        let side = (((MAX_TILE_POINTS / (n * n)) as f64).sqrt() as u32).max(1);
        let mut tiles = Vec::new();
        for y in (0..rect.height).step_by(side as usize) {
            for x in (0..rect.width).step_by(side as usize) {
                tiles.push(PixelRect {
                    x: rect.x + x,
                    y: rect.y + y,
                    width: side.min(rect.width - x),
                    height: side.min(rect.height - y),
                });
            }
        }
        while let Some(tile) = tiles.pop() {
            cancel.check()?;
            if !self.render_tile(tile, &mut chunk, windows, cancel)? {
                // A window would be too large: split the tile.
                if tile.width <= 1 && tile.height <= 1 {
                    return Err(Error::resource_limit(
                        "a stitched pixel would read more than a window's pixels",
                    ));
                }
                let (hw, hh) = (tile.width.div_ceil(2), tile.height.div_ceil(2));
                for (x, y, w, h) in [
                    (0, 0, hw, hh),
                    (hw, 0, tile.width - hw, hh),
                    (0, hh, hw, tile.height - hh),
                    (hw, hh, tile.width - hw, tile.height - hh),
                ] {
                    if w > 0 && h > 0 {
                        tiles.push(PixelRect {
                            x: tile.x + x,
                            y: tile.y + y,
                            width: w,
                            height: h,
                        });
                    }
                }
            }
        }
        Ok(chunk.stats)
    }

    /// The points of result rectangle `tile`, `n × n` a pixel, row-major by pixel.
    fn tile_points(&self, tile: PixelRect) -> Vec<[f64; 2]> {
        let n = self.supersample as usize;
        let offsets: Vec<f64> = (0..n).map(|s| (s as f64 + 0.5) / n as f64 - 0.5).collect();
        let mut points = Vec::with_capacity(tile.pixels() as usize * n * n);
        for y in 0..tile.height {
            for x in 0..tile.width {
                let b = [
                    f64::from(self.crop.x + tile.x + x),
                    f64::from(self.crop.y + tile.y + y),
                ];
                for oy in &offsets {
                    for ox in &offsets {
                        points.push(if n == 1 { b } else { [b[0] + ox, b[1] + oy] });
                    }
                }
            }
        }
        points
    }

    /// Render one tile into `chunk`; `false` when a frame's window would exceed the bound.
    fn render_tile(
        &self,
        tile: PixelRect,
        chunk: &mut Chunk<'_>,
        source: &mut dyn FrameWindows,
        cancel: &Cancel,
    ) -> Result<bool, Error> {
        let points = self.tile_points(tile);
        let plans: Vec<PointPlan> = points.par_iter().map(|&b| self.prepare(b)).collect();
        drop(points);
        let frames = self.rotations.len();
        let (fw, fh) = (self.lens.width, self.lens.height);
        let mut windows = vec![[u32::MAX, u32::MAX, 0, 0]; frames];
        let mut used = vec![false; frames];
        for plan in &plans {
            for k in 0..usize::from(plan.count) {
                let f = usize::from(plan.frames[k]);
                let [x0, x1, y0, y1] = taps(plan.positions[k], fw, fh);
                let w = &mut windows[f];
                *w = [w[0].min(x0), w[1].min(y0), w[2].max(x1), w[3].max(y1)];
                used[f] = true;
            }
        }
        for f in 0..frames {
            let [x0, y0, x1, y1] = windows[f];
            if used[f] && u64::from(x1 - x0 + 1) * u64::from(y1 - y0 + 1) > self.window_pixels {
                return Ok(false);
            }
        }
        let mut acc = vec![[0.0_f64; 3]; plans.len()];
        let mut low = vec![[f64::INFINITY; 3]; plans.len()];
        let mut high = vec![[f64::NEG_INFINITY; 3]; plans.len()];
        let mut clipped = vec![false; plans.len()];
        for f in 0..frames {
            if !used[f] {
                continue;
            }
            cancel.check()?;
            let [x0, y0, x1, y1] = windows[f];
            let rect = PixelRect {
                x: x0,
                y: y0,
                width: x1 - x0 + 1,
                height: y1 - y0 + 1,
            };
            let window = source.read(f, rect)?;
            let view = window.frame;
            let need = rect.pixels() as usize;
            if (window.x, window.y) != (x0, y0)
                || (view.width, view.height) != (rect.width as usize, rect.height as usize)
                || view.stride < view.width
                || view
                    .rgb
                    .iter()
                    .any(|p| p.len() < (view.height - 1) * view.stride + view.width)
                || view.levels.len() < (view.height - 1) * view.stride + view.width
                || need == 0
            {
                return Err(Error::internal(
                    "a frame window does not match the rectangle asked for",
                ));
            }
            let gain = self.gains[f].gain;
            let origin = [f64::from(x0), f64::from(y0)];
            acc.par_iter_mut()
                .zip(low.par_iter_mut())
                .zip(high.par_iter_mut())
                .zip(clipped.par_iter_mut())
                .zip(plans.par_iter())
                .for_each(|((((acc, low), high), clipped), plan)| {
                    for k in 0..usize::from(plan.count) {
                        if usize::from(plan.frames[k]) != f {
                            continue;
                        }
                        let q = clamp_position(plan.positions[k], fw, fh);
                        let (rgb, clip) = bilinear(&view, [q[0] - origin[0], q[1] - origin[1]]);
                        for c in 0..3 {
                            let v = gain[c] * rgb[c];
                            acc[c] += plan.weights[k] * v;
                            low[c] = low[c].min(v);
                            high[c] = high[c].max(v);
                        }
                        *clipped |= clip;
                    }
                });
        }
        // Average each pixel's points.
        let n2 = (self.supersample * self.supersample) as usize;
        let width = chunk.rect.width as usize;
        let plane = chunk.rect.pixels() as usize;
        for py in 0..tile.height as usize {
            for px in 0..tile.width as usize {
                let p = py * tile.width as usize + px;
                let mut sum = [0.0_f64; 3];
                let (mut any_clipped, mut any_uncovered) = (false, false);
                let mut overshoot = 0.0_f64;
                for s in 0..n2 {
                    let i = p * n2 + s;
                    let plan = &plans[i];
                    for c in 0..3 {
                        let v = acc[i][c] + plan.correction[c];
                        sum[c] += v;
                        if plan.count > 0 {
                            overshoot = overshoot.max(v - high[i][c]).max(low[i][c] - v);
                        }
                    }
                    any_clipped |= clipped[i];
                    any_uncovered |= plan.uncovered || plan.count == 0;
                }
                let o = (tile.y - chunk.rect.y) as usize + py;
                let o = o * width + (tile.x - chunk.rect.x) as usize + px;
                for (c, sum) in sum.into_iter().enumerate() {
                    chunk.out[c * plane + o] = (sum / n2 as f64) as f32;
                }
                chunk.stats.clipped_pixels += u64::from(any_clipped);
                chunk.stats.uncovered_pixels += u64::from(any_uncovered);
                chunk.stats.max_overshoot = chunk.stats.max_overshoot.max(overshoot as f32);
            }
        }
        Ok(true)
    }

    /// The preview: the whole warped bounding rectangle drawn from `proxies`, at most `max_side`
    /// pixels a side, with its overlays. Checks `cancel` per row.
    pub fn preview(
        &self,
        proxies: &[ColourProxy],
        max_side: u32,
        cancel: &Cancel,
    ) -> Result<PanoramaPreview, Error> {
        if proxies.len() != self.rotations.len()
            || proxies
                .iter()
                .any(|p| p.frame_dimensions() != (self.lens.width, self.lens.height))
        {
            return Err(Error::validation(
                "the preview needs one colour proxy a registered frame",
            ));
        }
        let (bw, bh) = (self.geometry.width, self.geometry.height);
        let cell = bw.max(bh).div_ceil(max_side.max(1)).max(1);
        let (w, h) = (bw.div_ceil(cell) as usize, bh.div_ceil(cell) as usize);
        let c = f64::from(cell);
        let rows: Vec<Vec<([f32; 3], bool)>> = (0..h)
            .into_par_iter()
            .map(|j| {
                cancel.check()?;
                Ok((0..w)
                    .map(|i| {
                        let b = [
                            i as f64 * c + (c - 1.0) / 2.0,
                            j as f64 * c + (c - 1.0) / 2.0,
                        ];
                        let plan = self.prepare(b);
                        if plan.uncovered || plan.count == 0 {
                            return ([0.0; 3], false);
                        }
                        let mut acc = [0.0_f64; 3];
                        for k in 0..usize::from(plan.count) {
                            let f = usize::from(plan.frames[k]);
                            let proxy = &proxies[f];
                            let factor = f64::from(proxy.factor());
                            let q = clamp_position(
                                plan.positions[k],
                                self.lens.width,
                                self.lens.height,
                            );
                            let local = [
                                (q[0] - (factor - 1.0) / 2.0) / factor,
                                (q[1] - (factor - 1.0) / 2.0) / factor,
                            ];
                            let (rgb, _) = bilinear(&proxy.view(), local);
                            for ch in 0..3 {
                                acc[ch] += plan.weights[k] * (self.gains[f].gain[ch] * rgb[ch]);
                            }
                        }
                        let n2 = 1.0_f64;
                        (
                            std::array::from_fn(|ch| ((acc[ch] + plan.correction[ch]) / n2) as f32),
                            true,
                        )
                    })
                    .collect())
            })
            .collect::<Result<_, Error>>()?;
        let mut rgb = [
            vec![0.0_f32; w * h],
            vec![0.0_f32; w * h],
            vec![0.0_f32; w * h],
        ];
        let mut covered = vec![false; w * h];
        for (j, row) in rows.into_iter().enumerate() {
            for (i, (value, cov)) in row.into_iter().enumerate() {
                for ch in 0..3 {
                    rgb[ch][j * w + i] = value[ch];
                }
                covered[j * w + i] = cov;
            }
        }
        cancel.check()?;
        let outline = trace_outline(&covered, w, h);
        let to_picture = |b: [f64; 2]| [(b[0] + 0.5) / c, (b[1] + 0.5) / c];
        let frames = self
            .order
            .iter()
            .enumerate()
            .map(|(k, &frame)| FrameOutline {
                frame,
                position: k + 1,
                outline: self.outline(frame).into_iter().map(to_picture).collect(),
            })
            .collect();
        let crop = self.crop;
        Ok(PanoramaPreview {
            width: w,
            height: h,
            cell,
            rgb,
            covered,
            overlays: PanoramaOverlays {
                covered: outline,
                crop: [
                    f64::from(crop.x) / c,
                    f64::from(crop.y) / c,
                    f64::from(crop.width) / c,
                    f64::from(crop.height) / c,
                ],
                frames,
                crop_loss: self.diagnostics.crop_loss,
            },
        })
    }

    /// Frame `frame`'s outline in the bounding rectangle, through the warp's inverse.
    fn outline(&self, frame: usize) -> Vec<[f64; 2]> {
        let rotation = &self.rotations[frame];
        let near = self
            .geometry
            .projection
            .plane(rotation.forward())
            .map_or(self.geometry.origin[0], |p| p[0]);
        let (w, h) = (
            f64::from(self.lens.width) - 1.0,
            f64::from(self.lens.height) - 1.0,
        );
        let n = OUTLINE_SAMPLES;
        let mut pixels = Vec::with_capacity(4 * n);
        for k in 0..n {
            let t = k as f64 / n as f64;
            pixels.push((w * t, 0.0));
        }
        for k in 0..n {
            let t = k as f64 / n as f64;
            pixels.push((w, h * t));
        }
        for k in 0..n {
            let t = k as f64 / n as f64;
            pixels.push((w * (1.0 - t), h));
        }
        for k in 0..n {
            let t = k as f64 / n as f64;
            pixels.push((0.0, h * (1.0 - t)));
        }
        pixels
            .into_iter()
            .filter_map(|pixel| {
                let d = self.lens.world_ray(rotation, pixel);
                let p = self.geometry.point(d, near)?;
                Some(self.mesh.invert(p, self.amount))
            })
            .collect()
    }
}

/// `q` clamped to a `width × height` frame's pixel centres.
#[inline]
fn clamp_position(q: [f64; 2], width: u32, height: u32) -> [f64; 2] {
    [
        q[0].clamp(0.0, f64::from(width) - 1.0),
        q[1].clamp(0.0, f64::from(height) - 1.0),
    ]
}

/// The tap columns and rows `[x₀, x₁, y₀, y₁]` a bilinear sample at `q` reads.
#[inline]
fn taps(q: [f64; 2], width: u32, height: u32) -> [u32; 4] {
    let q = clamp_position(q, width, height);
    let (x0, y0) = (q[0].floor() as u32, q[1].floor() as u32);
    [x0, (x0 + 1).min(width - 1), y0, (y0 + 1).min(height - 1)]
}

/// The most bytes [`plan_stitch`] holds at once besides the proxies, for a plan grid of `cells`
/// cells and `frames` frames: coverage and eligibility, the seam map and its composite, one
/// frame's cut (its samples, costs and the flow's residuals), the correction field and one frame's
/// blend planes, the gain samples, and the retained plan.
pub fn plan_scratch_bytes(cells: usize, frames: usize) -> usize {
    let coverage = cells * (4 + 1 + 4);
    let labels = cells;
    let composite = cells * size_of::<Option<SeamSample>>();
    // The frame's samples, its region, terminals and costs, capacities and result, then the cut:
    // widened capacities, the exact cut's residuals, trees and queues, and the band's masks,
    // with each coarser level a quarter of the one above.
    let exact = 4 * 4 + 1 + 1 + 4 + 4 + 1 + 8 + 8;
    let level = 2 * 4 + exact + 3 + 1 + 1;
    let cut = cells * (size_of::<Option<SeamSample>>() + 4 + 4 + 2 + 2 + 1) + cells * level * 4 / 3;
    let field = cells * (3 * 4 + 4);
    let blend = cells * (size_of::<Option<[f32; 3]>>() + 5 * 4 + 2 * 4);
    let gains = GAIN_LATTICE_CELLS * (6 * 3 * 4 + 64);
    let order = frames * frames * 8;
    coverage + labels + composite + cut.max(field + blend) + gains + order
}

/// The most bytes [`StitchPlan::render_chunk`] holds at once besides the chunk it returns: one
/// tile's point plans and accumulators and one frame's window.
pub fn render_scratch_bytes() -> usize {
    tile_scratch_bytes() + (MAX_WINDOW_PIXELS * WINDOW_PIXEL_BYTES) as usize
}

/// [`render_scratch_bytes`] without the window: one tile's point plans and accumulators.
pub fn tile_scratch_bytes() -> usize {
    MAX_TILE_POINTS * (size_of::<PointPlan>() + 3 * 3 * 8 + 1)
}

/// The outline of the `true` pixels of a `width × height` mask (pixel `(i, j)` covering
/// `[i, i + 1) × [j, j + 1)`): closed polygons along pixel edges, each corner once, clockwise on
/// screen (y down) around the marked pixels. At a vertex where two marked pixels touch only
/// diagonally the outline turns right, keeping them apart.
pub(crate) fn trace_outline(mask: &[bool], width: usize, height: usize) -> Vec<Vec<[f64; 2]>> {
    // Directions: right, down, left, up; each vertex's outgoing boundary edges, one bit each.
    const STEP: [(isize, isize); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];
    let at = |i: isize, j: isize| {
        i >= 0
            && j >= 0
            && (i as usize) < width
            && (j as usize) < height
            && mask[j as usize * width + i as usize]
    };
    let vw = width + 1;
    let mut out = vec![0_u8; vw * (height + 1)];
    for j in 0..height {
        for i in 0..width {
            if !mask[j * width + i] {
                continue;
            }
            let (x, y) = (i as isize, j as isize);
            if !at(x, y - 1) {
                out[j * vw + i] |= 1;
            }
            if !at(x + 1, y) {
                out[j * vw + i + 1] |= 2;
            }
            if !at(x, y + 1) {
                out[(j + 1) * vw + i + 1] |= 4;
            }
            if !at(x - 1, y) {
                out[(j + 1) * vw + i] |= 8;
            }
        }
    }
    let mut polygons = Vec::new();
    for start in 0..out.len() {
        while out[start] != 0 {
            let mut direction = out[start].trailing_zeros() as usize;
            let mut vertex = start;
            let mut path: Vec<(usize, usize)> = Vec::new();
            loop {
                out[vertex] &= !(1 << direction);
                path.push((vertex, direction));
                let (dx, dy) = STEP[direction];
                let (x, y) = ((vertex % vw) as isize + dx, (vertex / vw) as isize + dy);
                vertex = y as usize * vw + x as usize;
                if vertex == start {
                    break;
                }
                // Right, straight, then left.
                direction = [1, 0, 3]
                    .into_iter()
                    .map(|turn| (direction + turn) % 4)
                    .find(|&d| out[vertex] & (1 << d) != 0)
                    .unwrap_or(direction);
            }
            let corners: Vec<[f64; 2]> = (0..path.len())
                .filter(|&k| path[(k + path.len() - 1) % path.len()].1 != path[k].1)
                .map(|k| {
                    let v = path[k].0;
                    [(v % vw) as f64, (v / vw) as f64]
                })
                .collect();
            polygons.push(corners);
        }
    }
    polygons
}
