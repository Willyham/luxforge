//! A stitch's output geometry: the field the registered frames span, the projection's bounding
//! rectangle at the output's scale, which frames cover which output points, the largest covered
//! rectangle and the frames' stack order (`docs/design/panorama-merge.md`, "Projection and output
//! size").
//!
//! **A frame's footprint.** A world direction `d` records in frame `i` at
//! [`frame_position`]: its camera direction `Rᵢ·d`, in front of the camera, through the shared
//! lens. Under barrel distortion (`k < 0`) a direction whose ideal radius reaches the fold
//! `R/√(3|k|)` has no position, so nothing far outside the frame folds back into it. A point is
//! covered by the frame when its position lies within the frame's pixel centres,
//! `0 ≤ x ≤ W − 1` and `0 ≤ y ≤ H − 1` ([`inside`]): every bilinear tap then reads a recorded
//! pixel.
//!
//! **The bounding rectangle.** Each frame's boundary is sampled at [`EDGE_SAMPLES`] steps an edge
//! (its corners included), as world directions. A yaw interval per frame is its boundary's yaws
//! unwrapped about its optical axis; under Spherical a frame holding a pole spans every yaw and
//! reaches latitude ±90°. When the frames' yaw intervals cover the whole circle the stitch wraps:
//! its `u` runs over `[−π, π)` and its width is a whole turn's pixels, so the two ends meet
//! without a repeated column. Otherwise each interval is unwrapped to end before the centre of the widest
//! uncovered gap and `u` runs from the smallest start to the largest end. `v` runs over the
//! boundary samples' range (a pole's ±π/2 under Spherical). With `s` the output pixels per plane
//! unit, output point `(X, Y)` is the plane point `(u₀ + X/s, v₀ + Y/s)`, and the rectangle is
//! `⌊s·(u₁ − u₀) + ε⌋ + 1` by `⌊s·(v₁ − v₀) + ε⌋ + 1` pixels (`⌊2π·s + ε⌋` wide when it wraps),
//! with `ε` = [`SIZE_SLACK`].
//!
//! **The scale.** At the reference frame's centre pixel the output's local scale, output pixels
//! per input pixel, is the largest singular value of `s·J`, `J` the derivative of the plane point
//! by the reference's pixel there. Full scale `s₀ = 1/σ_max(J)`: about one output pixel to one
//! input pixel, never more. When the bounding rectangle at `s₀` exceeds the limits (65,000 pixels
//! a side, 512 MP and 6 GiB of float RGB for a paged derived source), the output is scaled down
//! uniformly by the largest `k < 1` that fits ([`fit_scale`]); a `k` under [`MIN_OUTPUT_SCALE`]
//! is refused as [`Refusal::OutputTooLarge`]. The scale fits the bounding rectangle, so every
//! Boundary Warp amount shares one scale and one seam map.

use super::camera::{Lens, Rotation};
use super::linalg::V3;
use super::projection::{Field, Projection, latitude, wrap_angle};
use super::refusal::Refusal;
use crate::Error;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Boundary samples an edge of each frame, for its footprint's extent.
pub const EDGE_SAMPLES: usize = 256;
/// A stitch that fits the limits only below this scale is refused as too large.
pub const MIN_OUTPUT_SCALE: f64 = 0.125;
/// Added to an extent in pixels before it is floored, so an extent that is a whole number of
/// pixels in exact arithmetic counts that whole number however it rounds.
pub const SIZE_SLACK: f64 = 1e-6;

/// The plane limits an output must fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OutputLimits {
    pub max_side: u32,
    pub max_pixels: u64,
    pub max_bytes: u64,
}

impl OutputLimits {
    /// A paged derived source's limits.
    pub(crate) fn paged() -> Self {
        let limits = crate::source::PAGED_DERIVED_SOURCE;
        Self {
            max_side: limits.max_side,
            max_pixels: limits.max_pixels,
            max_bytes: limits.max_bytes,
        }
    }

    fn fits(&self, width: u64, height: u64) -> bool {
        width <= u64::from(self.max_side)
            && height <= u64::from(self.max_side)
            && width * height <= self.max_pixels
            && width * height * 12 <= self.max_bytes
    }
}

/// Where world direction `d` (unit) records in a frame at `rotation`, or `None` behind the camera
/// or past barrel distortion's fold. See the module documentation.
#[inline]
pub(crate) fn frame_position(lens: &Lens, rotation: &Rotation, d: V3) -> Option<[f64; 2]> {
    let c = rotation.to_camera(d);
    if c[2] <= 1e-9 {
        return None;
    }
    let ideal = [lens.focal_px * c[0] / c[2], lens.focal_px * c[1] / c[2]];
    if lens.radial < 0.0 {
        let r = lens.radial_scale();
        if 3.0 * -lens.radial * (ideal[0] * ideal[0] + ideal[1] * ideal[1]) >= r * r {
            return None;
        }
    }
    let recorded = lens.distort(ideal);
    let (cx, cy) = lens.centre();
    Some([cx + recorded[0], cy + recorded[1]])
}

/// Whether frame position `q` lies within a `width × height` frame's pixel centres.
#[inline]
pub(crate) fn inside(q: [f64; 2], width: u32, height: u32) -> bool {
    q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= f64::from(width) - 1.0 && q[1] <= f64::from(height) - 1.0
}

/// How far `q` lies inside the frame: the distance to its nearest edge, negative outside.
#[inline]
pub(crate) fn depth(q: [f64; 2], width: u32, height: u32) -> f64 {
    let (w, h) = (f64::from(width) - 1.0, f64::from(height) - 1.0);
    q[0].min(w - q[0]).min(q[1]).min(h - q[1])
}

/// The world directions of a frame's boundary, clockwise from the top left, [`EDGE_SAMPLES`] an
/// edge.
pub(crate) fn boundary(lens: &Lens, rotation: &Rotation) -> Vec<V3> {
    let (w, h) = (f64::from(lens.width) - 1.0, f64::from(lens.height) - 1.0);
    let n = EDGE_SAMPLES;
    let mut points = Vec::with_capacity(4 * n);
    for k in 0..n {
        let t = k as f64 / n as f64;
        points.push((w * t, 0.0));
    }
    for k in 0..n {
        let t = k as f64 / n as f64;
        points.push((w, h * t));
    }
    for k in 0..n {
        let t = k as f64 / n as f64;
        points.push((w * (1.0 - t), h));
    }
    for k in 0..n {
        let t = k as f64 / n as f64;
        points.push((0.0, h * (1.0 - t)));
    }
    points
        .into_iter()
        .map(|p| lens.world_ray(rotation, p))
        .collect()
}

/// Whether a frame records world direction `d`.
fn records(lens: &Lens, rotation: &Rotation, d: V3) -> bool {
    frame_position(lens, rotation, d).is_some_and(|q| inside(q, lens.width, lens.height))
}

/// The poles a frame holds: up (`−y`) and down (`+y`).
fn poles(lens: &Lens, rotation: &Rotation) -> [bool; 2] {
    [
        records(lens, rotation, [0.0, -1.0, 0.0]),
        records(lens, rotation, [0.0, 1.0, 0.0]),
    ]
}

/// One frame's footprint extents.
#[derive(Clone, Copy, Debug)]
struct Footprint {
    /// The yaw interval, unwrapped about the frame's axis; `None` when it holds a pole.
    yaw: Option<[f64; 2]>,
    latitude: [f64; 2],
    /// The largest `|x/z|` and `|y/z|`, or `None` when part of it lies behind Perspective's plane.
    perspective: Option<[f64; 2]>,
}

fn footprint(lens: &Lens, rotation: &Rotation, samples: &[V3]) -> Footprint {
    let [up, down] = poles(lens, rotation);
    let mut lat = [f64::INFINITY, f64::NEG_INFINITY];
    let mut perspective = Some([0.0_f64, 0.0_f64]);
    for d in samples {
        let l = latitude(*d);
        lat = [lat[0].min(l), lat[1].max(l)];
        perspective = match (perspective, Projection::Perspective.plane(*d)) {
            (Some([u, v]), Some(p)) => Some([u.max(p[0].abs()), v.max(p[1].abs())]),
            _ => None,
        };
    }
    if up {
        lat[0] = -PI / 2.0;
    }
    if down {
        lat[1] = PI / 2.0;
    }
    let yaw = (!(up || down)).then(|| {
        let axis = rotation.forward();
        let centre = axis[0].atan2(axis[2]);
        let mut range = [f64::INFINITY, f64::NEG_INFINITY];
        for d in samples {
            let delta = wrap_angle(d[0].atan2(d[2]) - centre);
            range = [range[0].min(delta), range[1].max(delta)];
        }
        [centre + range[0], centre + range[1]]
    });
    Footprint {
        yaw,
        latitude: lat,
        perspective,
    }
}

/// The yaw extent of a set of intervals on the circle: `None` when they cover it, else the centre
/// of the widest uncovered gap.
fn gap_centre(intervals: &[[f64; 2]]) -> Option<f64> {
    let mut segments: Vec<[f64; 2]> = Vec::with_capacity(2 * intervals.len());
    for &[lo, hi] in intervals {
        if hi - lo >= 2.0 * PI {
            return None;
        }
        let a = wrap_angle(lo);
        let a = if a >= PI { a - 2.0 * PI } else { a };
        let b = a + (hi - lo);
        if b <= PI {
            segments.push([a, b]);
        } else {
            segments.push([a, PI]);
            segments.push([-PI, b - 2.0 * PI]);
        }
    }
    segments.sort_by(|x, y| x[0].total_cmp(&y[0]).then(x[1].total_cmp(&y[1])));
    // Sweep from −π, recording each uncovered gap.
    let mut gaps: Vec<[f64; 2]> = Vec::new();
    let mut reach = f64::NEG_INFINITY;
    let mut first = f64::INFINITY;
    for [a, b] in &segments {
        first = first.min(*a);
        if reach.is_finite() && *a > reach {
            gaps.push([reach, *a]);
        }
        reach = reach.max(*b);
    }
    // The gap across ±π, from the last end round to the first start.
    let wrap_gap = first + 2.0 * PI - reach;
    if gaps.is_empty() && wrap_gap <= 0.0 {
        return None;
    }
    let mut best = (wrap_gap, (reach + first + 2.0 * PI) / 2.0);
    for [a, b] in gaps {
        if b - a > best.0 {
            best = (b - a, (a + b) / 2.0);
        }
    }
    Some(wrap_angle(best.1))
}

/// The frames' field, and their yaw and latitude extents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Extent {
    pub field: Field,
    /// The frames' yaws cover the circle.
    pub wrap: bool,
    /// The centre of the widest uncovered yaw gap, each interval unwrapped to end before it.
    gap: Option<f64>,
    yaw: [f64; 2],
    latitude: [f64; 2],
}

/// The frames' footprints: their field and, per projection, their plane extents.
pub(crate) struct Footprints {
    frames: Vec<Footprint>,
    samples: Vec<Vec<V3>>,
    pub extent: Extent,
}

impl Footprints {
    pub(crate) fn of(lens: &Lens, rotations: &[Rotation]) -> Self {
        let samples: Vec<Vec<V3>> = rotations.iter().map(|r| boundary(lens, r)).collect();
        let frames: Vec<Footprint> = rotations
            .iter()
            .zip(&samples)
            .map(|(r, samples)| footprint(lens, r, samples))
            .collect();
        let latitude = frames
            .iter()
            .fold([f64::INFINITY, f64::NEG_INFINITY], |l, f| {
                [l[0].min(f.latitude[0]), l[1].max(f.latitude[1])]
            });
        let intervals: Vec<[f64; 2]> = frames.iter().filter_map(|f| f.yaw).collect();
        let pole = intervals.len() < frames.len();
        let gap = if pole { None } else { gap_centre(&intervals) };
        let (wrap, yaw) = match gap {
            None => (true, [-PI, PI]),
            Some(centre) => {
                let mut yaw = [f64::INFINITY, f64::NEG_INFINITY];
                for &[lo, hi] in &intervals {
                    let shift = unwrap_shift(lo, centre);
                    yaw = [yaw[0].min(lo + shift), yaw[1].max(hi + shift)];
                }
                (false, yaw)
            }
        };
        let perspective = frames.iter().try_fold([0.0_f64, 0.0_f64], |e, f| {
            f.perspective.map(|p| [e[0].max(p[0]), e[1].max(p[1])])
        });
        let field = Field {
            horizontal_deg: if wrap {
                360.0
            } else {
                (yaw[1] - yaw[0]).to_degrees()
            },
            vertical_deg: (latitude[1] - latitude[0]).to_degrees(),
            max_latitude_deg: latitude[0].abs().max(latitude[1].abs()).to_degrees(),
            perspective_extent: perspective,
        };
        Self {
            frames,
            samples,
            extent: Extent {
                field,
                wrap,
                gap,
                yaw,
                latitude,
            },
        }
    }

    /// The plane rectangle `[u₀, v₀, u₁, v₁]` the frames span under `projection`, and whether it
    /// wraps.
    pub(crate) fn plane_extent(&self, projection: Projection) -> ([f64; 4], bool) {
        let e = &self.extent;
        match projection {
            Projection::Perspective => {
                let mut r = [
                    f64::INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::NEG_INFINITY,
                ];
                for samples in &self.samples {
                    for d in samples {
                        if let Some([u, v]) = projection.plane(*d) {
                            r = [r[0].min(u), r[1].min(v), r[2].max(u), r[3].max(v)];
                        }
                    }
                }
                (r, false)
            }
            Projection::Cylindrical | Projection::Spherical => {
                let (mut v0, mut v1) = (f64::INFINITY, f64::NEG_INFINITY);
                for samples in &self.samples {
                    for d in samples {
                        if let Some([_, v]) = projection.plane(*d) {
                            v0 = v0.min(v);
                            v1 = v1.max(v);
                        }
                    }
                }
                if projection == Projection::Spherical {
                    v0 = v0.min(e.latitude[0]);
                    v1 = v1.max(e.latitude[1]);
                }
                if e.wrap {
                    ([-PI, v0, PI, v1], true)
                } else {
                    ([e.yaw[0], v0, e.yaw[1], v1], false)
                }
            }
        }
    }

    /// A frame's extent in the plane, `[u₀, v₀, u₁, v₁]`, with its yaw unwrapped as the stitch's
    /// is; infinite in `u` when the frame spans every yaw.
    pub(crate) fn frame_extent(&self, frame: usize, projection: Projection) -> [f64; 4] {
        let mut r = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for d in &self.samples[frame] {
            if let Some([u, v]) = projection.plane(*d) {
                r = [r[0].min(u), r[1].min(v), r[2].max(u), r[3].max(v)];
            }
        }
        let f = &self.frames[frame];
        if projection.is_angular() {
            match f.yaw {
                None => {
                    r[0] = f64::NEG_INFINITY;
                    r[2] = f64::INFINITY;
                    if projection == Projection::Spherical {
                        r[1] = r[1].min(f.latitude[0]);
                        r[3] = r[3].max(f.latitude[1]);
                    }
                }
                Some([lo, hi]) => {
                    let shift = self
                        .extent
                        .gap
                        .map_or(0.0, |centre| unwrap_shift(lo, centre));
                    r[0] = lo + shift;
                    r[2] = hi + shift;
                }
            }
        }
        r
    }
}

/// The multiple of 2π that moves `lo` into `(centre − 2π, centre]`.
fn unwrap_shift(lo: f64, centre: f64) -> f64 {
    let mut shift = 0.0;
    while lo + shift > centre {
        shift -= 2.0 * PI;
    }
    while lo + shift <= centre - 2.0 * PI {
        shift += 2.0 * PI;
    }
    shift
}

/// Full scale: output pixels per plane unit at which the reference frame's centre pixel maps to
/// at most one output pixel either way. See the module documentation.
pub(crate) fn full_scale(projection: Projection, lens: &Lens, reference: &Rotation) -> f64 {
    let d = reference.forward();
    let a = projection.differential(d, reference.right());
    let b = projection.differential(d, reference.down());
    let j = [
        [a[0] / lens.focal_px, b[0] / lens.focal_px],
        [a[1] / lens.focal_px, b[1] / lens.focal_px],
    ];
    1.0 / largest_singular_value(j)
}

/// The largest singular value of a 2 × 2 matrix.
pub(crate) fn largest_singular_value(j: [[f64; 2]; 2]) -> f64 {
    let t = j[0][0] * j[0][0] + j[0][1] * j[0][1] + j[1][0] * j[1][0] + j[1][1] * j[1][1];
    let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
    ((t + (t * t - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt()
}

/// The output's dimensions at `scale` (output pixels per plane unit) over the plane rectangle.
pub(crate) fn dimensions(extent: [f64; 4], wrap: bool, scale: f64) -> (u64, u64) {
    let width = if wrap {
        (2.0 * PI * scale + SIZE_SLACK).floor()
    } else {
        (scale * (extent[2] - extent[0]) + SIZE_SLACK).floor() + 1.0
    };
    let height = (scale * (extent[3] - extent[1]) + SIZE_SLACK).floor() + 1.0;
    (width.max(1.0) as u64, height.max(1.0) as u64)
}

/// The scale `k ≤ 1` bringing the bounding rectangle at full scale `s₀` within `limits`: 1 when
/// it fits, otherwise the largest scale found that fits; refused under [`MIN_OUTPUT_SCALE`].
pub(crate) fn fit_scale(
    extent: [f64; 4],
    wrap: bool,
    full: f64,
    limits: &OutputLimits,
) -> Result<f64, Error> {
    let (w, h) = dimensions(extent, wrap, full);
    if limits.fits(w, h) {
        return Ok(1.0);
    }
    let side = f64::from(limits.max_side) / w.max(h) as f64;
    let pixels =
        (limits.max_pixels.min(limits.max_bytes / 12) as f64 / (w as f64 * h as f64)).sqrt();
    let mut k = side.min(pixels).min(1.0);
    for _ in 0..64 {
        if k < MIN_OUTPUT_SCALE {
            break;
        }
        let (w, h) = dimensions(extent, wrap, full * k);
        if limits.fits(w, h) {
            return Ok(k);
        }
        k *= 0.999;
    }
    Err(Refusal::OutputTooLarge {
        width: w,
        height: h,
    }
    .into())
}

/// The output's frame of reference: the projection, the plane's origin and scale, and the
/// bounding rectangle's size. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputGeometry {
    pub projection: Projection,
    /// Output pixels per plane unit.
    pub plane_scale: f64,
    /// The uniform scale-down from full scale, at most 1.
    pub scale: f64,
    /// The plane point of bounding-rectangle pixel `(0, 0)`.
    pub origin: [f64; 2],
    /// The bounding rectangle, in output pixels.
    pub width: u32,
    pub height: u32,
    /// The rectangle spans the full circle of yaws: its two ends meet.
    pub wrap: bool,
}

impl OutputGeometry {
    /// The world direction (unit) bounding-rectangle point `p` shows.
    #[inline]
    pub fn direction(&self, p: [f64; 2]) -> V3 {
        self.projection.direction([
            self.origin[0] + p[0] / self.plane_scale,
            self.origin[1] + p[1] / self.plane_scale,
        ])
    }

    /// The bounding-rectangle point of world direction `d`, its yaw unwrapped to lie nearest
    /// `near_u` (plane units) under an angular projection.
    pub fn point(&self, d: V3, near_u: f64) -> Option<[f64; 2]> {
        let [mut u, v] = self.projection.plane(d)?;
        if self.projection.is_angular() {
            u = near_u + wrap_angle(u - near_u);
        }
        Some([
            (u - self.origin[0]) * self.plane_scale,
            (v - self.origin[1]) * self.plane_scale,
        ])
    }
}

/// The largest rectangle of `true` cells in a row-major `width × height` grid: the largest area,
/// ties to the smallest top row, then the smallest bottom row, then the smallest left column.
/// `[x, y, w, h]`; all zero when no cell is set.
pub(crate) fn largest_rectangle(cells: &[bool], width: usize, height: usize) -> [usize; 4] {
    let mut heights = vec![0_usize; width];
    let mut best: Option<(usize, [usize; 4])> = None;
    let mut stack: Vec<(usize, usize)> = Vec::with_capacity(width + 1);
    for y in 0..height {
        for x in 0..width {
            heights[x] = if cells[y * width + x] {
                heights[x] + 1
            } else {
                0
            };
        }
        // Every maximal rectangle with its bottom on row y, by the histogram's stack.
        stack.clear();
        #[allow(clippy::needless_range_loop)]
        for x in 0..=width {
            let h = if x < width { heights[x] } else { 0 };
            let mut start = x;
            while let Some(&(left, top_h)) = stack.last() {
                if top_h < h {
                    break;
                }
                stack.pop();
                if top_h > h {
                    // The tallest rectangle over [left, x) at height top_h.
                    let area = top_h * (x - left);
                    let rect = [left, y + 1 - top_h, x - left, top_h];
                    let better = best.is_none_or(|(best_area, b)| {
                        area > best_area
                            || (area == best_area
                                && (rect[1], rect[1] + rect[3], rect[0])
                                    < (b[1], b[1] + b[3], b[0]))
                    });
                    if better {
                        best = Some((area, rect));
                    }
                }
                start = left;
            }
            if h > 0 {
                stack.push((start, h));
            }
        }
    }
    best.map_or([0; 4], |(_, rect)| rect)
}

/// The frames' stack order: rows top to bottom, each left to right, from where each frame's centre
/// lands in the bounding rectangle. Frames whose centres lie within half a frame's height of the
/// row's first share its row. Ties keep request order.
pub(crate) fn stack_order(centres: &[[f64; 2]], row_gap: f64) -> Vec<usize> {
    let mut by_height: Vec<usize> = (0..centres.len()).collect();
    by_height.sort_by(|&a, &b| centres[a][1].total_cmp(&centres[b][1]).then(a.cmp(&b)));
    let mut order = Vec::with_capacity(centres.len());
    let mut start = 0;
    while start < by_height.len() {
        let top = centres[by_height[start]][1];
        let mut end = start + 1;
        while end < by_height.len() && centres[by_height[end]][1] - top <= row_gap {
            end += 1;
        }
        let mut row: Vec<usize> = by_height[start..end].to_vec();
        row.sort_by(|&a, &b| centres[a][0].total_cmp(&centres[b][0]).then(a.cmp(&b)));
        order.extend(row);
        start = end;
    }
    order
}
