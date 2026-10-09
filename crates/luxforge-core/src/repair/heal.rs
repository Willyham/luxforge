//! Heal: the membrane correction (`docs/design/corrections-numerics.md`, "Heal").
//!
//! For an operation with footprint `Ω` (the pixels with coverage above zero and a valid source),
//! Heal writes `(1 - a) * I(p) + a * D(E(S(p)) + f(p))` at every pixel of `Ω`, `S(p) = I(p + delta)`
//! and `E`, `D` the sRGB encode and decode extended over the real line. `f` is the membrane: zero
//! Laplacian over `Ω`, equal on the one-pixel ring around `Ω` to the encoded boundary difference
//! `E(I(q)) - E(S(q))`, with no condition (Neumann) where the ring leaves the stage or has no valid
//! source. It is solved by the frozen schedule: a start from each unknown's nearest boundary mean,
//! then three V(2, 2) multigrid cycles of red-black Gauss-Seidel. The study holds the schedule
//! within half a code of the converged membrane on its cases.
//!
//! This is the production transcription of `problem`, `start`, `solve` and `heal_with` in
//! `luxforge_reference::repair`: the same levels, classes, sums and order, so it is bit-identical to
//! the reference. Only the storage differs: each level is held in 16 x 16 tiles, allocated only
//! where the level has a cell that is not outside, so a long thin stroke across the frame costs its
//! own area and not its bounding box's. A cell in a tile that is not allocated is outside, which is
//! what the reference holds there, and a level past the first with no unknown cell changes
//! nothing, so the levels stop there.
//!
//! **Bounds.** The level-0 tiles are counted before anything is allocated and refused past
//! [`super::HEAL_CELLS_MAX`]; every level's storage is then reserved, and a failed reservation is
//! `resource-limit`. The work is `(start + 3 cycles x (2 + 2) iterations + residuals)` sweeps of the
//! cells, about 20 sweeps of the level-0 tiles. The token is checked per tile row of a scan and per
//! sweep.
use super::{RepairFrame, RepairOp, blend};
use crate::{Cancel, Error, colour::srgb};

/// A tile's side, in cells.
const TILE: usize = 16;
const TILE_CELLS: usize = TILE * TILE;

/// An unallocated tile.
const NONE: u32 = u32::MAX;

/// The frozen schedule: V-cycles, and red-black iterations before and after each coarse
/// correction.
const CYCLES: usize = 3;
const PRE: usize = 2;
const POST: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Outside,
    Unknown,
    Fixed,
}

/// One level of the solve, held in tiles.
struct Level {
    width: usize,
    height: usize,
    tiles_x: usize,
    /// Each tile's slot, or [`NONE`].
    slot: Vec<u32>,
    /// Each slot's tile, `(tx, ty)`, in raster order.
    tiles: Vec<(usize, usize)>,
    class: Vec<Class>,
    f: Vec<[f64; 3]>,
    /// The right-hand side, empty on level 0 (where it is zero).
    rhs: Vec<[f64; 3]>,
    unknowns: usize,
}

const ZERO: [f64; 3] = [0.0; 3];

#[inline]
fn add(sum: &mut [f64; 3], v: [f64; 3]) {
    for (s, x) in sum.iter_mut().zip(v) {
        *s += x;
    }
}

impl Level {
    #[inline]
    fn cell(&self, i: usize, j: usize) -> Option<usize> {
        if i >= self.width || j >= self.height {
            return None;
        }
        let slot = self.slot[(j / TILE) * self.tiles_x + i / TILE];
        (slot != NONE).then(|| slot as usize * TILE_CELLS + (j % TILE) * TILE + i % TILE)
    }

    #[inline]
    fn class_at(&self, i: usize, j: usize) -> Class {
        self.cell(i, j).map_or(Class::Outside, |c| self.class[c])
    }

    #[inline]
    fn rhs(&self, cell: usize) -> [f64; 3] {
        if self.rhs.is_empty() {
            ZERO
        } else {
            self.rhs[cell]
        }
    }

    /// The cells of every allocated tile, `(cell, i, j)`, slot by slot.
    fn cells(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.tiles
            .iter()
            .enumerate()
            .flat_map(move |(slot, &(tx, ty))| {
                (0..TILE_CELLS).filter_map(move |k| {
                    let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
                    (i < self.width && j < self.height).then_some((slot * TILE_CELLS + k, i, j))
                })
            })
    }

    /// The sum of `(i, j)`'s non-outside neighbours' values in the order `-x, +x, -y, +y`, added to
    /// `start`, and their count.
    #[inline]
    fn neighbours(&self, i: usize, j: usize, start: [f64; 3]) -> ([f64; 3], usize) {
        let mut sum = start;
        let mut count = 0usize;
        for (x, y) in [
            (i.wrapping_sub(1), j),
            (i + 1, j),
            (i, j.wrapping_sub(1)),
            (i, j + 1),
        ] {
            if let Some(n) = self.cell(x, y)
                && self.class[n] != Class::Outside
            {
                add(&mut sum, self.f[n]);
                count += 1;
            }
        }
        (sum, count)
    }

    /// One red-black Gauss-Seidel iteration on `a(f) = rhs`.
    fn sweep(&mut self, cancel: &Cancel) -> Result<(), Error> {
        for parity in [0, 1] {
            cancel.check()?;
            for slot in 0..self.tiles.len() {
                let (tx, ty) = self.tiles[slot];
                for k in 0..TILE_CELLS {
                    let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
                    let cell = slot * TILE_CELLS + k;
                    if i >= self.width
                        || j >= self.height
                        || (i + j) % 2 != parity
                        || self.class[cell] != Class::Unknown
                    {
                        continue;
                    }
                    let (sum, count) = self.neighbours(i, j, self.rhs(cell));
                    if count > 0 {
                        self.f[cell] = sum.map(|s| s / count as f64);
                    }
                }
            }
        }
        Ok(())
    }

    /// The residual `rhs - a(f)` at unknown cell `(i, j)`.
    #[inline]
    fn residual(&self, cell: usize, i: usize, j: usize) -> [f64; 3] {
        let (sum, count) = self.neighbours(i, j, ZERO);
        let rhs = self.rhs(cell);
        let f = self.f[cell];
        [0, 1, 2].map(|c| rhs[c] - (count as f64 * f[c] - sum[c]))
    }
}

/// The level above `fine`: `ceil` halves, a tile wherever a child tile is allocated, each cell
/// fixed if a child is fixed, else unknown if one is, else outside; values zero. Reserved before it
/// is allocated.
fn coarser(fine: &Level) -> Result<Level, Error> {
    let width = fine.width.div_ceil(2);
    let height = fine.height.div_ceil(2);
    let tiles_x = width.div_ceil(TILE);
    let tiles_y = height.div_ceil(TILE);
    let mut slot = vec![NONE; tiles_x * tiles_y];
    for &(tx, ty) in &fine.tiles {
        slot[(ty / 2) * tiles_x + tx / 2] = 0;
    }
    let mut tiles = Vec::new();
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let at = ty * tiles_x + tx;
            if slot[at] != NONE {
                slot[at] = tiles.len() as u32;
                tiles.push((tx, ty));
            }
        }
    }
    let cells = tiles.len() * TILE_CELLS;
    let mut level = Level {
        width,
        height,
        tiles_x,
        slot,
        tiles,
        class: reserved(cells, Class::Outside)?,
        f: reserved(cells, ZERO)?,
        rhs: reserved(cells, ZERO)?,
        unknowns: 0,
    };
    for slot in 0..level.tiles.len() {
        let (tx, ty) = level.tiles[slot];
        for k in 0..TILE_CELLS {
            let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
            if i >= width || j >= height {
                continue;
            }
            let (mut fixed, mut unknown) = (false, false);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                match fine.class_at(2 * i + dx, 2 * j + dy) {
                    Class::Fixed => fixed = true,
                    Class::Unknown => unknown = true,
                    Class::Outside => {}
                }
            }
            level.class[slot * TILE_CELLS + k] = if fixed {
                Class::Fixed
            } else if unknown {
                level.unknowns += 1;
                Class::Unknown
            } else {
                Class::Outside
            };
        }
    }
    Ok(level)
}

/// `len` copies of `value`, reserved first: a failed reservation is `resource-limit`.
fn reserved<T: Clone>(len: usize, value: T) -> Result<Vec<T>, Error> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| Error::resource_limit(format!("Heal scratch of {len} cells")))?;
    v.resize(len, value);
    Ok(v)
}

/// The start: up the levels each fixed cell takes the mean of its fixed children, then down from
/// the coarsest each unknown takes its parent's value.
fn start(levels: &mut [Level]) {
    for k in 1..levels.len() {
        let (fine, coarse) = levels.split_at_mut(k);
        let (fine, coarse) = (&fine[k - 1], &mut coarse[0]);
        for slot in 0..coarse.tiles.len() {
            let (tx, ty) = coarse.tiles[slot];
            for k in 0..TILE_CELLS {
                let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
                let cell = slot * TILE_CELLS + k;
                coarse.f[cell] = ZERO;
                if i >= coarse.width || j >= coarse.height || coarse.class[cell] != Class::Fixed {
                    continue;
                }
                let mut sum = ZERO;
                let mut n = 0usize;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    if let Some(child) = fine.cell(2 * i + dx, 2 * j + dy)
                        && fine.class[child] == Class::Fixed
                    {
                        add(&mut sum, fine.f[child]);
                        n += 1;
                    }
                }
                coarse.f[cell] = sum.map(|v| v / n as f64);
            }
        }
    }
    for k in (0..levels.len().saturating_sub(1)).rev() {
        let (fine, coarse) = levels.split_at_mut(k + 1);
        let (fine, coarse) = (&mut fine[k], &coarse[0]);
        for slot in 0..fine.tiles.len() {
            let (tx, ty) = fine.tiles[slot];
            for k in 0..TILE_CELLS {
                let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
                let cell = slot * TILE_CELLS + k;
                if i < fine.width && j < fine.height && fine.class[cell] == Class::Unknown {
                    let parent = coarse.cell(i / 2, j / 2).expect("an unknown's parent tile");
                    fine.f[cell] = coarse.f[parent];
                }
            }
        }
    }
}

/// One V-cycle on `levels[0]`.
fn v_cycle(levels: &mut [Level], cancel: &Cancel) -> Result<(), Error> {
    let (level, coarser) = levels.split_first_mut().expect("a level");
    for _ in 0..PRE {
        level.sweep(cancel)?;
    }
    if let Some(coarse) = coarser.first_mut() {
        cancel.check()?;
        for slot in 0..coarse.tiles.len() {
            let (tx, ty) = coarse.tiles[slot];
            for k in 0..TILE_CELLS {
                let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
                let cell = slot * TILE_CELLS + k;
                coarse.f[cell] = ZERO;
                coarse.rhs[cell] = ZERO;
                if i >= coarse.width || j >= coarse.height || coarse.class[cell] != Class::Unknown {
                    continue;
                }
                let mut sum = ZERO;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (x, y) = (2 * i + dx, 2 * j + dy);
                    if let Some(child) = level.cell(x, y)
                        && level.class[child] == Class::Unknown
                    {
                        add(&mut sum, level.residual(child, x, y));
                    }
                }
                coarse.rhs[cell] = sum;
            }
        }
        v_cycle(coarser, cancel)?;
        let coarse = &coarser[0];
        for slot in 0..level.tiles.len() {
            let (tx, ty) = level.tiles[slot];
            for k in 0..TILE_CELLS {
                let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
                let cell = slot * TILE_CELLS + k;
                if i >= level.width || j >= level.height || level.class[cell] != Class::Unknown {
                    continue;
                }
                if let Some(parent) = coarse.cell(i / 2, j / 2)
                    && coarse.class[parent] == Class::Unknown
                {
                    add(&mut level.f[cell], coarse.f[parent]);
                }
            }
        }
    }
    for _ in 0..POST {
        level.sweep(cancel)?;
    }
    Ok(())
}

/// One Heal over `frame`, in place. The frame holds the operation's declared reads.
pub(crate) fn heal(
    frame: &mut RepairFrame<'_>,
    op: &RepairOp,
    cancel: &Cancel,
) -> Result<(), Error> {
    heal_bounded(frame, op, cancel, super::HEAL_CELLS_MAX)
}

/// [`heal`] refusing more than `limit` level-0 cells.
pub(crate) fn heal_bounded(
    frame: &mut RepairFrame<'_>,
    op: &RepairOp,
    cancel: &Cancel,
    limit: usize,
) -> Result<(), Error> {
    let stage = op.stage();
    let Some(rect) = op.footprint.solve_rect() else {
        return Ok(());
    };
    let (rx, ry) = (rect.x0, rect.y0);
    let (width, height) = (rect.width as usize, rect.height as usize);
    let tiles_x = width.div_ceil(TILE);
    let tiles_y = height.div_ceil(TILE);
    // The tiles holding an unknown or a cell beside one.
    let mut slot = vec![NONE; tiles_x * tiles_y];
    for (n, (x, y)) in op.reached().enumerate() {
        if n % 65536 == 0 {
            cancel.check()?;
        }
        if op.alpha(x, y) > 0.0 {
            let (i, j) = ((x - rx) as usize, (y - ry) as usize);
            for (ni, nj) in [
                (i, j),
                (i.wrapping_sub(1), j),
                (i + 1, j),
                (i, j.wrapping_sub(1)),
                (i, j + 1),
            ] {
                if ni < width && nj < height {
                    slot[(nj / TILE) * tiles_x + ni / TILE] = 0;
                }
            }
        }
    }
    let mut tiles = Vec::new();
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let at = ty * tiles_x + tx;
            if slot[at] != NONE {
                slot[at] = tiles.len() as u32;
                tiles.push((tx, ty));
            }
        }
    }
    if tiles.is_empty() {
        return Ok(());
    }
    let cells = tiles.len() * TILE_CELLS;
    if cells > limit {
        return Err(Error::resource_limit(format!(
            "a Heal of {cells} cells is past the {limit} cells one Heal may solve"
        )));
    }
    let mut finest = Level {
        width,
        height,
        tiles_x,
        slot,
        tiles,
        class: reserved(cells, Class::Outside)?,
        f: reserved(cells, ZERO)?,
        rhs: Vec::new(),
        unknowns: 0,
    };
    // Classes and boundary differences.
    let encoded = |rgb: [f32; 3]| rgb.map(|v| srgb::encode(f64::from(v)));
    for slot in 0..finest.tiles.len() {
        cancel.check()?;
        let (tx, ty) = finest.tiles[slot];
        for k in 0..TILE_CELLS {
            let (i, j) = (tx * TILE + k % TILE, ty * TILE + k / TILE);
            if i >= width || j >= height {
                continue;
            }
            let (x, y) = (rx + i as u32, ry + j as u32);
            let cell = slot * TILE_CELLS + k;
            if op.alpha(x, y) > 0.0 {
                finest.class[cell] = Class::Unknown;
                finest.unknowns += 1;
            } else if op.valid(x, y) {
                let adjacent = (x > 0 && op.alpha(x - 1, y) > 0.0)
                    || (x + 1 < stage.width && op.alpha(x + 1, y) > 0.0)
                    || (y > 0 && op.alpha(x, y - 1) > 0.0)
                    || (y + 1 < stage.height && op.alpha(x, y + 1) > 0.0);
                if adjacent {
                    let i_q = encoded(frame.get(x, y));
                    let s_q = encoded(source(frame, op, x, y));
                    finest.class[cell] = Class::Fixed;
                    finest.f[cell] = [0, 1, 2].map(|c| i_q[c] - s_q[c]);
                }
            }
        }
    }
    // The levels, up to the first with no unknown (whose fixed means still start the one below).
    let mut levels = vec![finest];
    while let Some(last) = levels.last()
        && (last.width > 1 || last.height > 1)
        && (levels.len() == 1 || last.unknowns > 0)
    {
        cancel.check()?;
        let next = coarser(last)?;
        levels.push(next);
    }
    start(&mut levels);
    for _ in 0..CYCLES {
        v_cycle(&mut levels, cancel)?;
    }
    // The composite into the finest level's values, then the frame: a source pixel inside the
    // footprint is read before any pixel is written.
    let finest = &mut levels[0];
    let cells: Vec<(usize, usize, usize)> = finest
        .cells()
        .filter(|&(cell, _, _)| finest.class[cell] == Class::Unknown)
        .collect();
    for &(cell, i, j) in &cells {
        let (x, y) = (rx + i as u32, ry + j as u32);
        let s = source(frame, op, x, y);
        let f = finest.f[cell];
        let healed = [0, 1, 2].map(|c| srgb::decode(srgb::encode(f64::from(s[c])) + f[c]));
        let out = blend(op.alpha(x, y), frame.get(x, y), healed);
        finest.f[cell] = out.map(f64::from);
    }
    cancel.check()?;
    for &(cell, i, j) in &cells {
        let out = finest.f[cell].map(|v| v as f32);
        frame.set(rx + i as u32, ry + j as u32, out);
    }
    Ok(())
}

/// The source pixel of `(x, y)`, which is valid.
#[inline]
fn source(frame: &RepairFrame<'_>, op: &RepairOp, x: u32, y: u32) -> [f32; 3] {
    frame.get(
        (i64::from(x) + op.delta[0]) as u32,
        (i64::from(y) + op.delta[1]) as u32,
    )
}
