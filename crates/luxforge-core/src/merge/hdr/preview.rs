//! The merge preview's numerics: a picture of the merge drawn from the inputs' proxies, for viewing
//! only, and the facts its sheet shows beside it (`docs/design/panorama-merge.md`, "The merge
//! preview"). Nothing here reads a file, holds a frame or writes the result.
//!
//! **Colour proxy.** Beside each input's [`AnalysisProxy`] the merge keeps a [`ColourProxy`] on the
//! same grid: the same integer box factor `f` and whole boxes, so proxy pixel `(i, j)` averages the
//! full-resolution box `[i·f, (i + 1)·f) × [j·f, (j + 1)·f)` and its centre is the full-resolution
//! point `(f·i + (f − 1)/2, f·j + (f − 1)/2)`. It holds the box's mean linear RGB, summed in `f64`
//! in row order and rounded once to `f32`, and the box's largest [`SensorLevel`]: a box any of
//! whose pixels reached the roll-off weighs as that pixel does, as the resample takes the largest
//! tap. 14 bytes a proxy pixel, at most 14 MiB an input. It is built from the same row bands the
//! spool writes ([`ColourProxyBuilder::push_rows`]).
//!
//! **Proxy-scale registration.** A registered motion `q = c + R(θ)(p − c) + t` in full-resolution
//! coordinates is, on the proxy grid (`p = f·u + o`, `q = f·u' + o`, `o = (f − 1)/2`),
//! `u' = c' + R(θ)(u − c') + t/f` with `c' = (c − o)/f`: the same rotation about the proxy's centre
//! and the shift divided by the factor ([`proxy_transform`]). The identity stays the identity, so
//! an unmoved input is copied exactly.
//!
//! **The picture** ([`fuse_preview`]) covers the bracket's [`Coverage`] at proxy scale
//! ([`preview_rect`]): the proxy pixels whose centres lie in the coverage, shrunk until every
//! input's mapped corners read recorded proxy samples (the covered set is convex, so its corners
//! decide). It is drawn as the merge draws the result, at proxy scale: every input resampled once
//! by [`resample_rect`], the motion map's own proxy confidences and selections
//! ([`MotionMap::motion_on_proxy`]) and [`fuse_rect`]'s weights, normalization and feathering, in
//! bands of [`PREVIEW_BAND_ROWS`] rows, checking `cancel` per band. Because fusion is linear in
//! each input's normalized samples wherever its weights hold, a box every input recorded below the
//! roll-off fuses to the box mean of the full merge; the picture differs where a box straddles a
//! roll-off, a clipped site or a motion edge. Its [`FusionDiagnostics`] say whether highlights or
//! shadows were extended, by the fusion's own counts at proxy scale.
//!
//! **Display.** The picture is linear sRGB/D65 at the reference's brightness. [`display_pixel`]
//! clamps negative components to zero and passes values to the highlight shoulder start `k`
//! ([`SHOULDER_START`]) unchanged; above it, the largest channel `m` is compressed to
//! `k + (1 − k)·y/(1 + y)`, `y = (m − k)/(1 − k)`, which meets the line with slope 1 and approaches
//! 1, and every channel is scaled by the same ratio, keeping hue. The result is sRGB encoded and
//! quantized to 8 bits by the shared output quantizer. [`encode_preview_jpeg`] encodes it through
//! the export's JPEG codec. The picture is a display preview, never the saved pixels.
//!
//! **Overlays** ([`motion_overlays`]). Each 8-connected piece of a moving region inside the picture
//! is one [`MotionOverlay`]: the input shown in it, that input's offset, whether it is a fallback,
//! its bounds, and its outline: the piece's outer boundary traced along pixel edges (keeping
//! diagonal neighbours inside), simplified by Douglas–Peucker at [`OUTLINE_TOLERANCE`] pixels, the
//! tolerance doubled until at most [`MAX_OUTLINE_VERTICES`] vertices remain. Coordinates are the
//! picture's pixels: picture pixel `(i, j)` covers `[i, i + 1) × [j, j + 1)`. Holes are not drawn.
//!
//! **Bounds.** Every grid is the reference proxy's, at most 1 Mi pixels. The picture holds 12 bytes
//! a pixel (12 MiB at the largest); fusion holds one band of every input's aligned samples and the
//! band's motion selection ([`preview_scratch_bytes`], about 8 MiB for nine 1024-pixel-wide
//! inputs); the overlays one label a picture pixel and their outlines.

use super::align::{AlignmentEvidence, PixelRect, Registration, RigidTransform};
use super::coverage::Coverage;
use super::deghost::{MotionMap, MotionRect};
use super::exposure::ResolvedExposures;
use super::frame::{FrameRect, SensorLevel};
use super::fusion::{FusedRect, FusionDiagnostics, FusionParameters, fuse_rect};
use super::proxy::{AnalysisProxy, MIN_FRAME_SIDE};
use super::resample::{AlignedRect, SourceWindow, resample_rect};
use crate::{Cancel, Error};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Picture rows fused at a time: one band of every input's aligned samples.
pub const PREVIEW_BAND_ROWS: usize = 64;
/// The display transform's highlight shoulder starts at this linear level.
pub const SHOULDER_START: f64 = 0.7;
/// The Douglas–Peucker tolerance of a motion outline, in picture pixels.
pub const OUTLINE_TOLERANCE: f64 = 0.5;
/// The most vertices one motion outline keeps.
pub const MAX_OUTLINE_VERTICES: usize = 128;

/// One input's colour preview proxy. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct ColourProxy {
    frame_width: u32,
    frame_height: u32,
    factor: u32,
    width: usize,
    height: usize,
    rgb: [Vec<f32>; 3],
    peak: Vec<SensorLevel>,
}

impl ColourProxy {
    /// The bytes one colour proxy of a `width × height` frame holds.
    pub fn bytes_for(width: u32, height: u32) -> usize {
        let (w, h) = AnalysisProxy::dimensions_for(width, height);
        w * h * (3 * size_of::<f32>() + size_of::<SensorLevel>())
    }

    /// The full-resolution frame this reduces.
    pub fn frame_dimensions(&self) -> (u32, u32) {
        (self.frame_width, self.frame_height)
    }

    pub fn factor(&self) -> u32 {
        self.factor
    }

    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The proxy as resampling reads it: its mean samples and largest levels.
    pub fn view(&self) -> FrameRect<'_> {
        FrameRect {
            width: self.width,
            height: self.height,
            stride: self.width,
            rgb: [&self.rgb[0], &self.rgb[1], &self.rgb[2]],
            levels: &self.peak,
        }
    }
}

/// Builds one [`ColourProxy`] from a frame's rows in order, as [`super::ProxyBuilder`] builds the
/// analysis proxy: one row of box accumulators.
#[derive(Debug)]
pub struct ColourProxyBuilder {
    proxy: ColourProxy,
    rows: u32,
    sums: [Vec<f64>; 3],
    peak: Vec<SensorLevel>,
}

impl ColourProxyBuilder {
    /// A builder for a `width × height` upright frame; refused as the analysis proxy's is.
    pub fn new(width: u32, height: u32) -> Result<Self, Error> {
        crate::source::layout(width, height)?;
        if width < MIN_FRAME_SIDE || height < MIN_FRAME_SIDE {
            return Err(Error::validation(format!(
                "HDR alignment needs frames of at least {MIN_FRAME_SIDE} pixels a side"
            )));
        }
        let factor = AnalysisProxy::factor_for(width, height);
        let (w, h) = AnalysisProxy::dimensions_for(width, height);
        Ok(Self {
            proxy: ColourProxy {
                frame_width: width,
                frame_height: height,
                factor,
                width: w,
                height: h,
                rgb: std::array::from_fn(|_| Vec::with_capacity(w * h)),
                peak: Vec::with_capacity(w * h),
            },
            rows: 0,
            sums: std::array::from_fn(|_| vec![0.0; w]),
            peak: vec![SensorLevel(0); w],
        })
    }

    /// Take the next rows of the frame: `rows.width` must be the frame's width. Rows past the last
    /// whole box are counted and ignored.
    pub fn push_rows(&mut self, rows: &FrameRect<'_>) -> Result<(), Error> {
        rows.check()?;
        if rows.width != self.proxy.frame_width as usize {
            return Err(Error::validation("proxy rows must span the frame"));
        }
        let factor = self.proxy.factor as usize;
        let (w, h) = (self.proxy.width, self.proxy.height);
        let kept = w * factor;
        for y in 0..rows.height {
            if self.rows >= self.proxy.frame_height {
                return Err(Error::validation("proxy rows run past the frame"));
            }
            let complete = self.proxy.peak.len() == w * h;
            if !complete {
                for x in 0..kept {
                    let (rgb, level) = rows.at(x, y);
                    if !rgb.iter().all(|value| value.is_finite()) {
                        return Err(Error::validation("HDR input sample is not finite"));
                    }
                    let i = x / factor;
                    for (sum, value) in self.sums.iter_mut().zip(rgb) {
                        sum[i] += f64::from(value);
                    }
                    self.peak[i] = self.peak[i].max(level);
                }
            }
            self.rows += 1;
            if self.rows.is_multiple_of(self.proxy.factor) && !complete {
                let area = (factor * factor) as f64;
                for (plane, sum) in self.proxy.rgb.iter_mut().zip(&mut self.sums) {
                    plane.extend(sum.iter().map(|total| (total / area) as f32));
                    sum.fill(0.0);
                }
                self.proxy.peak.extend_from_slice(&self.peak);
                self.peak.fill(SensorLevel(0));
            }
        }
        Ok(())
    }

    /// The proxy, once every whole box is complete.
    pub fn finish(self) -> Result<ColourProxy, Error> {
        if self.proxy.peak.len() != self.proxy.width * self.proxy.height {
            return Err(Error::validation("proxy is missing frame rows"));
        }
        Ok(self.proxy)
    }
}

/// The longer side, in pixels, of the proxies a `width × height` bracket's preview is drawn from:
/// what the status line names ("Preview from 1,008-pixel frames" for a 6048 × 4024 bracket).
pub fn preview_proxy_side(width: u32, height: u32) -> u32 {
    let (w, h) = AnalysisProxy::dimensions_for(width, height);
    w.max(h) as u32
}

/// The alignment's largest residual: the largest predicted one-sigma position error of any fitted
/// input, in full-resolution pixels (`uncertainty_px`). `None` when nothing was fitted (alignment
/// off, or only the reference).
pub fn max_residual_px(registration: &Registration) -> Option<f64> {
    registration
        .inputs
        .iter()
        .filter_map(|input| match input.evidence {
            AlignmentEvidence::Fitted { uncertainty_px, .. } => Some(uncertainty_px),
            _ => None,
        })
        .reduce(f64::max)
}

/// `transform` (full-resolution coordinates) on the proxy grid of box factor `factor`: see the
/// module documentation.
pub fn proxy_transform(transform: &RigidTransform, factor: u32) -> RigidTransform {
    let f = f64::from(factor.max(1));
    let o = (f - 1.0) / 2.0;
    RigidTransform {
        center_x: (transform.center_x - o) / f,
        center_y: (transform.center_y - o) / f,
        rotation_rad: transform.rotation_rad,
        shift_x: transform.shift_x / f,
        shift_y: transform.shift_y / f,
    }
}

/// The picture's rectangle on the reference's `width × height` proxy grid of box factor `factor`:
/// the proxy pixels whose centres lie in `coverage`, shrunk until every one of `transforms` (proxy
/// scale, [`proxy_transform`]) maps its corners onto recorded proxy samples. Refused when nothing
/// remains.
pub fn preview_rect(
    coverage: &Coverage,
    factor: u32,
    width: usize,
    height: usize,
    transforms: &[RigidTransform],
) -> Result<PixelRect, Error> {
    let empty = || Error::validation("the HDR preview's coverage holds no proxy pixel");
    if width == 0 || height == 0 || coverage.rect.is_empty() {
        return Err(empty());
    }
    let f = f64::from(factor.max(1));
    let o = (f - 1.0) / 2.0;
    let rect = coverage.rect;
    // Proxy pixel u's centre f·u + o lies in [start, end] where u is in [ceil, floor].
    let first = |start: u32| ((f64::from(start) - o) / f).ceil().max(0.0) as i64;
    let last = |end: u32, length: usize| {
        (((f64::from(end) - o) / f).floor() as i64).min(length as i64 - 1)
    };
    let (mut x0, mut y0) = (first(rect.x), first(rect.y));
    let (mut x1, mut y1) = (
        last(rect.x + rect.width - 1, width),
        last(rect.y + rect.height - 1, height),
    );
    let affines: Vec<_> = transforms
        .iter()
        .filter(|transform| !transform.is_identity())
        .map(RigidTransform::affine)
        .collect();
    // A margin against the rounding of interior points between two corners on the boundary.
    const MARGIN: f64 = 1e-9;
    let (limit_x, limit_y) = ((width - 1) as f64 - MARGIN, (height - 1) as f64 - MARGIN);
    let covered = |x: i64, y: i64| {
        affines.iter().all(|affine| {
            let q = affine.map(x as f64, y as f64);
            q.0 >= MARGIN && q.1 >= MARGIN && q.0 <= limit_x && q.1 <= limit_y
        })
    };
    while x0 <= x1 && y0 <= y1 {
        // Each corner that reads outside moves both of its sides in by a pixel.
        let corners = [
            !covered(x0, y0),
            !covered(x1, y0),
            !covered(x0, y1),
            !covered(x1, y1),
        ];
        let left = corners[0] || corners[2];
        let right = corners[1] || corners[3];
        let top = corners[0] || corners[1];
        let bottom = corners[2] || corners[3];
        if !(left || right || top || bottom) {
            return Ok(PixelRect {
                x: x0 as u32,
                y: y0 as u32,
                width: (x1 - x0 + 1) as u32,
                height: (y1 - y0 + 1) as u32,
            });
        }
        x0 += i64::from(left);
        x1 -= i64::from(right);
        y0 += i64::from(top);
        y1 -= i64::from(bottom);
    }
    Err(empty())
}

/// The preview's picture: linear float32 sRGB/D65 at the reference's brightness, `width × height`
/// pixels covering `rect` of the reference's proxy grid (box factor `factor`), with the fusion's
/// counts.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPicture {
    pub rect: PixelRect,
    pub factor: u32,
    pub width: usize,
    pub height: usize,
    pub rgb: [Vec<f32>; 3],
    pub diagnostics: FusionDiagnostics,
}

impl PreviewPicture {
    /// The bytes a picture of a `width × height` bracket holds at most: its whole proxy grid.
    pub fn bytes_for(width: u32, height: u32) -> usize {
        let (w, h) = AnalysisProxy::dimensions_for(width, height);
        w * h * 3 * size_of::<f32>()
    }

    /// Whether a shorter exposure supplied highlights the reference lost.
    pub fn highlights_extended(&self) -> bool {
        self.diagnostics.highlight_extended > 0
    }

    /// Whether a longer exposure supplied shadows under the reference's shadow level.
    pub fn shadows_extended(&self) -> bool {
        self.diagnostics.shadow_extended > 0
    }

    /// Linear pixel `(x, y)`.
    pub fn at(&self, x: usize, y: usize) -> [f32; 3] {
        let i = y * self.width + x;
        [self.rgb[0][i], self.rgb[1][i], self.rgb[2][i]]
    }
}

/// The scratch [`fuse_preview`] holds beside the picture for `inputs` proxies `width` pixels wide:
/// one band of every input's aligned samples and levels and the band's motion selection.
pub fn preview_scratch_bytes(inputs: usize, width: usize) -> usize {
    let band = PREVIEW_BAND_ROWS * width;
    inputs * band * (3 * size_of::<f32>() + size_of::<SensorLevel>()) + band * 5
}

/// Draw the preview's picture: see the module documentation. `proxies`, `exposures` and
/// `registration` are in request order and of one bracket; `motion`, when deghosting found a map,
/// is on the same proxy grid and reference.
pub fn fuse_preview(
    proxies: &[ColourProxy],
    exposures: &ResolvedExposures,
    registration: &Registration,
    coverage: &Coverage,
    motion: Option<&MotionMap>,
    parameters: &FusionParameters,
    cancel: &Cancel,
) -> Result<PreviewPicture, Error> {
    let n = proxies.len();
    if n != registration.inputs.len() || n != exposures.inputs.len() {
        return Err(Error::validation(
            "the HDR preview takes one proxy per registered and resolved input",
        ));
    }
    if exposures.reference != registration.reference || registration.reference >= n {
        return Err(Error::validation(
            "the HDR preview needs the exposures of its registration's reference",
        ));
    }
    let frame = (registration.width, registration.height);
    if proxies
        .iter()
        .any(|proxy| proxy.frame_dimensions() != frame)
        || (coverage.frame_width, coverage.frame_height) != frame
    {
        return Err(Error::validation(
            "the HDR preview's proxies and coverage differ from the registration",
        ));
    }
    let base = &proxies[registration.reference];
    let (factor, (pw, ph)) = (base.factor, base.dimensions());
    if let Some(motion) = motion
        && (motion.factor() != factor
            || motion.grid() != (pw, ph)
            || motion.reference() != registration.reference)
    {
        return Err(Error::validation(
            "the HDR preview's motion map is not on its reference's proxy grid",
        ));
    }
    let transforms: Vec<RigidTransform> = registration
        .inputs
        .iter()
        .map(|input| proxy_transform(&input.transform, factor))
        .collect();
    let rect = preview_rect(coverage, factor, pw, ph, &transforms)?;
    let (width, height) = (rect.width as usize, rect.height as usize);
    let mut rgb: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0; width * height]);
    let mut aligned: Vec<AlignedRect> = (0..n).map(|_| AlignedRect::default()).collect();
    let mut selection = MotionRect::default();
    let mut diagnostics = FusionDiagnostics::default();
    for top in (0..height).step_by(PREVIEW_BAND_ROWS) {
        cancel.check()?;
        let rows = PREVIEW_BAND_ROWS.min(height - top);
        let band = PixelRect {
            x: rect.x,
            y: rect.y + top as u32,
            width: rect.width,
            height: rows as u32,
        };
        for ((proxy, transform), out) in proxies.iter().zip(&transforms).zip(&mut aligned) {
            let source = SourceWindow {
                x: 0,
                y: 0,
                frame: proxy.view(),
            };
            resample_rect(transform, &source, pw as u32, ph as u32, band, out, cancel)?;
        }
        if let Some(motion) = motion {
            motion.motion_on_proxy(band, &mut selection)?;
        }
        let views: Vec<FrameRect<'_>> = aligned.iter().map(AlignedRect::view).collect();
        let selected = selection.view();
        let span = top * width..(top + rows) * width;
        let [red, green, blue] = &mut rgb;
        let part = fuse_rect(
            exposures,
            parameters,
            &views,
            motion.map(|_| &selected),
            &mut FusedRect {
                width,
                height: rows,
                stride: width,
                rgb: [
                    &mut red[span.clone()],
                    &mut green[span.clone()],
                    &mut blue[span],
                ],
                limits: None,
            },
        )?;
        diagnostics.add(&part);
    }
    Ok(PreviewPicture {
        rect,
        factor,
        width,
        height,
        rgb,
        diagnostics,
    })
}

/// The highlight shoulder of the display transform: see the module documentation.
fn shoulder(m: f64) -> f64 {
    let k = SHOULDER_START;
    if m <= k {
        m
    } else {
        let y = (m - k) / (1.0 - k);
        k + (1.0 - k) * y / (1.0 + y)
    }
}

/// One linear pixel of the picture as display-referred 8-bit sRGB: see the module documentation.
pub fn display_pixel(rgb: [f32; 3]) -> [u8; 3] {
    let linear = rgb.map(|value| f64::from(value).max(0.0));
    let m = linear[0].max(linear[1]).max(linear[2]);
    let ratio = if m > SHOULDER_START && m.is_finite() {
        shoulder(m) / m
    } else {
        1.0
    };
    let quantizer = crate::colour::srgb::quantizer();
    linear.map(|value| quantizer.channel(value * ratio))
}

/// The picture as a display-referred sRGB JPEG ([`display_pixel`], the export's codec and
/// quality, its sRGB profile embedded), checking `cancel` between strips. The caller writes it.
pub fn encode_preview_jpeg(picture: &PreviewPicture, cancel: &Cancel) -> Result<Vec<u8>, Error> {
    if picture.width == 0 || picture.height == 0 {
        return Err(Error::validation("the HDR preview picture is empty"));
    }
    const ROWS: usize = 16;
    let width = picture.width;
    let bands = (0..picture.height).step_by(ROWS).map(|top| {
        let rows = ROWS.min(picture.height - top);
        let mut rgba = Vec::with_capacity(rows * width * 4);
        for y in top..top + rows {
            for x in 0..width {
                let [r, g, b] = display_pixel(picture.at(x, y));
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Ok((rows as u32, rgba))
    });
    let mut out = Vec::new();
    crate::export::encode::encode_jpeg_rows(
        &mut out,
        (width as u32, picture.height as u32),
        bands,
        &crate::export::encode::BASELINE_90,
        None,
        None,
        &mut |_| {},
        &|| cancel.check(),
    )?;
    Ok(out)
}

/// One moving region's piece inside the picture, as the sheet's deghost overlay draws it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionOverlay {
    /// The input shown in the region, in request order.
    pub input: usize,
    /// That input's resolved offset from the reference, in EV.
    pub offset_ev: f64,
    /// The reference did not record the region usably, so another exposure stands in.
    pub fallback: bool,
    /// The piece's bounds, in picture pixels.
    pub bounds: PixelRect,
    /// The piece's closed, simplified outer outline, in picture pixels; the last vertex joins the
    /// first.
    pub outline: Vec<[f64; 2]>,
}

/// The deghost overlay of `motion` over the picture covering `rect` of its proxy grid: one
/// [`MotionOverlay`] per 8-connected piece of a region inside `rect`, in raster order of the
/// pieces' first pixels. Checks `cancel` per piece.
pub fn motion_overlays(
    motion: &MotionMap,
    exposures: &ResolvedExposures,
    rect: PixelRect,
    cancel: &Cancel,
) -> Result<Vec<MotionOverlay>, Error> {
    let (gw, gh) = motion.grid();
    if rect.x as usize + rect.width as usize > gw || rect.y as usize + rect.height as usize > gh {
        return Err(Error::validation(
            "the HDR preview picture lies outside the motion map's grid",
        ));
    }
    let (w, h) = (rect.width as usize, rect.height as usize);
    let label = |x: usize, y: usize| motion.label(rect.x as usize + x, rect.y as usize + y);
    let mut piece = vec![0_u32; w * h];
    let mut overlays = Vec::new();
    let mut queue = VecDeque::new();
    for start in 0..w * h {
        let region = label(start % w, start / w);
        if region == 0 || piece[start] != 0 {
            continue;
        }
        cancel.check()?;
        let id = overlays.len() as u32 + 1;
        piece[start] = id;
        queue.push_back(start);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        while let Some(i) = queue.pop_front() {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let j = ny * w + nx;
                    if piece[j] == 0 && label(nx, ny) == region {
                        piece[j] = id;
                        queue.push_back(j);
                    }
                }
            }
        }
        let inside = |x: i64, y: i64| {
            x >= 0
                && y >= 0
                && (x as usize) < w
                && (y as usize) < h
                && piece[y as usize * w + x as usize] == id
        };
        let contour = trace(&inside, (start % w) as i64, (start / w) as i64, w, h)?;
        let outline = simplify(&contour, OUTLINE_TOLERANCE, MAX_OUTLINE_VERTICES);
        let shown = &motion.regions()[region as usize - 1];
        overlays.push(MotionOverlay {
            input: shown.selected,
            offset_ev: exposures
                .inputs
                .get(shown.selected)
                .map_or(0.0, |input| input.offset_ev),
            fallback: shown.fallback,
            bounds: PixelRect {
                x: x0 as u32,
                y: y0 as u32,
                width: (x1 - x0 + 1) as u32,
                height: (y1 - y0 + 1) as u32,
            },
            outline,
        });
    }
    Ok(overlays)
}

/// The outer boundary of the 8-connected piece `inside` holds whose first pixel in raster order is
/// `(x, y)`, traced along pixel edges with the piece on the right (y down), turning towards the
/// piece whenever the pixel ahead on the left is in it so diagonal neighbours stay inside. Its
/// vertices are the pixel corners where the boundary turns, from the first pixel's top-left corner.
pub(super) fn trace(
    inside: &impl Fn(i64, i64) -> bool,
    x: i64,
    y: i64,
    w: usize,
    h: usize,
) -> Result<Vec<(i64, i64)>, Error> {
    let start = (x, y);
    let mut vertex = start;
    let mut heading = (1_i64, 0_i64);
    let mut vertices = vec![start];
    // A boundary walks each pixel edge at most once.
    let limit = 2 * (w + 1) * (h + 1) + 4;
    for _ in 0..limit {
        vertex = (vertex.0 + heading.0, vertex.1 + heading.1);
        let left = (heading.1, -heading.0);
        let right = (-heading.1, heading.0);
        // A pixel's centre is its corner plus a half: from twice the centre, (2c − 1) / 2.
        let pixel = |side: (i64, i64)| {
            let doubled = (
                2 * vertex.0 + heading.0 + side.0,
                2 * vertex.1 + heading.1 + side.1,
            );
            inside((doubled.0 - 1).div_euclid(2), (doubled.1 - 1).div_euclid(2))
        };
        let next = if pixel(left) {
            left
        } else if pixel(right) {
            heading
        } else {
            right
        };
        if vertex == start && next == (1, 0) {
            return Ok(vertices);
        }
        if next != heading {
            vertices.push(vertex);
            heading = next;
        }
    }
    Err(Error::internal("an HDR motion outline did not close"))
}

/// Douglas–Peucker over the closed polygon `contour` at `tolerance`, doubled until at most `most`
/// (at least 3) vertices remain. The ring is anchored at its first vertex, the vertex furthest
/// from it and the vertex furthest from the chord between those two, so the outline always stays
/// a polygon, and each arc between anchors is simplified on its own. Under a tolerance of half a
/// pixel no single pixel corner is cut (a unit corner lies `√2/2` from its chord).
pub(super) fn simplify(contour: &[(i64, i64)], tolerance: f64, most: usize) -> Vec<[f64; 2]> {
    let points: Vec<[f64; 2]> = contour.iter().map(|&(x, y)| [x as f64, y as f64]).collect();
    if points.len() <= 3 {
        return points;
    }
    // Split the ring at its first vertex and the vertex furthest from it.
    let far = (1..points.len())
        .max_by(|&a, &b| {
            distance(points[0], points[a])
                .total_cmp(&distance(points[0], points[b]))
                .then(b.cmp(&a))
        })
        .unwrap_or(1);
    // And the vertex furthest from the chord between them, so the outline stays a polygon.
    let third = (1..points.len())
        .filter(|&k| k != far)
        .max_by(|&a, &b| {
            segment_distance(points[a], points[0], points[far])
                .total_cmp(&segment_distance(points[b], points[0], points[far]))
                .then(b.cmp(&a))
        })
        .unwrap_or(1);
    let mut tolerance = tolerance;
    loop {
        let mut keep = vec![false; points.len()];
        keep[0] = true;
        keep[far] = true;
        keep[third] = true;
        // The ring's three arcs between those anchors, each simplified on its own.
        let (a, b) = (far.min(third), far.max(third));
        let arcs: [Vec<usize>; 3] = [
            (0..=a).collect(),
            (a..=b).collect(),
            (b..points.len()).chain(std::iter::once(0)).collect(),
        ];
        for arc in &arcs {
            reduce(&points, arc, tolerance, &mut keep);
        }
        let kept: Vec<[f64; 2]> = points
            .iter()
            .zip(&keep)
            .filter(|(_, keep)| **keep)
            .map(|(point, _)| *point)
            .collect();
        if kept.len() <= most || tolerance > 1e6 {
            return kept;
        }
        tolerance *= 2.0;
    }
}

/// Mark the vertices of `chain` (indices into `points`, its ends kept) Douglas–Peucker keeps.
fn reduce(points: &[[f64; 2]], chain: &[usize], tolerance: f64, keep: &mut [bool]) {
    let mut stack = vec![(0, chain.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (start, end) = (points[chain[a]], points[chain[b]]);
        let (mut furthest, mut at) = (0.0, a);
        for (k, &i) in chain.iter().enumerate().take(b).skip(a + 1) {
            let d = segment_distance(points[i], start, end);
            if d > furthest {
                (furthest, at) = (d, k);
            }
        }
        if furthest > tolerance {
            keep[chain[at]] = true;
            stack.push((a, at));
            stack.push((at, b));
        }
    }
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The distance from `p` to the segment `a`–`b`.
fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx * dx + dy * dy;
    if length == 0.0 {
        return distance(p, a);
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / length).clamp(0.0, 1.0);
    distance(p, [a[0] + t * dx, a[1] + t * dy])
}
