//! The preview check for panoramas (P5; `docs/design/catalog.md`, "Grouping inside a view"): how
//! far the view moved from one frame to the next, measured on the two frames' grid fingerprints
//! ([`Fingerprint`]), for organizing to tell a sweep across a scene from a burst of one view.
//!
//! - **What it compares.** Each frame's [`GRID`] × [`GRID`] cells of mean log luminance, the
//!   clipped and dark ones left out. Two frames are compared at a shift by a correlation over the
//!   cells usable in both where the shifted grids overlap, each side less its rows' and its
//!   columns' means there, and normalized: an exposure step, which adds to every log, and a tone
//!   curve's contrast, which scales them, change nothing, and neither does a horizon, the largest
//!   structure of most scenes, which would otherwise correlate at every shift along it and match
//!   any other horizon across it. A comparison needs [`MIN_OVERLAP`] cells and
//!   [`MIN_STRUCTURE_EV`] of structure left on each side.
//! - **Where it looks.** A pan moves the view along one axis by [`MIN_SHIFT`] to [`MAX_SHIFT`]
//!   cells (12.5% to 75% of the frame, so the frames overlap by a quarter or more), drifting at
//!   most [`DRIFT`] cells across it, as a hand-held sweep does. Every such shift of both axes is
//!   compared first on the grids halved to 12 × 12, and the [`CANDIDATES`] best peaks of at least
//!   [`COARSE_MIN`] (each
//!   direction's best correlation over its drifts, at a shift better than its neighbours along it)
//!   are compared on the whole grid at each shift within a cell of theirs along and a halved cell
//!   across.
//! - **What it answers.** The best shift is a pan when its correlation is at least [`MATCH_MIN`]
//!   and beats by [`SAME_VIEW_MARGIN`] the frames' best correlation at every shift short of a pan
//!   (under [`MIN_SHIFT`] each way): a burst of one view or a drift, a scene that repeats itself
//!   along the pan, and one with too little structure to place it (fog, an even sky, which
//!   correlate nearly as well at any shift), answer `None`.
//!
//! The work is `O(cells)` per shift with every array on the stack: one whole shift for frames of one
//! view, which correlate too well unmoved for any pan to beat; otherwise 96 halved shifts, at most
//! 60 whole ones, and for a pan 25 more short of one. The probe keeps each pair's answer
//! ([`super::PanCache`]), so a view evaluated again searches nothing it searched before.
use super::{CELLS, Fingerprint, GRID};
use crate::catalog_types::Pan;

/// The least shift a pan moves, in cells: 3 of 24, an eighth of the frame. Less is a burst's
/// hand-held drift or a subject reframed.
const MIN_SHIFT: i32 = 3;
/// The most, in cells: 18 of 24, so the frames overlap by a quarter of the frame or more, as a
/// panorama's frames are shot to.
const MAX_SHIFT: i32 = 18;
/// How far a pan may drift across its axis, in cells either way: 8% of the frame.
const DRIFT: i32 = 2;
/// A comparison needs at least this many cells usable in both frames where they overlap: a sixth
/// of the grid.
const MIN_OVERLAP: u32 = 96;
/// Each side of a comparison needs at least this standard deviation of log luminance less its rows'
/// and columns' means over the cells compared, in EV: less (fog, an even sky, a horizon alone)
/// cannot place a shift.
const MIN_STRUCTURE_EV: f64 = 0.1;
/// The most halved shifts compared on the whole grid…
const CANDIDATES: usize = 4;
/// …each correlating at least this on the halved grids: a weaker one does not become a match.
const COARSE_MIN: f32 = 0.5;
/// The best shift is a pan when its correlation is at least this…
const MATCH_MIN: f32 = 0.85;
/// …and it beats the best at every shift short of a pan by this.
const SAME_VIEW_MARGIN: f32 = 0.15;

/// The halved grid's side.
const HALF: usize = GRID / 2;

/// A frame's cells' log luminance on a grid of `side` cells a side, `NaN` where a cell is
/// unusable.
struct Logs<const N: usize> {
    side: i32,
    cells: [f32; N],
}

impl Logs<CELLS> {
    fn of(print: &Fingerprint) -> Self {
        Self {
            side: GRID as i32,
            cells: std::array::from_fn(|cell| print.log(cell).unwrap_or(f32::NAN)),
        }
    }

    /// The grid halved: each cell the mean of the usable ones of its four, unusable when fewer
    /// than two are.
    fn halved(&self) -> Logs<{ HALF * HALF }> {
        Logs {
            side: HALF as i32,
            cells: std::array::from_fn(|cell| {
                let (x, y) = (cell % HALF * 2, cell / HALF * 2);
                let (mut sum, mut count) = (0.0, 0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let log = self.cells[(y + dy) * GRID + x + dx];
                    if log.is_finite() {
                        sum += log;
                        count += 1;
                    }
                }
                if count >= 2 {
                    sum / count as f32
                } else {
                    f32::NAN
                }
            }),
        }
    }
}

impl<const N: usize> Logs<N> {
    fn at(&self, x: i32, y: i32) -> f32 {
        self.cells[(y * self.side + x) as usize]
    }
}

/// The correlation of `a`'s cell `(x, y)` with `b`'s cell `(x − dx, y − dy)` over the overlap,
/// the second frame's view moved by `(dx, dy)` cells, each side less its rows' and its columns'
/// means there (over the cells usable in both); `None` with fewer than `min_overlap` cells or too
/// little structure.
fn correlate<const N: usize>(
    a: &Logs<N>,
    b: &Logs<N>,
    (dx, dy): (i32, i32),
    min_overlap: u32,
) -> Option<f32> {
    // The overlap's cells usable in both, by row and column of the overlap: sums of each side and
    // counts.
    let mut rows = [(0.0f64, 0.0f64, 0u32); GRID];
    let mut columns = [(0.0f64, 0.0f64, 0u32); GRID];
    let (mut total_a, mut total_b, mut n) = (0.0f64, 0.0f64, 0u32);
    overlap(a, b, (dx, dy), |row, column, p, q| {
        for line in [&mut rows[row], &mut columns[column]] {
            line.0 += p;
            line.1 += q;
            line.2 += 1;
        }
        total_a += p;
        total_b += q;
        n += 1;
    });
    if n < min_overlap {
        return None;
    }
    // Each line's means, less half the overlap's, so a cell less its row's and its column's means
    // plus the overlap's is a subtraction of two.
    let means = |lines: &[(f64, f64, u32); GRID], half_a: f64, half_b: f64| {
        lines.map(|(a, b, count)| {
            let count = f64::from(count.max(1));
            (a / count - half_a, b / count - half_b)
        })
    };
    let (half_a, half_b) = (total_a / f64::from(n) / 2.0, total_b / f64::from(n) / 2.0);
    let (rows, columns) = (
        means(&rows, half_a, half_b),
        means(&columns, half_a, half_b),
    );
    let (mut aa, mut bb, mut ab) = (0.0f64, 0.0f64, 0.0f64);
    overlap(a, b, (dx, dy), |row, column, p, q| {
        let (r, c) = (rows[row], columns[column]);
        let p = p - r.0 - c.0;
        let q = q - r.1 - c.1;
        aa += p * p;
        bb += q * q;
        ab += p * q;
    });
    let floor = MIN_STRUCTURE_EV * MIN_STRUCTURE_EV * f64::from(n);
    (aa >= floor && bb >= floor).then(|| (ab / (aa * bb).sqrt()) as f32)
}

/// Visit each cell of the overlap of `a` and `b` at a shift of `(dx, dy)` that is usable in both:
/// its row and column in the overlap, and both logs.
#[inline]
fn overlap<const N: usize>(
    a: &Logs<N>,
    b: &Logs<N>,
    (dx, dy): (i32, i32),
    mut visit: impl FnMut(usize, usize, f64, f64),
) {
    let side = a.side;
    for (row, y) in (dy.max(0)..(side + dy).min(side)).enumerate() {
        for (column, x) in (dx.max(0)..(side + dx).min(side)).enumerate() {
            let (p, q) = (a.at(x, y), b.at(x - dx, y - dy));
            if p.is_finite() && q.is_finite() {
                visit(row, column, f64::from(p), f64::from(q));
            }
        }
    }
}

/// A shift of `along` cells along a pan's axis and `drift` across it, as `(dx, dy)`.
fn shift(across: bool, along: i32, drift: i32) -> (i32, i32) {
    if across {
        (along, drift)
    } else {
        (drift, along)
    }
}

/// What searching two frames for a pan found: the best correlation, at its shift in cells (`NaN`
/// when the frames correlate too well unmoved to need it), and the best at every shift short of a
/// pan (`NaN` when the best is too weak a pan to need it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Search {
    pub correlation: f32,
    pub dx: i32,
    pub dy: i32,
    pub same_view: f32,
}

/// The pan from `a` to `b`, when their fingerprints show `b`'s view clearly moved along one axis
/// and still overlapping `a`'s; see the module documentation.
pub(super) fn pan(a: &Fingerprint, b: &Fingerprint) -> Option<Pan> {
    search(a, b)
        .filter(|found| {
            found.correlation >= MATCH_MIN
                && found.correlation - found.same_view >= SAME_VIEW_MARGIN
        })
        .map(|found| Pan {
            across: found.dx as f32 / GRID as f32,
            down: found.dy as f32 / GRID as f32,
        })
}

/// The best pan shift between `a` and `b` and the same-view correlation to judge it by, when the
/// frames are of one shape and some shift could be compared.
pub(super) fn search(a: &Fingerprint, b: &Fingerprint) -> Option<Search> {
    if !a.same_shape(b) {
        return None;
    }
    let (a, b) = (Logs::of(a), Logs::of(b));
    // Frames that correlate this well unmoved can show no pan the margin admits: a burst or a
    // bracket of one view, answered after one comparison.
    let unmoved = correlate(&a, &b, (0, 0), MIN_OVERLAP).unwrap_or(-1.0);
    if unmoved > 1.0 - SAME_VIEW_MARGIN {
        return Some(Search {
            correlation: f32::NAN,
            dx: 0,
            dy: 0,
            same_view: unmoved,
        });
    }
    let (half_a, half_b) = (a.halved(), b.halved());
    // The best halved shifts, best first: correlation, the shift along in whole cells, the axis.
    // Each direction's best correlation at each shift along it, over every drift, counts only at
    // a peak of that curve, so the rise towards no shift that any smooth scene shows takes one
    // place, not all of them.
    let (least, most) = ((MIN_SHIFT + 1) / 2, MAX_SHIFT / 2);
    // Correlation, the shift along and its drift in whole cells, the axis.
    let mut candidates = [(f32::NEG_INFINITY, 0, 0, true); CANDIDATES];
    let halved_drift = (DRIFT + 1) / 2;
    for across in [true, false] {
        for sign in [1, -1] {
            // The best correlation at each shift along, and its drift.
            let mut curve = [(f32::NEG_INFINITY, 0); HALF];
            for along in least..=most {
                for drift in -halved_drift..=halved_drift {
                    let halved = shift(across, sign * along, drift);
                    if let Some(r) = correlate(&half_a, &half_b, halved, MIN_OVERLAP / 4)
                        && r > curve[along as usize].0
                    {
                        curve[along as usize] = (r, drift);
                    }
                }
            }
            for along in least..=most {
                let (r, drift) = curve[along as usize];
                let peak = r.is_finite()
                    && (along == least || r >= curve[along as usize - 1].0)
                    && (along == most || r >= curve[along as usize + 1].0);
                if !peak {
                    continue;
                }
                if let Some(at) = candidates.iter().position(|(top, ..)| r > *top) {
                    candidates.copy_within(at..CANDIDATES - 1, at + 1);
                    candidates[at] = (r, sign * along * 2, drift * 2, across);
                }
            }
        }
    }
    // The best whole shift within a cell of a candidate's along and a halved cell across.
    let mut best: Option<(f32, (i32, i32))> = None;
    for (_, along, drift, across) in candidates.into_iter().filter(|(r, ..)| *r >= COARSE_MIN) {
        for along in along - 1..=along + 1 {
            if !(MIN_SHIFT..=MAX_SHIFT).contains(&along.abs()) {
                continue;
            }
            for drift in (drift - 2).max(-DRIFT)..=(drift + 2).min(DRIFT) {
                let whole = shift(across, along, drift);
                if let Some(r) = correlate(&a, &b, whole, MIN_OVERLAP)
                    && best.is_none_or(|(top, ..)| r > top)
                {
                    best = Some((r, whole));
                }
            }
        }
    }
    let (correlation, (dx, dy)) = best?;
    if correlation < MATCH_MIN {
        // No pan, whatever the frames' correlation without one.
        return Some(Search {
            correlation,
            dx,
            dy,
            same_view: f32::NAN,
        });
    }
    let still = MIN_SHIFT - 1;
    let same_view = (-still..=still)
        .flat_map(|dy| (-still..=still).map(move |dx| (dx, dy)))
        .filter_map(|still| correlate(&a, &b, still, MIN_OVERLAP))
        .fold(-1.0, f32::max);
    Some(Search {
        correlation,
        dx,
        dy,
        same_view,
    })
}
