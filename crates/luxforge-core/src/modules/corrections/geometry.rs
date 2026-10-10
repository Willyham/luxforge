//! Where a correction lies in the content stage: its footprint and its source's, in the stored
//! normalized coordinates (`x` a fraction of the content stage's width, `y` of its height, the frame
//! `[0, 1]` on both), and the absolute source position the API speaks in.
use super::{
    payload::{OFFSET_MAX, Operation},
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
    let aspect = f64::from(stage.width) / f64::from(stage.height);
    let radius = stroke.radius();
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for [x, y] in stroke.points() {
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
pub(crate) fn source_steps(stroke: &CorrectionStroke, operation: &Operation) -> [i32; 2] {
    let anchor = stroke.anchor();
    [
        anchor[0] + operation.offset[0],
        anchor[1] + operation.offset[1],
    ]
}

/// The absolute source anchor of `operation`, in normalized coordinates, exactly on the grid.
pub(crate) fn source(stroke: &CorrectionStroke, operation: &Operation) -> [f64; 2] {
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
