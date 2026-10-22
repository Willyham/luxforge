//! The picker's rules (`docs/design/ai-editing.md`, "Picking: hover, click, box and brush"): a
//! painted stroke becomes prompts, what the decoder answers is scored, and the decision is One,
//! Several or Nothing; a hover point picks one candidate, biased towards the whole object. This is
//! the prototype's stroke module (`probes/object-removal/src/stroke.rs`) transcribed, rule for rule
//! and tie for tie, so its recorded decisions are this module's tests (`tests.rs`).
//!
//! Coverage and area are counted on a working raster over the logits grid: each logit cell is split
//! into `k × k` sub-cells so that one sub-cell spans at most about four analysis pixels. A sub-cell
//! is inside a candidate when the bilinearly interpolated logit at its centre is above 0 (the same
//! interpolation as [`Logits::sample`]), and inside the stroke when its centre lies within the
//! brush radius of the polyline. Areas are sub-cell counts times the sub-cell's area in analysis
//! pixels. A 2 × 2 patch of cells whose four logits share a sign is counted whole, so the cost is
//! the number of cells plus the sub-cells along each candidate's edge.
use super::segment::{Candidate, Logits, Point, Prompt};

mod raster;
#[cfg(test)]
mod tests;

use raster::{Footprint, Grid, Shape, point_bounds};
pub(crate) use raster::{valid_cells, well_formed};

/// A painted stroke: a polyline in analysis pixels swept by a round brush.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stroke {
    pub(crate) points: Vec<Point>,
    pub(crate) radius: f32,
}

/// The rules' thresholds: the prototype's starting values for EfficientViT-SAM L0, the pinned
/// segmenter, which its corpus kept (`probes/object-removal/README.md`, "Decisions on the
/// corpus"). None is a published constant.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Params {
    /// Positive points sampled along the stroke, each also decoded as its own prompt.
    pub(crate) sample_points: usize,
    /// Candidates below this predicted IoU are dropped, except a whole (see `min_whole_iou`).
    pub(crate) min_predicted_iou: f32,
    /// A candidate that contains a candidate clearing every floor is kept down to this predicted
    /// IoU, and the hover pick's whole must reach it.
    pub(crate) min_whole_iou: f32,
    /// Candidates below this stability score (logit offset ±1) are dropped.
    pub(crate) min_stability: f32,
    /// Mask IoU above which two candidates are the same object.
    pub(crate) nms_iou: f32,
    /// A single candidate explaining at least this share of the stroke is "the one object".
    pub(crate) one_object_stroke_coverage: f32,
    /// A candidate the stroke touches for less than this share of its own area is not a target.
    pub(crate) min_object_coverage: f32,
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

/// One candidate's scores. `prompt` and `candidate` index the decoder results passed to
/// [`decide`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scored {
    pub(crate) prompt: usize,
    pub(crate) candidate: usize,
    pub(crate) predicted_iou: f32,
    pub(crate) stability: f32,
    /// Share of the stroke's area inside the candidate.
    pub(crate) stroke_coverage: f32,
    /// Share of the candidate's area under the stroke.
    pub(crate) object_coverage: f32,
    /// Candidate area in analysis pixels.
    pub(crate) area: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Decision {
    /// No candidate survived; the stroke is left as a plain painted region.
    Nothing,
    /// This object (an index into [`Resolution::scored`]).
    One(usize),
    /// Several distinct objects to choose among (indices into `scored`).
    Several(Vec<usize>),
}

impl Decision {
    /// The decision's name, as the query answers it.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Nothing => "nothing",
            Self::One(_) => "one",
            Self::Several(_) => "several",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Resolution {
    /// Surviving candidates after filtering and non-maximum suppression, largest first.
    pub(crate) scored: Vec<Scored>,
    pub(crate) decision: Decision,
}

/// The logit offset of the stability score, as SAM's automatic mask generator uses.
const STABILITY_OFFSET: f32 = 1.0;
/// Two survivors are distinct objects when they share less than this share of the smaller one.
const DISJOINT_OVERLAP: f64 = 0.1;
/// A candidate with at least this share of its area inside a larger one is nested in it: a part of
/// that whole.
pub(super) const NESTED_SHARE: f64 = 0.9;
/// The stroke is sampled about once per analysis pixel of its length, at most this many times.
const MAX_STROKE_SAMPLES: usize = 4096;

/// The prompts for a stroke: first the stroke's padded bounding box, then one single positive
/// point per sampled stroke point.
///
/// The box is the points' bounds grown by the brush radius and clamped to the image. The points
/// are chosen by farthest-point sampling over the polyline sampled about once per analysis pixel,
/// seeded with the sample nearest the samples' centroid; ties go to the earlier sample. Sampling
/// stops early when every remaining sample coincides with a chosen one, so a single-point stroke
/// gives the box and one point. An empty stroke gives no prompts.
pub(crate) fn prompts(stroke: &Stroke, image: (u32, u32), params: &Params) -> Vec<Prompt> {
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
pub(crate) fn decide(stroke: &Stroke, results: &[Vec<Candidate>], params: &Params) -> Resolution {
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
            iou > f64::from(params.nms_iou)
        });
        if !duplicate {
            kept.push((scored, shape));
        }
    }

    let n = kept.len();
    let mut shared = vec![0_u64; n * n];
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
/// the decoder score the part above the whole, so the whole is found by containment rather than by
/// IoU. `None` when no candidate has both a finite predicted IoU and a non-empty mask.
pub(crate) fn pick_hover(candidates: &[Candidate], params: &Params) -> Option<usize> {
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
/// at `logit > −offset`, over the logit cells whose centres lie inside the analysis input. The
/// offset's magnitude is used. An empty mask scores 0.
pub(crate) fn stability(logits: &Logits, offset: f32) -> f32 {
    let offset = offset.abs();
    let (vw, vh) = valid_cells(logits);
    let (mut strict, mut loose) = (0_u64, 0_u64);
    for row in logits.data.chunks_exact(logits.width.max(1)).take(vh) {
        for &v in &row[..vw] {
            strict += u64::from(v > offset);
            loose += u64::from(v > -offset);
        }
    }
    if loose == 0 {
        0.0
    } else {
        (strict as f64 / loose as f64) as f32
    }
}

/// The polyline sampled about once per analysis pixel of its length, endpoints included.
fn dense_samples(points: &[Point]) -> Vec<Point> {
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let lengths: Vec<f64> = points
        .windows(2)
        .map(|w| f64::from(w[1].x - w[0].x).hypot(f64::from(w[1].y - w[0].y)))
        .collect();
    let total: f64 = lengths.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return vec![first];
    }
    let n = (total.ceil() as usize).clamp(1, MAX_STROKE_SAMPLES) + 1;
    let (mut segment, mut start) = (0_usize, 0.0_f64);
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
            (f64::from(p.x) + t * f64::from(q.x - p.x)) as f32,
            (f64::from(p.y) + t * f64::from(q.y - p.y)) as f32,
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
    let cx = samples.iter().map(|p| f64::from(p.x)).sum::<f64>() / n;
    let cy = samples.iter().map(|p| f64::from(p.y)).sum::<f64>() / n;
    let d2 = |p: Point, x: f64, y: f64| (f64::from(p.x) - x).powi(2) + (f64::from(p.y) - y).powi(2);
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
        .map(|&p| d2(p, f64::from(first.x), f64::from(first.y)))
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
            *d = d.min(d2(p, f64::from(c.x), f64::from(c.y)));
        }
    }
    chosen
}

/// The analysis-pixel rectangle `[x0, y0, x1, y1)` of a candidate's logits above 0 among the cells
/// that can influence the image, grown by half a cell for the bilinear reach and clamped to the
/// analysis input; `None` for an empty candidate.
pub(crate) fn bounds(logits: &Logits) -> Option<[u32; 4]> {
    let (u0, v0, u1, v1) = raster::cells_above(logits, 0.0)?;
    let (cx, cy) = (
        f64::from(logits.cells_per_px.0),
        f64::from(logits.cells_per_px.1),
    );
    let (w, h) = (f64::from(logits.source.0), f64::from(logits.source.1));
    // Cell `u` holds its centre at `(u + 0.5) / cx`; a positive cell reaches half a cell either side.
    let x0 = ((u0 as f64) / cx).floor().clamp(0.0, w);
    let y0 = ((v0 as f64) / cy).floor().clamp(0.0, h);
    let x1 = ((u1 as f64 + 1.0) / cx).ceil().clamp(0.0, w);
    let y1 = ((v1 as f64 + 1.0) / cy).ceil().clamp(0.0, h);
    (x1 > x0 && y1 > y0).then_some([x0 as u32, y0 as u32, x1 as u32, y1 as u32])
}

/// A candidate's coverage over `rect` (analysis pixels `[x0, y0, x1, y1)`) sampled on a grid of at
/// most `max_pixels` cells of the rectangle's aspect: the hard-sigmoid prior
/// `clamp(0.5 + logit / 4, 0, 1)` of the bilinear logit at each cell's centre, as 8-bit codes,
/// row-major. What a hover overlay draws, so its frame uploads a bounded texture whatever the
/// object's size. Answers the grid's size and its codes.
pub(crate) fn coverage_preview(
    logits: &Logits,
    rect: [u32; 4],
    max_pixels: u32,
) -> ((u32, u32), Vec<u8>) {
    let (rw, rh) = (
        f64::from(rect[2].saturating_sub(rect[0]).max(1)),
        f64::from(rect[3].saturating_sub(rect[1]).max(1)),
    );
    let scale = (f64::from(max_pixels.max(1)) / (rw * rh)).sqrt().min(1.0);
    let width = ((rw * scale).floor() as u32).max(1);
    let height = ((rh * scale).floor() as u32).max(1);
    let mut codes = Vec::with_capacity(width as usize * height as usize);
    for j in 0..height {
        let y = f64::from(rect[1]) + (f64::from(j) + 0.5) * rh / f64::from(height);
        for i in 0..width {
            let x = f64::from(rect[0]) + (f64::from(i) + 0.5) * rw / f64::from(width);
            let logit = logits.sample(Point::new(x as f32, y as f32));
            let prior = (0.5 + f64::from(logit) / 4.0).clamp(0.0, 1.0);
            codes.push((prior * 255.0).round() as u8);
        }
    }
    ((width, height), codes)
}

/// A candidate's logits resampled at the centres of a prior grid of `size` stretched over the
/// analysis input, as hard-sigmoid probabilities: the prior the guided upsampler refines.
pub(crate) fn prior(logits: &Logits, size: (u32, u32)) -> Vec<f32> {
    let (w, h) = (f64::from(logits.source.0), f64::from(logits.source.1));
    let mut values = Vec::with_capacity(size.0 as usize * size.1 as usize);
    for j in 0..size.1 {
        let y = (f64::from(j) + 0.5) * h / f64::from(size.1);
        for i in 0..size.0 {
            let x = (f64::from(i) + 0.5) * w / f64::from(size.0);
            let logit = logits.sample(Point::new(x as f32, y as f32));
            values.push((0.5 + logit / 4.0).clamp(0.0, 1.0));
        }
    }
    values
}

/// The prior grid for a candidate: one cell per logit cell over the analysis input, at least one.
pub(crate) fn prior_size(logits: &Logits) -> (u32, u32) {
    let side = |source: u32, cells: f32| {
        ((f64::from(source) * f64::from(cells)).round() as u32).clamp(1, source.max(1))
    };
    (
        side(logits.source.0, logits.cells_per_px.0),
        side(logits.source.1, logits.cells_per_px.1),
    )
}
