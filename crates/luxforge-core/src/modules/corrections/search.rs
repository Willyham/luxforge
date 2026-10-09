//! The automatic source search's adapter: the bounded region a search of one destination reads,
//! and the ranking over that read. The candidate set, rejection rules, score, colour domain and
//! tie-break are the numerics study's (`docs/design/corrections-numerics.md`), whose unit ranks
//! the read; this file plans the read so it covers every candidate the frame admits, so a search
//! over the region ranks exactly what a search over the whole stage would.
use super::stroke::CorrectionStroke;
use crate::{Error, Region, Stage, tiles::region::RegionRead};

/// The candidate rings' farthest distance, in multiples of the footprint's extent.
const FARTHEST_RING: f64 = 4.5;
/// The largest footprint, across, the search reads at, in reduced pixels.
const FOOTPRINT_ACROSS: f64 = 64.0;
/// The ring's width as a share of the reduced radius, and its least width in reduced pixels.
const RING_SHARE: f64 = 0.25;
const RING_MIN: f64 = 2.0;
/// The largest reduction the search reads at.
const MAX_REDUCTION: u32 = 1 << 14;

/// One ranked source: its offset from the destination's anchor in grid steps, and its score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ranked {
    pub offset: [i32; 2],
    pub score: f64,
}

/// What one search reads: the stage rectangle and the reduction it is read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Planned {
    pub rect: Region,
    pub reduction: u32,
}

/// The destination's points in mask space (`u = x · W/H`, `v = y`) and their bounding box.
fn bounds(stroke: &CorrectionStroke, stage: Stage) -> [f64; 4] {
    let aspect = f64::from(stage.width) / f64::from(stage.height);
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for [x, y] in stroke.points() {
        let (u, v) = (x * aspect, y);
        bounds = [
            bounds[0].min(u),
            bounds[1].min(v),
            bounds[2].max(u),
            bounds[3].max(v),
        ];
    }
    bounds
}

/// The footprint's extent `E` in content heights: half the diagonal of the path's bounding box
/// in mask space plus the radius (a spot's is its radius).
pub(crate) fn extent(stroke: &CorrectionStroke, stage: Stage) -> f64 {
    let [u0, v0, u1, v1] = bounds(stroke, stage);
    0.5 * (u1 - u0).hypot(v1 - v0) + stroke.radius()
}

/// The reduction the search reads at: the least power of two `r ≥ 1` with `2 E H / r ≤ 64`, so the
/// footprint is at most 64 reduced pixels across and never read above full resolution.
pub(crate) fn reduction(extent: f64, stage: Stage) -> u32 {
    let across = 2.0 * extent * f64::from(stage.height);
    let mut reduction = 1;
    while across / f64::from(reduction) > FOOTPRINT_ACROSS && reduction < MAX_REDUCTION {
        reduction *= 2;
    }
    reduction
}

/// The region one search of `stroke` reads on a stage of `stage`'s size: every reduced pixel the
/// destination's grown footprint and ring reach, moved by every candidate offset up to the
/// farthest ring, clipped to the stage. A candidate whose read would leave it is one the frame
/// refuses anyway, so the search over this region and over the whole stage rank the same.
pub(crate) fn plan(stroke: &CorrectionStroke, stage: Stage) -> Planned {
    let extent = extent(stroke, stage);
    let reduction = reduction(extent, stage);
    let scale = f64::from(stage.height) / f64::from(reduction);
    let radius = stroke.radius() * scale;
    let reach = radius + 1.0 + (RING_SHARE * radius).max(RING_MIN);
    let farthest = (FARTHEST_RING * extent * scale).ceil() + 1.0;
    let [u0, v0, u1, v1] = bounds(stroke, stage);
    let near = |value: f64| (value * scale - reach).floor() - 1.0 - farthest;
    let far = |value: f64| (value * scale + reach).ceil() + 2.0 + farthest;
    let pixels =
        |value: f64, limit: u32| (value * f64::from(reduction)).clamp(0.0, f64::from(limit)) as u32;
    let (x0, y0) = (
        pixels(near(u0), stage.width),
        pixels(near(v0), stage.height),
    );
    let (x1, y1) = (pixels(far(u1), stage.width), pixels(far(v1), stage.height));
    Planned {
        rect: Region {
            x0,
            y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        },
        reduction,
    }
}

/// The ranked sources of `stroke` over `read`, best first, each offset exactly on the grid; empty
/// when no candidate survives the rejection rules. The ranking is the numerics study's search
/// unit's.
pub(crate) fn rank(
    read: &RegionRead,
    stroke: &CorrectionStroke,
    stage: Stage,
) -> Result<Vec<Ranked>, Error> {
    let _ = (read, stroke, stage);
    Err(Error::incompatible(
        "the automatic source search is not available in this build; send source-x and source-y",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_reads_its_footprint_at_most_64_reduced_pixels_across() {
        let stage = Stage {
            width: 6000,
            height: 4000,
        };
        // A 0.01-height spot is 80 px across: read at half resolution.
        let spot = CorrectionStroke::capture(&[[0.5, 0.5]], 0.01).unwrap();
        assert_eq!(extent(&spot, stage), spot.radius());
        assert_eq!(reduction(extent(&spot, stage), stage), 2);
        let small = CorrectionStroke::capture(&[[0.5, 0.5]], 0.002).unwrap();
        assert_eq!(reduction(extent(&small, stage), stage), 1);
        // The region covers the farthest ring on every side, inside the stage.
        let planned = plan(&spot, stage);
        let reach = 4.5 * extent(&spot, stage) * 4000.0;
        assert!(f64::from(planned.rect.width) > 2.0 * reach);
        assert!(planned.rect.x0 + planned.rect.width <= stage.width);
        // At the frame's corner it is clipped to the stage.
        let corner = CorrectionStroke::capture(&[[0.0, 0.0]], 0.01).unwrap();
        let clipped = plan(&corner, stage);
        assert_eq!((clipped.rect.x0, clipped.rect.y0), (0, 0));
    }
}
