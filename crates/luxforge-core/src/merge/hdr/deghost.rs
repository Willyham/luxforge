//! Motion: the aligned proxies on the reference's proxy grid ([`AlignedProxies`]), the moving
//! regions found on them ([`MotionMap`]), and the full-resolution [`MotionSelection`] of any
//! rectangle rendered from that map.
//!
//! Detection works on the aligned proxies, where a stop of exposure is a constant offset in
//! `log2` luminance:
//!
//! 1. Each input's log luminance is averaged over the usable pixels of its 3 × 3 neighbourhood
//!    (where its own pixel is usable) and brought to the reference by subtracting its resolved
//!    offset `o_k`.
//! 2. A pixel is a motion candidate where two inputs that both recorded it usably disagree by
//!    more than [`MOTION_THRESHOLD_EV`] (where either lost part of its neighbourhood, both
//!    averaged over the neighbours both recorded usably, so the edge of a still surface one
//!    exposure records past its roll-off or under its floor is no disagreement), or where one is
//!    past the roll-off (or under the noise floor) while the other's 3 × 3 neighbourhood, brought
//!    to its exposure, stays below
//!    [`CLIPPING_CONSISTENCY`] (above [`FLOOR_CONSISTENCY`]): differences exposure and clipping
//!    do not explain. A clipped sample is never compared by value.
//! 3. The candidates are opened by a 3 × 3 square (dropping isolated pixels and thin lines along
//!    edges a sub-pixel registration error leaves), then grown by [`MOTION_MARGIN`] pixels, and the
//!    8-connected components are the moving regions.
//! 4. Each region shows one exposure: the reference when at least [`REGION_USABLE`] of the
//!    region's pixels in it are covered and at most the highlight roll-off start
//!    ([`super::proxy::PROXY_CEILING`]); otherwise a recorded fallback: the exposure nearest the
//!    reference that is usable over that fraction, else the most usable one (nearest first, then
//!    the shorter, then request order).
//! 5. The confidence is 1 at least [`FEATHER`] pixels inside a region, falling by `1/FEATHER` per
//!    pixel (chessboard distance) to the region's edge and 0 outside: the boundary feathers rather
//!    than blending two positions of a subject.
//!
//! A full-resolution pixel's confidence is the bilinear interpolation of the proxy confidences
//! around its proxy position (clamped to the grid), and its selected input the region's of any of
//! those four proxy pixels; a 2 × 2 block is 8-connected, so it touches at most one region.
//! Elsewhere the selected input is the reference at confidence 0.
//!
//! Bounds: every pass is over the reference proxy grid (at most 1 Mi pixels). The aligned proxies
//! hold 8 bytes a pixel per input, detection a further 13 bytes per pixel per input and the map
//! 8 bytes a pixel. A region is at least one opened 3 × 3 core grown by the margin, 81 pixels, so
//! there are at most `pixels / 81` regions (12,945 on the largest proxy). `cancel` is checked
//! between passes and inputs.

use super::align::{DeghostMode, PixelRect, Registration};
use super::exposure::{FitSamples, MAX_FIT_SAMPLES, ResolvedExposures};
use super::frame::{MAX_INPUTS, MotionSelection, SensorLevel};
use super::proxy::{AnalysisProxy, PROXY_CEILING, PROXY_FLOOR, usable};
use crate::{Cancel, Error};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Two exposure-normalized samples further apart than this, in EV, disagree.
pub const MOTION_THRESHOLD_EV: f64 = 0.5;
/// How far, in proxy pixels, a moving region grows past its detected pixels.
pub const MOTION_MARGIN: usize = 3;
/// The width, in proxy pixels, of a region's confidence ramp.
pub const FEATHER: usize = 3;
/// The fraction of a region an exposure must record usably to show it.
pub const REGION_USABLE: f64 = 0.95;
/// A sample past the roll-off contradicts another exposure whose neighbourhood, brought to its
/// exposure, stays under this level fraction (more than half a stop below the roll-off start)...
pub const CLIPPING_CONSISTENCY: f64 = 0.5;
/// ...and a sample under the noise floor one whose neighbourhood stays over this, four times the
/// floor.
pub const FLOOR_CONSISTENCY: f64 = 4.0 * PROXY_FLOOR;

/// One input on the reference's proxy grid: `log2` luminance (`NaN` where the input did not record
/// the point, `-∞` where it recorded no light), largest level and mean level.
#[derive(Clone, Debug, PartialEq)]
struct AlignedProxy {
    log_luma: Vec<f32>,
    peak: Vec<SensorLevel>,
    mean: Vec<SensorLevel>,
}

impl AlignedProxy {
    #[inline]
    fn usable(&self, i: usize) -> bool {
        usable(self.log_luma[i], self.peak[i], self.mean[i])
    }

    /// Recorded and at most the roll-off start: what a region may show.
    #[inline]
    fn selectable(&self, i: usize) -> bool {
        !self.log_luma[i].is_nan()
            && !self.peak[i].is_clipped()
            && self.peak[i].fraction() <= PROXY_CEILING
    }
}

/// Every input's proxy resampled bilinearly onto the reference's proxy grid by its registered
/// motion (levels by the largest tap), for the exposure fit and motion detection.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignedProxies {
    reference: usize,
    factor: u32,
    width: usize,
    height: usize,
    frame_width: u32,
    frame_height: u32,
    inputs: Vec<AlignedProxy>,
}

impl AlignedProxies {
    /// Align `proxies` (request order) by `registration`. The proxies may be dropped afterwards;
    /// keep them only to register again on another reference.
    pub fn new(
        registration: &Registration,
        proxies: &[AnalysisProxy],
        cancel: &Cancel,
    ) -> Result<Self, Error> {
        if proxies.len() != registration.inputs.len() {
            return Err(Error::validation(
                "HDR motion takes one proxy per registered input",
            ));
        }
        let base = &proxies[registration.reference];
        if proxies
            .iter()
            .any(|proxy| proxy.frame_dimensions() != (registration.width, registration.height))
        {
            return Err(Error::validation(
                "HDR motion proxies differ from the registration",
            ));
        }
        let (factor, width, height) = (base.factor, base.width, base.height);
        let scale = f64::from(factor);
        let offset = (scale - 1.0) / 2.0;
        let mut inputs = Vec::with_capacity(proxies.len());
        for (proxy, alignment) in proxies.iter().zip(&registration.inputs) {
            cancel.check()?;
            if alignment.transform.is_identity() {
                inputs.push(AlignedProxy {
                    log_luma: proxy.log_luma.clone(),
                    peak: proxy.peak.clone(),
                    mean: proxy.mean.clone(),
                });
                continue;
            }
            let affine = alignment.transform.affine();
            let n = width * height;
            let mut aligned = AlignedProxy {
                log_luma: vec![f32::NAN; n],
                peak: vec![SensorLevel::CLIPPED; n],
                mean: vec![SensorLevel(0); n],
            };
            for y in 0..height {
                for x in 0..width {
                    let q = affine.map(scale * x as f64 + offset, scale * y as f64 + offset);
                    let (u, v) = ((q.0 - offset) / scale, (q.1 - offset) / scale);
                    if !(u >= 0.0
                        && v >= 0.0
                        && u <= (proxy.width - 1) as f64
                        && v <= (proxy.height - 1) as f64)
                    {
                        continue;
                    }
                    let x0 = (u.floor() as usize).min(proxy.width - 2);
                    let y0 = (v.floor() as usize).min(proxy.height - 2);
                    let (fx, fy) = (u - x0 as f64, v - y0 as f64);
                    let mut log = 0.0_f64;
                    let mut mean = 0.0_f64;
                    let mut peak = SensorLevel(0);
                    for (dx, dy, weight) in [
                        (0, 0, (1.0 - fx) * (1.0 - fy)),
                        (1, 0, fx * (1.0 - fy)),
                        (0, 1, (1.0 - fx) * fy),
                        (1, 1, fx * fy),
                    ] {
                        if weight == 0.0 {
                            continue;
                        }
                        let i = (y0 + dy) * proxy.width + x0 + dx;
                        log += weight * f64::from(proxy.log_luma[i]);
                        mean += weight * proxy.mean[i].fraction();
                        peak = peak.max(proxy.peak[i]);
                    }
                    let i = y * width + x;
                    // A tap that recorded no light leaves the point dark, never NaN.
                    aligned.log_luma[i] = if log.is_nan() {
                        f32::NEG_INFINITY
                    } else {
                        log as f32
                    };
                    aligned.peak[i] = peak;
                    aligned.mean[i] = SensorLevel::from_fraction(mean);
                }
            }
            inputs.push(aligned);
        }
        Ok(Self {
            reference: registration.reference,
            factor,
            width,
            height,
            frame_width: registration.width,
            frame_height: registration.height,
            inputs,
        })
    }

    pub fn reference(&self) -> usize {
        self.reference
    }

    /// Add grid points every input recorded to `samples`, until full: per point, each input's
    /// luminance as grey and its largest level, so the fit reads the same exposure-invariant
    /// evidence registration did.
    pub fn gather_fit_samples(&self, samples: &mut FitSamples) -> Result<(), Error> {
        let n = self.width * self.height;
        let step = ((n as f64 / MAX_FIT_SAMPLES as f64).sqrt().ceil() as usize).max(1);
        let mut rgb = vec![[0.0_f32; 3]; self.inputs.len()];
        let mut levels = vec![SensorLevel(0); self.inputs.len()];
        for y in (0..self.height).step_by(step) {
            for x in (0..self.width).step_by(step) {
                let i = y * self.width + x;
                if self.inputs.iter().any(|input| input.log_luma[i].is_nan()) {
                    continue;
                }
                for (k, input) in self.inputs.iter().enumerate() {
                    let luma = input.log_luma[i].exp2();
                    rgb[k] = [luma; 3];
                    levels[k] = input.peak[i];
                }
                if !samples.push(&rgb, &levels)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// One moving region.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionRegion {
    /// The region's proxy pixels and its bounds in full-resolution reference coordinates.
    pub proxy_pixels: u64,
    pub bounds: PixelRect,
    /// The input shown in it.
    pub selected: usize,
    /// The fractions of the region the reference and the selected input recorded usably.
    pub reference_usable: f64,
    pub selected_usable: f64,
    /// The reference was not usable here, so another exposure stands in.
    pub fallback: bool,
}

/// What motion detection did, for the merge's provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionDiagnostics {
    pub proxy_pixels: u64,
    /// Proxy pixels inside a moving region.
    pub motion_proxy_pixels: u64,
    pub regions: u32,
    /// Regions shown from the reference, and from a fallback exposure.
    pub reference_regions: u32,
    pub fallback_regions: u32,
    /// Fallback regions no exposure recorded usably over [`REGION_USABLE`] of their pixels.
    pub unusable_regions: u32,
    /// Per input, the regions shown from it.
    pub selected_regions: [u32; MAX_INPUTS],
}

/// The moving regions on the reference's proxy grid. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionMap {
    reference: usize,
    factor: u32,
    width: usize,
    height: usize,
    /// 0 outside every region, else the region's index plus one.
    labels: Vec<u32>,
    confidence: Vec<f32>,
    regions: Vec<MotionRegion>,
}

/// A full-resolution motion selection for one rectangle, reusable across rectangles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MotionRect {
    pub width: usize,
    pub height: usize,
    pub selected: Vec<u8>,
    pub confidence: Vec<f32>,
}

impl MotionRect {
    pub fn view(&self) -> MotionSelection<'_> {
        MotionSelection {
            width: self.width,
            height: self.height,
            stride: self.width,
            selected: &self.selected,
            confidence: &self.confidence,
        }
    }

    /// Whether any pixel feathers towards a selection.
    pub fn any(&self) -> bool {
        self.confidence.iter().any(|&c| c > 0.0)
    }
}

/// Find the moving regions of `aligned` with `exposures` resolved for the same request and
/// reference. `None` with deghosting off.
pub fn detect_motion(
    aligned: &AlignedProxies,
    exposures: &ResolvedExposures,
    mode: DeghostMode,
    cancel: &Cancel,
) -> Result<Option<MotionMap>, Error> {
    if mode == DeghostMode::Off {
        return Ok(None);
    }
    let n = aligned.inputs.len();
    if exposures.inputs.len() != n || exposures.reference != aligned.reference {
        return Err(Error::validation(
            "HDR motion needs the exposures of its registration's bracket and reference",
        ));
    }
    let (w, h) = (aligned.width, aligned.height);
    let pixels = w * h;
    // 1. Smoothed, exposure-normalized log luminance, and whether it saw its whole neighbourhood.
    let mut normalized = Vec::with_capacity(n);
    let mut whole = Vec::with_capacity(n);
    for (input, resolved) in aligned.inputs.iter().zip(&exposures.inputs) {
        cancel.check()?;
        let mut plane = vec![f32::NAN; pixels];
        let mut complete = vec![false; pixels];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if !input.usable(i) {
                    continue;
                }
                let (mut sum, mut count, mut neighbours) = (0.0_f64, 0_u32, 0_u32);
                for ny in y.saturating_sub(1)..(y + 2).min(h) {
                    for nx in x.saturating_sub(1)..(x + 2).min(w) {
                        let j = ny * w + nx;
                        neighbours += 1;
                        if input.usable(j) {
                            sum += f64::from(input.log_luma[j]);
                            count += 1;
                        }
                    }
                }
                plane[i] = (sum / f64::from(count) - resolved.offset_ev) as f32;
                complete[i] = count == neighbours;
            }
        }
        normalized.push(plane);
        whole.push(complete);
    }
    // Two inputs' neighbourhood means over the pixels both recorded usably, exposure-normalized:
    // where either lost part of its neighbourhood, its own mean would weigh pixels the other's
    // does not, and the edge of a surface one exposure records near its roll-off would read as a
    // disagreement.
    let shared = |a: usize, b: usize, i: usize| -> (f32, f32) {
        let (x, y) = (i % w, i / w);
        let (inputs, offsets) = (&aligned.inputs, &exposures.inputs);
        let (mut sum_a, mut sum_b, mut count) = (0.0_f64, 0.0_f64, 0_u32);
        for ny in y.saturating_sub(1)..(y + 2).min(h) {
            for nx in x.saturating_sub(1)..(x + 2).min(w) {
                let j = ny * w + nx;
                if inputs[a].usable(j) && inputs[b].usable(j) {
                    sum_a += f64::from(inputs[a].log_luma[j]);
                    sum_b += f64::from(inputs[b].log_luma[j]);
                    count += 1;
                }
            }
        }
        // Both centres are usable, so the count is at least one.
        let count = f64::from(count);
        (
            (sum_a / count - offsets[a].offset_ev) as f32,
            (sum_b / count - offsets[b].offset_ev) as f32,
        )
    };
    cancel.check()?;
    // 2. Candidates: two usable samples that disagree, or a sample past the roll-off (or under
    // the floor) where another exposure says it should have recorded the point usably.
    let mut bounds = Vec::with_capacity(n);
    for input in &aligned.inputs {
        cancel.check()?;
        bounds.push(neighbourhood_bounds(input, w, h));
    }
    cancel.check()?;
    let threshold = MOTION_THRESHOLD_EV as f32;
    let gain: Vec<Vec<f64>> = (0..n)
        .map(|a| {
            (0..n)
                .map(|b| (exposures.inputs[b].offset_ev - exposures.inputs[a].offset_ev).exp2())
                .collect()
        })
        .collect();
    let mut mask = vec![false; pixels];
    for (i, candidate) in mask.iter_mut().enumerate() {
        *candidate = (0..n).any(|a| {
            let va = normalized[a][i];
            if va.is_nan() {
                return false;
            }
            (0..n).any(|b| {
                if b == a {
                    return false;
                }
                let vb = normalized[b][i];
                if !vb.is_nan() {
                    let (va, vb) = if whole[a][i] && whole[b][i] {
                        (va, vb)
                    } else {
                        shared(a, b, i)
                    };
                    return (va - vb).abs() > threshold;
                }
                let other = &aligned.inputs[b];
                if other.log_luma[i].is_nan() {
                    return false;
                }
                let (low, high) = bounds[a][i];
                if other.peak[i].is_clipped() || other.peak[i].fraction() > PROXY_CEILING {
                    f64::from(high) * gain[a][b] < CLIPPING_CONSISTENCY
                } else if other.mean[i].fraction() < PROXY_FLOOR {
                    f64::from(low) * gain[a][b] > FLOOR_CONSISTENCY
                } else {
                    false
                }
            })
        });
    }
    drop((normalized, whole, bounds));
    cancel.check()?;
    // 3. Open, grow, label.
    mask = morphology(&mask, w, h, 1, false);
    mask = morphology(&mask, w, h, 1, true);
    mask = morphology(&mask, w, h, MOTION_MARGIN, true);
    cancel.check()?;
    let (labels, members) = label(&mask, w, h);
    cancel.check()?;
    // 4. One exposure per region.
    let reference = aligned.reference;
    let factor = aligned.factor;
    let mut regions = Vec::with_capacity(members.len());
    for pixels_of in &members {
        let count = pixels_of.len() as f64;
        let usable: Vec<f64> = aligned
            .inputs
            .iter()
            .map(|input| pixels_of.iter().filter(|&&i| input.selectable(i)).count() as f64 / count)
            .collect();
        // The fallback nearest the reference in exposure, the shorter first, then request order.
        let nearest = |a: &usize, b: &usize| {
            let (oa, ob) = (
                exposures.inputs[*a].offset_ev,
                exposures.inputs[*b].offset_ev,
            );
            oa.abs()
                .total_cmp(&ob.abs())
                .then(oa.total_cmp(&ob))
                .then(a.cmp(b))
        };
        let selected = if usable[reference] >= REGION_USABLE {
            reference
        } else if let Some(nearest) = (0..n)
            .filter(|&k| k != reference && usable[k] >= REGION_USABLE)
            .min_by(nearest)
        {
            nearest
        } else {
            (0..n)
                .min_by(|a, b| usable[*b].total_cmp(&usable[*a]).then(nearest(a, b)))
                .unwrap_or(reference)
        };
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for &i in pixels_of {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let to_frame = |p: usize| (p as u32).saturating_mul(factor);
        let right = (to_frame(x1 + 1)).min(aligned.frame_width);
        let bottom = (to_frame(y1 + 1)).min(aligned.frame_height);
        regions.push(MotionRegion {
            proxy_pixels: pixels_of.len() as u64,
            bounds: PixelRect {
                x: to_frame(x0),
                y: to_frame(y0),
                width: right - to_frame(x0),
                height: bottom - to_frame(y0),
            },
            selected,
            reference_usable: usable[reference],
            selected_usable: usable[selected],
            fallback: selected != reference,
        });
    }
    drop(members);
    cancel.check()?;
    // 5. Feathered confidence.
    let distance = chessboard_distance(&mask, w, h);
    let confidence = distance
        .iter()
        .map(|&d| (d as f32 / FEATHER as f32).min(1.0))
        .collect();
    Ok(Some(MotionMap {
        reference,
        factor,
        width: w,
        height: h,
        labels,
        confidence,
        regions,
    }))
}

/// Each pixel's smallest and largest mean level fraction over its recorded 3 × 3 neighbourhood.
fn neighbourhood_bounds(input: &AlignedProxy, w: usize, h: usize) -> Vec<(f32, f32)> {
    let mut bounds = vec![(f32::INFINITY, 0.0_f32); w * h];
    for y in 0..h {
        for x in 0..w {
            let (mut low, mut high) = (f32::INFINITY, 0.0_f32);
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let j = ny * w + nx;
                    if !input.log_luma[j].is_nan() {
                        let f = input.mean[j].fraction() as f32;
                        low = low.min(f);
                        high = high.max(f);
                    }
                }
            }
            bounds[y * w + x] = (low, high);
        }
    }
    bounds
}

/// Erode (`grow` false) or dilate (`grow` true) by a square of `radius`, separably. Pixels past
/// the edge count as set for erosion and unset for dilation, so the frame's edge neither eats nor
/// grows a region.
fn morphology(mask: &[bool], w: usize, h: usize, radius: usize, grow: bool) -> Vec<bool> {
    let pass = |source: &[bool], horizontal: bool| -> Vec<bool> {
        let mut out = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let (position, length) = if horizontal { (x, w) } else { (y, h) };
                let lo = position.saturating_sub(radius);
                let hi = (position + radius).min(length - 1);
                let mut values = (lo..=hi).map(|k| {
                    if horizontal {
                        source[y * w + k]
                    } else {
                        source[k * w + x]
                    }
                });
                out[y * w + x] = if grow {
                    values.any(|v| v)
                } else {
                    values.all(|v| v)
                };
            }
        }
        out
    };
    let across = pass(mask, true);
    pass(&across, false)
}

/// 8-connected components in raster order of their first pixel: each pixel's label (0 for none)
/// and each component's pixels.
fn label(mask: &[bool], w: usize, h: usize) -> (Vec<u32>, Vec<Vec<usize>>) {
    let mut labels = vec![0_u32; w * h];
    let mut members = Vec::new();
    let mut queue = VecDeque::new();
    for start in 0..w * h {
        if !mask[start] || labels[start] != 0 {
            continue;
        }
        let id = members.len() as u32 + 1;
        let mut pixels = Vec::new();
        labels[start] = id;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            pixels.push(i);
            let (x, y) = (i % w, i / w);
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let j = ny * w + nx;
                    if mask[j] && labels[j] == 0 {
                        labels[j] = id;
                        queue.push_back(j);
                    }
                }
            }
        }
        members.push(pixels);
    }
    (labels, members)
}

/// Each set pixel's chessboard distance to the nearest unset pixel (or past the frame's edge,
/// which does not count as unset): 1 on a region's boundary. 0 outside.
fn chessboard_distance(mask: &[bool], w: usize, h: usize) -> Vec<u32> {
    let far = (w + h) as u32;
    let mut d: Vec<u32> = mask.iter().map(|&m| if m { far } else { 0 }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let mut best = d[i];
            for (dx, dy) in [(-1_i64, 0_i64), (-1, -1), (0, -1), (1, -1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx >= 0 && ny >= 0 && (nx as usize) < w {
                    best = best.min(d[ny as usize * w + nx as usize] + 1);
                }
            }
            d[i] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if d[i] == 0 {
                continue;
            }
            let mut best = d[i];
            for (dx, dy) in [(1_i64, 0_i64), (1, 1), (0, 1), (-1, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx >= 0 && (nx as usize) < w && (ny as usize) < h {
                    best = best.min(d[ny as usize * w + nx as usize] + 1);
                }
            }
            d[i] = best;
        }
    }
    // A region reaching the frame's edge everywhere has no unset pixel: hold it at full confidence.
    d.iter().map(|&v| v.min(far)).collect()
}

impl MotionMap {
    pub fn regions(&self) -> &[MotionRegion] {
        &self.regions
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn diagnostics(&self) -> MotionDiagnostics {
        let mut diagnostics = MotionDiagnostics {
            proxy_pixels: (self.width * self.height) as u64,
            regions: self.regions.len() as u32,
            ..MotionDiagnostics::default()
        };
        for region in &self.regions {
            diagnostics.motion_proxy_pixels += region.proxy_pixels;
            diagnostics.selected_regions[region.selected] += 1;
            if region.fallback {
                diagnostics.fallback_regions += 1;
                if region.selected_usable < REGION_USABLE {
                    diagnostics.unusable_regions += 1;
                }
            } else {
                diagnostics.reference_regions += 1;
            }
        }
        diagnostics
    }

    /// The proxy region of reference proxy pixel `(x, y)`, if any.
    pub fn region_at(&self, x: usize, y: usize) -> Option<&MotionRegion> {
        let label = *self.labels.get(y * self.width + x)?;
        (label > 0).then(|| &self.regions[label as usize - 1])
    }

    /// The reference the map was detected for.
    pub fn reference(&self) -> usize {
        self.reference
    }

    /// The box factor of the proxy grid the map lies on.
    pub fn factor(&self) -> u32 {
        self.factor
    }

    /// The proxy grid's dimensions: the reference proxy's.
    pub fn grid(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The label of reference proxy pixel `(x, y)`: 0 outside every region, else the index into
    /// [`Self::regions`] plus one. 0 past the grid.
    pub fn label(&self, x: usize, y: usize) -> u32 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.labels[y * self.width + x]
    }

    /// Render the selection of `rect` on the proxy grid itself (proxy coordinates) into `out`:
    /// each proxy pixel's own confidence and its region's selected input, the reference at 0
    /// elsewhere. This is [`Self::render`] sampled at the proxy pixels' centres, for a picture
    /// drawn at proxy scale.
    pub fn render_proxy(&self, rect: PixelRect, out: &mut MotionRect) -> Result<(), Error> {
        let (width, height) = (rect.width as usize, rect.height as usize);
        if rect.x as usize + width > self.width || rect.y as usize + height > self.height {
            return Err(Error::validation(
                "HDR motion rectangle lies outside the proxy grid",
            ));
        }
        let n = width * height;
        out.width = width;
        out.height = height;
        out.selected.clear();
        out.selected.resize(n, self.reference as u8);
        out.confidence.clear();
        out.confidence.resize(n, 0.0);
        for y in 0..height {
            let row = (rect.y as usize + y) * self.width + rect.x as usize;
            for x in 0..width {
                let label = self.labels[row + x];
                if label == 0 {
                    continue;
                }
                let o = y * width + x;
                out.confidence[o] = self.confidence[row + x].clamp(0.0, 1.0);
                out.selected[o] = self.regions[label as usize - 1].selected as u8;
            }
        }
        Ok(())
    }

    /// Render the full-resolution selection of `rect` (reference coordinates) into `out`. See
    /// the module documentation.
    pub fn render(&self, rect: PixelRect, out: &mut MotionRect) -> Result<(), Error> {
        let (width, height) = (rect.width as usize, rect.height as usize);
        if width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > super::fusion::MAX_FUSION_PIXELS)
        {
            return Err(Error::resource_limit(
                "HDR motion rectangle exceeds 1 Mi pixels",
            ));
        }
        let n = width * height;
        out.width = width;
        out.height = height;
        out.selected.clear();
        out.selected.resize(n, self.reference as u8);
        out.confidence.clear();
        out.confidence.resize(n, 0.0);
        if self.regions.is_empty() {
            return Ok(());
        }
        let scale = f64::from(self.factor);
        let offset = (scale - 1.0) / 2.0;
        let position = |value: u32, length: usize| {
            let u = ((f64::from(value) - offset) / scale).clamp(0.0, (length - 1) as f64);
            let base = (u.floor() as usize).min(length - 2);
            (base, u - base as f64)
        };
        let columns: Vec<(usize, f64)> = (0..width)
            .map(|x| position(rect.x + x as u32, self.width))
            .collect();
        for y in 0..height {
            let (y0, fy) = position(rect.y + y as u32, self.height);
            for (x, &(x0, fx)) in columns.iter().enumerate() {
                let i = y0 * self.width + x0;
                let taps = [i, i + 1, i + self.width, i + self.width + 1];
                let label = taps.iter().map(|&t| self.labels[t]).max().unwrap_or(0);
                if label == 0 {
                    continue;
                }
                let c = |t: usize| f64::from(self.confidence[taps[t]]);
                let value = (c(0) * (1.0 - fx) + c(1) * fx) * (1.0 - fy)
                    + (c(2) * (1.0 - fx) + c(3) * fx) * fy;
                let o = y * width + x;
                out.confidence[o] = (value as f32).clamp(0.0, 1.0);
                out.selected[o] = self.regions[label as usize - 1].selected as u8;
            }
        }
        Ok(())
    }
}
