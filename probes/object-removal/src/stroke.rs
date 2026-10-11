//! Turning a painted stroke into prompts, scoring what the decoder returns, and deciding between
//! "remove the one object", "choose among several" and "nothing found". Also the hover pick.
//!
//! Coverage and area are counted on a working raster over the logits grid: each logit cell is
//! split into `k × k` sub-cells so that one sub-cell spans at most about four source pixels. A
//! sub-cell is inside a candidate when the bilinearly interpolated logit at its centre is above 0
//! (the same interpolation as `Logits::sample`), and inside the stroke when its centre lies within
//! the brush radius of the polyline. Areas are sub-cell counts times the sub-cell's area in source
//! pixels. A 2 × 2 patch of cells whose four logits share a sign is counted whole, so the cost is
//! the number of cells plus the sub-cells along each candidate's edge.

use std::borrow::Cow;

use crate::mask::{Logits, Point};
use crate::segment::{Candidate, Family, Prompt};

/// A painted stroke: a polyline in source pixels swept by a round brush.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub points: Vec<Point>,
    pub radius: f32,
}

/// Starting values, to be tuned on the probe corpus; none is a published constant.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Positive points sampled along the stroke, each also decoded as its own prompt.
    pub sample_points: usize,
    /// Candidates below this predicted IoU are dropped, except a whole (see `min_whole_iou`).
    pub min_predicted_iou: f32,
    /// A candidate that contains a candidate clearing every floor is kept down to this predicted
    /// IoU, and the hover pick's whole must reach it: a prompt on a part makes SAM 2.1 score the
    /// part above the whole (clothes 0.80, the whole boy 0.61).
    pub min_whole_iou: f32,
    /// Candidates below this stability score (logit offset ±1) are dropped.
    pub min_stability: f32,
    /// Mask-IoU above which two candidates are the same object.
    pub nms_iou: f32,
    /// A single candidate explaining at least this share of the stroke is "the one object".
    pub one_object_stroke_coverage: f32,
    /// A candidate the stroke touches for less than this share of its own area is not a target.
    pub min_object_coverage: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            sample_points: 4,
            min_predicted_iou: 0.7,
            min_whole_iou: 0.5,
            min_stability: 0.85,
            nms_iou: 0.7,
            one_object_stroke_coverage: 0.7,
            min_object_coverage: 0.05,
        }
    }
}

impl Params {
    /// The starting values for a segmenter family. SAM 2.1 scores small objects lower than
    /// EfficientViT-SAM does: on the corpus it found the kayaks at predicted IoU 0.65 to 0.70 and a
    /// walker behind the boy at 0.61, so its floor is 0.6. At 0.6 two of its nine corpus strokes
    /// went from nothing to the right several objects and none of the rest changed; at 0.65 the
    /// kayaks were still missed. EfficientViT-SAM's decisions did not change at 0.6 either, so it
    /// keeps the default.
    pub fn for_family(family: Family) -> Params {
        match family {
            Family::EfficientVitL0 => Params::default(),
            Family::Sam21Small => Params {
                min_predicted_iou: 0.6,
                ..Params::default()
            },
        }
    }
}

/// One candidate's scores. `prompt` and `candidate` index the decoder results passed to `decide`.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub prompt: usize,
    pub candidate: usize,
    pub predicted_iou: f32,
    pub stability: f32,
    /// Share of the stroke's area inside the candidate.
    pub stroke_coverage: f32,
    /// Share of the candidate's area under the stroke.
    pub object_coverage: f32,
    /// Candidate area in source pixels.
    pub area: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// No candidate survived; the stroke is left as a plain painted region.
    Nothing,
    /// Remove this object (an index into `Resolution::scored`).
    One(usize),
    /// Several distinct objects: offer each for the person to choose (indices into `scored`).
    Several(Vec<usize>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    /// Surviving candidates after filtering and non-maximum suppression.
    pub scored: Vec<Scored>,
    pub decision: Decision,
}

/// The logit offset of the stability score, as SAM's automatic mask generator uses.
const STABILITY_OFFSET: f32 = 1.0;
/// Two survivors are distinct objects when they share less than this share of the smaller one.
const DISJOINT_OVERLAP: f64 = 0.1;
/// A candidate with at least this share of its area inside a larger one is nested in it: a part of
/// that whole.
const NESTED_SHARE: f64 = 0.9;
/// The working raster's sub-cells span at most this many source pixels.
const WORKING_CELL_PX: f64 = 4.0;
/// The stroke is sampled about once per source pixel of its length, at most this many times.
const MAX_STROKE_SAMPLES: usize = 4096;

/// The prompts for a stroke: first the stroke's padded bounding box, then one single positive
/// point per sampled stroke point.
///
/// The box is the points' bounds grown by the brush radius and clamped to the image. The points
/// are chosen by farthest-point sampling over the polyline sampled about once per source pixel,
/// seeded with the sample nearest the samples' centroid; ties go to the earlier sample. Sampling
/// stops early when every remaining sample coincides with a chosen one, so a single-point stroke
/// gives the box and one point. An empty stroke gives no prompts.
pub fn prompts(stroke: &Stroke, image: (u32, u32), params: &Params) -> Vec<Prompt> {
    let Some((lo, hi)) = point_bounds(&stroke.points) else {
        return Vec::new();
    };
    let (w, h) = (image.0 as f32, image.1 as f32);
    let r = stroke.radius.max(0.0);
    let clamp = |p: Point| Point::new(p.x.clamp(0.0, w), p.y.clamp(0.0, h));
    let mut out = vec![Prompt {
        bbox: Some((
            clamp(Point::new(lo.x - r, lo.y - r)),
            clamp(Point::new(hi.x + r, hi.y + r)),
        )),
        ..Prompt::default()
    }];
    let samples = dense_samples(&stroke.points);
    for p in farthest_points(&samples, params.sample_points) {
        out.push(Prompt {
            positive: vec![clamp(p)],
            ..Prompt::default()
        });
    }
    out
}

/// Scores the decoder's results for `prompts(stroke, …)` and decides.
///
/// Every candidate is scored on the working raster of the first well-formed candidate's logits; a
/// candidate from a differently shaped grid is resampled onto it. A candidate the stroke does not
/// touch is dropped, and so is one below `min_stability`, `min_object_coverage` or
/// `min_predicted_iou`, except that a whole is kept down to `min_whole_iou` when it contains (at
/// least 90 % of the area of) a smaller candidate that clears every floor. The rest are suppressed
/// greedily from the largest down: a candidate whose mask IoU with a kept one exceeds `nms_iou` is
/// a duplicate of that larger one. Ties order by predicted IoU, stability, then prompt and
/// candidate index. Survivors are listed in that order.
///
/// Parts yield to their wholes: a survivor with at least 90 % of its area inside a larger one is
/// that one's part.
///
/// - **One** when a survivor covers at least `one_object_stroke_coverage` of the stroke. An
///   explaining part yields to an explaining whole; among the rest the best object coverage wins,
///   then the larger, then the earlier.
/// - **Several** when no survivor explains the stroke but two or more are disjoint. Parts are set
///   aside, and the rest are chosen greedily by stroke coverage, each sharing less than 10 % of the
///   smaller one's area with every one already chosen, and listed by stroke coverage.
/// - **Nothing** otherwise.
pub fn decide(stroke: &Stroke, results: &[Vec<Candidate>], params: &Params) -> Resolution {
    let nothing = || Resolution {
        scored: Vec::new(),
        decision: Decision::Nothing,
    };
    let Some(reference) = results
        .iter()
        .flatten()
        .map(|c| &c.logits)
        .find(|l| well_formed(l))
    else {
        return nothing();
    };
    let grid = Grid::new(reference);
    let footprint = Footprint::new(stroke, &grid);
    if footprint.count == 0 {
        return nothing();
    }

    let mut touched: Vec<(Scored, Shape)> = Vec::new();
    for (prompt, candidates) in results.iter().enumerate() {
        for (index, candidate) in candidates.iter().enumerate() {
            let Some(shape) = Shape::new(&candidate.logits, reference, &grid) else {
                continue;
            };
            let under = grid.footprint_overlap(&shape, &footprint);
            if under == 0 {
                continue;
            }
            let scored = Scored {
                prompt,
                candidate: index,
                predicted_iou: candidate.predicted_iou,
                stability: stability(&candidate.logits, STABILITY_OFFSET),
                stroke_coverage: (under as f64 / footprint.count as f64) as f32,
                object_coverage: (under as f64 / shape.count as f64) as f32,
                area: shape.count as f64 * grid.cell_area(),
            };
            touched.push((scored, shape));
        }
    }

    // The floors, with the whole's lower IoU floor when it contains a candidate that clears them.
    let credible = |s: &Scored, iou: f32| {
        s.predicted_iou >= iou
            && s.stability >= params.min_stability
            && s.object_coverage >= params.min_object_coverage
    };
    let strong: Vec<bool> = touched
        .iter()
        .map(|(s, _)| credible(s, params.min_predicted_iou))
        .collect();
    let admitted: Vec<bool> = (0..touched.len())
        .map(|i| {
            strong[i]
                || (credible(&touched[i].0, params.min_whole_iou)
                    && (0..touched.len())
                        .any(|j| strong[j] && grid.contains(&touched[i].1, &touched[j].1)))
        })
        .collect();
    let mut pool: Vec<(Scored, Shape)> = touched
        .into_iter()
        .zip(admitted)
        .filter_map(|(t, keep)| keep.then_some(t))
        .collect();

    // Non-maximum suppression, largest first, so a near-duplicate always yields to the larger.
    pool.sort_by(|(a, sa), (b, sb)| {
        sb.count
            .cmp(&sa.count)
            .then(b.predicted_iou.total_cmp(&a.predicted_iou))
            .then(b.stability.total_cmp(&a.stability))
            .then(a.prompt.cmp(&b.prompt))
            .then(a.candidate.cmp(&b.candidate))
    });
    let mut kept: Vec<(Scored, Shape)> = Vec::new();
    for (scored, shape) in pool {
        let duplicate = kept.iter().any(|(_, k)| {
            let both = grid.overlap(&shape, k);
            let iou = both as f64 / (shape.count + k.count - both) as f64;
            iou > params.nms_iou as f64
        });
        if !duplicate {
            kept.push((scored, shape));
        }
    }

    let n = kept.len();
    let mut shared = vec![0u64; n * n];
    for i in 0..n {
        shared[i * n + i] = kept[i].1.count;
        for j in i + 1..n {
            let both = grid.overlap(&kept[i].1, &kept[j].1);
            shared[i * n + j] = both;
            shared[j * n + i] = both;
        }
    }
    let count = |i: usize| kept[i].1.count;
    // Survivors run largest first, so `j < i` means `j` is at least as large as `i`.
    let part_of =
        |i: usize, j: usize| j < i && shared[i * n + j] as f64 >= NESTED_SHARE * count(i) as f64;
    let scored: Vec<Scored> = kept.iter().map(|(s, _)| s.clone()).collect();

    let explains: Vec<usize> = (0..n)
        .filter(|&i| scored[i].stroke_coverage >= params.one_object_stroke_coverage)
        .collect();
    let one = explains
        .iter()
        .copied()
        .filter(|&i| !explains.iter().any(|&j| part_of(i, j)))
        .min_by(|&a, &b| {
            scored[b]
                .object_coverage
                .total_cmp(&scored[a].object_coverage)
                .then(count(b).cmp(&count(a)))
                .then(a.cmp(&b))
        });
    let decision = if let Some(best) = one {
        Decision::One(best)
    } else {
        let mut order: Vec<usize> = (0..n).filter(|&i| !(0..n).any(|j| part_of(i, j))).collect();
        order.sort_by(|&a, &b| {
            scored[b]
                .stroke_coverage
                .total_cmp(&scored[a].stroke_coverage)
                .then(count(b).cmp(&count(a)))
                .then(a.cmp(&b))
        });
        let mut chosen: Vec<usize> = Vec::new();
        for i in order {
            let disjoint = chosen.iter().all(|&j| {
                (shared[i * n + j] as f64) < DISJOINT_OVERLAP * count(i).min(count(j)) as f64
            });
            if disjoint {
                chosen.push(i);
            }
        }
        if chosen.len() >= 2 {
            Decision::Several(chosen)
        } else {
            Decision::Nothing
        }
    };
    Resolution { scored, decision }
}

/// For a single hover point: the candidate to highlight, biased toward the whole object.
///
/// The candidate with the best predicted IoU (the earlier on a tie), unless a larger candidate
/// contains at least 90 % of its area and reaches `min_whole_iou` and `min_stability`: then the
/// largest such whole, then the higher predicted IoU, then the earlier. A click on a part makes
/// SAM 2.1 score the part above the whole, so the whole is found by containment rather than by
/// IoU. `None` when no candidate has both a finite predicted IoU and a non-empty mask.
pub fn pick_hover(candidates: &[Candidate], params: &Params) -> Option<usize> {
    let reference = candidates
        .iter()
        .map(|c| &c.logits)
        .find(|l| well_formed(l))?;
    let grid = Grid::new(reference);
    let shapes: Vec<Option<Shape>> = candidates
        .iter()
        .map(|c| Shape::new(&c.logits, reference, &grid))
        .collect();
    let best = (0..candidates.len())
        .filter(|&i| shapes[i].is_some() && candidates[i].predicted_iou.is_finite())
        .max_by(|&a, &b| {
            candidates[a]
                .predicted_iou
                .total_cmp(&candidates[b].predicted_iou)
                .then(b.cmp(&a))
        })?;
    let part = shapes[best].as_ref()?;
    let mut pick = best;
    for (i, (c, shape)) in candidates.iter().zip(&shapes).enumerate() {
        let (Some(shape), Some(current)) = (shape, shapes[pick].as_ref()) else {
            continue;
        };
        let larger = shape.count > current.count
            || (shape.count == current.count && c.predicted_iou > candidates[pick].predicted_iou);
        if i == best || !larger {
            continue;
        }
        let whole = c.predicted_iou >= params.min_whole_iou
            && stability(&c.logits, STABILITY_OFFSET) >= params.min_stability
            && grid.contains(shape, part);
        if whole {
            pick = i;
        }
    }
    Some(pick)
}

/// SAM's stability score: the IoU of the mask thresholded at `logit > +offset` against the mask
/// at `logit > −offset`, over the logit cells whose centres lie inside the source image. The
/// offset's magnitude is used. An empty mask scores 0.
pub fn stability(logits: &Logits, offset: f32) -> f32 {
    let offset = offset.abs();
    let (vw, vh) = valid_cells(logits);
    let (mut strict, mut loose) = (0u64, 0u64);
    for row in logits.data.chunks_exact(logits.width.max(1)).take(vh) {
        for &v in &row[..vw] {
            strict += (v > offset) as u64;
            loose += (v > -offset) as u64;
        }
    }
    if loose == 0 {
        0.0
    } else {
        (strict as f64 / loose as f64) as f32
    }
}

// Logit geometry, shared with `refine`.

pub(crate) fn well_formed(l: &Logits) -> bool {
    l.width > 0
        && l.height > 0
        && l.data.len() == l.width * l.height
        && l.cells_per_px.0.is_finite()
        && l.cells_per_px.1.is_finite()
        && l.cells_per_px.0 > 0.0
        && l.cells_per_px.1 > 0.0
}

/// Cells along one axis whose centres lie inside the source, at least one for a non-empty source.
fn valid_along(cells: usize, cells_per_px: f32, source: u32) -> usize {
    if cells == 0 || source == 0 {
        return 0;
    }
    let n = (source as f64 * cells_per_px as f64 - 0.5).ceil().max(1.0);
    (n as usize).min(cells)
}

/// The logit cells whose centres lie inside the source image, from the top-left: SAM's automatic
/// mask generator likewise scores masks cropped to the image, so a padded frame's padding is out.
pub(crate) fn valid_cells(l: &Logits) -> (usize, usize) {
    if !well_formed(l) {
        return (0, 0);
    }
    (
        valid_along(l.width, l.cells_per_px.0, l.source.0),
        valid_along(l.height, l.cells_per_px.1, l.source.1),
    )
}

/// Inclusive cell bounds `(u0, v0, u1, v1)` of the logits above `threshold` among the cells that
/// can influence the image: the valid cells and the first row and column beyond them.
pub(crate) fn cells_above(l: &Logits, threshold: f32) -> Option<(usize, usize, usize, usize)> {
    let (vw, vh) = valid_cells(l);
    let limit = ((vw + 1).min(l.width), (vh + 1).min(l.height));
    bounds_above(&l.data, l.width, limit, threshold)
}

fn bounds_above(
    data: &[f32],
    width: usize,
    limit: (usize, usize),
    threshold: f32,
) -> Option<(usize, usize, usize, usize)> {
    let (mut u0, mut v0, mut u1, mut v1) = (usize::MAX, usize::MAX, 0, 0);
    for (v, row) in data.chunks_exact(width.max(1)).take(limit.1).enumerate() {
        for (u, &x) in row[..limit.0].iter().enumerate() {
            if x > threshold {
                u0 = u0.min(u);
                v0 = v0.min(v);
                u1 = u1.max(u);
                v1 = v1.max(v);
            }
        }
    }
    (u0 != usize::MAX).then_some((u0, v0, u1, v1))
}

fn same_geometry(a: &Logits, b: &Logits) -> bool {
    a.width == b.width
        && a.height == b.height
        && a.source == b.source
        && a.cells_per_px == b.cells_per_px
}

/// `l` sampled at the cell centres of `to`'s grid.
fn resample(l: &Logits, to: &Logits) -> Vec<f32> {
    let (cx, cy) = (to.cells_per_px.0 as f64, to.cells_per_px.1 as f64);
    let mut out = Vec::with_capacity(to.width * to.height);
    for v in 0..to.height {
        for u in 0..to.width {
            let p = Point::new(
                ((u as f64 + 0.5) / cx) as f32,
                ((v as f64 + 0.5) / cy) as f32,
            );
            out.push(l.sample(p));
        }
    }
    out
}

// Stroke geometry, shared with `refine`.

fn point_bounds(points: &[Point]) -> Option<(Point, Point)> {
    let first = *points.first()?;
    Some(points.iter().fold((first, first), |(lo, hi), p| {
        (
            Point::new(lo.x.min(p.x), lo.y.min(p.y)),
            Point::new(hi.x.max(p.x), hi.y.max(p.y)),
        )
    }))
}

fn segment_dist2(x: f64, y: f64, p: Point, q: Point) -> f64 {
    let (px, py) = (p.x as f64, p.y as f64);
    let (dx, dy) = (q.x as f64 - px, q.y as f64 - py);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((x - px) * dx + (y - py) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (ex, ey) = (x - (px + t * dx), y - (py + t * dy));
    ex * ex + ey * ey
}

/// The polyline's segments; a single point is one degenerate segment.
fn segments(points: &[Point]) -> impl Iterator<Item = (Point, Point)> + '_ {
    let single = (points.len() == 1).then(|| (points[0], points[0]));
    points.windows(2).map(|w| (w[0], w[1])).chain(single)
}

/// First and one-past-last index of the cells whose centres `(i + 0.5) · cell` lie in `[lo, hi]`,
/// clamped to `[0, limit)`.
fn cell_span(lo: f64, hi: f64, cell: f64, limit: usize) -> (usize, usize) {
    let first = (lo / cell - 0.5).ceil().max(0.0);
    let last = (hi / cell - 0.5).floor() + 1.0;
    let end = last.min(limit as f64).max(0.0) as usize;
    ((first as usize).min(end), end)
}

/// The window `(x, y, width, height)` of a grid of `limit` cells, each `cell` source pixels, that
/// holds every cell centre within `radius` of the stroke; `None` when there is none.
pub(crate) fn sweep_window(
    points: &[Point],
    radius: f64,
    cell: (f64, f64),
    limit: (usize, usize),
) -> Option<(usize, usize, usize, usize)> {
    let (lo, hi) = point_bounds(points)?;
    let (x0, x1) = cell_span(lo.x as f64 - radius, hi.x as f64 + radius, cell.0, limit.0);
    let (y0, y1) = cell_span(lo.y as f64 - radius, hi.y as f64 + radius, cell.1, limit.1);
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// Over `window` of a grid of `cell`-sized cells, row-major: whether each cell's centre lies within
/// `radius` of the stroke's polyline. Each segment only visits the cells of its own bounds.
pub(crate) fn sweep(
    points: &[Point],
    radius: f64,
    cell: (f64, f64),
    window: (usize, usize, usize, usize),
) -> Vec<bool> {
    let (wx, wy, ww, wh) = window;
    let mut bits = vec![false; ww * wh];
    let r2 = radius * radius;
    for (p, q) in segments(points) {
        let (xa, xb) = (p.x.min(q.x) as f64 - radius, p.x.max(q.x) as f64 + radius);
        let (ya, yb) = (p.y.min(q.y) as f64 - radius, p.y.max(q.y) as f64 + radius);
        let (a0, a1) = cell_span(xa, xb, cell.0, wx + ww);
        let (b0, b1) = cell_span(ya, yb, cell.1, wy + wh);
        for b in b0.max(wy)..b1 {
            let y = (b as f64 + 0.5) * cell.1;
            let row = &mut bits[(b - wy) * ww..(b - wy + 1) * ww];
            for a in a0.max(wx)..a1 {
                if segment_dist2((a as f64 + 0.5) * cell.0, y, p, q) <= r2 {
                    row[a - wx] = true;
                }
            }
        }
    }
    bits
}

/// The polyline sampled about once per source pixel of its length, endpoints included.
fn dense_samples(points: &[Point]) -> Vec<Point> {
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let lengths: Vec<f64> = points
        .windows(2)
        .map(|w| ((w[1].x - w[0].x) as f64).hypot((w[1].y - w[0].y) as f64))
        .collect();
    let total: f64 = lengths.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return vec![first];
    }
    let n = (total.ceil() as usize).clamp(1, MAX_STROKE_SAMPLES) + 1;
    let (mut segment, mut start) = (0usize, 0.0f64);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let s = total * i as f64 / (n - 1) as f64;
        while segment + 1 < lengths.len() && start + lengths[segment] < s {
            start += lengths[segment];
            segment += 1;
        }
        let t = if lengths[segment] > 0.0 {
            ((s - start) / lengths[segment]).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (p, q) = (points[segment], points[segment + 1]);
        out.push(Point::new(
            (p.x as f64 + t * (q.x - p.x) as f64) as f32,
            (p.y as f64 + t * (q.y - p.y) as f64) as f32,
        ));
    }
    out
}

/// Up to `k` samples by farthest-point sampling, seeded with the sample nearest the centroid.
/// Ties go to the earlier sample.
fn farthest_points(samples: &[Point], k: usize) -> Vec<Point> {
    if samples.is_empty() || k == 0 {
        return Vec::new();
    }
    let n = samples.len() as f64;
    let cx = samples.iter().map(|p| p.x as f64).sum::<f64>() / n;
    let cy = samples.iter().map(|p| p.y as f64).sum::<f64>() / n;
    let d2 = |p: Point, x: f64, y: f64| (p.x as f64 - x).powi(2) + (p.y as f64 - y).powi(2);
    let mut seed = 0;
    for (i, &p) in samples.iter().enumerate() {
        if d2(p, cx, cy) < d2(samples[seed], cx, cy) {
            seed = i;
        }
    }
    let first = samples[seed];
    let mut chosen = vec![first];
    let mut nearest: Vec<f64> = samples
        .iter()
        .map(|&p| d2(p, first.x as f64, first.y as f64))
        .collect();
    while chosen.len() < k {
        let mut next = 0;
        for (i, &d) in nearest.iter().enumerate() {
            if d > nearest[next] {
                next = i;
            }
        }
        if nearest[next] <= 0.0 {
            break;
        }
        let c = samples[next];
        chosen.push(c);
        for (d, &p) in nearest.iter_mut().zip(samples) {
            *d = d.min(d2(p, c.x as f64, c.y as f64));
        }
    }
    chosen
}

// The working raster.

/// One axis of the working raster: sub-cell `i` has its centre at `(i + 0.5) · px` source pixels
/// and interpolates between cells `lo[i]` and `hi[i]` with weight `t[i]` on `hi[i]`.
struct Axis {
    px: f64,
    lo: Vec<usize>,
    t: Vec<f32>,
    /// Runs of consecutive sub-cells sharing `lo`, as `(cell, start, end)`.
    runs: Vec<(usize, usize, usize)>,
}

impl Axis {
    fn new(cells: usize, cells_per_px: f32, source: u32) -> Self {
        let cell_px = 1.0 / cells_per_px as f64;
        let k = ((cell_px / WORKING_CELL_PX).ceil() as usize).max(1);
        let px = cell_px / k as f64;
        let n = ((source as f64 / px - 0.5).ceil().max(0.0) as usize).min(cells * k);
        let last = (cells - 1) as f64;
        let (mut lo, mut t) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let mut runs: Vec<(usize, usize, usize)> = Vec::new();
        for i in 0..n {
            let u = ((i as f64 + 0.5) / k as f64 - 0.5).clamp(0.0, last);
            let c = u.floor() as usize;
            lo.push(c);
            t.push((u - c as f64) as f32);
            match runs.last_mut() {
                Some(run) if run.0 == c => run.2 = i + 1,
                _ => runs.push((c, i, i + 1)),
            }
        }
        Self { px, lo, t, runs }
    }

    fn len(&self) -> usize {
        self.lo.len()
    }

    /// The runs whose lower cell lies in `[first, last]`.
    fn runs_in(&self, first: usize, last: usize) -> &[(usize, usize, usize)] {
        let a = self.runs.partition_point(|r| r.0 < first);
        let b = self.runs.partition_point(|r| r.0 <= last);
        &self.runs[a..b.max(a)]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Patch {
    Out,
    In,
    Mixed,
}

struct Grid {
    width: usize,
    height: usize,
    x: Axis,
    y: Axis,
}

impl Grid {
    fn new(l: &Logits) -> Self {
        Self {
            width: l.width,
            height: l.height,
            x: Axis::new(l.width, l.cells_per_px.0, l.source.0),
            y: Axis::new(l.height, l.cells_per_px.1, l.source.1),
        }
    }

    fn cell_area(&self) -> f64 {
        self.x.px * self.y.px
    }

    /// The interpolated logit at sub-cell `(a, b)`, as `Logits::sample` computes it.
    fn value(&self, data: &[f32], a: usize, b: usize) -> f32 {
        let (u0, fu) = (self.x.lo[a], self.x.t[a]);
        let (v0, fv) = (self.y.lo[b], self.y.t[b]);
        let (u1, v1) = ((u0 + 1).min(self.width - 1), (v0 + 1).min(self.height - 1));
        let at = |u: usize, v: usize| data[v * self.width + u];
        let top = at(u0, v0) * (1.0 - fu) + at(u1, v0) * fu;
        let bottom = at(u0, v1) * (1.0 - fu) + at(u1, v1) * fu;
        top * (1.0 - fv) + bottom * fv
    }

    /// Whether every sub-cell interpolating from cells `(u, v)` to `(u + 1, v + 1)` is inside,
    /// outside or either: a bilinear value lies between its corners.
    fn patch(&self, data: &[f32], u: usize, v: usize) -> Patch {
        let (u1, v1) = ((u + 1).min(self.width - 1), (v + 1).min(self.height - 1));
        let c = [
            data[v * self.width + u],
            data[v * self.width + u1],
            data[v1 * self.width + u],
            data[v1 * self.width + u1],
        ];
        if c.iter().all(|&x| x > 0.0) {
            Patch::In
        } else if c.iter().all(|&x| x <= 0.0) {
            Patch::Out
        } else {
            Patch::Mixed
        }
    }

    /// Sub-cells inside the shape, whose positive cells are `cells`.
    fn count(&self, data: &[f32], cells: (usize, usize, usize, usize)) -> u64 {
        let (u0, v0, u1, v1) = cells;
        let mut n = 0u64;
        for &(v, b0, b1) in self.y.runs_in(v0.saturating_sub(1), v1) {
            for &(u, a0, a1) in self.x.runs_in(u0.saturating_sub(1), u1) {
                match self.patch(data, u, v) {
                    Patch::In => n += ((a1 - a0) * (b1 - b0)) as u64,
                    Patch::Out => {}
                    Patch::Mixed => {
                        for b in b0..b1 {
                            n += (a0..a1).filter(|&a| self.value(data, a, b) > 0.0).count() as u64;
                        }
                    }
                }
            }
        }
        n
    }

    /// Sub-cells inside both shapes.
    fn overlap(&self, a: &Shape, b: &Shape) -> u64 {
        let u0 = a.cells.0.max(b.cells.0).saturating_sub(1);
        let v0 = a.cells.1.max(b.cells.1).saturating_sub(1);
        let (u1, v1) = (a.cells.2.min(b.cells.2), a.cells.3.min(b.cells.3));
        if u0 > u1 || v0 > v1 {
            return 0;
        }
        let mut n = 0u64;
        for &(v, y0, y1) in self.y.runs_in(v0, v1) {
            for &(u, x0, x1) in self.x.runs_in(u0, u1) {
                let (pa, pb) = (self.patch(&a.data, u, v), self.patch(&b.data, u, v));
                if pa == Patch::Out || pb == Patch::Out {
                    continue;
                }
                if pa == Patch::In && pb == Patch::In {
                    n += ((x1 - x0) * (y1 - y0)) as u64;
                    continue;
                }
                for y in y0..y1 {
                    for x in x0..x1 {
                        let ia = pa == Patch::In || self.value(&a.data, x, y) > 0.0;
                        let ib = pb == Patch::In || self.value(&b.data, x, y) > 0.0;
                        n += (ia && ib) as u64;
                    }
                }
            }
        }
        n
    }

    /// Whether `outer` is larger than `inner` and holds at least 90 % of its area.
    fn contains(&self, outer: &Shape, inner: &Shape) -> bool {
        outer.count > inner.count
            && self.overlap(outer, inner) as f64 >= NESTED_SHARE * inner.count as f64
    }

    /// Sub-cells inside both the shape and the stroke.
    fn footprint_overlap(&self, shape: &Shape, footprint: &Footprint) -> u64 {
        let (u0, v0, u1, v1) = shape.cells;
        let mut n = 0u64;
        for &(b, a0, a1) in &footprint.runs {
            let v = self.y.lo[b];
            if v + 1 < v0 || v > v1 {
                continue;
            }
            let start = self.x.runs.partition_point(|r| r.2 <= a0);
            for &(u, s, e) in &self.x.runs[start..] {
                if s >= a1 {
                    break;
                }
                if u + 1 < u0 || u > u1 {
                    continue;
                }
                let (s, e) = (s.max(a0), e.min(a1));
                match self.patch(&shape.data, u, v) {
                    Patch::In => n += (e - s) as u64,
                    Patch::Out => {}
                    Patch::Mixed => {
                        n += (s..e)
                            .filter(|&a| self.value(&shape.data, a, b) > 0.0)
                            .count() as u64
                    }
                }
            }
        }
        n
    }
}

/// A candidate on the working raster.
struct Shape<'a> {
    data: Cow<'a, [f32]>,
    cells: (usize, usize, usize, usize),
    count: u64,
}

impl<'a> Shape<'a> {
    fn new(l: &'a Logits, reference: &Logits, grid: &Grid) -> Option<Self> {
        if !well_formed(l) {
            return None;
        }
        let data: Cow<'a, [f32]> = if same_geometry(l, reference) {
            Cow::Borrowed(&l.data)
        } else {
            Cow::Owned(resample(l, reference))
        };
        let cells = bounds_above(&data, grid.width, (grid.width, grid.height), 0.0)?;
        let count = grid.count(&data, cells);
        (count > 0).then_some(Self { data, cells, count })
    }
}

/// The stroke on the working raster, as row runs `(row, start, end)`.
struct Footprint {
    runs: Vec<(usize, usize, usize)>,
    count: u64,
}

impl Footprint {
    /// The radius is at least half a sub-cell's diagonal, so a stroke thinner than the raster
    /// still covers the sub-cells it passes through.
    fn new(stroke: &Stroke, grid: &Grid) -> Self {
        let cell = (grid.x.px, grid.y.px);
        let radius = (stroke.radius.max(0.0) as f64).max(0.5 * cell.0.hypot(cell.1));
        let mut runs = Vec::new();
        let mut count = 0u64;
        if let Some(window) =
            sweep_window(&stroke.points, radius, cell, (grid.x.len(), grid.y.len()))
        {
            let (wx, wy, ww, _) = window;
            let bits = sweep(&stroke.points, radius, cell, window);
            for (row, line) in bits.chunks_exact(ww).enumerate() {
                let mut a = 0;
                while a < ww {
                    if !line[a] {
                        a += 1;
                        continue;
                    }
                    let start = a;
                    while a < ww && line[a] {
                        a += 1;
                    }
                    runs.push((wy + row, wx + start, wx + a));
                    count += (a - start) as u64;
                }
            }
        }
        Self { runs, count }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sam21_starts_from_a_lower_iou_floor_and_nothing_else_differs() {
        assert_eq!(
            Params::for_family(Family::EfficientVitL0),
            Params::default()
        );
        let sam = Params::for_family(Family::Sam21Small);
        assert_eq!(sam.min_predicted_iou, 0.6);
        assert_eq!(
            Params {
                min_predicted_iou: Params::default().min_predicted_iou,
                ..sam
            },
            Params::default()
        );
    }

    /// A 512 × 384 image under a 64 × 64 grid over the padded 512² frame, as EfficientViT-SAM
    /// lays it out: one cell is 8 source pixels, and rows 48 and beyond are padding.
    const IMAGE: (u32, u32) = (512, 384);

    fn logits(inside: impl Fn(f32, f32) -> bool) -> Logits {
        let cpp = 64.0 / 512.0;
        let mut data = Vec::with_capacity(64 * 64);
        for v in 0..64 {
            for u in 0..64 {
                let (x, y) = ((u as f32 + 0.5) / cpp, (v as f32 + 0.5) / cpp);
                data.push(if y >= IMAGE.1 as f32 {
                    -20.0
                } else if inside(x, y) {
                    8.0
                } else {
                    -8.0
                });
            }
        }
        Logits {
            width: 64,
            height: 64,
            data,
            source: IMAGE,
            cells_per_px: (cpp, cpp),
        }
    }

    fn disc(cx: f32, cy: f32, r: f32) -> impl Fn(f32, f32) -> bool {
        move |x, y| (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> impl Fn(f32, f32) -> bool {
        move |x, y| x >= x0 && x < x1 && y >= y0 && y < y1
    }

    fn candidate(inside: impl Fn(f32, f32) -> bool, predicted_iou: f32) -> Candidate {
        Candidate {
            logits: logits(inside),
            predicted_iou,
        }
    }

    fn line(a: (f32, f32), b: (f32, f32), radius: f32) -> Stroke {
        Stroke {
            points: vec![Point::new(a.0, a.1), Point::new(b.0, b.1)],
            radius,
        }
    }

    #[test]
    fn stroke_prompts_are_the_padded_box_then_farthest_points() {
        let stroke = line((100.0, 50.0), (200.0, 50.0), 10.0);
        let p = prompts(&stroke, IMAGE, &Params::default());
        assert_eq!(p.len(), 5);
        assert_eq!(
            p[0].bbox,
            Some((Point::new(90.0, 40.0), Point::new(210.0, 60.0)))
        );
        assert!(p[0].positive.is_empty() && p[0].negative.is_empty());
        // Seeded at the middle, then both ends (the earlier first), then the first quarter.
        let xs: Vec<f32> = p[1..].iter().map(|q| q.positive[0].x).collect();
        assert_eq!(xs, vec![150.0, 100.0, 200.0, 125.0]);
        assert!(
            p[1..]
                .iter()
                .all(|q| q.positive.len() == 1 && q.positive[0].y == 50.0 && q.bbox.is_none())
        );
    }

    #[test]
    fn stroke_prompts_clamp_to_the_image_and_handle_a_single_point() {
        let stroke = Stroke {
            points: vec![Point::new(5.0, 380.0)],
            radius: 20.0,
        };
        let p = prompts(&stroke, IMAGE, &Params::default());
        assert_eq!(p.len(), 2, "one point prompt for a single-point stroke");
        assert_eq!(
            p[0].bbox,
            Some((Point::new(0.0, 360.0), Point::new(25.0, 384.0)))
        );
        assert_eq!(p[1].positive, vec![Point::new(5.0, 380.0)]);
        let none = Stroke {
            points: Vec::new(),
            radius: 5.0,
        };
        assert!(prompts(&none, IMAGE, &Params::default()).is_empty());
    }

    #[test]
    fn stroke_prompts_follow_a_bent_polyline() {
        // An L: the farthest points reach both ends and the corner region.
        let stroke = Stroke {
            points: vec![
                Point::new(100.0, 100.0),
                Point::new(300.0, 100.0),
                Point::new(300.0, 300.0),
            ],
            radius: 4.0,
        };
        let p = prompts(&stroke, IMAGE, &Params::default());
        let pts: Vec<Point> = p[1..].iter().map(|q| q.positive[0]).collect();
        assert!(pts.contains(&Point::new(100.0, 100.0)));
        assert!(pts.contains(&Point::new(300.0, 300.0)));
        for q in &pts {
            let on_stroke = (q.y == 100.0 && (100.0..=300.0).contains(&q.x))
                || (q.x == 300.0 && (100.0..=300.0).contains(&q.y));
            assert!(on_stroke, "{q:?} is on the polyline");
        }
    }

    #[test]
    fn stroke_stability_is_the_iou_of_the_offset_thresholds() {
        let mut l = logits(rect(0.0, 0.0, 256.0, 384.0));
        assert_eq!(stability(&l, 1.0), 1.0);
        // Soften half the positive cells to +0.5: they pass −1 but not +1.
        let (vw, vh) = valid_cells(&l);
        assert_eq!((vw, vh), (64, 48));
        let mut softened = 0;
        for v in 0..vh {
            for u in 0..16 {
                l.data[v * 64 + u] = 0.5;
                softened += 1;
            }
        }
        let positive = 32 * 48;
        let expected = (positive - softened) as f32 / positive as f32;
        assert!((stability(&l, 1.0) - expected).abs() < 1e-6);
        // Padding rows do not count, whatever they hold.
        for x in &mut l.data[48 * 64..] {
            *x = 0.0;
        }
        assert!((stability(&l, 1.0) - expected).abs() < 1e-6);
        assert_eq!(stability(&logits(|_, _| false), 1.0), 0.0);
    }

    #[test]
    fn stroke_over_one_object_removes_it() {
        let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
        let object = disc(200.0, 200.0, 60.0);
        let results = vec![
            vec![candidate(&object, 0.95)],
            vec![candidate(&object, 0.93)],
            vec![candidate(disc(200.0, 200.0, 30.0), 0.9)],
        ];
        let r = decide(&stroke, &results, &Params::default());
        let Decision::One(i) = r.decision else {
            panic!("expected one object, got {:?}", r.decision);
        };
        let s = &r.scored[i];
        assert_eq!(
            (s.prompt, s.candidate),
            (0, 0),
            "the larger object, then the higher IoU"
        );
        assert_eq!(s.stroke_coverage, 1.0);
        let area = std::f32::consts::PI * 60.0 * 60.0;
        assert!(
            (s.area as f32 - area).abs() / area < 0.06,
            "area {}",
            s.area
        );
        let stroke_area = 60.0 * 20.0 + std::f32::consts::PI * 100.0;
        assert!((s.object_coverage - stroke_area / area).abs() < 0.03);
        assert!(s.stability > 0.99);
    }

    #[test]
    fn stroke_over_two_disjoint_objects_offers_both() {
        let stroke = line((100.0, 200.0), (400.0, 200.0), 8.0);
        let left = disc(120.0, 200.0, 50.0);
        let right = disc(360.0, 200.0, 70.0);
        let background = |x: f32, y: f32| !left(x, y) && !right(x, y);
        let results = vec![
            vec![candidate(background, 0.8)],
            vec![candidate(&left, 0.94)],
            vec![candidate(&right, 0.92)],
            vec![candidate(disc(360.0, 200.0, 30.0), 0.97)],
        ];
        let r = decide(&stroke, &results, &Params::default());
        // The background is under the stroke but the stroke covers little of it.
        assert_eq!(r.scored.len(), 3, "both objects and the right one's part");
        let Decision::Several(ref picks) = r.decision else {
            panic!("expected several, got {:?}", r.decision);
        };
        let prompts: Vec<usize> = picks.iter().map(|&i| r.scored[i].prompt).collect();
        assert_eq!(
            prompts,
            vec![2, 1],
            "listed by stroke coverage, without the part"
        );
        assert!(r.scored[picks[0]].stroke_coverage > r.scored[picks[1]].stroke_coverage);
        assert!(r.scored[picks[0]].stroke_coverage < 0.7);
    }

    #[test]
    fn stroke_over_a_part_removes_its_whole() {
        let whole = rect(150.0, 110.0, 300.0, 290.0);
        let part = disc(200.0, 180.0, 45.0);
        let results = vec![
            vec![candidate(&whole, 0.9)],
            vec![candidate(&part, 0.96), candidate(&whole, 0.85)],
        ];
        let params = Params::default();
        let decide_one = |stroke: &Stroke| {
            let r = decide(stroke, &results, &params);
            assert_eq!(
                r.scored.len(),
                2,
                "the part and the whole survive: {:?}",
                r.scored
            );
            match r.decision {
                Decision::One(i) => (r.scored[i].clone(), r.scored),
                d => panic!("expected one, got {d:?}"),
            }
        };

        // Mostly in the part and partly beyond it: both explain the stroke, and the whole wins.
        let crossing = line((175.0, 180.0), (250.0, 180.0), 12.0);
        let (pick, scored) = decide_one(&crossing);
        assert!(
            scored.iter().all(|s| s.stroke_coverage >= 0.7),
            "{scored:?}"
        );
        assert!(pick.area > 20_000.0, "the whole wins, area {}", pick.area);

        // Crossing well beyond the part: only the whole explains it.
        let long = line((175.0, 180.0), (280.0, 270.0), 12.0);
        let (pick, _) = decide_one(&long);
        assert!(pick.area > 20_000.0);

        // Entirely within the part: both explain it fully, and the part still yields to its whole
        // although its object coverage is better.
        let inside = line((180.0, 180.0), (220.0, 180.0), 14.0);
        let (pick, scored) = decide_one(&inside);
        assert!(
            scored.iter().all(|s| s.stroke_coverage == 1.0),
            "{scored:?}"
        );
        assert!(scored[1].object_coverage > scored[0].object_coverage);
        assert!(pick.area > 20_000.0, "the whole wins, area {}", pick.area);

        // A whole the stroke barely touches, such as a table under a cup, is no target.
        let table = rect(20.0, 20.0, 500.0, 380.0);
        let r = decide(
            &inside,
            &[vec![candidate(&part, 0.96), candidate(&table, 0.9)]],
            &params,
        );
        assert_eq!(r.scored.len(), 1);
        assert!(r.scored[0].area < 10_000.0);
        assert_eq!(r.decision, Decision::One(0));
    }

    #[test]
    fn stroke_keeps_a_whole_scored_below_its_part() {
        // SAM 2.1's three masks for points on a boy's waist: his belt, his clothes at 0.80, and the
        // whole boy at 0.61, below `min_predicted_iou`.
        let belt = rect(180.0, 190.0, 270.0, 215.0);
        let clothes = rect(170.0, 150.0, 280.0, 250.0);
        let boy = rect(150.0, 110.0, 300.0, 290.0);
        let stroke = line((190.0, 200.0), (260.0, 200.0), 14.0);
        let results = |boy_iou: f32| {
            vec![
                vec![
                    candidate(&belt, 0.72),
                    candidate(&clothes, 0.8),
                    candidate(&boy, boy_iou),
                ],
                vec![candidate(&clothes, 0.82), candidate(&boy, boy_iou)],
            ]
        };
        let params = Params::default();
        let one = |r: &Resolution| match r.decision {
            Decision::One(i) => r.scored[i].area,
            ref d => panic!("expected one, got {d:?}"),
        };
        let r = decide(&stroke, &results(0.61), &params);
        assert!(one(&r) > 25_000.0, "the whole boy: {:?}", r.scored);
        // Below the whole's floor the boy is dropped and his clothes are the target.
        let r = decide(&stroke, &results(0.45), &params);
        let area = one(&r);
        assert!((10_000.0..12_000.0).contains(&area), "the clothes: {area}");
        // A low-IoU candidate that holds no credible part is not rescued.
        let r = decide(&stroke, &[vec![candidate(&boy, 0.61)]], &params);
        assert_eq!(r.decision, Decision::Nothing);
    }

    #[test]
    fn stroke_over_nothing_decides_nothing() {
        let stroke = line((20.0, 20.0), (80.0, 40.0), 6.0);
        let results = vec![
            vec![candidate(disc(300.0, 250.0, 60.0), 0.95)],
            vec![candidate(disc(300.0, 250.0, 60.0), 0.95)],
        ];
        let r = decide(&stroke, &results, &Params::default());
        assert_eq!(r.decision, Decision::Nothing);
        assert!(r.scored.is_empty());
        assert_eq!(
            decide(&stroke, &[], &Params::default()).decision,
            Decision::Nothing
        );
        let outside = line((600.0, 20.0), (700.0, 40.0), 6.0);
        let r = decide(
            &outside,
            &[vec![candidate(|_, _| true, 0.99)]],
            &Params::default(),
        );
        assert_eq!(r.decision, Decision::Nothing);
    }

    #[test]
    fn stroke_drops_unstable_and_low_iou_candidates() {
        let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
        let object = disc(200.0, 200.0, 60.0);
        let low_iou = candidate(&object, 0.4);
        let mut unstable = candidate(&object, 0.97);
        for x in &mut unstable.logits.data {
            if *x > 0.0 {
                *x = 0.6;
            }
        }
        let r = decide(
            &stroke,
            &[vec![low_iou.clone()], vec![unstable.clone()]],
            &Params::default(),
        );
        assert_eq!(r.decision, Decision::Nothing);
        assert!(r.scored.is_empty());

        // With a good candidate beside them, only it survives.
        let good = candidate(disc(200.0, 200.0, 40.0), 0.9);
        let r = decide(
            &stroke,
            &[vec![low_iou.clone()], vec![unstable], vec![good.clone()]],
            &Params::default(),
        );
        assert_eq!(r.scored.len(), 1);
        assert_eq!(r.decision, Decision::One(0));
        assert_eq!(r.scored[0].prompt, 2);

        // At the whole's floor, the low-IoU disc holds the good one and is kept as its whole.
        let whole = Candidate {
            predicted_iou: 0.55,
            ..low_iou
        };
        let r = decide(&stroke, &[vec![whole], vec![good]], &Params::default());
        assert_eq!(r.scored.len(), 2);
        assert_eq!(r.decision, Decision::One(0));
        assert_eq!(r.scored[0].prompt, 0);
    }

    #[test]
    fn stroke_near_duplicates_keep_the_larger() {
        let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
        let results = vec![
            vec![candidate(disc(200.0, 200.0, 56.0), 0.97)],
            vec![candidate(disc(200.0, 200.0, 62.0), 0.9)],
            vec![candidate(disc(200.0, 200.0, 62.0), 0.9)],
        ];
        let r = decide(&stroke, &results, &Params::default());
        assert_eq!(r.scored.len(), 1, "{:?}", r.scored);
        assert_eq!((r.scored[0].prompt, r.scored[0].candidate), (1, 0));
        assert_eq!(r.decision, Decision::One(0));

        // Well apart in size, they are a part and its whole, not duplicates.
        let results = vec![
            vec![candidate(disc(200.0, 200.0, 30.0), 0.97)],
            vec![candidate(disc(200.0, 200.0, 62.0), 0.9)],
        ];
        let r = decide(&stroke, &results, &Params::default());
        assert_eq!(r.scored.len(), 2);
        assert!(
            r.scored[0].area > r.scored[1].area,
            "survivors run largest first"
        );
    }

    #[test]
    fn stroke_scores_candidates_on_other_grids() {
        // The same disc on SAM 2.1's squashed 256² frame scores like the padded frame's.
        let stroke = line((170.0, 200.0), (230.0, 200.0), 10.0);
        let padded = candidate(disc(200.0, 200.0, 60.0), 0.9);
        let (cx, cy) = (256.0 / 512.0, 256.0 / 384.0);
        let mut data = Vec::new();
        for v in 0..256 {
            for u in 0..256 {
                let (x, y) = ((u as f32 + 0.5) / cx, (v as f32 + 0.5) / cy);
                data.push(if disc(200.0, 200.0, 60.0)(x, y) {
                    8.0
                } else {
                    -8.0
                });
            }
        }
        let squashed = Candidate {
            logits: Logits {
                width: 256,
                height: 256,
                data,
                source: IMAGE,
                cells_per_px: (cx, cy),
            },
            predicted_iou: 0.95,
        };
        let alone = decide(&stroke, &[vec![squashed.clone()]], &Params::default());
        let mixed = decide(&stroke, &[vec![padded], vec![squashed]], &Params::default());
        assert_eq!(alone.decision, Decision::One(0));
        assert_eq!(mixed.scored.len(), 1, "duplicates across grids");
        let (a, m) = (alone.scored[0].area, mixed.scored[0].area);
        assert!((a - m).abs() / a < 0.05, "{a} vs {m}");
    }

    #[test]
    fn stroke_hover_prefers_the_whole_object() {
        let params = Params::default();
        // SAM 2.1 on a click at a boy's waist: his clothes score 0.80 and the whole boy 0.61.
        let belt = candidate(rect(180.0, 190.0, 270.0, 215.0), 0.7);
        let clothes = candidate(rect(170.0, 150.0, 280.0, 250.0), 0.8);
        let boy = candidate(rect(150.0, 110.0, 300.0, 290.0), 0.61);
        let three = [belt.clone(), clothes.clone(), boy.clone()];
        assert_eq!(pick_hover(&three, &params), Some(2));

        // A doubtful or unstable whole does not count, and the best IoU stands.
        let doubtful = Candidate {
            predicted_iou: 0.45,
            ..boy.clone()
        };
        assert_eq!(
            pick_hover(&[belt.clone(), clothes.clone(), doubtful], &params),
            Some(1)
        );
        let mut unstable = boy.clone();
        for x in &mut unstable.logits.data {
            if *x > 0.0 {
                *x = 0.6;
            }
        }
        assert_eq!(pick_hover(&[clothes.clone(), unstable], &params), Some(0));

        // A larger candidate that does not hold the best one is not its whole.
        let around = candidate(|x, y| !rect(170.0, 150.0, 280.0, 250.0)(x, y), 0.79);
        assert_eq!(pick_hover(&[clothes.clone(), around], &params), Some(0));

        // Equal wholes: the higher predicted IoU, then the earlier.
        let surer = Candidate {
            predicted_iou: 0.65,
            ..boy.clone()
        };
        assert_eq!(
            pick_hover(&[clothes.clone(), boy.clone(), surer], &params),
            Some(2)
        );
        assert_eq!(
            pick_hover(&[clothes.clone(), boy.clone(), boy.clone()], &params),
            Some(1)
        );
        // Equal best IoUs: the earlier.
        assert_eq!(pick_hover(&[boy.clone(), boy.clone()], &params), Some(0));
        assert_eq!(pick_hover(&[], &params), None);
        assert_eq!(pick_hover(&[candidate(|_, _| false, 0.9)], &params), None);
    }

    #[test]
    fn stroke_decisions_are_deterministic() {
        let stroke = Stroke {
            points: vec![
                Point::new(90.0, 190.0),
                Point::new(250.0, 215.0),
                Point::new(410.0, 205.0),
            ],
            radius: 9.0,
        };
        let results = vec![
            vec![candidate(rect(60.0, 150.0, 460.0, 260.0), 0.8)],
            vec![
                candidate(disc(120.0, 200.0, 50.0), 0.94),
                candidate(disc(120.0, 200.0, 52.0), 0.94),
            ],
            vec![candidate(disc(360.0, 200.0, 70.0), 0.92)],
            vec![candidate(disc(360.0, 200.0, 69.0), 0.92)],
        ];
        let params = Params::default();
        let first = decide(&stroke, &results, &params);
        for _ in 0..3 {
            assert_eq!(decide(&stroke, &results, &params), first);
            assert_eq!(
                prompts(&stroke, IMAGE, &params),
                prompts(&stroke, IMAGE, &params)
            );
        }
    }

    #[test]
    fn stroke_raster_is_exact_against_brute_force() {
        // The patch shortcut must count exactly what per-sub-cell evaluation counts.
        let l = logits(disc(211.0, 147.0, 77.0));
        let grid = Grid::new(&l);
        let shape = Shape::new(&l, &l, &grid).expect("non-empty");
        let mut brute = 0u64;
        for b in 0..grid.y.len() {
            for a in 0..grid.x.len() {
                brute += (grid.value(&l.data, a, b) > 0.0) as u64;
            }
        }
        assert_eq!(shape.count, brute);
        // And the grid's sub-cell centres interpolate as Logits::sample does.
        for (a, b) in [(0, 0), (5, 7), (63, 47), (40, 12)] {
            let p = Point::new(
                ((a as f64 + 0.5) * grid.x.px) as f32,
                ((b as f64 + 0.5) * grid.y.px) as f32,
            );
            assert!((grid.value(&l.data, a, b) - l.sample(p)).abs() < 1e-4);
        }
    }
}
