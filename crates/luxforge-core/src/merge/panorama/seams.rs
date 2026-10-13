//! The stitch's seams (`docs/design/panorama-merge.md`, "Exposure, colour and seams"): which frame
//! each plan cell is taken from, placed by a minimum-cost cut through every overlap, so a moving
//! subject is taken whole from one frame rather than doubled.
//!
//! **Eligibility.** A frame is eligible at a cell when it covers that cell and its eight
//! neighbours (every corner pixel of each inside the frame), so a seam's one-cell transition
//! never reads past a frame's edge.
//!
//! **Order.** The reference frame is taken first, over every cell it is eligible at. Each further
//! frame is the one with the most eligible cells shared with the frames already taken (ties to the
//! lower index); frames sharing none follow in index order.
//!
//! **One cut a frame.** Adding frame `j` to the composite of the frames taken so far is a binary
//! labelling of their overlap `O` (cells where `j` is eligible and the composite has a label),
//! solved as a minimum s–t cut on the 4-connected grid (Boykov and Kolmogorov's algorithm, exact
//! up to [`DIRECT_CUT_NODES`] cells, banded coarse to fine beyond; see [`min_cut`]). An overlap cell
//! beside a cell only the composite holds is tied to the composite; one beside a cell only `j`
//! holds, to `j` (the composite wins a cell beside both). Cutting between neighbours `p` and `q`
//! costs `c(p) + c(q) + 1`, the `+ 1` preferring short seams, with `c` the per-cell difference
//! quantized at 1/32 and capped at 255:
//!
//! - the mean absolute difference of the two `log2` colours (gains applied),
//! - plus the difference of their `log2` brightness gradients to the right and below, within `O`,
//! - plus [`CLIP_COST`] where either sample is clipped.
//!
//! Cells on `j`'s side of the cut, and the cells only `j` holds, take `j`.

use super::photometric::GainSample;
use crate::{Cancel, Error};
use rayon::prelude::*;

/// A cell with no frame.
pub(crate) const NONE: u8 = u8::MAX;
/// The added cost of a cell where either frame's sample is clipped, in stops.
pub const CLIP_COST: f32 = 4.0;
/// Seam costs are quantized at this many steps a stop...
const COST_STEPS: f32 = 32.0;
/// ...up to this.
const MAX_COST: f32 = 255.0;

/// One frame's sample at one cell, as seams compare it: its `log2` colour with its gain applied,
/// and whether any tap it read was clipped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SeamSample {
    pub log: [f32; 3],
    pub clipped: bool,
}

impl SeamSample {
    pub(crate) fn of(rgb: GainSample, clipped: bool) -> Self {
        Self {
            log: rgb.map(|v| v.max(1.0 / 65536.0).log2()),
            clipped,
        }
    }

    fn brightness(&self) -> f32 {
        (self.log[0] + self.log[1] + self.log[2]) / 3.0
    }
}

/// The grid seams are placed on: `width × height` cells, row-major.
pub(crate) struct SeamGrid<'a> {
    pub width: usize,
    pub height: usize,
    /// Per cell, the frames eligible there, one bit a frame.
    pub eligible: &'a [u32],
    /// Per frame, the cells `[x₀, y₀, x₁, y₁)` it may be eligible at.
    pub bounds: &'a [[usize; 4]],
}

/// The order frames are added in. See the module documentation.
pub(crate) fn frame_order(grid: &SeamGrid<'_>, frames: usize, reference: usize) -> Vec<usize> {
    let mut shared = vec![0_u64; frames * frames];
    for &bits in grid.eligible {
        let mut rest = bits;
        while rest != 0 {
            let a = rest.trailing_zeros() as usize;
            rest &= rest - 1;
            let mut others = rest;
            while others != 0 {
                let b = others.trailing_zeros() as usize;
                others &= others - 1;
                shared[a * frames + b] += 1;
                shared[b * frames + a] += 1;
            }
        }
    }
    let mut order = vec![reference];
    let mut taken = vec![false; frames];
    taken[reference] = true;
    while order.len() < frames {
        let next = (0..frames)
            .filter(|&j| !taken[j])
            .max_by(|&a, &b| {
                let score = |j: usize| order.iter().map(|&i| shared[i * frames + j]).sum::<u64>();
                score(a).cmp(&score(b)).then(b.cmp(&a))
            })
            .unwrap_or(0);
        taken[next] = true;
        order.push(next);
    }
    order
}

/// Every cell's frame, or [`NONE`]. `sample(frame, cell)` is the frame's sample at an eligible
/// cell. Checks `cancel` per frame and per cut phase.
pub(crate) fn seam_labels(
    grid: &SeamGrid<'_>,
    order: &[usize],
    sample: &(dyn Fn(usize, usize) -> Option<SeamSample> + Sync),
    cancel: &Cancel,
) -> Result<Vec<u8>, Error> {
    let (gw, gh) = (grid.width, grid.height);
    let mut labels = vec![NONE; gw * gh];
    let mut composite: Vec<Option<SeamSample>> = vec![None; gw * gh];
    for &j in order {
        cancel.check()?;
        let [bx0, by0, bx1, by1] = grid.bounds[j];
        let (x0, y0) = (bx0.saturating_sub(1), by0.saturating_sub(1));
        let (x1, y1) = ((bx1 + 1).min(gw), (by1 + 1).min(gh));
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let global = |i: usize| (y0 + i / w) * gw + x0 + i % w;
        let bit = 1_u32 << j;
        let eligible = |cell: usize| grid.eligible[cell] & bit != 0;
        let samples: Vec<Option<SeamSample>> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let cell = global(i);
                if eligible(cell) {
                    sample(j, cell)
                } else {
                    None
                }
            })
            .collect();
        let region: Vec<bool> = (0..w * h)
            .map(|i| samples[i].is_some() && labels[global(i)] != NONE)
            .collect();
        let only_j: Vec<bool> = (0..w * h)
            .map(|i| samples[i].is_some() && labels[global(i)] == NONE)
            .collect();
        if region.iter().any(|&r| r) {
            let mut source = vec![false; w * h];
            let mut sink = vec![false; w * h];
            for i in 0..w * h {
                if !region[i] {
                    continue;
                }
                let (x, y) = (x0 + i % w, y0 + i / w);
                let neighbours = [
                    (x > 0).then(|| (x - 1, y)),
                    (x + 1 < gw).then(|| (x + 1, y)),
                    (y > 0).then(|| (x, y - 1)),
                    (y + 1 < gh).then(|| (x, y + 1)),
                ];
                for (nx, ny) in neighbours.into_iter().flatten() {
                    let cell = ny * gw + nx;
                    let local = (nx >= x0 && nx < x1 && ny >= y0 && ny < y1)
                        .then(|| (ny - y0) * w + (nx - x0));
                    let j_here = local.is_some_and(|l| samples[l].is_some());
                    if labels[cell] != NONE && !j_here {
                        source[i] = true;
                    } else if j_here && labels[cell] == NONE {
                        sink[i] = true;
                    }
                }
            }
            // Per-cell differences, then the cut's capacities.
            let cost: Vec<f32> = (0..w * h)
                .map(|i| {
                    if !region[i] {
                        return 0.0;
                    }
                    let (a, b) = (
                        samples[i].unwrap_or(NULL),
                        composite[global(i)].unwrap_or(NULL),
                    );
                    let mut d = (0..3).map(|c| (a.log[c] - b.log[c]).abs()).sum::<f32>() / 3.0;
                    let x = i % w;
                    for step in [(x + 1 < w).then_some(1), (i + w < w * h).then_some(w)] {
                        let Some(step) = step else { continue };
                        let k = i + step;
                        if !region[k] {
                            continue;
                        }
                        let (a2, b2) = (
                            samples[k].unwrap_or(NULL),
                            composite[global(k)].unwrap_or(NULL),
                        );
                        let ga = a2.brightness() - a.brightness();
                        let gb = b2.brightness() - b.brightness();
                        d += (ga - gb).abs();
                    }
                    if a.clipped || b.clipped {
                        d += CLIP_COST;
                    }
                    (d * COST_STEPS).round().min(MAX_COST)
                })
                .collect();
            let mut right = vec![0_u16; w * h];
            let mut down = vec![0_u16; w * h];
            for i in 0..w * h {
                if !region[i] {
                    continue;
                }
                if i % w + 1 < w && region[i + 1] {
                    right[i] = (cost[i] + cost[i + 1]) as u16 + 1;
                }
                if i + w < w * h && region[i + w] {
                    down[i] = (cost[i] + cost[i + w]) as u16 + 1;
                }
            }
            let keep = min_cut(w, h, &region, &source, &sink, &right, &down, cancel)?;
            for i in 0..w * h {
                if region[i] && !keep[i] {
                    labels[global(i)] = j as u8;
                    composite[global(i)] = samples[i];
                }
            }
        }
        for i in 0..w * h {
            if only_j[i] {
                labels[global(i)] = j as u8;
                composite[global(i)] = samples[i];
            }
        }
    }
    Ok(labels)
}

const NULL: SeamSample = SeamSample {
    log: [0.0; 3],
    clipped: false,
};

/// The source side of a minimum s–t cut of a `width × height` 4-connected grid: `region` cells
/// are the nodes, `source` and `sink` cells link to their terminal (a cell that is both stays
/// with the source), and `right[i]` and `down[i]` are the capacities, both ways, between cell `i`
/// and its right and lower neighbours.
///
/// Up to [`DIRECT_CUT_NODES`] nodes the cut is exact ([`exact_cut`]). Beyond, it is banded
/// coarse to fine, after Lombaert and others: the grid is coarsened by two a side (a coarse cell's
/// capacity to its neighbour is the sum of the fine capacities between them), cut recursively,
/// and the cut then refined exactly within [`BAND_BLOCKS`] coarse cells of the coarse boundary,
/// with the cells outside the band held to their coarse side. The seam therefore follows the
/// coarse optimum's route at full resolution; it is deterministic and checks `cancel` throughout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn min_cut(
    width: usize,
    height: usize,
    region: &[bool],
    source: &[bool],
    sink: &[bool],
    right: &[u16],
    down: &[u16],
    cancel: &Cancel,
) -> Result<Vec<bool>, Error> {
    let right: Vec<u32> = right.iter().map(|&c| u32::from(c)).collect();
    let down: Vec<u32> = down.iter().map(|&c| u32::from(c)).collect();
    let grid = CutGrid {
        width,
        height,
        region,
        source,
        sink,
        right: &right,
        down: &down,
    };
    banded_cut(&grid, DIRECT_CUT_NODES, cancel)
}

/// Region nodes up to which the cut is solved exactly.
pub const DIRECT_CUT_NODES: usize = 1 << 15;
/// The refinement band's half-width, in coarse cells.
pub const BAND_BLOCKS: usize = 2;

/// One cut problem; see [`min_cut`].
struct CutGrid<'a> {
    width: usize,
    height: usize,
    region: &'a [bool],
    source: &'a [bool],
    sink: &'a [bool],
    right: &'a [u32],
    down: &'a [u32],
}

/// [`min_cut`] with the exact solve's node limit `direct`.
fn banded_cut(grid: &CutGrid<'_>, direct: usize, cancel: &Cancel) -> Result<Vec<bool>, Error> {
    let (width, height) = (grid.width, grid.height);
    let n = width * height;
    if grid.region.iter().filter(|&&r| r).count() <= direct || n <= 1 {
        return exact_cut(grid, cancel);
    }
    cancel.check()?;
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let block = |i: usize| (i / width / 2) * cw + (i % width) / 2;
    let mut c_region = vec![false; cw * ch];
    let mut c_source = vec![false; cw * ch];
    let mut c_sink = vec![false; cw * ch];
    let mut c_right = vec![0_u32; cw * ch];
    let mut c_down = vec![0_u32; cw * ch];
    for i in 0..n {
        if !grid.region[i] {
            continue;
        }
        let b = block(i);
        c_region[b] = true;
        c_source[b] |= grid.source[i];
        c_sink[b] |= grid.sink[i];
        if i % width + 1 < width && grid.region[i + 1] && block(i + 1) != b {
            c_right[b] = c_right[b].saturating_add(grid.right[i]);
        }
        if i + width < n && grid.region[i + width] && block(i + width) != b {
            c_down[b] = c_down[b].saturating_add(grid.down[i]);
        }
    }
    let coarse = banded_cut(
        &CutGrid {
            width: cw,
            height: ch,
            region: &c_region,
            source: &c_source,
            sink: &c_sink,
            right: &c_right,
            down: &c_down,
        },
        direct,
        cancel,
    )?;
    cancel.check()?;
    // Coarse cells on the boundary, or holding both terminals, and the band around them.
    let mut band = vec![false; cw * ch];
    for b in 0..cw * ch {
        if !c_region[b] {
            continue;
        }
        let (x, y) = (b % cw, b / cw);
        let differs = [
            (x > 0).then(|| b - 1),
            (x + 1 < cw).then(|| b + 1),
            (y > 0).then(|| b - cw),
            (y + 1 < ch).then(|| b + cw),
        ]
        .into_iter()
        .flatten()
        .any(|k| c_region[k] && coarse[k] != coarse[b]);
        if differs || (c_source[b] && c_sink[b]) {
            for yy in y.saturating_sub(BAND_BLOCKS)..=(y + BAND_BLOCKS).min(ch - 1) {
                for xx in x.saturating_sub(BAND_BLOCKS)..=(x + BAND_BLOCKS).min(cw - 1) {
                    band[yy * cw + xx] = true;
                }
            }
        }
    }
    // The refinement: band cells, held at the band's edge to their neighbours' coarse sides.
    let in_band: Vec<bool> = (0..n).map(|i| grid.region[i] && band[block(i)]).collect();
    let mut source = vec![false; n];
    let mut sink = vec![false; n];
    for i in 0..n {
        if !in_band[i] {
            continue;
        }
        source[i] = grid.source[i];
        sink[i] = grid.sink[i];
        let (x, y) = (i % width, i / width);
        for k in [
            (x > 0).then(|| i - 1),
            (x + 1 < width).then(|| i + 1),
            (y > 0).then(|| i - width),
            (y + 1 < height).then(|| i + width),
        ]
        .into_iter()
        .flatten()
        {
            if grid.region[k] && !in_band[k] {
                if coarse[block(k)] {
                    source[i] = true;
                } else {
                    sink[i] = true;
                }
            }
        }
    }
    let refined = exact_cut(
        &CutGrid {
            width,
            height,
            region: &in_band,
            source: &source,
            sink: &sink,
            right: grid.right,
            down: grid.down,
        },
        cancel,
    )?;
    Ok((0..n)
        .map(|i| {
            grid.region[i]
                && if in_band[i] {
                    refined[i]
                } else {
                    coarse[block(i)]
                }
        })
        .collect())
}

/// The exact cut of [`min_cut`]'s problem by Boykov and Kolmogorov's augmenting-path algorithm,
/// which keeps its two search trees between augmentations: deterministic, checking `cancel` every
/// [`CUT_CHECK`] steps. The source side is the source tree once no path remains: every node the
/// source still reaches.
fn exact_cut(grid: &CutGrid<'_>, cancel: &Cancel) -> Result<Vec<bool>, Error> {
    let CutGrid {
        width,
        height,
        region,
        source,
        sink,
        right,
        down,
    } = *grid;
    const FREE: u8 = 0;
    const S: u8 = 1;
    const T: u8 = 2;
    const TERMINAL: u8 = 4;
    const ORPHAN: u8 = 5;
    let n = width * height;
    // Residuals to the right, left, below and above: `d ^ 1` is `d`'s reverse.
    let mut res = [
        vec![0_i32; n],
        vec![0_i32; n],
        vec![0_i32; n],
        vec![0_i32; n],
    ];
    for i in 0..n {
        if !region[i] {
            continue;
        }
        if i % width + 1 < width && region[i + 1] {
            let c = i32::try_from(right[i].min(MAX_CAPACITY)).unwrap_or(0);
            res[0][i] = c;
            res[1][i + 1] = c;
        }
        if i + width < n && region[i + width] {
            let c = i32::try_from(down[i].min(MAX_CAPACITY)).unwrap_or(0);
            res[2][i] = c;
            res[3][i + width] = c;
        }
    }
    let neighbour = |i: usize, dir: u8| -> Option<usize> {
        match dir {
            0 => (i % width + 1 < width).then_some(i + 1),
            1 => (!i.is_multiple_of(width)).then(|| i - 1),
            2 => (i + width < n).then_some(i + width),
            _ => (i >= width).then(|| i - width),
        }
    };
    let mut tree = vec![FREE; n];
    // The direction to a node's parent, TERMINAL for a tree's root, ORPHAN when lost.
    let mut parent = vec![ORPHAN; n];
    let mut stamp = vec![0_u32; n];
    let mut dist = vec![0_u32; n];
    let mut active = vec![false; n];
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for i in 0..n {
        if !region[i] {
            continue;
        }
        if source[i] {
            tree[i] = S;
        } else if sink[i] {
            tree[i] = T;
        } else {
            continue;
        }
        parent[i] = TERMINAL;
        dist[i] = 1;
        active[i] = true;
        queue.push_back(i);
    }
    let mut time = 0_u32;
    let mut steps = 0_usize;
    let mut orphans: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    // The residual of the arc between a node and its tree parent, in the tree's direction.
    let tree_arc = |res: &[Vec<i32>; 4], tree: u8, p: usize, dir: u8, q: usize| -> i32 {
        if tree == S {
            res[usize::from(dir ^ 1)][q]
        } else {
            res[usize::from(dir)][p]
        }
    };
    loop {
        // Grow the trees until they touch: an arc a → b, a in S and b in T.
        let mut touch: Option<(usize, u8, usize)> = None;
        while let Some(&p) = queue.front() {
            steps += 1;
            if steps.is_multiple_of(CUT_CHECK) {
                cancel.check()?;
            }
            if !active[p] || tree[p] == FREE {
                active[p] = false;
                queue.pop_front();
                continue;
            }
            for dir in 0..4_u8 {
                let Some(q) = neighbour(p, dir) else { continue };
                // The arc away from the tree's root: p → q in S, q → p in T.
                let open = if tree[p] == S {
                    res[usize::from(dir)][p] > 0
                } else {
                    res[usize::from(dir ^ 1)][q] > 0
                };
                if !open {
                    continue;
                }
                if tree[q] == FREE {
                    tree[q] = tree[p];
                    parent[q] = dir ^ 1;
                    stamp[q] = stamp[p];
                    dist[q] = dist[p] + 1;
                    active[q] = true;
                    queue.push_back(q);
                } else if tree[q] != tree[p] {
                    touch = Some(if tree[p] == S {
                        (p, dir, q)
                    } else {
                        (q, dir ^ 1, p)
                    });
                    break;
                } else if stamp[q] <= stamp[p] && dist[q] > dist[p] {
                    // A shorter way to the root.
                    parent[q] = dir ^ 1;
                    stamp[q] = stamp[p];
                    dist[q] = dist[p] + 1;
                }
            }
            if touch.is_some() {
                break;
            }
            active[p] = false;
            queue.pop_front();
        }
        let Some((a, d, b)) = touch else { break };
        time += 1;
        // The bottleneck along source → a → b → sink.
        let mut flow = res[usize::from(d)][a];
        let mut x = a;
        while parent[x] != TERMINAL {
            let pd = parent[x];
            let y = neighbour(x, pd).unwrap_or(x);
            flow = flow.min(res[usize::from(pd ^ 1)][y]);
            x = y;
        }
        let mut x = b;
        while parent[x] != TERMINAL {
            let pd = parent[x];
            flow = flow.min(res[usize::from(pd)][x]);
            x = neighbour(x, pd).unwrap_or(x);
        }
        // Push it, orphaning every node whose parent arc saturates.
        res[usize::from(d)][a] -= flow;
        res[usize::from(d ^ 1)][b] += flow;
        let mut x = a;
        while parent[x] != TERMINAL {
            let pd = parent[x];
            let y = neighbour(x, pd).unwrap_or(x);
            res[usize::from(pd ^ 1)][y] -= flow;
            res[usize::from(pd)][x] += flow;
            if res[usize::from(pd ^ 1)][y] == 0 {
                parent[x] = ORPHAN;
                orphans.push_back(x);
            }
            x = y;
        }
        let mut x = b;
        while parent[x] != TERMINAL {
            let pd = parent[x];
            let y = neighbour(x, pd).unwrap_or(x);
            res[usize::from(pd)][x] -= flow;
            res[usize::from(pd ^ 1)][y] += flow;
            if res[usize::from(pd)][x] == 0 {
                parent[x] = ORPHAN;
                orphans.push_back(x);
            }
            x = y;
        }
        // Adopt each orphan into its tree, or free it.
        while let Some(p) = orphans.pop_front() {
            steps += 1;
            if steps.is_multiple_of(CUT_CHECK) {
                cancel.check()?;
            }
            let side = tree[p];
            let mut best: Option<(u32, u8)> = None;
            for dir in 0..4_u8 {
                let Some(q) = neighbour(p, dir) else { continue };
                if tree[q] != side || tree_arc(&res, side, p, dir, q) <= 0 {
                    continue;
                }
                // Does q still reach its root?
                let mut j = q;
                let mut length = 0_u32;
                let reaches = loop {
                    if stamp[j] == time {
                        length += dist[j];
                        break true;
                    }
                    length += 1;
                    match parent[j] {
                        TERMINAL => {
                            stamp[j] = time;
                            dist[j] = 1;
                            break true;
                        }
                        ORPHAN => break false,
                        pd => j = neighbour(j, pd).unwrap_or(j),
                    }
                };
                if !reaches {
                    continue;
                }
                // Mark the walk with its distances for later walks.
                let mut j = q;
                let mut left = length;
                while stamp[j] != time {
                    stamp[j] = time;
                    dist[j] = left;
                    left -= 1;
                    j = neighbour(j, parent[j]).unwrap_or(j);
                }
                if best.is_none_or(|(b, _)| length < b) {
                    best = Some((length, dir));
                }
            }
            if let Some((length, dir)) = best {
                parent[p] = dir;
                stamp[p] = time;
                dist[p] = length + 1;
                continue;
            }
            // No way back: free p, waking its tree neighbours and orphaning its children.
            for dir in 0..4_u8 {
                let Some(q) = neighbour(p, dir) else { continue };
                if tree[q] != side {
                    continue;
                }
                if tree_arc(&res, side, p, dir, q) > 0 && !active[q] {
                    active[q] = true;
                    queue.push_back(q);
                }
                if parent[q] != TERMINAL
                    && parent[q] != ORPHAN
                    && neighbour(q, parent[q]) == Some(p)
                {
                    parent[q] = ORPHAN;
                    orphans.push_back(q);
                }
            }
            tree[p] = FREE;
            active[p] = false;
        }
    }
    Ok((0..n).map(|i| region[i] && tree[i] == S).collect())
}

/// The largest capacity the exact cut holds, so a reversed residual (at most twice it) fits.
const MAX_CAPACITY: u32 = 1 << 29;

/// Steps of the cut between cancellation checks.
const CUT_CHECK: usize = 1 << 14;

#[cfg(test)]
mod tests {
    use super::*;

    /// A brute-force minimum cut over every labelling of a small region.
    fn brute(
        width: usize,
        region: &[bool],
        source: &[bool],
        sink: &[bool],
        right: &[u16],
        down: &[u16],
    ) -> u64 {
        let cells: Vec<usize> = (0..region.len()).filter(|&i| region[i]).collect();
        let mut best = u64::MAX;
        for mask in 0..(1_u32 << cells.len()) {
            let mut side = vec![false; region.len()];
            for (k, &c) in cells.iter().enumerate() {
                side[c] = mask & (1 << k) != 0;
            }
            if cells
                .iter()
                .any(|&c| (source[c] && !side[c]) || (sink[c] && !source[c] && side[c]))
            {
                continue;
            }
            let mut cost = 0_u64;
            for &c in &cells {
                if c % width + 1 < width && region[c + 1] && side[c] != side[c + 1] {
                    cost += u64::from(right[c]);
                }
                if c + width < region.len() && region[c + width] && side[c] != side[c + width] {
                    cost += u64::from(down[c]);
                }
            }
            best = best.min(cost);
        }
        best
    }

    #[test]
    fn panorama_stitch_min_cut_matches_brute_force() {
        let mut draws = super::super::linalg::Draws(7);
        for trial in 0..60 {
            let (w, h) = (2 + trial % 3, 2 + (trial / 3) % 3);
            let n = w * h;
            let region: Vec<bool> = (0..n).map(|_| draws.below(5) > 0).collect();
            let source: Vec<bool> = (0..n).map(|_| draws.below(4) == 0).collect();
            let sink: Vec<bool> = (0..n).map(|_| draws.below(4) == 0).collect();
            let right: Vec<u16> = (0..n).map(|_| 1 + draws.below(9) as u16).collect();
            let down: Vec<u16> = (0..n).map(|_| 1 + draws.below(9) as u16).collect();
            let side = min_cut(
                w,
                h,
                &region,
                &source,
                &sink,
                &right,
                &down,
                &Cancel::never(),
            )
            .unwrap();
            let mut cost = 0_u64;
            for c in 0..n {
                if !region[c] {
                    continue;
                }
                assert!(
                    !source[c] || side[c],
                    "trial {trial}: a source cell left the source"
                );
                assert!(
                    !sink[c] || source[c] || !side[c],
                    "trial {trial}: a sink cell joined the source"
                );
                if c % w + 1 < w && region[c + 1] && side[c] != side[c + 1] {
                    cost += u64::from(right[c]);
                }
                if c + w < n && region[c + w] && side[c] != side[c + w] {
                    cost += u64::from(down[c]);
                }
            }
            assert_eq!(
                cost,
                brute(w, &region, &source, &sink, &right, &down),
                "trial {trial}"
            );
        }
    }

    /// A cut's cost, checking it keeps every terminal on its side.
    fn cut_cost(grid: &CutGrid<'_>, side: &[bool]) -> u64 {
        let w = grid.width;
        let mut cost = 0_u64;
        for c in 0..side.len() {
            if !grid.region[c] {
                continue;
            }
            assert!(!grid.source[c] || side[c], "a source cell left the source");
            assert!(
                !grid.sink[c] || grid.source[c] || !side[c],
                "a sink cell joined the source"
            );
            if c % w + 1 < w && grid.region[c + 1] && side[c] != side[c + 1] {
                cost += u64::from(grid.right[c]);
            }
            if c + w < side.len() && grid.region[c + w] && side[c] != side[c + w] {
                cost += u64::from(grid.down[c]);
            }
        }
        cost
    }

    #[test]
    fn panorama_stitch_banded_cut_is_near_the_exact_cut() {
        for trial in 0..4 {
            let (w, h) = (72, 60);
            let n = w * h;
            // An overlap with ragged ends, a winding low-cost valley, and noise.
            let region: Vec<bool> = (0..n)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    x >= (y * 7 + trial * 5) % 6 && x + (y * 3 + trial) % 5 < w
                })
                .collect();
            let source: Vec<bool> = (0..n).map(|i| region[i] && i % w < 6).collect();
            let sink: Vec<bool> = (0..n).map(|i| region[i] && i % w + 6 >= w).collect();
            let cost = |i: usize| {
                let (x, y) = (i % w, i / w);
                let valley = 36.0 + 12.0 * (y as f64 / 7.0 + trial as f64).sin();
                (((x as f64 - valley).abs() * 2.0).min(60.0) as u32) + draws_noise(i, trial)
            };
            let right: Vec<u32> = (0..n)
                .map(|i| cost(i) + cost((i + 1).min(n - 1)) + 1)
                .collect();
            let down: Vec<u32> = (0..n)
                .map(|i| cost(i) + cost((i + w).min(n - 1)) + 1)
                .collect();
            let grid = CutGrid {
                width: w,
                height: h,
                region: &region,
                source: &source,
                sink: &sink,
                right: &right,
                down: &down,
            };
            let exact = cut_cost(&grid, &exact_cut(&grid, &Cancel::never()).unwrap());
            let banded = cut_cost(&grid, &banded_cut(&grid, 256, &Cancel::never()).unwrap());
            assert!(banded >= exact, "trial {trial}");
            assert!(
                banded as f64 <= exact as f64 * (1.0 + BANDED_CUT_EXCESS),
                "trial {trial}: banded {banded} against exact {exact}"
            );
        }
    }

    /// Deterministic noise of 0 to 7.
    fn draws_noise(i: usize, trial: usize) -> u32 {
        let mut x = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ trial as u64;
        x ^= x >> 29;
        (x.wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 61) as u32
    }

    /// How far above the exact cut the banded cut may come, as a fraction.
    const BANDED_CUT_EXCESS: f64 = 0.10;
}
