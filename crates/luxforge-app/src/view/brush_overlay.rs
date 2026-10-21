//! The painted-path overlay's geometry: an operation's outline, its dashed source outline, the arrow
//! from source to destination, its pin and handles and the source crosshair, each placed through
//! the core's map and the shared warped-path drawing ([`super::warped_path`]), so they follow Lens,
//! Perspective, straighten, crop, quarter turns and mirroring with the picture. Pure geometry in
//! canvas logical points: what draws it is the canvas's, and every figure is bounded by the
//! warped path's segment budget whatever the path's length.
use super::{
    mask_canvas::Placement,
    warped_path::{MAX_SEGMENTS, Outline, curve_placed, line},
};
use crate::brush::{Handle, HeldOperation, ViewPoint};
use iced::Point;
use std::{cell::RefCell, collections::HashMap, rc::Rc};

/// The arrowhead's length and half-angle, in logical points and radians.
const ARROW_HEAD: f32 = 7.0;
const ARROW_SPREAD: f32 = 0.45;
/// Points around a spot's circle.
const CIRCLE_POINTS: usize = 32;
/// Grid cells across a radius when a stroke's outline is traced: its vertices are about a sixth of
/// the radius apart around a curve, and a straight side is one edge.
const CELLS_PER_RADIUS: f64 = 6.0;
/// The distance evaluations one stroke's outline aims to stay within: a long thin stroke is traced
/// on a coarser grid rather than costing more, down to [`CELLS_PER_RADIUS_MIN`], below which a
/// stroke longer than about 1,400 radii costs about 72 evaluations a radius of its length.
const EVALUATIONS_MAX: f64 = 100_000.0;
const CELLS_PER_RADIUS_MIN: f64 = 1.5;

fn view(point: Point) -> ViewPoint {
    (point.x, point.y)
}

/// One segment of a path in mask space, measured as the repair's coverage measures it.
struct Capsule {
    a: [f64; 2],
    d: [f64; 2],
    len2: f64,
}

impl Capsule {
    /// The squared distance from `p` to the segment.
    fn distance2(&self, p: [f64; 2]) -> f64 {
        let (ex, ey) = (p[0] - self.a[0], p[1] - self.a[1]);
        let t = ((ex * self.d[0] + ey * self.d[1]) / self.len2).clamp(0.0, 1.0);
        let (fx, fy) = (ex - t * self.d[0], ey - t * self.d[1]);
        fx * fx + fy * fy
    }
}

/// A grid edge: the vertex it starts at, and whether it runs down (`true`) or right (`false`).
type EdgeKey = (i32, i32, bool);

/// Vertices a side of one tile of the sampled distance.
const TILE: i32 = 16;

/// The squared distance to the path at grid vertices, held in 16 x 16 tiles allocated only where a
/// segment reaches, so a lookup inside a tile is an index and the hash is paid once a tile.
#[derive(Default)]
struct Field {
    tiles: HashMap<(i32, i32), Box<[f64; (TILE * TILE) as usize]>>,
}

impl Field {
    fn split(i: i32, j: i32) -> ((i32, i32), usize) {
        (
            (i.div_euclid(TILE), j.div_euclid(TILE)),
            (j.rem_euclid(TILE) * TILE + i.rem_euclid(TILE)) as usize,
        )
    }

    /// The squared distance at vertex `(i, j)`; infinite where no segment reached.
    fn at(&self, i: i32, j: i32) -> f64 {
        let (tile, index) = Self::split(i, j);
        self.tiles
            .get(&tile)
            .map_or(f64::INFINITY, |held| held[index])
    }

    /// Lower the vertices of `[i0, i1] x [j0, j1]` to their squared distance to `capsule` where it
    /// is nearer, positions `cell` apart.
    fn lower(&mut self, capsule: &Capsule, [i0, i1, j0, j1]: [i32; 4], cell: f64) {
        for tj in j0.div_euclid(TILE)..=j1.div_euclid(TILE) {
            for ti in i0.div_euclid(TILE)..=i1.div_euclid(TILE) {
                let held = self
                    .tiles
                    .entry((ti, tj))
                    .or_insert_with(|| Box::new([f64::INFINITY; (TILE * TILE) as usize]));
                for j in j0.max(tj * TILE)..=j1.min(tj * TILE + TILE - 1) {
                    let row = ((j - tj * TILE) * TILE) as usize;
                    for i in i0.max(ti * TILE)..=i1.min(ti * TILE + TILE - 1) {
                        let d2 = capsule.distance2([f64::from(i) * cell, f64::from(j) * cell]);
                        let slot = &mut held[row + (i - ti * TILE) as usize];
                        if d2 < *slot {
                            *slot = d2;
                        }
                    }
                }
            }
        }
    }
}

/// The boundary of the union of a stroke's capsules at `radius`, in mask space: the shape its
/// coverage has, whatever the path does. Coverage is the distance to the path's nearest segment, so
/// a stroke brushed back over itself covers what one pass covers, and its outline is that of one
/// pass; offsetting the path's sides would draw a figure that crosses itself wherever the path
/// turns back.
///
/// Traced by marching squares over the distance to the path, sampled only at grid vertices within
/// reach of a segment, in tiles, so the work follows the stroke's area rather than its box and
/// aims at [`EVALUATIONS_MAX`]. The distance is linear across a straight side, so a side's
/// crossings are exact; caps and inner corners are within a small share of a cell. Every loop is
/// closed and keeps the covered area on the same side, so a hole the path encloses is its own loop
/// and the loops fill as one shape. A path with no segment of any length answers no loop.
fn union_loops(mask: &[[f64; 2]], radius: f64) -> Vec<Vec<[f64; 2]>> {
    let capsules: Vec<Capsule> = mask
        .windows(2)
        .filter_map(|pair| {
            let d = [pair[1][0] - pair[0][0], pair[1][1] - pair[0][1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            (len2 > 0.0).then_some(Capsule {
                a: pair[0],
                d,
                len2,
            })
        })
        .collect();
    if capsules.is_empty() || radius.is_nan() || radius <= 0.0 {
        return Vec::new();
    }
    // Each segment is visited in pieces no longer than the radius, each over its box grown by the
    // reach: about `(3 k + 4)^2` vertices a piece at `k` cells across a radius.
    let pieces: Vec<usize> = capsules
        .iter()
        .map(|capsule| ((capsule.len2.sqrt() / radius).ceil() as usize).max(1))
        .collect();
    let total: usize = pieces.iter().sum();
    let affordable = ((EVALUATIONS_MAX / total as f64).sqrt() - 4.0) / 3.0;
    let cell = radius / affordable.clamp(CELLS_PER_RADIUS_MIN, CELLS_PER_RADIUS);
    // An edge the boundary crosses has both ends within `radius + cell` of the path, so every vertex
    // a crossing reads is sampled.
    let reach = radius + 2.0 * cell;
    let mut field = Field::default();
    for (capsule, &count) in capsules.iter().zip(&pieces) {
        for piece in 0..count {
            let ends = [piece, piece + 1].map(|end| {
                let t = end as f64 / count as f64;
                [
                    capsule.a[0] + t * capsule.d[0],
                    capsule.a[1] + t * capsule.d[1],
                ]
            });
            let low = |axis: usize| ((ends[0][axis].min(ends[1][axis]) - reach) / cell).ceil();
            let high = |axis: usize| ((ends[0][axis].max(ends[1][axis]) + reach) / cell).floor();
            field.lower(
                capsule,
                [low(0) as i32, high(0) as i32, low(1) as i32, high(1) as i32],
                cell,
            );
        }
    }
    let r2 = radius * radius;
    let position = |(i, j): (i32, i32)| [f64::from(i) * cell, f64::from(j) * cell];
    // Where the boundary crosses an edge, from its two ends in one order, so the two cells that
    // share the edge place it identically. Linear in the distance, not its square.
    let crossing = |(i, j, down): EdgeKey| {
        let (p, q) = ((i, j), if down { (i, j + 1) } else { (i + 1, j) });
        let (dp, dq) = (field.at(p.0, p.1).sqrt(), field.at(q.0, q.1).sqrt());
        let t = (radius - dp) / (dq - dp);
        let (a, b) = (position(p), position(q));
        [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]
    };
    // A cell's corners are within a cell's diagonal of its first, so a first corner that far
    // outside or inside the radius settles the whole cell without reading the others.
    let diagonal = cell * std::f64::consts::SQRT_2;
    let clear_out = (radius + diagonal).powi(2);
    let clear_in = if radius > diagonal {
        (radius - diagonal).powi(2)
    } else {
        -1.0
    };
    let mut order: Vec<(i32, i32)> = field.tiles.keys().copied().collect();
    order.sort_unstable_by_key(|&(ti, tj)| (tj, ti));
    // Taken around a cell, its corners leave the covered area at one crossing and come back at the
    // next; each boundary piece runs from a leaving crossing to a returning one, so the covered
    // area is on the same side of every piece and each crossing leaves one cell and returns to the
    // cell beside it.
    let mut next: HashMap<EdgeKey, EdgeKey> = HashMap::new();
    let mut leaving: Vec<EdgeKey> = Vec::new();
    for (ti, tj) in order {
        let held = &field.tiles[&(ti, tj)];
        for oj in 0..TILE {
            for oi in 0..TILE {
                let first = held[(oj * TILE + oi) as usize];
                if first >= clear_out || first < clear_in {
                    continue;
                }
                let (i, j) = (ti * TILE + oi, tj * TILE + oj);
                let corners = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                let values = corners.map(|(ci, cj)| {
                    let (oi, oj) = (ci - ti * TILE, cj - tj * TILE);
                    if oi < TILE && oj < TILE {
                        held[(oj * TILE + oi) as usize]
                    } else {
                        field.at(ci, cj)
                    }
                });
                let inside = values.map(|value| value < r2);
                if inside.iter().all(|&v| v) || inside.iter().all(|&v| !v) {
                    continue;
                }
                let edges: [EdgeKey; 4] = [
                    (i, j, false),
                    (i + 1, j, true),
                    (i, j + 1, false),
                    (i, j, true),
                ];
                let crossings: Vec<(EdgeKey, bool)> = (0..4)
                    .filter(|&k| inside[k] != inside[(k + 1) % 4])
                    .map(|k| (edges[k], inside[k]))
                    .collect();
                let mut link = |from: EdgeKey, to: EdgeKey| {
                    next.insert(from, to);
                    leaving.push(from);
                };
                match crossings.len() {
                    2 => {
                        if crossings[0].1 {
                            link(crossings[0].0, crossings[1].0);
                        } else {
                            link(crossings[1].0, crossings[0].0);
                        }
                    }
                    4 => {
                        // A saddle: the cell's centre decides whether its two covered corners join.
                        let centre = values.iter().map(|value| value.sqrt()).sum::<f64>() / 4.0;
                        let step = if centre < radius { 1 } else { 3 };
                        for k in 0..4 {
                            if crossings[k].1 {
                                link(crossings[k].0, crossings[(k + step) % 4].0);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut loops = Vec::new();
    for start in leaving {
        let mut ring = Vec::new();
        let mut key = start;
        while let Some(following) = next.remove(&key) {
            ring.push(crossing(key));
            key = following;
        }
        if ring.is_empty() {
            continue;
        }
        let ring = straightened(ring, cell / 64.0);
        if ring.len() >= 3 {
            loops.push(ring);
        }
    }
    loops
}

/// A closed ring without the vertices that lie on the line from the vertex kept before them to the
/// one after them, within `tolerance`: a straight side traced cell by cell becomes one edge.
fn straightened(ring: Vec<[f64; 2]>, tolerance: f64) -> Vec<[f64; 2]> {
    if ring.len() < 4 {
        return ring;
    }
    let mut kept: Vec<[f64; 2]> = Vec::with_capacity(ring.len());
    for (index, &point) in ring.iter().enumerate() {
        let Some(&previous) = kept.last() else {
            kept.push(point);
            continue;
        };
        let following = ring[(index + 1) % ring.len()];
        let (dx, dy) = (following[0] - previous[0], following[1] - previous[1]);
        let length = dx.hypot(dy);
        let off = ((point[0] - previous[0]) * dy - (point[1] - previous[1]) * dx).abs();
        if length == 0.0 || off > tolerance * length {
            kept.push(point);
        }
    }
    kept
}

/// The outline of `points` at `radius` (units of the content height), as closed loops of
/// normalized content positions: a circle for a spot; for a stroke, the boundary of the area its
/// coverage covers ([`union_loops`]). Built in mask space, where the radius is round, and returned
/// in content coordinates.
fn envelope(points: &[[f64; 2]], radius: f64, aspect: f64) -> Vec<Vec<[f64; 2]>> {
    let to_content = |[x, y]: [f64; 2]| [x / aspect, y];
    let mask: Vec<[f64; 2]> = points.iter().map(|[x, y]| [x * aspect, *y]).collect();
    let loops = union_loops(&mask, radius);
    if !loops.is_empty() {
        return loops
            .into_iter()
            .map(|ring| ring.into_iter().map(to_content).collect())
            .collect();
    }
    let centre = mask.first().copied().unwrap_or_default();
    vec![
        (0..CIRCLE_POINTS)
            .map(|step| {
                let angle = std::f64::consts::TAU * step as f64 / CIRCLE_POINTS as f64;
                to_content([
                    centre[0] + radius * angle.cos(),
                    centre[1] + radius * angle.sin(),
                ])
            })
            .collect(),
    ]
}

/// The most ring vertices one outline seeds the warped path with, over all its loops: half its
/// segment budget, so each edge keeps room to bend with the warp.
const MAX_RING: usize = 512;

/// A closed loop of content positions drawn through the warped path, one seed per edge and at most
/// `budget` drawn segments, each position placed by `place`. A ring longer than `seeds` is drawn
/// through every n-th vertex and reported approximate, so a long stroke's outline is never cut
/// short by the budget.
fn loop_outline(
    placement: &Placement,
    ring: &[[f64; 2]],
    seeds: usize,
    budget: usize,
    dashed: bool,
    place: impl Fn(f64, f64) -> Option<Point>,
) -> Outline {
    let stride = ring.len().div_ceil(seeds.max(1)).max(1);
    let ring: Vec<[f64; 2]> = ring.iter().step_by(stride).copied().collect();
    let edges = ring.len();
    if edges < 2 {
        return Outline::default();
    }
    let mut outline = curve_placed(
        placement,
        |t| {
            let at = (t * edges as f64).clamp(0.0, edges as f64);
            let index = (at.floor() as usize).min(edges - 1);
            let fraction = at - index as f64;
            let (a, b) = (ring[index], ring[(index + 1) % edges]);
            (
                a[0] + (b[0] - a[0]) * fraction,
                a[1] + (b[1] - a[1]) * fraction,
            )
        },
        edges,
        dashed,
        budget,
        place,
    );
    outline.approximate |= stride > 1;
    outline
}

/// Loops drawn through the warped path, each its own outline, sharing one outline's seeds and
/// segment budget by their lengths.
fn loops_outline(placement: &Placement, loops: &[Vec<[f64; 2]>], dashed: bool) -> Vec<Outline> {
    loops_placed(placement, loops, dashed, |x, y| {
        placement.canvas_point(x, y)
    })
}

/// [`loops_outline`] with each position placed by `place`.
fn loops_placed(
    placement: &Placement,
    loops: &[Vec<[f64; 2]>],
    dashed: bool,
    place: impl Fn(f64, f64) -> Option<Point> + Copy,
) -> Vec<Outline> {
    let total = loops.iter().map(Vec::len).sum::<usize>().max(1);
    loops
        .iter()
        .map(|ring| {
            let share = |whole: usize| (whole * ring.len() / total).max(3);
            loop_outline(
                placement,
                ring,
                share(MAX_RING),
                share(MAX_SEGMENTS),
                dashed,
                place,
            )
        })
        .collect()
}

pub(crate) fn merged(loops: Vec<Outline>) -> Outline {
    loops.into_iter().fold(Outline::default(), |mut all, one| {
        all.segments.extend(one.segments);
        all.approximate |= one.approximate;
        all
    })
}

/// An operation's outline, loop by loop: the boundary of what its coverage covers, so a stroke
/// brushed back over itself has the outline of one pass over that area. What lies off the
/// photograph is left out, so these are drawn, not filled: [`footprint`] is what fills.
fn outline_loops(placement: &Placement, held: &HeldOperation) -> Vec<Outline> {
    loops_outline(
        placement,
        &envelope(&held.points, held.radius, placement.map.aspect()),
        false,
    )
}

/// An operation's outline: its loops ([`outline_loops`]) as one figure.
pub(crate) fn outline(placement: &Placement, held: &HeldOperation) -> Outline {
    merged(outline_loops(placement, held))
}

/// `ring` clipped to the rectangle `[min, max]`, as Sutherland–Hodgman clips a polygon to a convex
/// window: where the ring leaves the rectangle it follows the edge to where it comes back, so the
/// clipped ring bounds what the ring bounds inside it, whatever it did outside. A walk that runs
/// along an edge and back bounds nothing.
fn clipped(ring: &[[f64; 2]], min: [f64; 2], max: [f64; 2]) -> Vec<[f64; 2]> {
    let mut ring = ring.to_vec();
    for (axis, bound, below) in [
        (0, min[0], false),
        (0, max[0], true),
        (1, min[1], false),
        (1, max[1], true),
    ] {
        let inside = |p: [f64; 2]| {
            if below {
                p[axis] <= bound
            } else {
                p[axis] >= bound
            }
        };
        if ring.iter().all(|&p| inside(p)) {
            continue;
        }
        let cross = |a: [f64; 2], b: [f64; 2]| {
            let t = (bound - a[axis]) / (b[axis] - a[axis]);
            let mut at = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])];
            at[axis] = bound;
            at
        };
        let mut kept = Vec::with_capacity(ring.len() + 4);
        for (index, &b) in ring.iter().enumerate() {
            let a = ring[(index + ring.len() - 1) % ring.len()];
            match (inside(a), inside(b)) {
                (true, true) => kept.push(b),
                (true, false) => kept.push(cross(a, b)),
                (false, true) => kept.extend([cross(a, b), b]),
                (false, false) => {}
            }
        }
        ring = kept;
    }
    ring
}

/// How far an operation's source lies from its destination, in content coordinates: its source
/// less its path's first point.
fn source_shift(held: &HeldOperation) -> Option<[f64; 2]> {
    let (source, anchor) = (held.source?, *held.points.first()?);
    Some([source[0] - anchor[0], source[1] - anchor[1]])
}

/// `rings` moved by `shift` in content coordinates.
fn moved(rings: &[Vec<[f64; 2]>], [dx, dy]: [f64; 2]) -> Vec<Vec<[f64; 2]>> {
    rings
        .iter()
        .map(|ring| ring.iter().map(|[x, y]| [x + dx, y + dy]).collect())
        .collect()
}

/// One operation's figure, its outline traced once: the outline loop by loop and, with a source,
/// the same loops moved to where the source lies, drawn through the warp on their own.
pub(crate) struct Figure {
    pub(crate) loops: Vec<Outline>,
    pub(crate) source: Option<Outline>,
    /// The loops in content coordinates, as traced.
    rings: Rc<Vec<Vec<[f64; 2]>>>,
}

impl Figure {
    /// The figure's footprint to fill, loop by loop, when it differs from its outline: its loops
    /// clipped to the photograph as it is drawn, the output stage's rectangle on the canvas. The
    /// outline leaves out what the output does not show — a warp zoomed to cover its output
    /// refuses content near the frame's edges, not only off it — so filling its loops would close
    /// each gap with a chord across the picture, filling or cutting away what the stroke never
    /// covered. Each loop is clipped to the content frame first ([`clipped`]), so no position off
    /// it reaches the map, then traced through the warped path to where the map sends it, shown or
    /// not ([`Placement::canvas_point_unbounded`]), and clipped to the output's rectangle there,
    /// following its edge where the loop leaves it.
    ///
    /// `None` when every loop is drawn whole and on the photograph, and the outline's loops are
    /// the footprint: the per-frame check reads the outline's own segments and evaluates no map.
    pub(crate) fn footprint(&self, placement: &Placement) -> Option<Vec<Outline>> {
        let (width, height) = placement.map.output();
        let (low, high) = (
            placement.view.canvas_point(0.0, 0.0),
            placement.view.canvas_point(width, height),
        );
        let min = [f64::from(low.x.min(high.x)), f64::from(low.y.min(high.y))];
        let max = [f64::from(low.x.max(high.x)), f64::from(low.y.max(high.y))];
        if self
            .loops
            .iter()
            .all(|outline| drawn_within(outline, min, max))
        {
            return None;
        }
        let inside: Vec<Vec<[f64; 2]>> = self
            .rings
            .iter()
            .map(|ring| clipped(ring, [0.0; 2], [1.0; 2]))
            .filter(|ring| ring.len() >= 3)
            .collect();
        let traced = loops_placed(placement, &inside, false, |x, y| {
            placement.canvas_point_unbounded(x, y)
        });
        Some(
            traced
                .into_iter()
                .filter_map(|outline| {
                    let ring = clipped(&vertices(&outline), min, max);
                    (ring.len() >= 3).then(|| closed(&ring, outline.approximate))
                })
                .collect(),
        )
    }
}

/// How far apart, in canvas logical points, two drawn positions may be and still be one: the
/// warped path's leaves meet at positions evaluated apart, which can differ in their last bits.
const SAME_POINT: f64 = 0.01;

/// Whether `outline` is drawn whole and inside `[min, max]`: at least one segment, each starting
/// where the one before it ended, the last ending where the first began, and every point within
/// the rectangle.
fn drawn_within(outline: &Outline, min: [f64; 2], max: [f64; 2]) -> bool {
    let (Some(first), Some(last)) = (outline.segments.first(), outline.segments.last()) else {
        return false;
    };
    let near = |a: Point, b: Point| {
        f64::from((a.x - b.x).abs()) <= SAME_POINT && f64::from((a.y - b.y).abs()) <= SAME_POINT
    };
    let within = |p: Point| {
        let (x, y) = (f64::from(p.x), f64::from(p.y));
        x >= min[0] - SAME_POINT
            && x <= max[0] + SAME_POINT
            && y >= min[1] - SAME_POINT
            && y <= max[1] + SAME_POINT
    };
    near(last.1, first.0)
        && outline
            .segments
            .windows(2)
            .all(|pair| near(pair[0].1, pair[1].0))
        && outline
            .segments
            .iter()
            .all(|(a, b)| within(*a) && within(*b))
}

/// The positions a closed outline's segments pass through, in order, each once: a segment that
/// starts where the last ended adds only its end, and the end of the loop, back where it began,
/// adds nothing.
fn vertices(outline: &Outline) -> Vec<[f64; 2]> {
    let apart = |a: [f64; 2], b: [f64; 2]| {
        (a[0] - b[0]).abs() > SAME_POINT || (a[1] - b[1]).abs() > SAME_POINT
    };
    let mut points: Vec<[f64; 2]> = Vec::with_capacity(outline.segments.len() + 1);
    for (a, b) in &outline.segments {
        for p in [a, b] {
            let p = [f64::from(p.x), f64::from(p.y)];
            if points.last().is_none_or(|&last| apart(last, p)) {
                points.push(p);
            }
        }
    }
    if points.len() > 1 && !apart(points[0], points[points.len() - 1]) {
        points.pop();
    }
    points
}

/// `ring`, in canvas logical points, as a closed outline: one segment per edge, the last back to
/// the first.
fn closed(ring: &[[f64; 2]], approximate: bool) -> Outline {
    let point = |p: [f64; 2]| Point::new(p[0] as f32, p[1] as f32);
    Outline {
        segments: (0..ring.len())
            .map(|index| (point(ring[index]), point(ring[(index + 1) % ring.len()])))
            .collect(),
        approximate,
    }
}

/// The figures a canvas traced last, so a redraw that changes neither a path, its radius nor the
/// photograph's aspect (a hover, a pan, a zoom, the pointer moving with the button up) draws them
/// again without retracing. Holds the few figures one frame draws: the gesture in progress and the
/// selected and hovered corrections.
#[derive(Default)]
pub(crate) struct Traces {
    held: RefCell<Vec<Trace>>,
}

/// One traced path: what it was traced from, and its loops in content coordinates.
struct Trace {
    points: Vec<[f64; 2]>,
    radius: f64,
    aspect: f64,
    rings: Rc<Vec<Vec<[f64; 2]>>>,
}

/// The traces a canvas keeps.
const TRACES_HELD: usize = 4;

impl Traces {
    /// The loops of `points` at `radius` on a photograph of `aspect` ([`envelope`]), traced only
    /// when no trace held is of exactly the same path. The latest is kept first.
    fn rings(&self, points: &[[f64; 2]], radius: f64, aspect: f64) -> Rc<Vec<Vec<[f64; 2]>>> {
        let mut held = self.held.borrow_mut();
        let same = |trace: &Trace| {
            trace.radius.to_bits() == radius.to_bits()
                && trace.aspect.to_bits() == aspect.to_bits()
                && trace.points.len() == points.len()
                && trace.points.iter().zip(points).all(|(a, b)| {
                    a[0].to_bits() == b[0].to_bits() && a[1].to_bits() == b[1].to_bits()
                })
        };
        if let Some(index) = held.iter().position(same) {
            let trace = held.remove(index);
            let rings = Rc::clone(&trace.rings);
            held.insert(0, trace);
            return rings;
        }
        let rings = Rc::new(envelope(points, radius, aspect));
        held.insert(
            0,
            Trace {
                points: points.to_vec(),
                radius,
                aspect,
                rings: Rc::clone(&rings),
            },
        );
        held.truncate(TRACES_HELD);
        rings
    }
}

impl std::fmt::Debug for Traces {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Traces")
            .field("held", &self.held.borrow().len())
            .finish()
    }
}

/// The figure of `held` ([`Figure`]). The source outline is the destination's loops translated
/// before the warp, so a figure traces its coverage's boundary once, whatever it draws, and not at
/// all when `traces` holds its path.
pub(crate) fn figure(placement: &Placement, held: &HeldOperation, traces: &Traces) -> Figure {
    let rings = traces.rings(&held.points, held.radius, placement.map.aspect());
    let source = source_shift(held)
        .map(|shift| merged(loops_outline(placement, &moved(&rings, shift), false)));
    Figure {
        loops: loops_outline(placement, &rings, false),
        source,
        rings,
    }
}

/// Its source outline, dashed: the same outline moved to where its first point samples from.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn source_outline(placement: &Placement, held: &HeldOperation) -> Outline {
    let Some(shift) = source_shift(held) else {
        return Outline::default();
    };
    let rings = envelope(&held.points, held.radius, placement.map.aspect());
    merged(loops_outline(placement, &moved(&rings, shift), true))
}

/// The arrow from an operation's source to its destination: the warped line between their anchors
/// and a head at the destination, along the line's last drawn segment.
pub(crate) fn arrow(placement: &Placement, held: &HeldOperation) -> Outline {
    let (Some(source), Some(anchor)) = (held.source, held.points.first()) else {
        return Outline::default();
    };
    let mut drawn = line(placement, (source[0], source[1]), (anchor[0], anchor[1]));
    if let Some(&(from, to)) = drawn.segments.last() {
        let angle = (from.y - to.y).atan2(from.x - to.x);
        for spread in [-ARROW_SPREAD, ARROW_SPREAD] {
            let wing = Point::new(
                to.x + ARROW_HEAD * (angle + spread).cos(),
                to.y + ARROW_HEAD * (angle + spread).sin(),
            );
            drawn.segments.push((to, wing));
        }
    }
    drawn
}

/// Where a content position is drawn, when the map places it inside the output.
fn placed(placement: &Placement, at: [f64; 2]) -> Option<Point> {
    let (x, y) = placement.map.to_output(at[0], at[1])?;
    placement
        .map
        .visible(x, y)
        .then(|| placement.view.canvas_point(x, y))
}

/// An operation's pin, at its path's first point.
pub(crate) fn pin(placement: &Placement, held: &HeldOperation) -> Option<ViewPoint> {
    placed(placement, *held.points.first()?).map(view)
}

/// The selected operation's handles where they are drawn, in hit order: the pin, the source pin
/// and, for a spot, the size handle on the right of its outline.
pub(crate) fn handles(placement: &Placement, held: &HeldOperation) -> Vec<(Handle, ViewPoint)> {
    let mut handles = Vec::with_capacity(3);
    if let Some(at) = pin(placement, held) {
        handles.push((Handle::Pin, at));
    }
    if let Some(at) = held.source.and_then(|source| placed(placement, source)) {
        handles.push((Handle::Source, view(at)));
    }
    if let [[x, y]] = held.points[..]
        && let Some(at) = placed(placement, [x + held.radius / placement.map.aspect(), y])
    {
        handles.push((Handle::Size, view(at)));
    }
    handles
}

/// The source crosshair at a content position: two short canvas-space strokes through it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn crosshair(placement: &Placement, at: [f64; 2], arm: f32) -> Vec<(Point, Point)> {
    let Some(centre) = placed(placement, at) else {
        return Vec::new();
    };
    vec![
        (
            Point::new(centre.x - arm, centre.y),
            Point::new(centre.x + arm, centre.y),
        ),
        (
            Point::new(centre.x, centre.y - arm),
            Point::new(centre.x, centre.y + arm),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{brush::ContentMap, view::canvas_view::CanvasView};

    fn held(points: Vec<[f64; 2]>, radius: f64, source: Option<[f64; 2]>) -> HeldOperation {
        HeldOperation {
            operation: "correction-1".into(),
            points,
            radius,
            source,
        }
    }

    fn warped(turns: u8, mirror: bool) -> Placement {
        let geometry = crate::state::testing::nonlinear_mapping(
            crate::state::testing::TestOrientation { mirror, turns },
            40,
            -25,
        );
        let map = ContentMap::new(&geometry).unwrap();
        let (w, h) = map.output();
        Placement {
            view: CanvasView::fit((w, h), iced::Size::new(1200.0, 850.0)).unwrap(),
            map,
            scale_factor: 2.0,
        }
    }

    /// The distance in canvas points from `point` to the nearest drawn segment.
    fn gap(outline: &Outline, point: Point) -> f32 {
        outline
            .segments
            .iter()
            .map(|(a, b)| {
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let length = dx * dx + dy * dy;
                let t = if length == 0.0 {
                    0.0
                } else {
                    (((point.x - a.x) * dx + (point.y - a.y) * dy) / length).clamp(0.0, 1.0)
                };
                (point.x - (a.x + t * dx)).hypot(point.y - (a.y + t * dy))
            })
            .fold(f32::INFINITY, f32::min)
    }

    #[test]
    fn a_spot_outline_is_its_radius_through_the_warp_in_every_orientation() {
        for turns in 0..4 {
            for mirror in [false, true] {
                let placement = warped(turns, mirror);
                let aspect = placement.map.aspect();
                let spot = held(vec![[0.5, 0.45]], 0.05, Some([0.62, 0.45]));
                let drawn = outline(&placement, &spot);
                assert!(!drawn.segments.is_empty() && drawn.segments.len() <= 1024);
                assert!(!drawn.approximate, "{turns} {mirror}");
                // Every point of the true circle, mapped by the core, lies on the drawn outline.
                for step in 0..64 {
                    let angle = step as f64 * std::f64::consts::TAU / 64.0;
                    let on = [0.5 + 0.05 * angle.cos() / aspect, 0.45 + 0.05 * angle.sin()];
                    let point = placed(&placement, on).expect("inside the frame");
                    assert!(gap(&drawn, point) < 0.5, "{turns} {mirror} {step}");
                }
                // The pin is the spot's centre, the size handle its radius to the right in
                // content, and the source pin the source, each where the core places them.
                let found = handles(&placement, &spot);
                assert_eq!(
                    found.iter().map(|(handle, _)| *handle).collect::<Vec<_>>(),
                    [Handle::Pin, Handle::Source, Handle::Size]
                );
                let size = placed(&placement, [0.5 + 0.05 / aspect, 0.45]).unwrap();
                assert_eq!(found[2].1, (size.x, size.y));
                let source = source_outline(&placement, &spot);
                assert!(!source.segments.is_empty());
                let arrowed = arrow(&placement, &spot);
                let tip = placed(&placement, [0.5, 0.45]).unwrap();
                assert!(gap(&arrowed, tip) < 0.01);
            }
        }
    }

    #[test]
    fn a_stroke_outline_bounds_its_path_and_its_budget() {
        let placement = warped(1, false);
        let raw: Vec<[f64; 2]> = (0..400)
            .map(|index| {
                let t = index as f64 / 399.0;
                [0.2 + 0.6 * t, 0.5 + 0.15 * (t * 9.0).sin()]
            })
            .collect();
        // A stored path, as the list query reads it: the host's decimation at its radius.
        let path = crate::brush::StrokeCapture::of(&raw)
            .posted(0.02)
            .unwrap()
            .to_vec();
        let stroke = held(path.clone(), 0.02, None);
        let drawn = outline(&placement, &stroke);
        assert!(drawn.segments.len() <= 1024, "{}", drawn.segments.len());
        assert!(!drawn.approximate);
        // No source is set, so there is neither a source outline nor an arrow.
        assert!(source_outline(&placement, &stroke).segments.is_empty());
        assert!(arrow(&placement, &stroke).segments.is_empty());
        // A stroke has no size handle: it is resized from its row.
        assert_eq!(
            handles(&placement, &stroke)
                .iter()
                .map(|(handle, _)| *handle)
                .collect::<Vec<_>>(),
            [Handle::Pin]
        );
        // The pin is the first point, as Lightroom's is.
        assert_eq!(
            pin(&placement, &stroke),
            placed(&placement, path[0]).map(view)
        );
        // The start cap reaches the radius behind the first point, against the first segment's
        // direction in mask space, where a radius is round.
        let aspect = placement.map.aspect();
        let angle = (path[1][1] - path[0][1]).atan2((path[1][0] - path[0][0]) * aspect);
        let behind = [
            path[0][0] - 0.02 * angle.cos() / aspect,
            path[0][1] - 0.02 * angle.sin(),
        ];
        let behind = placed(&placement, behind).unwrap();
        assert!(gap(&drawn, behind) < 1.0, "{}", gap(&drawn, behind));
        // However long a path, the outline stays in budget and is whole.
        let long = outline(&placement, &held(raw, 0.02, None));
        assert!(long.segments.len() <= 1024, "{}", long.segments.len());
        assert!(gap(&long, behind) < 4.0, "{}", gap(&long, behind));
    }

    /// The distance in mask space from `p` to a path's nearest segment: what coverage measures.
    fn to_path(p: [f64; 2], path: &[[f64; 2]]) -> f64 {
        path.windows(2)
            .map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                let d = [b[0] - a[0], b[1] - a[1]];
                let len2 = d[0] * d[0] + d[1] * d[1];
                let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0);
                (p[0] - a[0] - t * d[0]).hypot(p[1] - a[1] - t * d[1])
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// The area a closed ring bounds, signed by its winding.
    fn area(ring: &[[f64; 2]]) -> f64 {
        (0..ring.len())
            .map(|i| {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                a[0] * b[1] - b[0] * a[1]
            })
            .sum::<f64>()
            / 2.0
    }

    /// A stroke brushed right, back left over the same row and right again has the outline of one
    /// pass: one loop, every vertex on the boundary of what its coverage covers, none inside it,
    /// and the area of one capsule. Offsetting the path's sides, as the outline once did, zigzags
    /// across the stroke at every turn.
    #[test]
    fn a_stroke_brushed_back_and_forth_has_the_outline_of_one_pass() {
        let radius = 0.05;
        let once = [[0.2, 0.5], [0.8, 0.5]];
        let revisited = [[0.2, 0.5], [0.8, 0.5], [0.3, 0.5], [0.7, 0.5], [0.25, 0.5]];
        // Back and forth with turns that do not retrace exactly, as a hand paints.
        let scrubbed = [
            [0.2, 0.5],
            [0.8, 0.52],
            [0.3, 0.48],
            [0.75, 0.51],
            [0.35, 0.5],
        ];
        for path in [&once[..], &revisited[..], &scrubbed[..]] {
            let loops = union_loops(path, radius);
            assert_eq!(loops.len(), 1, "{path:?}");
            let cell = radius / CELLS_PER_RADIUS;
            for vertex in &loops[0] {
                let d = to_path(*vertex, path);
                // On a side or a cap to a twentieth of a cell; where two passes meet at an angle,
                // the distance has a kink and a vertex is within a quarter of one.
                assert!(
                    (d - radius).abs() < cell / 4.0,
                    "{path:?}: {vertex:?} at {d}"
                );
            }
        }
        let [one, back] = [&once[..], &revisited[..]].map(|path| union_loops(path, radius));
        let capsule = 2.0 * radius * 0.6 + std::f64::consts::PI * radius * radius;
        for ring in [&one[0], &back[0]] {
            assert!((area(ring).abs() - capsule).abs() < capsule * 0.002);
        }
        assert_eq!(one, back, "going back over a covered row changes nothing");
        // Through the warp, the drawn figures are the same figure.
        let placement = warped(0, false);
        let drawn = |points: &[[f64; 2]]| {
            let aspect = placement.map.aspect();
            let content: Vec<[f64; 2]> = points.iter().map(|[x, y]| [x / aspect, *y]).collect();
            outline(&placement, &held(content, radius, None))
        };
        let (single, double) = (drawn(&once), drawn(&revisited));
        for (a, b) in [(&single, &double), (&double, &single)] {
            for (from, to) in &a.segments {
                assert!(gap(b, *from) < 0.01 && gap(b, *to) < 0.01);
            }
        }
    }

    /// A stroke that closes on itself encloses a hole: its outline is two loops, wound opposite
    /// ways, so the footprint fills as a ring.
    #[test]
    fn a_closed_stroke_outlines_its_hole() {
        let radius = 0.03;
        let square = [[0.3, 0.3], [0.7, 0.3], [0.7, 0.7], [0.3, 0.7], [0.3, 0.3]];
        let loops = union_loops(&square, radius);
        assert_eq!(loops.len(), 2);
        let areas: Vec<f64> = loops.iter().map(|ring| area(ring)).collect();
        assert!(areas[0].signum() != areas[1].signum(), "{areas:?}");
        let (outer, inner) = (
            areas[0].abs().max(areas[1].abs()),
            areas[0].abs().min(areas[1].abs()),
        );
        // The outer boundary is the square grown by the radius, its corners round; the hole is the
        // square shrunk by it, its corners square.
        let side = 0.4;
        let grown = (side + 2.0 * radius).powi(2) - (4.0 - std::f64::consts::PI) * radius * radius;
        assert!((outer - grown).abs() < grown * 0.002, "{outer} {grown}");
        let shrunk = (side - 2.0 * radius).powi(2);
        assert!((inner - shrunk).abs() < shrunk * 0.01, "{inner} {shrunk}");
    }

    /// Strokes that leave the content frame entirely: out through the right edge, round the corner
    /// and back in through the top; out through the left edge and back in lower down; out through
    /// the top and round two corners, back in through the bottom.
    const OFF_THE_FRAME: [&[[f64; 2]]; 3] = [
        &[[0.6, 0.2], [1.3, 0.2], [1.3, -0.3], [0.8, -0.3], [0.8, 0.1]],
        &[[0.2, 0.3], [-0.3, 0.3], [-0.3, 0.7], [0.2, 0.7]],
        &[
            [0.5, 0.5],
            [0.5, -0.2],
            [1.2, -0.2],
            [1.2, 1.2],
            [0.3, 1.2],
            [0.3, 0.8],
        ],
    ];

    /// The footprint of a stroke along `path` at `radius` that leaves the photograph, filled as the
    /// canvas fills it — each loop one closed path, even-odd — sampled over the photograph as drawn
    /// and a margin round it: filled on the photograph exactly where the stroke covers, and nowhere
    /// off it. A short stroke at its centre fills its outline's own loops.
    fn assert_fills_what_it_covers(placement: &Placement, path: &[[f64; 2]], radius: f64) {
        let traces = Traces::default();
        let on = held(vec![[0.45, 0.5], [0.55, 0.5]], 0.03, None);
        assert!(
            figure(placement, &on, &traces)
                .footprint(placement)
                .is_none()
        );
        let aspect = placement.map.aspect();
        let figure = figure(placement, &held(path.to_vec(), radius, None), &traces);
        let loops = figure.footprint(placement).expect("off the photograph");
        let polygons: Vec<Vec<Point>> = loops
            .iter()
            .filter_map(|outline| {
                let first = outline.segments.first()?.0;
                Some(
                    std::iter::once(first)
                        .chain(outline.segments.iter().map(|(_, b)| *b))
                        .collect(),
                )
            })
            .collect();
        let filled = |p: Point| {
            polygons.iter().fold(false, |inside, polygon| {
                let crossings = (0..polygon.len())
                    .filter(|&i| {
                        let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
                        (a.y > p.y) != (b.y > p.y)
                            && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x)
                    })
                    .count();
                inside ^ (crossings % 2 == 1)
            })
        };
        let mask: Vec<[f64; 2]> = path.iter().map(|[x, y]| [x * aspect, *y]).collect();
        let (width, height) = placement.map.output();
        let steps = 80;
        for j in 0..steps {
            for i in 0..steps {
                // The output stage and a tenth of it either side.
                let (x, y) = (
                    width * (1.2 * (i as f64 + 0.5) / steps as f64 - 0.1),
                    height * (1.2 * (j as f64 + 0.5) / steps as f64 - 0.1),
                );
                let point = placement.view.canvas_point(x, y);
                let shown = (0.0..=width).contains(&x) && (0.0..=height).contains(&y);
                if !shown {
                    assert!(!filled(point), "{path:?} off the photograph at {x}, {y}");
                    continue;
                }
                let at = placement.content_point(point).expect("on the photograph");
                let d = to_path([at.0 * aspect, at.1], &mask);
                if (d - radius).abs() < radius / 10.0 {
                    continue;
                }
                assert_eq!(filled(point), d < radius, "{path:?} at {at:?}");
            }
        }
    }

    /// With no geometry, a stroke painted off the photograph and back fills what it covers on the
    /// photograph and nothing else. Filling the outline's loops instead closes each stretch off the
    /// photograph with a chord across it.
    #[test]
    fn a_stroke_painted_off_the_photograph_and_back_fills_what_it_covers() {
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let placement = Placement {
            map: ContentMap::from_affine((6000, 4000), (6000, 4000), id, id).unwrap(),
            view: CanvasView::fit((6000.0, 4000.0), iced::Size::new(1200.0, 800.0)).unwrap(),
            scale_factor: 2.0,
        };
        for path in OFF_THE_FRAME {
            assert_fills_what_it_covers(&placement, path, 0.03);
        }
    }

    /// **Under a warp** zoomed to cover its output, content near the frame's edges is not shown and
    /// the outline has gaps there even where the stroke never leaves the content frame. The
    /// footprint is clipped to the output as drawn, so it fills no chord across those gaps.
    #[test]
    fn a_stroke_past_a_warped_photographs_edge_fills_what_it_covers() {
        let map = ContentMap::new(&crate::state::testing::perspective_mapping()).unwrap();
        let (width, height) = map.output();
        let placement = Placement {
            map,
            view: CanvasView::fit((width, height), iced::Size::new(1200.0, 800.0)).unwrap(),
            scale_factor: 2.0,
        };
        // Out to the right and round to the top inside the content frame, through what the warp
        // does not show.
        let within: &[[f64; 2]] = &[[0.5, 0.5], [0.985, 0.5], [0.985, 0.015], [0.6, 0.015]];
        let radius = 0.01;
        let rings = envelope(within, radius, placement.map.aspect());
        assert!(
            rings
                .iter()
                .flatten()
                .all(|p| (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1])),
            "the stroke stays inside the content frame"
        );
        assert!(
            within
                .iter()
                .any(|p| placement.canvas_point(p[0], p[1]).is_none()),
            "and leaves what the warp shows"
        );
        assert_fills_what_it_covers(&placement, within, radius);
        for path in OFF_THE_FRAME {
            assert_fills_what_it_covers(&placement, path, 0.03);
        }
    }

    /// **Under a quarter turn and a crop** the map places every content position, so the outline
    /// has no gaps, but the crop leaves part of the content unshown: the footprint fills only the
    /// cropped photograph.
    #[test]
    fn a_stroke_past_a_turned_crop_fills_only_the_photograph() {
        // A quarter turn clockwise of 6000 × 4000 to 4000 × 6000, cropped to 3000 × 4500 from
        // (500, 800): output (x', y') = (3500 - y, x - 800).
        let placement = Placement {
            map: ContentMap::from_affine(
                (6000, 4000),
                (3000, 4500),
                [0.0, -1.0, 3500.0, 1.0, 0.0, -800.0],
                [0.0, 1.0, 800.0, -1.0, 0.0, 3500.0],
            )
            .unwrap(),
            view: CanvasView::fit((3000.0, 4500.0), iced::Size::new(1200.0, 800.0)).unwrap(),
            scale_factor: 2.0,
        };
        // Out past the crop's right and top, inside the content frame.
        let within: &[[f64; 2]] = &[[0.5, 0.5], [0.95, 0.5], [0.95, 0.05], [0.6, 0.05]];
        assert_fills_what_it_covers(&placement, within, 0.03);
        for path in OFF_THE_FRAME {
            assert_fills_what_it_covers(&placement, path, 0.03);
        }
    }

    /// However long and tangled a stroke, its outline is traced within the evaluation bound and
    /// drawn within the segment budget.
    #[test]
    fn a_scribbled_stroke_outline_stays_in_budget() {
        let placement = warped(1, false);
        let scribble: Vec<[f64; 2]> = (0..1000)
            .map(|index| {
                let t = index as f64 / 999.0;
                [0.5 + 0.3 * (t * 40.0).sin(), 0.5 + 0.3 * (t * 37.0).cos()]
            })
            .collect();
        let drawn = outline(&placement, &held(scribble, 0.004, None));
        assert!(!drawn.segments.is_empty());
        assert!(
            drawn.segments.len() <= MAX_SEGMENTS + 3 * 64,
            "{}",
            drawn.segments.len()
        );
    }

    /// A figure traces once: its source outline is its outline moved by the source's offset, and
    /// lies where tracing the moved path would put it.
    #[test]
    fn a_figure_draws_its_source_from_its_own_trace() {
        let placement = warped(0, false);
        let path = vec![[0.3, 0.5], [0.6, 0.52], [0.35, 0.5], [0.55, 0.48]];
        let stroke = held(path.clone(), 0.03, Some([0.32, 0.25]));
        let drawn = figure(&placement, &stroke, &Traces::default());
        assert_eq!(drawn.loops.len(), 1);
        let source = drawn.source.expect("a source");
        let traced = outline(
            &placement,
            &held(
                path.iter().map(|[x, y]| [x + 0.02, y - 0.25]).collect(),
                0.03,
                None,
            ),
        );
        for (from, to) in &source.segments {
            assert!(gap(&traced, *from) < 1.0 && gap(&traced, *to) < 1.0);
        }
        assert!(
            figure(&placement, &held(path, 0.03, None), &Traces::default())
                .source
                .is_none()
        );
    }

    /// A redraw of an unchanged path reuses its trace; a changed path, radius or aspect is traced
    /// again, and only the latest few traces are kept.
    #[test]
    fn traces_are_reused_until_the_path_changes() {
        let traces = Traces::default();
        let path = vec![[0.3, 0.5], [0.6, 0.52]];
        let first = traces.rings(&path, 0.03, 1.5);
        assert!(Rc::ptr_eq(&first, &traces.rings(&path, 0.03, 1.5)));
        let longer = vec![[0.3, 0.5], [0.6, 0.52], [0.62, 0.6]];
        for (points, radius, aspect) in
            [(&longer, 0.03, 1.5), (&path, 0.04, 1.5), (&path, 0.03, 1.0)]
        {
            assert!(!Rc::ptr_eq(&first, &traces.rings(points, radius, aspect)));
        }
        assert!(Rc::ptr_eq(&first, &traces.rings(&path, 0.03, 1.5)));
        for step in 0..TRACES_HELD {
            traces.rings(&[[0.1, 0.1], [0.2, 0.1 + step as f64 * 0.01]], 0.03, 1.5);
        }
        assert!(!Rc::ptr_eq(&first, &traces.rings(&path, 0.03, 1.5)));
        assert_eq!(traces.held.borrow().len(), TRACES_HELD);
    }

    /// What one trace of a worst-case live stroke costs, in release:
    /// `cargo test -p luxforge-app --release --bin luxforge trace_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, run on request"]
    fn trace_cost() {
        let aspect = 1.5;
        // A 400-position stroke brushed back and forth across one band, at the default size.
        let scrub: Vec<[f64; 2]> = (0..400)
            .map(|index| {
                let pass = index / 40;
                let t = (index % 40) as f64 / 39.0;
                let x = if pass % 2 == 0 { t } else { 1.0 - t };
                [0.2 + 0.6 * x, 0.5 + 0.004 * pass as f64]
            })
            .collect();
        // A stroke over 2,000 radii at the smallest radius: a slow sine across the frame.
        let long: Vec<[f64; 2]> = (0..1000)
            .map(|index| {
                let t = index as f64 / 999.0;
                [0.05 + 0.9 * t, 0.5 + 0.3 * (t * 12.0).sin()]
            })
            .collect();
        let length = |points: &[[f64; 2]]| {
            points
                .windows(2)
                .map(|p| ((p[1][0] - p[0][0]) * aspect).hypot(p[1][1] - p[0][1]))
                .sum::<f64>()
        };
        let placement = warped(0, false);
        for (name, points, radius) in [
            ("scrub, 400 positions, r 0.02", &scrub, 0.02),
            ("scrub, 400 positions, r 0.005", &scrub, 0.005),
            ("long, 1000 positions, r 0.0005", &long, 0.0005),
        ] {
            let mask: Vec<[f64; 2]> = points.iter().map(|[x, y]| [x * aspect, *y]).collect();
            let runs = 20;
            let start = std::time::Instant::now();
            let mut vertices = 0;
            for _ in 0..runs {
                vertices = std::hint::black_box(union_loops(&mask, radius))
                    .iter()
                    .map(Vec::len)
                    .sum::<usize>();
            }
            let trace = start.elapsed().as_secs_f64() * 1e6 / f64::from(runs);
            let held = held(
                points.clone(),
                radius,
                Some([points[0][0], points[0][1] - 0.1]),
            );
            let start = std::time::Instant::now();
            for _ in 0..runs {
                std::hint::black_box(figure(&placement, &held, &Traces::default()));
            }
            let whole = start.elapsed().as_secs_f64() * 1e6 / f64::from(runs);
            let traces = Traces::default();
            figure(&placement, &held, &traces);
            let start = std::time::Instant::now();
            for _ in 0..runs {
                std::hint::black_box(figure(&placement, &held, &traces));
            }
            let redraw = start.elapsed().as_secs_f64() * 1e6 / f64::from(runs);
            eprintln!(
                "{name}: {:.0} radii long, trace {trace:.0} us ({vertices} vertices), \
                 figure with source through the map {whole:.0} us, redrawn from its trace \
                 {redraw:.0} us",
                length(points) / radius
            );
        }
    }

    #[test]
    fn a_crosshair_is_drawn_only_inside_the_frame() {
        let placement = warped(0, false);
        assert_eq!(crosshair(&placement, [0.5, 0.5], 6.0).len(), 2);
        assert!(crosshair(&placement, [-0.8, -0.8], 6.0).is_empty());
    }
}
