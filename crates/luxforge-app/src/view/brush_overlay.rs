//! The painted-path overlay's geometry: an operation's outline, its dashed source outline, the arrow
//! from source to destination, its pin and handles and the source crosshair, each placed through
//! the core's map and the shared warped-path drawing ([`super::warped_path`]), so they follow Lens,
//! Perspective, straighten, crop, quarter turns and mirroring with the picture. Pure geometry in
//! canvas logical points: what draws it is the canvas's, and every figure is bounded by the
//! warped path's segment budget whatever the path's length.
use super::{
    mask_canvas::Placement,
    warped_path::{Outline, curve, line},
};
use crate::brush::{Handle, HeldOperation, ViewPoint};
use iced::Point;

/// The arrowhead's length and half-angle, in logical points and radians.
const ARROW_HEAD: f32 = 7.0;
const ARROW_SPREAD: f32 = 0.45;
/// Points per round cap and per circle of the envelope.
const CAP_POINTS: usize = 12;
const CIRCLE_POINTS: usize = 32;

fn view(point: Point) -> ViewPoint {
    (point.x, point.y)
}

/// The envelope of `points` at `radius` (units of the content height), as one closed loop of
/// normalized content positions: a circle for a spot; for a stroke, each segment's two sides joined
/// by round caps at its ends. Built in mask space, where the radius is round, and returned in
/// content coordinates.
fn envelope(points: &[[f64; 2]], radius: f64, aspect: f64) -> Vec<[f64; 2]> {
    let to_mask = |[x, y]: [f64; 2]| [x * aspect, y];
    let to_content = |[x, y]: [f64; 2]| [x / aspect, y];
    let around = |centre: [f64; 2], from: f64, sweep: f64, count: usize| {
        (0..=count).map(move |step| {
            let angle = from + sweep * step as f64 / count as f64;
            [
                centre[0] + radius * angle.cos(),
                centre[1] + radius * angle.sin(),
            ]
        })
    };
    let mask: Vec<[f64; 2]> = points.iter().copied().map(to_mask).collect();
    let mut segments: Vec<([f64; 2], [f64; 2], f64)> = mask
        .windows(2)
        .filter_map(|pair| {
            let (dx, dy) = (pair[1][0] - pair[0][0], pair[1][1] - pair[0][1]);
            (dx != 0.0 || dy != 0.0).then(|| (pair[0], pair[1], dy.atan2(dx)))
        })
        .collect();
    let Some(&(first, _, start)) = segments.first() else {
        let centre = mask.first().copied().unwrap_or_default();
        return around(centre, 0.0, std::f64::consts::TAU, CIRCLE_POINTS)
            .map(to_content)
            .collect();
    };
    let side = |point: [f64; 2], angle: f64, sign: f64| {
        let normal = angle + sign * std::f64::consts::FRAC_PI_2;
        [
            point[0] + radius * normal.cos(),
            point[1] + radius * normal.sin(),
        ]
    };
    let mut ring = Vec::with_capacity(segments.len() * 4 + 2 * CAP_POINTS + 2);
    for &(a, b, angle) in &segments {
        ring.push(side(a, angle, -1.0));
        ring.push(side(b, angle, -1.0));
    }
    let &(_, last, end) = segments.last().expect("a segment");
    ring.extend(around(
        last,
        end - std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
        CAP_POINTS,
    ));
    segments.reverse();
    for &(a, b, angle) in &segments {
        ring.push(side(b, angle, 1.0));
        ring.push(side(a, angle, 1.0));
    }
    ring.extend(around(
        first,
        start + std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
        CAP_POINTS,
    ));
    ring.into_iter().map(to_content).collect()
}

/// The most ring vertices one outline seeds the warped path with: half its segment budget, so
/// each edge keeps room to bend with the warp.
const MAX_RING: usize = 512;

/// A closed loop of content positions drawn through the warped path, one seed per edge. A ring
/// longer than [`MAX_RING`] is drawn through every n-th vertex and reported approximate, so a long
/// stroke's outline is never cut short by the budget.
fn loop_outline(placement: &Placement, ring: &[[f64; 2]], dashed: bool) -> Outline {
    let stride = ring.len().div_ceil(MAX_RING).max(1);
    let ring: Vec<[f64; 2]> = ring.iter().step_by(stride).copied().collect();
    let edges = ring.len();
    if edges < 2 {
        return Outline::default();
    }
    let mut outline = curve(
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
    );
    outline.approximate |= stride > 1;
    outline
}

/// An operation's outline: the envelope of its painted path at its radius.
pub(crate) fn outline(placement: &Placement, held: &HeldOperation) -> Outline {
    loop_outline(
        placement,
        &envelope(&held.points, held.radius, placement.map.aspect()),
        false,
    )
}

/// Its source outline, dashed: the same envelope moved to where its first point samples from.
pub(crate) fn source_outline(placement: &Placement, held: &HeldOperation) -> Outline {
    let (Some(source), Some(anchor)) = (held.source, held.points.first()) else {
        return Outline::default();
    };
    let (dx, dy) = (source[0] - anchor[0], source[1] - anchor[1]);
    let moved: Vec<[f64; 2]> = held.points.iter().map(|[x, y]| [x + dx, y + dy]).collect();
    loop_outline(
        placement,
        &envelope(&moved, held.radius, placement.map.aspect()),
        true,
    )
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
        let path = luxforge_core::path::decimate(&raw, 0.02).unwrap();
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
        // However long a path, the outline stays in budget and is whole, approximately.
        let long = outline(&placement, &held(raw, 0.02, None));
        assert!(long.segments.len() <= 1024, "{}", long.segments.len());
        assert!(long.approximate);
        assert!(gap(&long, behind) < 4.0, "{}", gap(&long, behind));
    }

    #[test]
    fn a_crosshair_is_drawn_only_inside_the_frame() {
        let placement = warped(0, false);
        assert_eq!(crosshair(&placement, [0.5, 0.5], 6.0).len(), 2);
        assert!(crosshair(&placement, [-0.8, -0.8], 6.0).is_empty());
    }
}
