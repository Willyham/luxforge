//! Independent f64 reference for the Clone and Heal repair contract and the automatic source
//! search (`docs/design/corrections-numerics.md`).
//!
//! Written from the numerics document, never from the core: the core's `repair` units are
//! held to this file by exact-buffer tests, and the GPU programs to it within the spatial class's
//! declared tolerance. Every function here is a literal transcription of an equation the document
//! freezes, in the same order, so a production unit that writes the same expressions in the same
//! order is bit-identical to it.
//!
//! The pieces, in the order a repair layer evaluates them:
//!
//! - **Coverage** ([`Compiled::coverage`]): the mask brush's capsule profile
//!   ([`crate::mask::capsule_profile`]'s equation) at the distance to the operation's nearest
//!   segment, times `opacity / 100`.
//! - **Source** ([`delta`]): a stored offset (source minus destination, in stored normalized
//!   units) rounded to a whole pixel of the stage, half away from zero. No interpolation.
//! - **Validity**: a destination pixel whose source pixel is outside the stage is not repaired.
//! - **Clone** ([`clone`]): `(1 - a) * I(p) + a * I(p + delta)`.
//! - **Heal** ([`heal`]): Clone's source plus a correction `f`, solved in the sRGB-encoded domain
//!   as a membrane over the operation's footprint with the boundary difference as its Dirichlet
//!   values, by the frozen multigrid schedule ([`SCHEDULE`]: three V(2, 2) cycles of red-black
//!   Gauss-Seidel from a zero start).
//! - **Order** ([`apply`]): operations in stored order, each reading only the image the ones
//!   before it left, rounded to `f32` between operations as the stage's planes hold it.
//! - **Search** ([`search`]): the deterministic automatic source.
//!
//! Nothing here validates a payload: production refuses an illegal one before any of this runs.

use crate::{
    mask::{self, Segment, Stage},
    srgb,
};

/// `sum += v`, channel by channel in order.
fn add(sum: &mut [f64; 3], v: [f64; 3]) {
    for (s, x) in sum.iter_mut().zip(v) {
        *s += x;
    }
}

/// A planar-equivalent linear-light RGB image, row by row. Values are finite and may lie outside
/// `[0, 1]` (a developed RAW's continuous range).
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[f64; 3]>,
}

impl Image {
    pub fn new(width: usize, height: usize, pixels: Vec<[f64; 3]>) -> Self {
        assert!(width > 0 && height > 0);
        assert_eq!(pixels.len(), width * height);
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn stage(&self) -> Stage {
        Stage::new(self.width as u32, self.height as u32)
    }

    pub fn at(&self, x: usize, y: usize) -> [f64; 3] {
        self.pixels[y * self.width + x]
    }

    /// Every value rounded to the nearest `f32`, as the stage's planes hold it.
    pub fn rounded(&self) -> Self {
        Self {
            pixels: self
                .pixels
                .iter()
                .map(|p| p.map(|v| f64::from(v as f32)))
                .collect(),
            ..*self
        }
    }
}

/// Which repair an operation performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Clone,
    Heal,
}

/// One stored operation, as the numerics document names its fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub kind: Kind,
    /// The painted paths, each a list of stored normalized positions (`x` a fraction of the
    /// content width, `y` of its height). One path for a spot or a stroke.
    pub paths: Vec<Vec<[f64; 2]>>,
    /// The brush radius in content-height units: the outer edge of the footprint.
    pub radius: f64,
    /// The percentage of the radius the edge ramps over, `0..=100`; `0` is a hard edge.
    pub feather: f64,
    /// `0..=100`.
    pub opacity: f64,
    /// Source anchor minus destination anchor, in stored normalized units (`x` a fraction of the
    /// width, `y` of the height): a stored offset of `k` grid steps is `k / 16384`.
    pub offset: [f64; 2],
}

/// The source displacement in whole stage pixels: `round(offset_x * W)`, `round(offset_y * H)`,
/// half away from zero (`f64::round`). A grid-stored offset is a multiple of `1 / 16384`, so the
/// products are exact and every implementation rounds the same value.
pub fn delta(offset: [f64; 2], stage: &Stage) -> [i64; 2] {
    [
        (offset[0] * f64::from(stage.width)).round() as i64,
        (offset[1] * f64::from(stage.height)).round() as i64,
    ]
}

/// An operation bound to a stage: its segments in mask space and the per-operation terms.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub kind: Kind,
    pub segments: Vec<Segment>,
    pub radius: f64,
    /// `radius * (feather / 100)`, a divisor, with the hard edge as its own branch.
    pub band: f64,
    /// `opacity / 100`.
    pub amount: f64,
    pub delta: [i64; 2],
}

impl Compiled {
    pub fn new(op: &Operation, stage: &Stage) -> Self {
        let mut segments = Vec::new();
        for path in &op.paths {
            let uv: Vec<(f64, f64)> = path
                .iter()
                .map(|[x, y]| stage.position_uv(*x, *y))
                .collect();
            if uv.len() == 1 {
                segments.push(mask::segment(uv[0], uv[0]));
            } else {
                for pair in uv.windows(2) {
                    segments.push(mask::segment(pair[0], pair[1]));
                }
            }
        }
        Self {
            kind: op.kind,
            segments,
            radius: op.radius,
            band: op.radius * (op.feather / 100.0),
            amount: op.opacity / 100.0,
            delta: delta(op.offset, stage),
        }
    }

    /// The distance from mask-space point `(u, v)` to the nearest segment: one square root of the
    /// smallest squared distance.
    pub fn distance(&self, u: f64, v: f64) -> f64 {
        let mut nearest = f64::INFINITY;
        for segment in &self.segments {
            let d2 = mask::segment_distance2(segment, u, v);
            if d2 < nearest {
                nearest = d2;
            }
        }
        nearest.sqrt()
    }

    /// The capsule profile at distance `d`:
    ///
    /// ```text
    /// s = if band == 0 { if d <= R { 1 } else { 0 } }
    ///     else         { smooth(clamp((R - d) / band, 0, 1)) }
    /// ```
    pub fn profile(&self, d: f64) -> f64 {
        if self.band == 0.0 {
            if d <= self.radius { 1.0 } else { 0.0 }
        } else {
            mask::smooth(((self.radius - d) / self.band).clamp(0.0, 1.0))
        }
    }

    /// Coverage at stage pixel `(px, py)`: `profile(d) * (opacity / 100)` at the pixel's centre.
    pub fn coverage(&self, stage: &Stage, px: usize, py: usize) -> f64 {
        let (u, v) = stage.pixel_uv(px as u32, py as u32);
        self.profile(self.distance(u, v)) * self.amount
    }

    /// Whether stage pixel `(px, py)`'s source lies inside the stage.
    pub fn valid(&self, stage: &Stage, px: usize, py: usize) -> bool {
        let sx = px as i64 + self.delta[0];
        let sy = py as i64 + self.delta[1];
        sx >= 0 && sy >= 0 && sx < i64::from(stage.width) && sy < i64::from(stage.height)
    }

    fn source(&self, px: usize, py: usize) -> (usize, usize) {
        (
            (px as i64 + self.delta[0]) as usize,
            (py as i64 + self.delta[1]) as usize,
        )
    }

    /// The repaired coverage: coverage where the source is valid, else `0`.
    fn alpha(&self, stage: &Stage, px: usize, py: usize) -> f64 {
        if self.valid(stage, px, py) {
            self.coverage(stage, px, py)
        } else {
            0.0
        }
    }
}

/// `(1 - a) * i + a * h`, the one blend both repairs end in. `a = 0` is skipped by the caller,
/// so the pixel is the input exactly.
fn blend(a: f64, i: [f64; 3], h: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|c| (1.0 - a) * i[c] + a * h[c])
}

/// One Clone operation over `image`: every pixel with `a > 0` becomes
/// `(1 - a) * I(p) + a * I(p + delta)`; every other pixel is unchanged. Not rounded.
pub fn clone(image: &Image, op: &Compiled) -> Image {
    let stage = image.stage();
    let mut out = image.clone();
    for y in 0..image.height {
        for x in 0..image.width {
            let a = op.alpha(&stage, x, y);
            if a > 0.0 {
                let (sx, sy) = op.source(x, y);
                out.pixels[y * image.width + x] = blend(a, image.at(x, y), image.at(sx, sy));
            }
        }
    }
    out
}

/// The frozen schedule of the Heal solver: `cycles` multigrid V-cycles from a zero start, each with
/// `pre` red-black Gauss-Seidel iterations before the coarse correction and `post` after it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Schedule {
    /// Whether the unknowns start from their nearest fixed ancestor's boundary mean
    /// ([`start`]) rather than from zero.
    pub start: bool,
    pub cycles: usize,
    pub pre: usize,
    pub post: usize,
}

/// The schedule the numerics document freezes: three V(2, 2) cycles.
pub const SCHEDULE: Schedule = Schedule {
    start: true,
    cycles: 3,
    pre: 2,
    post: 2,
};

/// A cell's role in one level of the solve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Not part of the problem: Neumann, left out of every neighbour sum.
    Outside,
    /// An unknown: at the finest level a footprint pixel whose source is valid.
    Unknown,
    /// A Dirichlet cell: at the finest level the boundary difference, at a coarser level a zero
    /// correction.
    Fixed,
}

/// One level of the solve: its classes and values, row by row.
#[derive(Clone, Debug)]
pub struct Level {
    pub width: usize,
    pub height: usize,
    pub class: Vec<Class>,
    pub f: Vec<[f64; 3]>,
}

/// The four neighbours of cell `(i, j)` in the frozen order `-x, +x, -y, +y`, those inside the
/// level.
fn neighbours(width: usize, height: usize, i: usize, j: usize) -> impl Iterator<Item = usize> {
    [
        (i.wrapping_sub(1), j),
        (i + 1, j),
        (i, j.wrapping_sub(1)),
        (i, j + 1),
    ]
    .into_iter()
    .filter(move |&(x, y)| x < width && y < height)
    .map(move |(x, y)| y * width + x)
}

impl Level {
    /// The next coarser level's classes: `ceil` halves, each cell over its up to four children
    /// `(2i, 2j), (2i+1, 2j), (2i, 2j+1), (2i+1, 2j+1)`: fixed if any child is fixed, else unknown if
    /// any child is, else outside. Its values are zero.
    pub fn coarser(&self) -> Self {
        let width = self.width.div_ceil(2);
        let height = self.height.div_ceil(2);
        let mut class = vec![Class::Outside; width * height];
        for j in 0..height {
            for i in 0..width {
                let mut fixed = false;
                let mut unknown = false;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (x, y) = (2 * i + dx, 2 * j + dy);
                    if x >= self.width || y >= self.height {
                        continue;
                    }
                    match self.class[y * self.width + x] {
                        Class::Fixed => fixed = true,
                        Class::Unknown => unknown = true,
                        Class::Outside => {}
                    }
                }
                class[j * width + i] = if fixed {
                    Class::Fixed
                } else if unknown {
                    Class::Unknown
                } else {
                    Class::Outside
                };
            }
        }
        Self {
            width,
            height,
            f: vec![[0.0; 3]; width * height],
            class,
        }
    }

    /// One red-black Gauss-Seidel iteration on `a(f) = rhs`: every unknown cell with `(i + j)` even,
    /// then every one with it odd, set to
    ///
    /// ```text
    /// f(c) = (rhs(c) + f(n1) + f(n2) + ...) / count
    /// ```
    ///
    /// over its non-outside neighbours in the order `-x, +x, -y, +y`, summed from `rhs(c)` in that
    /// order. A cell with no such neighbour keeps its value. Within one colour no cell reads
    /// another of its colour, so the order of the cells does not matter.
    pub fn sweep(&mut self, rhs: &[[f64; 3]]) {
        for parity in [0, 1] {
            for j in 0..self.height {
                for i in 0..self.width {
                    let at = j * self.width + i;
                    if (i + j) % 2 != parity || self.class[at] != Class::Unknown {
                        continue;
                    }
                    let mut sum = rhs[at];
                    let mut count = 0usize;
                    for k in neighbours(self.width, self.height, i, j) {
                        if self.class[k] == Class::Outside {
                            continue;
                        }
                        add(&mut sum, self.f[k]);
                        count += 1;
                    }
                    if count > 0 {
                        self.f[at] = sum.map(|s| s / count as f64);
                    }
                }
            }
        }
    }

    /// The residual `rhs - a(f)` at each unknown cell, zero elsewhere, with
    /// `a(f)(c) = count * f(c) - (f(n1) + f(n2) + ...)`, the neighbour sum from zero in the frozen
    /// order.
    pub fn residual(&self, rhs: &[[f64; 3]]) -> Vec<[f64; 3]> {
        let mut r = vec![[0.0; 3]; self.f.len()];
        for j in 0..self.height {
            for i in 0..self.width {
                let at = j * self.width + i;
                if self.class[at] != Class::Unknown {
                    continue;
                }
                let mut sum = [0.0; 3];
                let mut count = 0usize;
                for k in neighbours(self.width, self.height, i, j) {
                    if self.class[k] == Class::Outside {
                        continue;
                    }
                    add(&mut sum, self.f[k]);
                    count += 1;
                }
                for c in 0..3 {
                    r[at][c] = rhs[at][c] - (count as f64 * self.f[at][c] - sum[c]);
                }
            }
        }
        r
    }
}

/// The coarse right-hand side: each unknown coarse cell the sum of its unknown children's
/// residuals, in the order `(0,0), (1,0), (0,1), (1,1)`; zero elsewhere.
fn restrict(fine: &Level, residual: &[[f64; 3]], coarse: &Level) -> Vec<[f64; 3]> {
    let mut rhs = vec![[0.0; 3]; coarse.f.len()];
    for j in 0..coarse.height {
        for i in 0..coarse.width {
            if coarse.class[j * coarse.width + i] != Class::Unknown {
                continue;
            }
            let mut sum = [0.0; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = (2 * i + dx, 2 * j + dy);
                if x >= fine.width || y >= fine.height {
                    continue;
                }
                let at = y * fine.width + x;
                if fine.class[at] == Class::Unknown {
                    add(&mut sum, residual[at]);
                }
            }
            rhs[j * coarse.width + i] = sum;
        }
    }
    rhs
}

/// One V-cycle on `levels[0]`, `a(f) = rhs`, the coarser levels below it in `levels[1..]`:
/// `pre` iterations; then, unless this is the coarsest level, the residual restricted to the next
/// level, its correction from zero by one V-cycle there, and each unknown cell whose parent is
/// unknown corrected by its parent's value (piecewise constant); then `post` iterations.
fn v_cycle(levels: &mut [Level], rhs: &[[f64; 3]], schedule: Schedule) {
    let (level, coarser) = levels.split_first_mut().expect("a level");
    for _ in 0..schedule.pre {
        level.sweep(rhs);
    }
    if let Some(coarse) = coarser.first_mut() {
        let residual = level.residual(rhs);
        let coarse_rhs = restrict(level, &residual, coarse);
        coarse.f.iter_mut().for_each(|v| *v = [0.0; 3]);
        v_cycle(coarser, &coarse_rhs, schedule);
        let coarse = &coarser[0];
        for j in 0..level.height {
            for i in 0..level.width {
                let at = j * level.width + i;
                let parent = (j / 2) * coarse.width + i / 2;
                if level.class[at] == Class::Unknown && coarse.class[parent] == Class::Unknown {
                    for c in 0..3 {
                        level.f[at][c] += coarse.f[parent][c];
                    }
                }
            }
        }
    }
    for _ in 0..schedule.post {
        level.sweep(rhs);
    }
}

/// The membrane solve: the levels from the finest up to `1 x 1` ([`Level::coarser`]), the finest's
/// unknowns from zero and its fixed cells holding the boundary differences, then `schedule.cycles`
/// V-cycles on `a(f) = 0`. Answers the finest level.
pub fn solve(mut finest: Level, schedule: Schedule) -> Level {
    for (class, f) in finest.class.iter().zip(finest.f.iter_mut()) {
        if *class == Class::Unknown {
            *f = [0.0; 3];
        }
    }
    let mut levels = vec![finest];
    while levels.last().is_some_and(|l| l.width > 1 || l.height > 1) {
        let next = levels.last().expect("a level").coarser();
        levels.push(next);
    }
    if schedule.start {
        start(&mut levels);
    }
    let zero = vec![[0.0; 3]; levels[0].f.len()];
    for _ in 0..schedule.cycles {
        v_cycle(&mut levels, &zero, schedule);
    }
    levels.swap_remove(0)
}

/// The start of the finest level's unknowns: up the levels, each coarse fixed cell holds the mean
/// of its fixed children's values (summed in the child order, divided by their count); then down
/// from the coarsest, whose unknowns hold zero, each unknown takes its parent's value. So an
/// unknown starts from the boundary mean of the nearest enclosing block that holds boundary cells.
/// The coarse values are scratch: each V-cycle zeroes a coarse level before it solves there.
pub fn start(levels: &mut [Level]) {
    for k in 1..levels.len() {
        let (fine, coarse) = levels.split_at_mut(k);
        let (fine, coarse) = (&fine[k - 1], &mut coarse[0]);
        for j in 0..coarse.height {
            for i in 0..coarse.width {
                let at = j * coarse.width + i;
                coarse.f[at] = [0.0; 3];
                if coarse.class[at] != Class::Fixed {
                    continue;
                }
                let mut sum = [0.0; 3];
                let mut n = 0usize;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (x, y) = (2 * i + dx, 2 * j + dy);
                    if x < fine.width
                        && y < fine.height
                        && fine.class[y * fine.width + x] == Class::Fixed
                    {
                        let v = fine.f[y * fine.width + x];
                        add(&mut sum, v);
                        n += 1;
                    }
                }
                coarse.f[at] = sum.map(|v| v / n as f64);
            }
        }
    }
    for k in (0..levels.len().saturating_sub(1)).rev() {
        let (fine, coarse) = levels.split_at_mut(k + 1);
        let (fine, coarse) = (&mut fine[k], &coarse[0]);
        for j in 0..fine.height {
            for i in 0..fine.width {
                let at = j * fine.width + i;
                if fine.class[at] == Class::Unknown {
                    fine.f[at] = coarse.f[(j / 2) * coarse.width + i / 2];
                }
            }
        }
    }
}

/// The finest level of one Heal operation's solve: its rectangle in the stage (`x0, y0`), its
/// classes and the encoded boundary differences, with the coverage of each cell.
#[derive(Clone, Debug)]
pub struct Problem {
    pub x0: usize,
    pub y0: usize,
    pub level: Level,
    /// Coverage `a` of each cell, zero outside the footprint.
    pub alpha: Vec<f64>,
}

/// The finest level of `op`'s solve over `image`, or `None` when no pixel is repaired.
///
/// - The footprint `Ω`: pixels with coverage above zero and a valid source.
/// - The rectangle: `Ω`'s bounding box grown by one pixel on every side, clipped to the stage.
/// - Unknown: a pixel of `Ω`. Fixed: a pixel not in `Ω`, with a valid source, 4-adjacent to one
///   that is, holding `d = E(I(q)) - E(I(q + delta))` per channel, `E` the sRGB encode extended
///   over the real line (`srgb::encode_extended`). Outside: every other pixel.
pub fn problem(image: &Image, op: &Compiled) -> Option<Problem> {
    let stage = image.stage();
    let (w, h) = (image.width, image.height);
    let mut alpha = vec![0.0; w * h];
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let a = op.alpha(&stage, x, y);
            if a > 0.0 {
                alpha[y * w + x] = a;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let (x0, y0) = (x0.saturating_sub(1), y0.saturating_sub(1));
    let (x1, y1) = ((x1 + 1).min(w), (y1 + 1).min(h));
    let (lw, lh) = (x1 - x0, y1 - y0);
    let mut level = Level {
        width: lw,
        height: lh,
        class: vec![Class::Outside; lw * lh],
        f: vec![[0.0; 3]; lw * lh],
    };
    let mut cell_alpha = vec![0.0; lw * lh];
    let inside = |x: usize, y: usize| alpha[y * w + x] > 0.0;
    for j in 0..lh {
        for i in 0..lw {
            let (x, y) = (x0 + i, y0 + j);
            let at = j * lw + i;
            if inside(x, y) {
                level.class[at] = Class::Unknown;
                cell_alpha[at] = alpha[y * w + x];
            } else if op.valid(&stage, x, y) {
                let adjacent = (x > 0 && inside(x - 1, y))
                    || (x + 1 < w && inside(x + 1, y))
                    || (y > 0 && inside(x, y - 1))
                    || (y + 1 < h && inside(x, y + 1));
                if adjacent {
                    let (sx, sy) = op.source(x, y);
                    let (i_q, s_q) = (image.at(x, y), image.at(sx, sy));
                    level.class[at] = Class::Fixed;
                    level.f[at] = [0, 1, 2]
                        .map(|c| srgb::encode_extended(i_q[c]) - srgb::encode_extended(s_q[c]));
                }
            }
        }
    }
    Some(Problem {
        x0,
        y0,
        level,
        alpha: cell_alpha,
    })
}

/// One Heal operation over `image` with `schedule`: for every pixel of the footprint,
/// `(1 - a) * I(p) + a * D(E(S(p)) + f(p))`, `S(p) = I(p + delta)`, `E` and `D` the extended sRGB
/// encode and decode; every other pixel is unchanged. Not rounded.
pub fn heal_with(image: &Image, op: &Compiled, schedule: Schedule) -> Image {
    let mut out = image.clone();
    let Some(problem) = problem(image, op) else {
        return out;
    };
    let solved = solve(problem.level, schedule);
    for j in 0..solved.height {
        for i in 0..solved.width {
            let at = j * solved.width + i;
            if solved.class[at] != Class::Unknown {
                continue;
            }
            let (x, y) = (problem.x0 + i, problem.y0 + j);
            let (sx, sy) = op.source(x, y);
            let s = image.at(sx, sy);
            let f = solved.f[at];
            let healed =
                [0, 1, 2].map(|c| srgb::decode_encoded(srgb::encode_extended(s[c]) + f[c]));
            out.pixels[y * image.width + x] = blend(problem.alpha[at], image.at(x, y), healed);
        }
    }
    out
}

/// One Heal operation with the frozen [`SCHEDULE`].
pub fn heal(image: &Image, op: &Compiled) -> Image {
    heal_with(image, op, SCHEDULE)
}

/// Every operation in stored order: each reads only the image the ones before it left, and its
/// output is rounded to `f32`, as the stage's planes hold it, before the next reads it. The input
/// is taken as it is (its values are the `f32` planes the stage holds).
pub fn apply(image: &Image, ops: &[Operation]) -> Image {
    let stage = image.stage();
    let mut current = image.clone();
    for op in ops {
        let compiled = Compiled::new(op, &stage);
        current = match op.kind {
            Kind::Clone => clone(&current, &compiled),
            Kind::Heal => heal(&current, &compiled),
        }
        .rounded();
    }
    current
}

// ---- The automatic source --------------------------------------------------------------------

/// The four distances of the candidate rings, in multiples of the footprint's extent.
pub const RINGS: [f64; 4] = [1.5, 2.25, 3.25, 4.5];

/// Directions, at `15°` steps from `+x` (right) towards `+y` (down).
pub const DIRECTIONS: usize = 24;

/// The largest footprint, across, the search reads at, in reduced pixels.
pub const FOOTPRINT_ACROSS: f64 = 64.0;

/// The weight of the texture-energy term.
pub const TEXTURE_WEIGHT: f64 = 1.0;

/// The weight of the distance penalty, per multiple of the extent.
pub const DISTANCE_WEIGHT: f64 = 0.002;

/// The ring's width as a share of the reduced radius, and its least width in reduced pixels.
pub const RING_SHARE: f64 = 0.25;
pub const RING_MIN: f64 = 2.0;

/// Direction `j`'s unit vector, from square roots only (correctly rounded everywhere): the cosines
/// of multiples of `15°` are `0`, `(√6 − √2)/4`, `1/2`, `√2/2`, `√3/2`, `(√6 + √2)/4` and `1`.
pub fn direction(j: usize) -> [f64; 2] {
    let s2 = 2f64.sqrt();
    let s3 = 3f64.sqrt();
    let s6 = 6f64.sqrt();
    // cos of 0°, 15°, ..., 90°.
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
    let quadrant = k / 6;
    let r = k % 6;
    let (c, s) = (first[r], first[6 - r]);
    match quadrant {
        0 => [c, s],
        1 => [-s, c],
        2 => [-c, -s],
        _ => [s, -c],
    }
}

/// The footprint's extent `E` in content-height units: half the diagonal of the paths' points'
/// bounding box plus the radius (a spot's is its radius).
pub fn extent(op: &Operation, stage: &Stage) -> f64 {
    let (mut u0, mut v0, mut u1, mut v1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for path in &op.paths {
        for [x, y] in path {
            let (u, v) = stage.position_uv(*x, *y);
            u0 = u0.min(u);
            v0 = v0.min(v);
            u1 = u1.max(u);
            v1 = v1.max(v);
        }
    }
    let du = u1 - u0;
    let dv = v1 - v0;
    0.5 * (du * du + dv * dv).sqrt() + op.radius
}

/// The reduction the search reads at: the least power of two `r >= 1` with
/// `2 * E * H / r <= 64`.
pub fn reduction(extent: f64, stage: &Stage) -> usize {
    let across = 2.0 * extent * f64::from(stage.height);
    let mut r = 1usize;
    while across / r as f64 > FOOTPRINT_ACROSS && r < 1 << 14 {
        r *= 2;
    }
    r
}

/// `image` reduced by `r`: `ceil` sizes, each reduced pixel the mean of the stage pixels of its
/// `r x r` block that exist (summed row by row, left to right), then encoded per channel
/// (`srgb::encode_extended`).
pub fn reduced(image: &Image, r: usize) -> Image {
    let (w, h) = (image.width.div_ceil(r), image.height.div_ceil(r));
    let mut pixels = vec![[0.0; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0; 3];
            let mut n = 0usize;
            for sy in y * r..((y + 1) * r).min(image.height) {
                for sx in x * r..((x + 1) * r).min(image.width) {
                    let p = image.at(sx, sy);
                    add(&mut sum, p);
                    n += 1;
                }
            }
            pixels[y * w + x] = sum.map(|s| srgb::encode_extended(s / n as f64));
        }
    }
    Image::new(w, h, pixels)
}

/// One ranked candidate: its ring and direction, its offset in stored normalized units (not yet
/// on the grid), its reduced delta and its score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub ring: usize,
    pub direction: usize,
    pub offset: [f64; 2],
    pub delta: [i64; 2],
    pub score: f64,
}

/// What the search answers.
#[derive(Clone, Debug, PartialEq)]
pub enum Search {
    /// Every surviving candidate, best first: ascending score, then nearer ring, then smaller
    /// direction.
    Ranked(Vec<Candidate>),
    /// No candidate survived the rejection rules.
    NoSource,
}

/// The reduced-pixel sets of the destination: the footprint grown by one reduced pixel (`F+`),
/// the footprint (`F`) and the ring (`Q`), as sorted coordinate lists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sets {
    pub grown: Vec<[i64; 2]>,
    pub interior: Vec<[i64; 2]>,
    pub ring: Vec<[i64; 2]>,
}

/// The destination's sets at reduction `r`, with `d` measured in reduced pixels at each reduced
/// pixel's nominal centre `((x + 0.5) r / H, (y + 0.5) r / H)` of mask space:
/// `F: d <= R_r`, `F+: d <= R_r + 1`, `Q: R_r + 1 < d <= R_r + 1 + rho`, with
/// `R_r = R H / r` and `rho = max(0.25 R_r, 2)`.
pub fn sets(op: &Operation, stage: &Stage, r: usize) -> Sets {
    let compiled = Compiled::new(op, stage);
    let scale = f64::from(stage.height) / r as f64;
    let rr = op.radius * scale;
    let rho = (RING_SHARE * rr).max(RING_MIN);
    let reach = rr + 1.0 + rho;
    let (mut u0, mut v0, mut u1, mut v1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for s in &compiled.segments {
        u0 = u0.min(s.ax.min(s.ax + s.ex));
        v0 = v0.min(s.ay.min(s.ay + s.ey));
        u1 = u1.max(s.ax.max(s.ax + s.ex));
        v1 = v1.max(s.ay.max(s.ay + s.ey));
    }
    let x0 = (u0 * scale - reach).floor() as i64 - 1;
    let x1 = (u1 * scale + reach).ceil() as i64 + 1;
    let y0 = (v0 * scale - reach).floor() as i64 - 1;
    let y1 = (v1 * scale + reach).ceil() as i64 + 1;
    let mut found = Sets::default();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let u = (x as f64 + 0.5) * r as f64 / f64::from(stage.height);
            let v = (y as f64 + 0.5) * r as f64 / f64::from(stage.height);
            let d = compiled.distance(u, v) * scale;
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

/// The RMS of the per-channel differences across horizontally and vertically adjacent pairs of
/// `set` (both members in it), each pixel read at its place plus `shift`: `sqrt(sum / (3 n))`,
/// zero for no pair.
fn energy(image: &Image, set: &[[i64; 2]], shift: [i64; 2]) -> f64 {
    let member = |p: [i64; 2]| {
        set.binary_search_by(|q| (q[1], q[0]).cmp(&(p[1], p[0])))
            .is_ok()
    };
    let mut sum = 0.0;
    let mut n = 0usize;
    for &[x, y] in set {
        for next in [[x + 1, y], [x, y + 1]] {
            if member(next) {
                let a = image.at((x + shift[0]) as usize, (y + shift[1]) as usize);
                let b = image.at((next[0] + shift[0]) as usize, (next[1] + shift[1]) as usize);
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

/// The automatic source of `op` over `reduced`, the prefix before it reduced by `r` and encoded
/// ([`reduced`]), searching inside `window`, `[x0, y0, x1, y1)` of the reduced stage.
///
/// For ring `i` and direction `j`: `o = RINGS[i] * E * direction(j)` in mask space,
/// `delta = (round(o_u H / r), round(o_v H / r))`. Rejected when the bounding box of
/// `(F+ ∪ Q')` moved by `delta` leaves `window` (`Q'` the ring inside the window), or when
/// `F+` moved by `delta` meets `F+`. Scored
///
/// ```text
/// A = sqrt(sum over q in Q', c of (J_c(q) - J_c(q + delta))^2 / (3 |Q'|))     -- 0 for empty Q'
/// T = |energy(F moved by delta) - energy(Q')|
/// score = A + T + 0.002 RINGS[i]
/// ```
pub fn search(
    reduced: &Image,
    op: &Operation,
    stage: &Stage,
    r: usize,
    window: [i64; 4],
) -> Search {
    let e = extent(op, stage);
    let scale = f64::from(stage.height) / r as f64;
    let found = sets(op, stage, r);
    let inside = |p: [i64; 2]| {
        p[0] >= window[0] && p[1] >= window[1] && p[0] < window[2] && p[1] < window[3]
    };
    let ring: Vec<[i64; 2]> = found.ring.iter().copied().filter(|p| inside(*p)).collect();
    let ring_energy = energy(reduced, &ring, [0, 0]);
    let (mut bx0, mut by0, mut bx1, mut by1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for p in found.grown.iter().chain(&ring) {
        bx0 = bx0.min(p[0]);
        by0 = by0.min(p[1]);
        bx1 = bx1.max(p[0]);
        by1 = by1.max(p[1]);
    }
    let grown = &found.grown;
    let member = |p: [i64; 2]| {
        grown
            .binary_search_by(|q| (q[1], q[0]).cmp(&(p[1], p[0])))
            .is_ok()
    };
    let mut ranked = Vec::new();
    for (i, m) in RINGS.iter().enumerate() {
        for j in 0..DIRECTIONS {
            let [cu, cv] = direction(j);
            let ou = m * e * cu;
            let ov = m * e * cv;
            let delta = [(ou * scale).round() as i64, (ov * scale).round() as i64];
            // The frame and window.
            if bx0 + delta[0] < window[0]
                || by0 + delta[1] < window[1]
                || bx1 + delta[0] >= window[2]
                || by1 + delta[1] >= window[3]
            {
                continue;
            }
            // The destination.
            if grown
                .iter()
                .any(|p| member([p[0] + delta[0], p[1] + delta[1]]))
            {
                continue;
            }
            let mut sum = 0.0;
            for q in &ring {
                let a = reduced.at(q[0] as usize, q[1] as usize);
                let b = reduced.at((q[0] + delta[0]) as usize, (q[1] + delta[1]) as usize);
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
            let t = (energy(reduced, &found.interior, delta) - ring_energy).abs();
            let score = a + TEXTURE_WEIGHT * t + DISTANCE_WEIGHT * m;
            ranked.push(Candidate {
                ring: i,
                direction: j,
                offset: [ou / stage.aspect(), ov],
                delta,
                score,
            });
        }
    }
    if ranked.is_empty() {
        return Search::NoSource;
    }
    ranked.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then(a.ring.cmp(&b.ring))
            .then(a.direction.cmp(&b.direction))
    });
    Search::Ranked(ranked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_are_unit_vectors_at_fifteen_degree_steps() {
        for j in 0..DIRECTIONS {
            let [c, s] = direction(j);
            let angle = (j as f64 * 15.0).to_radians();
            assert!((c - angle.cos()).abs() < 1e-15, "{j}");
            assert!((s - angle.sin()).abs() < 1e-15, "{j}");
        }
        assert_eq!(direction(0), [1.0, 0.0]);
        assert_eq!(direction(6), [-0.0, 1.0]);
    }

    #[test]
    fn delta_rounds_half_away_from_zero() {
        let stage = Stage::new(100, 50);
        assert_eq!(delta([0.005, -0.01], &stage), [1, -1]);
        assert_eq!(delta([-0.005, 0.01], &stage), [-1, 1]);
        assert_eq!(delta([0.0, 0.0], &stage), [0, 0]);
    }
}
