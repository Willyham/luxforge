//! The painted-path overlay's geometry: an operation's outline, its dashed source outline, the arrow
//! from source to destination, its pin and handles and the source crosshair, each placed through
//! the core's map and the shared warped-path drawing ([`super::warped_path`]), so they follow Lens,
//! Perspective, straighten, crop, quarter turns and mirroring with the picture. Pure geometry in
//! canvas logical points: what draws it is the canvas's, and every figure is bounded by the
//! warped path's segment budget whatever the path's length.
use super::{
    mask_canvas::Placement,
    warped_path::{MAX_SEGMENTS, Outline, curve_within, line},
};
use crate::brush::{Handle, HeldOperation, ViewPoint};
use iced::Point;
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    fn distance(&self, p: [f64; 2]) -> f64 {
        let (ex, ey) = (p[0] - self.a[0], p[1] - self.a[1]);
        let t = ((ex * self.d[0] + ey * self.d[1]) / self.len2).clamp(0.0, 1.0);
        (ex - t * self.d[0]).hypot(ey - t * self.d[1])
    }
}

/// A grid edge: the vertex it starts at, and whether it runs down (`true`) or right (`false`).
type EdgeKey = (i32, i32, bool);

/// The boundary of the union of a stroke's capsules at `radius`, in mask space: the shape its
/// coverage has, whatever the path does. Coverage is the distance to the path's nearest segment, so
/// a stroke brushed back over itself covers what one pass covers, and its outline is that of one
/// pass; offsetting the path's sides would draw a figure that crosses itself wherever the path
/// turns back.
///
/// Traced by marching squares over the distance to the path, sampled only at grid vertices within
/// reach of a segment, so the work follows the stroke's area rather than its box and is bounded by
/// [`EVALUATIONS_MAX`]. The distance is linear across a straight side, so a side's crossings are
/// exact; caps and inner corners are within a small share of a cell. Every loop is closed and keeps
/// the covered area on the same side, so a hole the path encloses is its own loop and the loops
/// fill as one shape. A path with no segment of any length answers no loop.
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
    // a crossing reads is recorded; one that is not is farther than `reach`.
    let reach = radius + 2.0 * cell;
    let mut distance: HashMap<(i32, i32), f64> = HashMap::new();
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
            for j in low(1) as i32..=high(1) as i32 {
                for i in low(0) as i32..=high(0) as i32 {
                    let d = capsule.distance([f64::from(i) * cell, f64::from(j) * cell]);
                    if d <= reach {
                        distance
                            .entry((i, j))
                            .and_modify(|held| *held = held.min(d))
                            .or_insert(d);
                    }
                }
            }
        }
    }
    let at = |vertex: (i32, i32)| distance.get(&vertex).copied().unwrap_or(reach);
    let position = |(i, j): (i32, i32)| [f64::from(i) * cell, f64::from(j) * cell];
    // Where the boundary crosses an edge, from its two ends in one order, so the two cells that
    // share the edge place it identically.
    let crossing = |(i, j, down): EdgeKey| {
        let (p, q) = ((i, j), if down { (i, j + 1) } else { (i + 1, j) });
        let (dp, dq) = (at(p), at(q));
        let t = (radius - dp) / (dq - dp);
        let (a, b) = (position(p), position(q));
        [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]
    };
    let cells: BTreeSet<(i32, i32)> = distance
        .iter()
        .filter(|&(_, d)| *d < radius)
        .flat_map(|(&(i, j), _)| [(i - 1, j - 1), (i, j - 1), (i - 1, j), (i, j)])
        .collect();
    // Taken around a cell, its corners leave the covered area at one crossing and come back at the
    // next; each boundary piece runs from a leaving crossing to a returning one, so the covered
    // area is on the same side of every piece and each crossing leaves one cell and returns to the
    // cell beside it.
    let mut next: BTreeMap<EdgeKey, EdgeKey> = BTreeMap::new();
    for &(i, j) in &cells {
        let corners = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
        let edges: [EdgeKey; 4] = [
            (i, j, false),
            (i + 1, j, true),
            (i, j + 1, false),
            (i, j, true),
        ];
        let inside = corners.map(|corner| at(corner) < radius);
        let crossings: Vec<(EdgeKey, bool)> = (0..4)
            .filter(|&k| inside[k] != inside[(k + 1) % 4])
            .map(|k| (edges[k], inside[k]))
            .collect();
        match crossings.len() {
            2 => {
                let (leaves, returns) = if crossings[0].1 {
                    (crossings[0].0, crossings[1].0)
                } else {
                    (crossings[1].0, crossings[0].0)
                };
                next.insert(leaves, returns);
            }
            4 => {
                // A saddle: the cell's centre decides whether its two covered corners join.
                let centre = corners.iter().map(|&corner| at(corner)).sum::<f64>() / 4.0;
                let step = if centre < radius { 1 } else { 3 };
                for k in 0..4 {
                    if crossings[k].1 {
                        next.insert(crossings[k].0, crossings[(k + step) % 4].0);
                    }
                }
            }
            _ => {}
        }
    }
    let mut loops = Vec::new();
    while let Some((&start, _)) = next.iter().next() {
        let mut ring = Vec::new();
        let mut key = start;
        while let Some(following) = next.remove(&key) {
            ring.push(crossing(key));
            key = following;
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
/// `budget` drawn segments. A ring longer than `seeds` is drawn through every n-th vertex and
/// reported approximate, so a long stroke's outline is never cut short by the budget.
fn loop_outline(
    placement: &Placement,
    ring: &[[f64; 2]],
    seeds: usize,
    budget: usize,
    dashed: bool,
) -> Outline {
    let stride = ring.len().div_ceil(seeds.max(1)).max(1);
    let ring: Vec<[f64; 2]> = ring.iter().step_by(stride).copied().collect();
    let edges = ring.len();
    if edges < 2 {
        return Outline::default();
    }
    let mut outline = curve_within(
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
    );
    outline.approximate |= stride > 1;
    outline
}

/// Loops drawn through the warped path, each its own outline, sharing one outline's seeds and
/// segment budget by their lengths.
fn loops_outline(placement: &Placement, loops: &[Vec<[f64; 2]>], dashed: bool) -> Vec<Outline> {
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
            )
        })
        .collect()
}

fn merged(loops: Vec<Outline>) -> Outline {
    loops.into_iter().fold(Outline::default(), |mut all, one| {
        all.segments.extend(one.segments);
        all.approximate |= one.approximate;
        all
    })
}

/// An operation's outline, loop by loop: the boundary of what its coverage covers, so a stroke
/// brushed back over itself has the outline of one pass over that area. Filled together, the loops
/// are its footprint, any hole it encloses left open.
pub(crate) fn outline_loops(placement: &Placement, held: &HeldOperation) -> Vec<Outline> {
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

/// Its source outline, dashed: the same outline moved to where its first point samples from.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn source_outline(placement: &Placement, held: &HeldOperation) -> Outline {
    let (Some(source), Some(anchor)) = (held.source, held.points.first()) else {
        return Outline::default();
    };
    let (dx, dy) = (source[0] - anchor[0], source[1] - anchor[1]);
    let moved: Vec<[f64; 2]> = held.points.iter().map(|[x, y]| [x + dx, y + dy]).collect();
    merged(loops_outline(
        placement,
        &envelope(&moved, held.radius, placement.map.aspect()),
        true,
    ))
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

    #[test]
    fn a_crosshair_is_drawn_only_inside_the_frame() {
        let placement = warped(0, false);
        assert_eq!(crosshair(&placement, [0.5, 0.5], 6.0).len(), 2);
        assert!(crosshair(&placement, [-0.8, -0.8], 6.0).is_empty());
    }
}
