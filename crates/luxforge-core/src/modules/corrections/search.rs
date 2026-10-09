//! The automatic source, as the module asks it: the numerics study's search unit
//! ([`crate::repair::search`]) planned without a pixel, its region read through the stage questions
//! ([`crate::StageQuestions::region_before`]) and ranked over that read. The read's block means are
//! the unit's own reduction — the same blocks, summed in the same order — so the module encodes
//! them exactly as [`crate::repair::search::reduce`] does and the ranking is the unit's, bit for
//! bit.
use super::stroke::CorrectionStroke;
use crate::{
    Error, Stage,
    colour::srgb,
    path::quantize,
    repair::search::{self, Plan, Reduced, Search},
    tiles::region::RegionRead,
};

/// One ranked source: its offset from the destination's anchor in grid steps, and its score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ranked {
    pub offset: [i32; 2],
    pub score: f64,
}

/// The search's geometric half for `stroke` on a content stage of `stage`'s size: the reduction,
/// the surviving candidates and the block-aligned region their scores read. Reads no pixel.
pub(crate) fn plan(stroke: &CorrectionStroke, stage: Stage) -> Plan {
    let points: Vec<[f64; 2]> = stroke.points().collect();
    search::plan(&points, stroke.radius(), stage)
}

/// The ranked sources of `plan` over `read`, its region read at its reduction, best first, each
/// offset put on the grid; empty when no candidate survives.
pub(crate) fn rank(plan: &Plan, read: &RegionRead) -> Result<Vec<Ranked>, Error> {
    if read.rect != plan.region || read.reduction != plan.r {
        return Err(Error::internal(
            "the automatic source was read over another region than its plan names",
        ));
    }
    let reduced = Reduced {
        r: read.reduction,
        x0: i64::from(read.rect.x0 / read.reduction),
        y0: i64::from(read.rect.y0 / read.reduction),
        width: i64::from(read.size[0]),
        height: i64::from(read.size[1]),
        values: read
            .linear
            .iter()
            .map(|mean| mean.map(srgb::encode))
            .collect(),
    };
    Ok(match search::rank(plan, &reduced) {
        Search::NoSource => Vec::new(),
        Search::Ranked(candidates) => candidates
            .into_iter()
            .map(|candidate| Ranked {
                offset: candidate.offset.map(quantize),
                score: candidate.score,
            })
            .collect(),
    })
}
