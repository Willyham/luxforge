//! Where a correction lies in the content stage: its footprint and its source's, in the stored
//! normalized coordinates (`x` a fraction of the content stage's width, `y` of its height, the frame
//! `[0, 1]` on both), and the absolute source position the API speaks in.
use super::{
    payload::{Brush, OFFSET_MAX},
    stroke::CorrectionStroke,
};
use crate::{
    Error, Stage,
    path::{quantize, steps_to_units},
};

/// A footprint: the path's bounding box grown by the radius, `[x0, y0, x1, y1]` in stored
/// coordinates. The radius is in content heights, so it grows `x` by `radius / aspect`.
pub(crate) type Footprint = [f64; 4];

/// The footprint of `stroke` on a content stage of `stage`'s aspect.
pub(crate) fn footprint(stroke: &CorrectionStroke, stage: Stage) -> Footprint {
    footprint_of(stroke.points(), stroke.radius(), stage)
}

fn footprint_of(
    points: impl IntoIterator<Item = [f64; 2]>,
    radius: f64,
    stage: Stage,
) -> Footprint {
    let aspect = f64::from(stage.width) / f64::from(stage.height);
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for [x, y] in points {
        bounds = [
            bounds[0].min(x),
            bounds[1].min(y),
            bounds[2].max(x),
            bounds[3].max(y),
        ];
    }
    [
        bounds[0] - radius / aspect,
        bounds[1] - radius,
        bounds[2] + radius / aspect,
        bounds[3] + radius,
    ]
}

/// `footprint` moved by `offset` grid steps.
pub(crate) fn shifted(footprint: Footprint, offset: [i32; 2]) -> Footprint {
    let [dx, dy] = offset.map(steps_to_units);
    [
        footprint[0] + dx,
        footprint[1] + dy,
        footprint[2] + dx,
        footprint[3] + dy,
    ]
}

/// How a source that would leave the frame is brought back inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFit {
    /// Moved inward, on each axis it leaves by, just far enough: a source placed at a position, by
    /// the API, an `⌥`-click, a `⌘`-drag or a source drag, stays as near it as the frame allows.
    Clamp,
    /// Turned to the other side, on each axis it leaves by, when that side holds it, and moved
    /// inward otherwise: the window's set offset carried to a press by an edge or a corner keeps
    /// its distance from the destination rather than being pushed onto it.
    Turn,
}

/// `offset` grid steps changed, on each axis whose source footprint would leave the content
/// frame, so the source's whole footprint lies inside it, a stage pixel in from each edge so that a
/// whole-pixel offset rounded at the stage never reaches past it. Where the footprint is wider than
/// the frame on an axis, its part inside the frame is what must stay inside. An axis that already
/// fits is unchanged, so a fitting offset is its own answer.
pub(crate) fn fit_offset(
    footprint: Footprint,
    offset: [i32; 2],
    stage: Stage,
    fit: SourceFit,
) -> [i32; 2] {
    let steps = crate::path::COORDINATE_STEPS_PER_UNIT;
    let mut fitted = offset;
    for (axis, pixels) in [stage.width, stage.height].into_iter().enumerate() {
        let margin = 1.0 / f64::from(pixels.max(1));
        let (low, high) = (margin, 1.0 - margin);
        let (b0, b1) = (footprint[axis], footprint[axis + 2]);
        let (n0, n1) = if b1 - b0 <= high - low {
            (b0, b1)
        } else {
            (b0.max(low), b1.min(high))
        };
        let least = ((low - n0) * steps).ceil();
        let most = ((high - n1) * steps).floor().max(least);
        let fits = |k: i32| (least..=most).contains(&f64::from(k));
        let k = offset[axis];
        fitted[axis] = if fits(k) {
            k
        } else if fit == SourceFit::Turn && fits(-k) {
            -k
        } else {
            f64::from(k).clamp(least, most) as i32
        };
    }
    fitted
}

/// Where a correction drawn along `points` (stored positions, the first its anchor) at `radius`
/// samples from when its source is asked to be at `source`, on a content stage of `content`
/// pixels: the asked position when its whole footprint lies inside the frame, otherwise the
/// position `fit` brings it to ([`SourceFit`]), exactly on the stored grid. The commands store a
/// sent source this way with [`SourceFit::Clamp`]; a client that carries an offset from an earlier
/// correction asks for [`SourceFit::Turn`] before it sends one. `None` for no points, an empty
/// stage or a position that is not a number.
pub fn fit_correction_source(
    points: &[[f64; 2]],
    radius: f64,
    source: [f64; 2],
    content: (u32, u32),
    fit: SourceFit,
) -> Option<[f64; 2]> {
    let anchor = (*points.first()?).map(quantize);
    if content.0 == 0
        || content.1 == 0
        || !radius.is_finite()
        || !source
            .iter()
            .chain(points.iter().flatten())
            .all(|value| value.is_finite())
    {
        return None;
    }
    let stage = Stage {
        width: content.0,
        height: content.1,
    };
    let footprint = footprint_of(points.iter().copied(), radius, stage);
    let offset = [
        quantize(source[0]) - anchor[0],
        quantize(source[1]) - anchor[1],
    ];
    let fitted = fit_offset(footprint, offset, stage, fit);
    Some([anchor[0] + fitted[0], anchor[1] + fitted[1]].map(steps_to_units))
}

/// Whether `footprint` meets the content frame with any area.
pub(crate) fn meets_frame(footprint: Footprint) -> bool {
    footprint[2] > 0.0 && footprint[0] < 1.0 && footprint[3] > 0.0 && footprint[1] < 1.0
}

/// The offset that puts the source anchor at the absolute position `source`, for `stroke`'s
/// anchor: `quantize(source) − anchor`, exactly on the grid. A zero offset or one outside the
/// stored range is refused by name.
pub(crate) fn offset_to(stroke: &CorrectionStroke, source: [f64; 2]) -> Result<[i32; 2], Error> {
    let anchor = stroke.anchor();
    let offset = [
        quantize(source[0]) - anchor[0],
        quantize(source[1]) - anchor[1],
    ];
    checked_offset(offset)
}

/// An offset held to the stored rules: within `±49152` grid steps on each axis, and not zero.
pub(crate) fn checked_offset(offset: [i32; 2]) -> Result<[i32; 2], Error> {
    if offset == [0, 0] {
        return Err(Error::validation(
            "a correction's source must differ from its destination",
        ));
    }
    if offset.iter().any(|value| value.abs() > OFFSET_MAX) {
        return Err(Error::validation(format!(
            "a correction's source is more than {OFFSET_MAX} grid steps from its destination"
        )));
    }
    Ok(offset)
}

/// The absolute source anchor of `operation` drawn with `stroke`, in grid steps.
pub(crate) fn source_steps(stroke: &CorrectionStroke, operation: &Brush) -> [i32; 2] {
    let anchor = stroke.anchor();
    [
        anchor[0] + operation.offset[0],
        anchor[1] + operation.offset[1],
    ]
}

/// The absolute source anchor of `operation`, in normalized coordinates, exactly on the grid.
pub(crate) fn source(stroke: &CorrectionStroke, operation: &Brush) -> [f64; 2] {
    source_steps(stroke, operation).map(steps_to_units)
}

/// Refuse a destination footprint, or the source footprint `offset` moves it to, that does not
/// meet the content frame.
pub(crate) fn check_footprints(
    stroke: &CorrectionStroke,
    offset: [i32; 2],
    stage: Stage,
) -> Result<(), Error> {
    let destination = footprint(stroke, stage);
    if !meets_frame(destination) {
        return Err(Error::validation(
            "a correction's destination must meet the photograph's frame",
        ));
    }
    if !meets_frame(shifted(destination, offset)) {
        return Err(Error::validation(
            "a correction's source must meet the photograph's frame",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_footprint_grows_by_the_radius_in_content_heights() {
        let stroke = CorrectionStroke::capture(&[[0.5, 0.5]], 0.25).unwrap();
        let stage = Stage {
            width: 200,
            height: 100,
        };
        assert_eq!(footprint(&stroke, stage), [0.375, 0.25, 0.625, 0.75]);
        assert!(meets_frame(footprint(&stroke, stage)));
        assert!(!meets_frame(shifted(
            footprint(&stroke, stage),
            [quantize(0.7), 0]
        )));
        let off = CorrectionStroke::capture(&[[-0.3, 0.5]], 0.25).unwrap();
        assert!(!meets_frame(footprint(&off, stage)));
        assert_eq!(
            check_footprints(&off, [100, 0], stage).unwrap_err().detail,
            "a correction's destination must meet the photograph's frame"
        );
    }

    /// Whether the source footprint `offset` puts `points` at lies inside the frame, a stage pixel
    /// in from each edge.
    fn inside(points: &[[f64; 2]], radius: f64, offset: [i32; 2], stage: Stage) -> bool {
        let stroke = CorrectionStroke::capture(points, radius).unwrap();
        let [x0, y0, x1, y1] = shifted(footprint(&stroke, stage), offset);
        let (mx, my) = (1.0 / f64::from(stage.width), 1.0 / f64::from(stage.height));
        x0 >= mx && y0 >= my && x1 <= 1.0 - mx && y1 <= 1.0 - my
    }

    /// A press in each corner carrying an offset that points out of the frame, as the window's set
    /// source does, gets a source inside it: turned to the other side where that side holds it,
    /// moved inward where it does not, and kept where it already fits. Clamping moves only the
    /// axes that leave, just far enough.
    #[test]
    fn a_source_in_each_corner_is_placed_inside_the_frame() {
        for stage in [
            Stage {
                width: 6000,
                height: 4000,
            },
            Stage {
                width: 480,
                height: 320,
            },
            Stage {
                width: 3000,
                height: 4500,
            },
        ] {
            for (x, y, dx, dy) in [
                (0.0, 0.0, 1.0, 1.0),
                (1.0, 0.0, -1.0, 1.0),
                (0.0, 1.0, 1.0, -1.0),
                (1.0, 1.0, -1.0, -1.0),
            ] {
                for inset in [0.0, 0.03, 0.08] {
                    let at = [x + dx * inset, y + dy * inset];
                    for (points, radius) in [
                        (vec![at], 0.03),
                        (
                            vec![[at[0] + dx * 0.15, at[1]], at, [at[0], at[1] + dy * 0.1]],
                            0.02,
                        ),
                    ] {
                        let stroke = CorrectionStroke::capture(&points, radius).unwrap();
                        let fp = footprint(&stroke, stage);
                        // Outward on both axes, by a little more than the footprint.
                        let out = [-dx, -dy].map(|d| quantize(d * 0.12));
                        let turned = fit_offset(fp, out, stage, SourceFit::Turn);
                        assert!(
                            inside(&points, radius, turned, stage),
                            "{stage:?} {points:?} turned {turned:?}"
                        );
                        if inset >= 0.03 {
                            // Room on the far side: the offset keeps its length, mirrored.
                            assert_eq!(turned, [-out[0], -out[1]], "{stage:?} {points:?}");
                        }
                        let clamped = fit_offset(fp, out, stage, SourceFit::Clamp);
                        assert!(
                            inside(&points, radius, clamped, stage),
                            "{stage:?} {points:?} clamped {clamped:?}"
                        );
                        // Just inside: on an axis it moved, one step back towards the asked offset
                        // leaves the frame.
                        for axis in 0..2 {
                            if clamped[axis] != out[axis] {
                                let mut further = clamped;
                                further[axis] += (out[axis] - clamped[axis]).signum();
                                assert!(
                                    !inside(&points, radius, further, stage),
                                    "{stage:?} {points:?} axis {axis}"
                                );
                            }
                        }
                        // An offset that fits is its own answer.
                        assert_eq!(fit_offset(fp, turned, stage, SourceFit::Clamp), turned);
                        assert_eq!(fit_offset(fp, clamped, stage, SourceFit::Turn), clamped);
                        // Through the public form, on the grid and at the same place.
                        let anchor = stroke.anchor();
                        let sent = [anchor[0] + out[0], anchor[1] + out[1]].map(steps_to_units);
                        let points: Vec<[f64; 2]> = stroke.points().collect();
                        assert_eq!(
                            fit_correction_source(
                                &points,
                                radius,
                                sent,
                                (stage.width, stage.height),
                                SourceFit::Turn
                            ),
                            Some(
                                [anchor[0] + turned[0], anchor[1] + turned[1]].map(steps_to_units)
                            )
                        );
                    }
                }
            }
        }
        // A footprint wider than the frame keeps only its part inside the frame inside: a stroke
        // across the whole width can take a source above or below it, not beside it.
        let stage = Stage {
            width: 600,
            height: 400,
        };
        let across = CorrectionStroke::capture(&[[-0.1, 0.5], [1.1, 0.5]], 0.05).unwrap();
        let fitted = fit_offset(
            footprint(&across, stage),
            [quantize(0.2), quantize(0.2)],
            stage,
            SourceFit::Clamp,
        );
        assert_eq!(fitted, [0, quantize(0.2)]);
    }

    #[test]
    fn an_absolute_source_is_stored_as_an_exact_offset() {
        let stroke = CorrectionStroke::capture(&[[0.5, 0.5], [0.6, 0.5]], 0.02).unwrap();
        let offset = offset_to(&stroke, [0.4, 0.35]).unwrap();
        assert_eq!(offset, [quantize(0.4) - 8192, quantize(0.35) - 8192]);
        assert!(offset_to(&stroke, [0.5, 0.5]).is_err());
        assert!(checked_offset([49153, 0]).is_err());
        assert!(checked_offset([-49152, 1]).is_ok());
    }
}
