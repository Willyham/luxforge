//! The automatic source (`docs/design/corrections-numerics.md`, "Automatic sources"): a
//! deterministic, model-free ranking of 96 candidate offsets around a destination, read from the
//! prefix before the operation reduced until the destination's footprint is at most 64 pixels
//! across.
//!
//! The production transcription of `extent`, `reduction`, `reduced`, `sets` and `search` in
//! `luxforge_reference::repair`, in the same order, so the ranking is bit-identical to the
//! reference for the same input. The search is split in two: [`plan`] reads no pixel and rejects
//! every candidate the frame or the destination rules out, answering `no-source` before any read,
//! and names the block-aligned region its survivors read; [`rank`] scores them over that region
//! reduced ([`reduce`]). Its cost is the reduction of that region plus, for each surviving
//! candidate, a pass over the destination's ring and footprint, at most a few thousand reduced
//! pixels each.
use super::{RepairFrame, footprint::segments};
use crate::{
    Cancel, Error,
    colour::srgb,
    modules::{Region, Stage},
};

/// The candidate rings, in multiples of the footprint's extent.
pub(crate) const RINGS: [f64; 4] = [1.5, 2.25, 3.25, 4.5];

/// Directions at `15°` steps from `+x` (right) towards `+y` (down).
pub(crate) const DIRECTIONS: usize = 24;

/// The largest footprint the search reads at, across, in reduced pixels.
pub(crate) const FOOTPRINT_ACROSS: f64 = 64.0;

/// The texture term's weight against the ring term.
pub(crate) const TEXTURE_WEIGHT: f64 = 1.0;

/// The distance penalty per multiple of the extent.
pub(crate) const DISTANCE_WEIGHT: f64 = 0.002;

/// The ring's width as a share of the reduced radius, and its least width in reduced pixels.
pub(crate) const RING_SHARE: f64 = 0.25;
pub(crate) const RING_MIN: f64 = 2.0;

/// The largest reduction, which no stage the editor admits needs.
const REDUCTION_MAX: u32 = 1 << 14;

/// Direction `j`'s unit vector, from square roots alone: the cosines of multiples of `15°` are
/// `0`, `(√6 − √2)/4`, `1/2`, `√2/2`, `√3/2`, `(√6 + √2)/4` and `1`, and a correctly rounded square
/// root makes the table the same on every platform.
pub(crate) fn direction(j: usize) -> [f64; 2] {
    let s2 = 2f64.sqrt();
    let s3 = 3f64.sqrt();
    let s6 = 6f64.sqrt();
    let first = [
        1.0,
        (s6 + s2) / 4.0,
        s3 / 2.0,
        s2 / 2.0,
        0.5,
        (s6 - s2) / 4.0,
        0.0,
    ];
    let k = j % DIRECTIONS;
    let (c, s) = (first[k % 6], first[6 - k % 6]);
    match k / 6 {
        0 => [c, s],
        1 => [-s, c],
        2 => [-c, -s],
        _ => [s, -c],
    }
}

/// The footprint's extent `E` in content heights: half the diagonal of the path's bounding box
/// plus the radius.
pub(crate) fn extent(points: &[[f64; 2]], radius: f64, stage: Stage) -> f64 {
    let aspect = f64::from(stage.width) / f64::from(stage.height);
    let (mut u0, mut v0, mut u1, mut v1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for [x, y] in points {
        let (u, v) = (x * aspect, *y);
        u0 = u0.min(u);
        v0 = v0.min(v);
        u1 = u1.max(u);
        v1 = v1.max(v);
    }
    let du = u1 - u0;
    let dv = v1 - v0;
    0.5 * (du * du + dv * dv).sqrt() + radius
}

/// The least power of two `r >= 1` with `2 E H / r <= 64`.
pub(crate) fn reduction(extent: f64, stage: Stage) -> u32 {
    let across = 2.0 * extent * f64::from(stage.height);
    let mut r = 1u32;
    while across / f64::from(r) > FOOTPRINT_ACROSS && r < REDUCTION_MAX {
        r *= 2;
    }
    r
}

/// A window of the prefix reduced by `r` and encoded: reduced pixel `(x, y)` of the stage's blocks
/// at `values[(y - y0) * width + (x - x0)]`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reduced {
    pub(crate) r: u32,
    pub(crate) x0: i64,
    pub(crate) y0: i64,
    pub(crate) width: i64,
    pub(crate) height: i64,
    pub(crate) values: Vec<[f64; 3]>,
}

impl Reduced {
    /// Nothing, for a plan with no survivor.
    pub(crate) fn empty(r: u32) -> Self {
        Self {
            r,
            x0: 0,
            y0: 0,
            width: 0,
            height: 0,
            values: Vec::new(),
        }
    }

    #[inline]
    fn at(&self, x: i64, y: i64) -> [f64; 3] {
        self.values[((y - self.y0) * self.width + (x - self.x0)) as usize]
    }
}

/// `region` of `frame` (whole `r x r` blocks of the stage, the last ones cut by the stage's edge)
/// reduced by `r`: each reduced pixel the mean of the stage pixels of its block, summed row by row,
/// left to right, in `f64`, then encoded per channel. Reserved before it is allocated.
pub(crate) fn reduce(
    frame: &RepairFrame<'_>,
    region: Region,
    r: u32,
    cancel: &Cancel,
) -> Result<Reduced, Error> {
    let stage = frame.stage;
    let aligned = region.x0.is_multiple_of(r)
        && region.y0.is_multiple_of(r)
        && (region.x1().is_multiple_of(r) || region.x1() == stage.width)
        && (region.y1().is_multiple_of(r) || region.y1() == stage.height);
    if !aligned || !frame.holds(region) {
        return Err(Error::internal(format!(
            "a search window {region:?} that is not whole blocks of {r} the frame holds"
        )));
    }
    let (w, h) = (
        i64::from(region.width.div_ceil(r)),
        i64::from(region.height.div_ceil(r)),
    );
    let mut values = Vec::new();
    values
        .try_reserve_exact((w * h) as usize)
        .map_err(|_| Error::resource_limit("the automatic source's reduced window"))?;
    for by in 0..h {
        cancel.check()?;
        for bx in 0..w {
            let mut sum = [0.0; 3];
            let mut n = 0usize;
            let (sx0, sy0) = (region.x0 + bx as u32 * r, region.y0 + by as u32 * r);
            for sy in sy0..(sy0 + r).min(region.y1()) {
                for sx in sx0..(sx0 + r).min(region.x1()) {
                    let p = frame.get(sx, sy);
                    for (s, v) in sum.iter_mut().zip(p) {
                        *s += f64::from(v);
                    }
                    n += 1;
                }
            }
            values.push(sum.map(|s| srgb::encode(s / n as f64)));
        }
    }
    Ok(Reduced {
        r,
        x0: i64::from(region.x0 / r),
        y0: i64::from(region.y0 / r),
        width: w,
        height: h,
        values,
    })
}

/// One ranked candidate: its ring and direction, its offset in stored normalized units (`x` a
/// fraction of the width, `y` of the height; the module puts it on the grid), its reduced delta
/// and its score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub(crate) ring: usize,
    pub(crate) direction: usize,
    pub(crate) offset: [f64; 2],
    pub(crate) delta: [i64; 2],
    pub(crate) score: f64,
}

/// What the search answers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Search {
    /// Every surviving candidate, best first: ascending score, then nearer ring, then smaller
    /// direction.
    Ranked(Vec<Candidate>),
    /// No candidate survived: the explicit `no-source` outcome.
    NoSource,
}

/// The destination's reduced-pixel sets, each sorted by `(y, x)`: the footprint grown by one
/// reduced pixel, the footprint and the ring.
#[derive(Clone, Debug)]
struct Sets {
    grown: Vec<[i64; 2]>,
    interior: Vec<[i64; 2]>,
    ring: Vec<[i64; 2]>,
}

fn sets(points: &[[f64; 2]], radius: f64, stage: Stage, r: u32) -> Sets {
    let segments = segments(points, stage);
    let h = f64::from(stage.height);
    let scale = h / f64::from(r);
    let rr = radius * scale;
    let rho = (RING_SHARE * rr).max(RING_MIN);
    let reach = rr + 1.0 + rho;
    let (mut u0, mut v0, mut u1, mut v1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for s in &segments {
        let [a, b, c, d] = s.bounds();
        u0 = u0.min(a);
        v0 = v0.min(b);
        u1 = u1.max(c);
        v1 = v1.max(d);
    }
    let x0 = (u0 * scale - reach).floor() as i64 - 1;
    let x1 = (u1 * scale + reach).ceil() as i64 + 1;
    let y0 = (v0 * scale - reach).floor() as i64 - 1;
    let y1 = (v1 * scale + reach).ceil() as i64 + 1;
    let mut found = Sets {
        grown: Vec::new(),
        interior: Vec::new(),
        ring: Vec::new(),
    };
    for y in y0..=y1 {
        for x in x0..=x1 {
            let u = (x as f64 + 0.5) * f64::from(r) / h;
            let v = (y as f64 + 0.5) * f64::from(r) / h;
            let mut nearest = f64::INFINITY;
            for segment in &segments {
                let d2 = segment.distance2(u, v);
                if d2 < nearest {
                    nearest = d2;
                }
            }
            let d = nearest.sqrt() * scale;
            if d <= rr {
                found.interior.push([x, y]);
            }
            if d <= rr + 1.0 {
                found.grown.push([x, y]);
            } else if d <= rr + 1.0 + rho {
                found.ring.push([x, y]);
            }
        }
    }
    found
}

fn member(set: &[[i64; 2]], p: [i64; 2]) -> bool {
    set.binary_search_by(|q| (q[1], q[0]).cmp(&(p[1], p[0])))
        .is_ok()
}

/// The RMS per-channel difference across horizontally and vertically adjacent pairs of `set`,
/// each read at its place moved by `shift`; zero for no pair.
fn energy(reduced: &Reduced, set: &[[i64; 2]], shift: [i64; 2]) -> f64 {
    let mut sum = 0.0;
    let mut n = 0usize;
    for &[x, y] in set {
        for next in [[x + 1, y], [x, y + 1]] {
            if member(set, next) {
                let a = reduced.at(x + shift[0], y + shift[1]);
                let b = reduced.at(next[0] + shift[0], next[1] + shift[1]);
                for c in 0..3 {
                    let d = a[c] - b[c];
                    sum += d * d;
                }
                n += 1;
            }
        }
    }
    if n == 0 {
        0.0
    } else {
        (sum / (3 * n) as f64).sqrt()
    }
}

/// The geometric half of the search, which reads no pixel: the reduction, the destination's sets,
/// every candidate that survives the frame and overlap rules in ring-then-direction order, and the
/// stage pixels the survivors' scores read. A destination no candidate survives for answers
/// `no-source` without reading anything.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) r: u32,
    sets: Sets,
    /// The ring inside the reduced stage.
    ring: Vec<[i64; 2]>,
    survivors: Vec<Candidate>,
    /// Whole `r x r` blocks of the stage holding every reduced pixel the scores read.
    pub(crate) region: Region,
}

/// Plan the search for the destination `points` at `radius` on `stage`.
///
/// For ring `i` and direction `j`: `o = RINGS[i] * E * direction(j)` in mask space and
/// `delta = (round(o_u H / r), round(o_v H / r))` reduced pixels. A candidate is rejected when the
/// bounding box of `F+` and the ring's in-frame part `Q'`, moved by `delta`, leaves the reduced
/// stage, or when `F+` moved by `delta` meets `F+`.
pub(crate) fn plan(points: &[[f64; 2]], radius: f64, stage: Stage) -> Plan {
    let e = extent(points, radius, stage);
    let r = reduction(e, stage);
    let scale = f64::from(stage.height) / f64::from(r);
    let found = sets(points, radius, stage, r);
    let frame = [
        0,
        0,
        i64::from(stage.width.div_ceil(r)),
        i64::from(stage.height.div_ceil(r)),
    ];
    let inside =
        |p: [i64; 2]| p[0] >= frame[0] && p[1] >= frame[1] && p[0] < frame[2] && p[1] < frame[3];
    let ring: Vec<[i64; 2]> = found.ring.iter().copied().filter(|p| inside(*p)).collect();
    let (mut bx0, mut by0, mut bx1, mut by1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for p in found.grown.iter().chain(&ring) {
        bx0 = bx0.min(p[0]);
        by0 = by0.min(p[1]);
        bx1 = bx1.max(p[0]);
        by1 = by1.max(p[1]);
    }
    let aspect = f64::from(stage.width) / f64::from(stage.height);
    let mut survivors = Vec::new();
    let mut reads: Option<[i64; 4]> = None;
    let mut include = |x0: i64, y0: i64, x1: i64, y1: i64| {
        reads = Some(match reads {
            None => [x0, y0, x1, y1],
            Some([a, b, c, d]) => [a.min(x0), b.min(y0), c.max(x1), d.max(y1)],
        });
    };
    for (i, m) in RINGS.iter().enumerate() {
        for j in 0..DIRECTIONS {
            let [cu, cv] = direction(j);
            let ou = m * e * cu;
            let ov = m * e * cv;
            let delta = [(ou * scale).round() as i64, (ov * scale).round() as i64];
            if bx0 + delta[0] < frame[0]
                || by0 + delta[1] < frame[1]
                || bx1 + delta[0] >= frame[2]
                || by1 + delta[1] >= frame[3]
            {
                continue;
            }
            if found
                .grown
                .iter()
                .any(|p| member(&found.grown, [p[0] + delta[0], p[1] + delta[1]]))
            {
                continue;
            }
            include(
                bx0 + delta[0],
                by0 + delta[1],
                bx1 + delta[0],
                by1 + delta[1],
            );
            survivors.push(Candidate {
                ring: i,
                direction: j,
                offset: [ou / aspect, ov],
                delta,
                score: 0.0,
            });
        }
    }
    let region = match reads {
        None => Region::EMPTY,
        Some([x0, y0, x1, y1]) => {
            // The destination's ring is read too.
            let (x0, y0, x1, y1) = ring.iter().fold((x0, y0, x1, y1), |(a, b, c, d), p| {
                (a.min(p[0]), b.min(p[1]), c.max(p[0]), d.max(p[1]))
            });
            let (x0, y0) = (x0.max(0) as u32 * r, y0.max(0) as u32 * r);
            let x1 = ((x1 + 1) as u32 * r).min(stage.width);
            let y1 = ((y1 + 1) as u32 * r).min(stage.height);
            Region {
                x0,
                y0,
                width: x1 - x0,
                height: y1 - y0,
            }
        }
    };
    Plan {
        r,
        sets: found,
        ring,
        survivors,
        region,
    }
}

/// The scores of `plan`'s survivors over `reduced` (its `region` reduced by `plan.r`), ranked:
///
/// ```text
/// A = sqrt(sum over q in Q', c of (J_c(q) - J_c(q + delta))^2 / (3 |Q'|))     -- 0 for empty Q'
/// T = |energy(F moved by delta) - energy(Q')|
/// score = A + T + 0.002 RINGS[i]
/// ```
///
/// best first by score, then nearer ring, then smaller direction; `no-source` when nothing
/// survived.
pub(crate) fn rank(plan: &Plan, reduced: &Reduced) -> Search {
    if plan.survivors.is_empty() {
        return Search::NoSource;
    }
    let ring = &plan.ring;
    let ring_energy = energy(reduced, ring, [0, 0]);
    let mut ranked = plan.survivors.clone();
    for candidate in &mut ranked {
        let delta = candidate.delta;
        let mut sum = 0.0;
        for q in ring {
            let a = reduced.at(q[0], q[1]);
            let b = reduced.at(q[0] + delta[0], q[1] + delta[1]);
            for c in 0..3 {
                let d = a[c] - b[c];
                sum += d * d;
            }
        }
        let a = if ring.is_empty() {
            0.0
        } else {
            (sum / (3 * ring.len()) as f64).sqrt()
        };
        let t = (energy(reduced, &plan.sets.interior, delta) - ring_energy).abs();
        candidate.score = a + TEXTURE_WEIGHT * t + DISTANCE_WEIGHT * RINGS[candidate.ring];
    }
    ranked.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then(a.ring.cmp(&b.ring))
            .then(a.direction.cmp(&b.direction))
    });
    Search::Ranked(ranked)
}
