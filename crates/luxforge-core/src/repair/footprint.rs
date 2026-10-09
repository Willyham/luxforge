//! A repair operation's footprint bound to a stage: its coverage at every pixel and the rectangle
//! outside which that coverage is exactly zero (`docs/design/corrections-numerics.md`, "Coverage").
//!
//! The coverage is the mask brush's capsule profile at the distance to the operation's nearest
//! segment, times `opacity / 100`, evaluated at the pixel's centre in mask space. It is the
//! production transcription of `Compiled::coverage` in `luxforge_reference::repair`, in the same
//! spelling and order, so it is bit-identical to it: the segment's `len2` and the feather's band
//! stay divisors, the square root is taken once of the smallest squared distance, and the hard edge
//! (`band == 0`) is its own branch.
//!
//! **Why an index.** A stroke holds up to 1,024 positions, so testing every segment at every pixel
//! of a large footprint would cost a million distance evaluations per thousand pixels. The uniform
//! grid below lists each segment in every cell its radius-grown box reaches, conservatively by a
//! pixel, so a pixel tests only its own cell's segments. A segment the cell omits is farther than
//! the radius from every pixel of the cell, so the nearest listed segment is the nearest segment
//! wherever the coverage is not exactly zero, and where it is zero both answers are zero: the result
//! is bit-identical to testing every segment.
use crate::modules::{Region, Stage};

/// The frozen easing on `s` already clamped to `[0, 1]`: the mask brush's.
#[inline]
fn smooth(s: f64) -> f64 {
    s * s * (3.0 - 2.0 * s)
}

/// One segment in mask space with its pixel-independent terms: the mask brush's spelling. A
/// degenerate segment (a spot, or a repeated position) is `e = 0, len2 = 1`, so the projection is
/// `t = 0` and the distance is to its point, with no branch per pixel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Segment {
    pub(crate) ax: f64,
    pub(crate) ay: f64,
    pub(crate) ex: f64,
    pub(crate) ey: f64,
    pub(crate) len2: f64,
}

impl Segment {
    pub(crate) fn new(a: (f64, f64), b: (f64, f64)) -> Self {
        let ex = b.0 - a.0;
        let ey = b.1 - a.1;
        let len2 = ex * ex + ey * ey;
        if len2 == 0.0 {
            Self {
                ax: a.0,
                ay: a.1,
                ex: 0.0,
                ey: 0.0,
                len2: 1.0,
            }
        } else {
            Self {
                ax: a.0,
                ay: a.1,
                ex,
                ey,
                len2,
            }
        }
    }

    /// The squared distance from mask-space point `(u, v)` to this segment.
    #[inline]
    pub(crate) fn distance2(&self, u: f64, v: f64) -> f64 {
        let wx = u - self.ax;
        let wy = v - self.ay;
        let t = ((wx * self.ex + wy * self.ey) / self.len2).clamp(0.0, 1.0);
        let qx = self.ax + t * self.ex;
        let qy = self.ay + t * self.ey;
        let dx = u - qx;
        let dy = v - qy;
        dx * dx + dy * dy
    }

    /// The segment's mask-space box `[u0, v0, u1, v1]`.
    pub(crate) fn bounds(&self) -> [f64; 4] {
        let bx = self.ax + self.ex;
        let by = self.ay + self.ey;
        [
            self.ax.min(bx),
            self.ay.min(by),
            self.ax.max(bx),
            self.ay.max(by),
        ]
    }
}

/// The segments of one stored path through the stored-position spelling of mask space
/// (`u = x * W/H`, `v = y`): `n - 1` segments for `n >= 2` positions, one degenerate segment for a
/// spot.
pub(crate) fn segments(points: &[[f64; 2]], stage: Stage) -> Vec<Segment> {
    let aspect = f64::from(stage.width) / f64::from(stage.height);
    let uv: Vec<(f64, f64)> = points.iter().map(|[x, y]| (x * aspect, *y)).collect();
    match uv.as_slice() {
        [] => Vec::new(),
        [only] => vec![Segment::new(*only, *only)],
        _ => uv
            .windows(2)
            .map(|pair| Segment::new(pair[0], pair[1]))
            .collect(),
    }
}

/// Cells the index spans along its longer side, at most.
const CELLS_MAX: u32 = 64;

/// The least cell side, in pixels.
const CELL_MIN: u32 = 16;

/// The pixel index range `[first, last]` a mask-space interval `[low, high]` reaches at stage
/// height `h`, widened by one pixel on each side for rounding, clipped to `0..limit`. `None` when
/// it misses the stage.
fn pixel_range(low: f64, high: f64, h: f64, limit: u32) -> Option<(u32, u32)> {
    let first = (low * h - 0.5).floor() - 1.0;
    let last = (high * h - 0.5).ceil() + 1.0;
    if !first.is_finite() || !last.is_finite() || last < 0.0 || first >= f64::from(limit) {
        return None;
    }
    Some((
        first.max(0.0) as u32,
        last.min(f64::from(limit) - 1.0) as u32,
    ))
}

/// A footprint bound to a stage.
#[derive(Clone, Debug)]
pub(crate) struct Footprint {
    stage: Stage,
    segments: Vec<Segment>,
    radius: f64,
    /// `radius * (feather / 100)`, a divisor; the hard edge is `band == 0`.
    band: f64,
    /// `opacity / 100`.
    amount: f64,
    /// Every pixel whose coverage may be above zero.
    support: Region,
    /// The index over `support`: `cell` pixels a side, `columns` across.
    cell: u32,
    columns: u32,
    offsets: Vec<u32>,
    entries: Vec<u32>,
}

impl Footprint {
    /// `points` stored normalized positions (`x` a fraction of the width, `y` of the height),
    /// `radius` in content heights, `feather` and `opacity` in `0..=100`.
    pub(crate) fn new(
        points: &[[f64; 2]],
        radius: f64,
        feather: f64,
        opacity: f64,
        stage: Stage,
    ) -> Self {
        let segments = segments(points, stage);
        let h = f64::from(stage.height);
        let mut support: Option<(u32, u32, u32, u32)> = None;
        for segment in &segments {
            let [u0, v0, u1, v1] = segment.bounds();
            let (Some((x0, x1)), Some((y0, y1))) = (
                pixel_range(u0 - radius, u1 + radius, h, stage.width),
                pixel_range(v0 - radius, v1 + radius, h, stage.height),
            ) else {
                continue;
            };
            support = Some(match support {
                None => (x0, y0, x1, y1),
                Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
            });
        }
        let support = support.map_or(Region::EMPTY, |(x0, y0, x1, y1)| Region {
            x0,
            y0,
            width: x1 - x0 + 1,
            height: y1 - y0 + 1,
        });
        let side = support.width.max(support.height);
        let cell = (radius * h)
            .ceil()
            .clamp(f64::from(CELL_MIN), f64::from(side.max(CELL_MIN))) as u32;
        let cell = cell.max(side.div_ceil(CELLS_MAX)).max(1);
        let columns = support.width.div_ceil(cell).max(1);
        let rows = support.height.div_ceil(cell).max(1);
        let cells = (columns * rows) as usize;
        let mut footprint = Self {
            stage,
            segments,
            radius,
            band: radius * (feather / 100.0),
            amount: opacity / 100.0,
            support,
            cell,
            columns,
            offsets: Vec::new(),
            entries: Vec::new(),
        };
        if support.is_empty() {
            return footprint;
        }
        // Count, then fill contiguous cell slices, each segment in segment order.
        let mut offsets = vec![0u32; cells + 1];
        footprint.visit(|cell, _| offsets[cell + 1] += 1);
        for cell in 1..offsets.len() {
            offsets[cell] += offsets[cell - 1];
        }
        let mut entries = vec![0u32; offsets[cells] as usize];
        let mut next: Vec<u32> = offsets[..cells].to_vec();
        footprint.visit(|cell, segment| {
            entries[next[cell] as usize] = segment as u32;
            next[cell] += 1;
        });
        footprint.offsets = offsets;
        footprint.entries = entries;
        footprint
    }

    /// Every `(cell, segment)` pair the index lists, in segment order: each cell its radius-grown
    /// box reaches whose centre lies within the radius plus the cell's half diagonal and two pixels
    /// of the segment, so a cell no pixel of which the segment reaches is left out and a long
    /// diagonal segment lists only the cells along it.
    fn visit(&self, mut each: impl FnMut(usize, usize)) {
        let h = f64::from(self.stage.height);
        let s = self.support;
        let cell = f64::from(self.cell);
        // The radius plus half a cell's diagonal and two pixels, in mask space, squared.
        let reach = self.radius + (std::f64::consts::FRAC_1_SQRT_2 * cell + 2.0) / h;
        let reach2 = reach * reach;
        for (index, segment) in self.segments.iter().enumerate() {
            let [u0, v0, u1, v1] = segment.bounds();
            let (Some((x0, x1)), Some((y0, y1))) = (
                pixel_range(u0 - self.radius, u1 + self.radius, h, self.stage.width),
                pixel_range(v0 - self.radius, v1 + self.radius, h, self.stage.height),
            ) else {
                continue;
            };
            let (x0, x1) = (x0.max(s.x0), x1.min(s.x1() - 1));
            let (y0, y1) = (y0.max(s.y0), y1.min(s.y1() - 1));
            if x0 > x1 || y0 > y1 {
                continue;
            }
            for row in (y0 - s.y0) / self.cell..=(y1 - s.y0) / self.cell {
                for column in (x0 - s.x0) / self.cell..=(x1 - s.x0) / self.cell {
                    // The cell's centre in mask space.
                    let cx = (f64::from(s.x0 + column * self.cell) + 0.5 * cell) / h;
                    let cy = (f64::from(s.y0 + row * self.cell) + 0.5 * cell) / h;
                    if segment.distance2(cx, cy) <= reach2 {
                        each((row * self.columns + column) as usize, index);
                    }
                }
            }
        }
    }

    /// The Heal solve's rectangle (`docs/design/corrections-numerics.md`, "Heal"): a function of
    /// the segments' box and the radius alone, the reference's `solve_rect`,
    ///
    /// ```text
    /// x0 = max(0, floor((u0 - R) H - 0.5) - 1)      x1 = min(W, floor((u1 + R) H - 0.5) + 3)
    /// ```
    ///
    /// and likewise down, which holds every pixel within the radius and the ring around them.
    /// `None` when it holds no pixel.
    pub(crate) fn solve_rect(&self) -> Option<Region> {
        let (mut u0, mut v0, mut u1, mut v1) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for s in &self.segments {
            let [a, b, c, d] = s.bounds();
            u0 = u0.min(a);
            v0 = v0.min(b);
            u1 = u1.max(c);
            v1 = v1.max(d);
        }
        let h = f64::from(self.stage.height);
        let low = |a: f64| ((a - self.radius) * h - 0.5).floor() - 1.0;
        let high = |b: f64| ((b + self.radius) * h - 0.5).floor() + 3.0;
        let x0 = low(u0).max(0.0);
        let y0 = low(v0).max(0.0);
        let x1 = high(u1).min(f64::from(self.stage.width));
        let y1 = high(v1).min(h);
        (x0 < x1 && y0 < y1).then_some(Region {
            x0: x0 as u32,
            y0: y0 as u32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    /// The index's cell rows and columns over the support.
    pub(crate) fn cells(&self) -> (u32, u32) {
        if self.support.is_empty() {
            return (0, 0);
        }
        (self.support.height.div_ceil(self.cell), self.columns)
    }

    /// The pixels of cell `(row, column)`, or `None` when no segment reaches it, in which case
    /// every one of its pixels' coverage is exactly zero.
    pub(crate) fn cell_region(&self, row: u32, column: u32) -> Option<Region> {
        let at = (row * self.columns + column) as usize;
        if self.offsets.is_empty() || self.offsets[at] == self.offsets[at + 1] {
            return None;
        }
        let s = self.support;
        let x0 = s.x0 + column * self.cell;
        let y0 = s.y0 + row * self.cell;
        Some(Region {
            x0,
            y0,
            width: self.cell.min(s.x1() - x0),
            height: self.cell.min(s.y1() - y0),
        })
    }

    /// The pixels whose coverage may be above zero; every other pixel's is exactly zero.
    pub(crate) fn support(&self) -> Region {
        self.support
    }

    pub(crate) fn stage(&self) -> Stage {
        self.stage
    }

    pub(crate) fn segments_ref(&self) -> &[Segment] {
        &self.segments
    }

    pub(crate) fn radius(&self) -> f64 {
        self.radius
    }

    pub(crate) fn band(&self) -> f64 {
        self.band
    }

    pub(crate) fn amount(&self) -> f64 {
        self.amount
    }

    /// The capsule profile at distance `d`:
    /// `if band == 0 { d <= R } else { smooth(clamp((R - d) / band, 0, 1)) }`.
    #[inline]
    fn profile(&self, d: f64) -> f64 {
        if self.band == 0.0 {
            if d <= self.radius { 1.0 } else { 0.0 }
        } else {
            smooth(((self.radius - d) / self.band).clamp(0.0, 1.0))
        }
    }

    /// Coverage at stage pixel `(px, py)`: `profile(d) * (opacity / 100)` at its centre
    /// `((px + 0.5) / H, (py + 0.5) / H)`. Exactly zero outside [`Self::support`].
    #[inline]
    pub(crate) fn coverage(&self, px: u32, py: u32) -> f64 {
        let s = self.support;
        if !s.contains(px, py) {
            return 0.0;
        }
        let cell = ((py - s.y0) / self.cell * self.columns + (px - s.x0) / self.cell) as usize;
        let listed = &self.entries[self.offsets[cell] as usize..self.offsets[cell + 1] as usize];
        if listed.is_empty() {
            return 0.0;
        }
        let h = f64::from(self.stage.height);
        let u = (f64::from(px) + 0.5) / h;
        let v = (f64::from(py) + 0.5) / h;
        let mut nearest = f64::INFINITY;
        for &index in listed {
            let d2 = self.segments[index as usize].distance2(u, v);
            if d2 < nearest {
                nearest = d2;
            }
        }
        self.profile(nearest.sqrt()) * self.amount
    }
}
