//! The working raster the picker counts on (see the module documentation), and the stroke's
//! footprint on it. Transcribed from the prototype's stroke module.
use super::{NESTED_SHARE, Stroke};
use crate::modules::ai::segment::{Logits, Point};
use std::borrow::Cow;

/// One sub-cell spans at most this many analysis pixels along each axis.
const WORKING_CELL_PX: f64 = 4.0;

pub(crate) fn well_formed(l: &Logits) -> bool {
    l.width > 0
        && l.height > 0
        && l.data.len() == l.width * l.height
        && l.cells_per_px.0.is_finite()
        && l.cells_per_px.1.is_finite()
        && l.cells_per_px.0 > 0.0
        && l.cells_per_px.1 > 0.0
}

/// Cells along one axis whose centres lie inside the source, at least one for a non-empty source.
fn valid_along(cells: usize, cells_per_px: f32, source: u32) -> usize {
    if cells == 0 || source == 0 {
        return 0;
    }
    let n = (source as f64 * cells_per_px as f64 - 0.5).ceil().max(1.0);
    (n as usize).min(cells)
}

/// The logit cells whose centres lie inside the source image, from the top-left: SAM's automatic
/// mask generator likewise scores masks cropped to the image, so a padded frame's padding is out.
pub(crate) fn valid_cells(l: &Logits) -> (usize, usize) {
    if !well_formed(l) {
        return (0, 0);
    }
    (
        valid_along(l.width, l.cells_per_px.0, l.source.0),
        valid_along(l.height, l.cells_per_px.1, l.source.1),
    )
}

/// Inclusive cell bounds `(u0, v0, u1, v1)` of the logits above `threshold` among the cells that
/// can influence the image: the valid cells and the first row and column beyond them.
pub(crate) fn cells_above(l: &Logits, threshold: f32) -> Option<(usize, usize, usize, usize)> {
    let (vw, vh) = valid_cells(l);
    let limit = ((vw + 1).min(l.width), (vh + 1).min(l.height));
    bounds_above(&l.data, l.width, limit, threshold)
}

fn bounds_above(
    data: &[f32],
    width: usize,
    limit: (usize, usize),
    threshold: f32,
) -> Option<(usize, usize, usize, usize)> {
    let (mut u0, mut v0, mut u1, mut v1) = (usize::MAX, usize::MAX, 0, 0);
    for (v, row) in data.chunks_exact(width.max(1)).take(limit.1).enumerate() {
        for (u, &x) in row[..limit.0].iter().enumerate() {
            if x > threshold {
                u0 = u0.min(u);
                v0 = v0.min(v);
                u1 = u1.max(u);
                v1 = v1.max(v);
            }
        }
    }
    (u0 != usize::MAX).then_some((u0, v0, u1, v1))
}

fn same_geometry(a: &Logits, b: &Logits) -> bool {
    a.width == b.width
        && a.height == b.height
        && a.source == b.source
        && a.cells_per_px == b.cells_per_px
}

/// `l` sampled at the cell centres of `to`'s grid.
fn resample(l: &Logits, to: &Logits) -> Vec<f32> {
    let (cx, cy) = (to.cells_per_px.0 as f64, to.cells_per_px.1 as f64);
    let mut out = Vec::with_capacity(to.width * to.height);
    for v in 0..to.height {
        for u in 0..to.width {
            let p = Point::new(
                ((u as f64 + 0.5) / cx) as f32,
                ((v as f64 + 0.5) / cy) as f32,
            );
            out.push(l.sample(p));
        }
    }
    out
}

// Stroke geometry.

pub(super) fn point_bounds(points: &[Point]) -> Option<(Point, Point)> {
    let first = *points.first()?;
    Some(points.iter().fold((first, first), |(lo, hi), p| {
        (
            Point::new(lo.x.min(p.x), lo.y.min(p.y)),
            Point::new(hi.x.max(p.x), hi.y.max(p.y)),
        )
    }))
}

fn segment_dist2(x: f64, y: f64, p: Point, q: Point) -> f64 {
    let (px, py) = (p.x as f64, p.y as f64);
    let (dx, dy) = (q.x as f64 - px, q.y as f64 - py);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((x - px) * dx + (y - py) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (ex, ey) = (x - (px + t * dx), y - (py + t * dy));
    ex * ex + ey * ey
}

/// The polyline's segments; a single point is one degenerate segment.
fn segments(points: &[Point]) -> impl Iterator<Item = (Point, Point)> + '_ {
    let single = (points.len() == 1).then(|| (points[0], points[0]));
    points.windows(2).map(|w| (w[0], w[1])).chain(single)
}

/// First and one-past-last index of the cells whose centres `(i + 0.5) · cell` lie in `[lo, hi]`,
/// clamped to `[0, limit)`.
fn cell_span(lo: f64, hi: f64, cell: f64, limit: usize) -> (usize, usize) {
    let first = (lo / cell - 0.5).ceil().max(0.0);
    // One past the last centre in range, the prototype's expression exactly.
    let last = f64::floor(hi / cell - 0.5) + 1.0;
    let end = last.min(limit as f64).max(0.0) as usize;
    ((first as usize).min(end), end)
}

/// The window `(x, y, width, height)` of a grid of `limit` cells, each `cell` analysis pixels, that
/// holds every cell centre within `radius` of the stroke; `None` when there is none.
fn sweep_window(
    points: &[Point],
    radius: f64,
    cell: (f64, f64),
    limit: (usize, usize),
) -> Option<(usize, usize, usize, usize)> {
    let (lo, hi) = point_bounds(points)?;
    let (x0, x1) = cell_span(lo.x as f64 - radius, hi.x as f64 + radius, cell.0, limit.0);
    let (y0, y1) = cell_span(lo.y as f64 - radius, hi.y as f64 + radius, cell.1, limit.1);
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// Over `window` of a grid of `cell`-sized cells, row-major: whether each cell's centre lies within
/// `radius` of the stroke's polyline. Each segment only visits the cells of its own bounds.
fn sweep(
    points: &[Point],
    radius: f64,
    cell: (f64, f64),
    window: (usize, usize, usize, usize),
) -> Vec<bool> {
    let (wx, wy, ww, wh) = window;
    let mut bits = vec![false; ww * wh];
    let r2 = radius * radius;
    for (p, q) in segments(points) {
        let (xa, xb) = (p.x.min(q.x) as f64 - radius, p.x.max(q.x) as f64 + radius);
        let (ya, yb) = (p.y.min(q.y) as f64 - radius, p.y.max(q.y) as f64 + radius);
        let (a0, a1) = cell_span(xa, xb, cell.0, wx + ww);
        let (b0, b1) = cell_span(ya, yb, cell.1, wy + wh);
        for b in b0.max(wy)..b1 {
            let y = (b as f64 + 0.5) * cell.1;
            let row = &mut bits[(b - wy) * ww..(b - wy + 1) * ww];
            for a in a0.max(wx)..a1 {
                if segment_dist2((a as f64 + 0.5) * cell.0, y, p, q) <= r2 {
                    row[a - wx] = true;
                }
            }
        }
    }
    bits
}

/// One axis of the working raster: sub-cell `i` has its centre at `(i + 0.5) · px` analysis pixels
/// and interpolates between cells `lo[i]` and `hi[i]` with weight `t[i]` on `hi[i]`.
pub(super) struct Axis {
    pub(super) px: f64,
    lo: Vec<usize>,
    t: Vec<f32>,
    /// Runs of consecutive sub-cells sharing `lo`, as `(cell, start, end)`.
    runs: Vec<(usize, usize, usize)>,
}

impl Axis {
    fn new(cells: usize, cells_per_px: f32, source: u32) -> Self {
        let cell_px = 1.0 / cells_per_px as f64;
        let k = ((cell_px / WORKING_CELL_PX).ceil() as usize).max(1);
        let px = cell_px / k as f64;
        let n = ((source as f64 / px - 0.5).ceil().max(0.0) as usize).min(cells * k);
        let last = (cells - 1) as f64;
        let (mut lo, mut t) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let mut runs: Vec<(usize, usize, usize)> = Vec::new();
        for i in 0..n {
            let u = ((i as f64 + 0.5) / k as f64 - 0.5).clamp(0.0, last);
            let c = u.floor() as usize;
            lo.push(c);
            t.push((u - c as f64) as f32);
            match runs.last_mut() {
                Some(run) if run.0 == c => run.2 = i + 1,
                _ => runs.push((c, i, i + 1)),
            }
        }
        Self { px, lo, t, runs }
    }

    pub(super) fn len(&self) -> usize {
        self.lo.len()
    }

    /// The runs whose lower cell lies in `[first, last]`.
    fn runs_in(&self, first: usize, last: usize) -> &[(usize, usize, usize)] {
        let a = self.runs.partition_point(|r| r.0 < first);
        let b = self.runs.partition_point(|r| r.0 <= last);
        &self.runs[a..b.max(a)]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Patch {
    Out,
    In,
    Mixed,
}

pub(super) struct Grid {
    width: usize,
    height: usize,
    pub(super) x: Axis,
    pub(super) y: Axis,
}

impl Grid {
    pub(super) fn new(l: &Logits) -> Self {
        Self {
            width: l.width,
            height: l.height,
            x: Axis::new(l.width, l.cells_per_px.0, l.source.0),
            y: Axis::new(l.height, l.cells_per_px.1, l.source.1),
        }
    }

    pub(super) fn cell_area(&self) -> f64 {
        self.x.px * self.y.px
    }

    /// The interpolated logit at sub-cell `(a, b)`, as `Logits::sample` computes it.
    pub(super) fn value(&self, data: &[f32], a: usize, b: usize) -> f32 {
        let (u0, fu) = (self.x.lo[a], self.x.t[a]);
        let (v0, fv) = (self.y.lo[b], self.y.t[b]);
        let (u1, v1) = ((u0 + 1).min(self.width - 1), (v0 + 1).min(self.height - 1));
        let at = |u: usize, v: usize| data[v * self.width + u];
        let top = at(u0, v0) * (1.0 - fu) + at(u1, v0) * fu;
        let bottom = at(u0, v1) * (1.0 - fu) + at(u1, v1) * fu;
        top * (1.0 - fv) + bottom * fv
    }

    /// Whether every sub-cell interpolating from cells `(u, v)` to `(u + 1, v + 1)` is inside,
    /// outside or either: a bilinear value lies between its corners.
    fn patch(&self, data: &[f32], u: usize, v: usize) -> Patch {
        let (u1, v1) = ((u + 1).min(self.width - 1), (v + 1).min(self.height - 1));
        let c = [
            data[v * self.width + u],
            data[v * self.width + u1],
            data[v1 * self.width + u],
            data[v1 * self.width + u1],
        ];
        if c.iter().all(|&x| x > 0.0) {
            Patch::In
        } else if c.iter().all(|&x| x <= 0.0) {
            Patch::Out
        } else {
            Patch::Mixed
        }
    }

    /// Sub-cells inside the shape, whose positive cells are `cells`.
    fn count(&self, data: &[f32], cells: (usize, usize, usize, usize)) -> u64 {
        let (u0, v0, u1, v1) = cells;
        let mut n = 0u64;
        for &(v, b0, b1) in self.y.runs_in(v0.saturating_sub(1), v1) {
            for &(u, a0, a1) in self.x.runs_in(u0.saturating_sub(1), u1) {
                match self.patch(data, u, v) {
                    Patch::In => n += ((a1 - a0) * (b1 - b0)) as u64,
                    Patch::Out => {}
                    Patch::Mixed => {
                        for b in b0..b1 {
                            n += (a0..a1).filter(|&a| self.value(data, a, b) > 0.0).count() as u64;
                        }
                    }
                }
            }
        }
        n
    }

    /// Sub-cells inside both shapes.
    pub(super) fn overlap(&self, a: &Shape, b: &Shape) -> u64 {
        let u0 = a.cells.0.max(b.cells.0).saturating_sub(1);
        let v0 = a.cells.1.max(b.cells.1).saturating_sub(1);
        let (u1, v1) = (a.cells.2.min(b.cells.2), a.cells.3.min(b.cells.3));
        if u0 > u1 || v0 > v1 {
            return 0;
        }
        let mut n = 0u64;
        for &(v, y0, y1) in self.y.runs_in(v0, v1) {
            for &(u, x0, x1) in self.x.runs_in(u0, u1) {
                let (pa, pb) = (self.patch(&a.data, u, v), self.patch(&b.data, u, v));
                if pa == Patch::Out || pb == Patch::Out {
                    continue;
                }
                if pa == Patch::In && pb == Patch::In {
                    n += ((x1 - x0) * (y1 - y0)) as u64;
                    continue;
                }
                for y in y0..y1 {
                    for x in x0..x1 {
                        let ia = pa == Patch::In || self.value(&a.data, x, y) > 0.0;
                        let ib = pb == Patch::In || self.value(&b.data, x, y) > 0.0;
                        n += (ia && ib) as u64;
                    }
                }
            }
        }
        n
    }

    /// Whether `outer` is larger than `inner` and holds at least 90 % of its area.
    pub(super) fn contains(&self, outer: &Shape, inner: &Shape) -> bool {
        outer.count > inner.count
            && self.overlap(outer, inner) as f64 >= NESTED_SHARE * inner.count as f64
    }

    /// Sub-cells inside both the shape and the stroke.
    pub(super) fn footprint_overlap(&self, shape: &Shape, footprint: &Footprint) -> u64 {
        let (u0, v0, u1, v1) = shape.cells;
        let mut n = 0u64;
        for &(b, a0, a1) in &footprint.runs {
            let v = self.y.lo[b];
            if v + 1 < v0 || v > v1 {
                continue;
            }
            let start = self.x.runs.partition_point(|r| r.2 <= a0);
            for &(u, s, e) in &self.x.runs[start..] {
                if s >= a1 {
                    break;
                }
                if u + 1 < u0 || u > u1 {
                    continue;
                }
                let (s, e) = (s.max(a0), e.min(a1));
                match self.patch(&shape.data, u, v) {
                    Patch::In => n += (e - s) as u64,
                    Patch::Out => {}
                    Patch::Mixed => {
                        n += (s..e)
                            .filter(|&a| self.value(&shape.data, a, b) > 0.0)
                            .count() as u64
                    }
                }
            }
        }
        n
    }
}

/// A candidate on the working raster.
pub(super) struct Shape<'a> {
    data: Cow<'a, [f32]>,
    cells: (usize, usize, usize, usize),
    pub(super) count: u64,
}

impl<'a> Shape<'a> {
    pub(super) fn new(l: &'a Logits, reference: &Logits, grid: &Grid) -> Option<Self> {
        if !well_formed(l) {
            return None;
        }
        let data: Cow<'a, [f32]> = if same_geometry(l, reference) {
            Cow::Borrowed(&l.data)
        } else {
            Cow::Owned(resample(l, reference))
        };
        let cells = bounds_above(&data, grid.width, (grid.width, grid.height), 0.0)?;
        let count = grid.count(&data, cells);
        (count > 0).then_some(Self { data, cells, count })
    }
}

/// The stroke on the working raster, as row runs `(row, start, end)`.
pub(super) struct Footprint {
    runs: Vec<(usize, usize, usize)>,
    pub(super) count: u64,
}

impl Footprint {
    /// The radius is at least half a sub-cell's diagonal, so a stroke thinner than the raster
    /// still covers the sub-cells it passes through.
    pub(super) fn new(stroke: &Stroke, grid: &Grid) -> Self {
        let cell = (grid.x.px, grid.y.px);
        let radius = (stroke.radius.max(0.0) as f64).max(0.5 * cell.0.hypot(cell.1));
        let mut runs = Vec::new();
        let mut count = 0u64;
        if let Some(window) =
            sweep_window(&stroke.points, radius, cell, (grid.x.len(), grid.y.len()))
        {
            let (wx, wy, ww, _) = window;
            let bits = sweep(&stroke.points, radius, cell, window);
            for (row, line) in bits.chunks_exact(ww).enumerate() {
                let mut a = 0;
                while a < ww {
                    if !line[a] {
                        a += 1;
                        continue;
                    }
                    let start = a;
                    while a < ww && line[a] {
                        a += 1;
                    }
                    runs.push((wy + row, wx + start, wx + a));
                    count += (a - start) as u64;
                }
            }
        }
        Self { runs, count }
    }
}
