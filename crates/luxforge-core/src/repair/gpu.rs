//! One group of repair operations as GPU data for `repair.wgsl` (`docs/design/corrections-numerics.md`,
//! "GPU form"): the block (operation table, segments, tile maps, reverse tables, the apply's cell
//! index), the passes and the planes, in the spatial convention's terms.
//!
//! A **group** is consecutive operations none of whose reads (destination, ring or source) meets an
//! earlier destination of the group, so all of them read the same input and one dispatch per pass
//! evaluates them all; the render integration chains groups. Built from the operations' geometry
//! alone, reading no pixel: `O(operations + segments x tiles near them + tiles)`.
//!
//! **Atlas.** Each level's planes hold 8 x 8 texel tiles, [`ATLAS_TILES_ACROSS`] across. An
//! operation's level-0 grid is its Heal solve rectangle ([`super::footprint::Footprint::solve_rect`]),
//! and a level-0 tile is placed when its centre lies within the radius plus the tile's half
//! diagonal and two pixels of the path (conservative: a placed tile may hold only outside cells,
//! which change nothing). A coarser tile is placed where a child tile is. Clone needs no solve, so
//! a Clone operation has level 0 alone.
//!
//! **Levels.** A Heal operation's hierarchy stops after the first level that can hold no unknown,
//! `floor(log2(2 R + 1)) + 2` levels for radius `R` pixels, since an unknown coarse cell needs a
//! whole block with no fixed cell; an operation whose ring the stage's edge or an invalid source
//! interrupts keeps every level to `1 x 1`, where that argument does not hold. Levels past the
//! first with no unknown change nothing, as the CPU unit's truncation does.
//!
//! **Passes** for `L` levels: gather (level 0, reads the input), `L - 1` coarse passes (classes and
//! boundary means up), `L - 1` start passes (down), then per V-cycle and level `k < L - 1`: two
//! red-black iterations of two half passes each, one restriction to `k + 1` (but not at the last
//! level that can hold unknowns), the cycle below, one correction and two more iterations; then the
//! composite (level 0, reads the input). Every plane is `rgba32float`, fixed in size by its level's
//! tile capacity.
use super::{RepairKind, RepairOp};
use crate::modules::Stage;

/// Atlas tiles across every level's planes.
pub(crate) const ATLAS_TILES_ACROSS: u32 = 256;

/// A tile's side in texels: the GPU workgroup's.
const TILE: u32 = 8;

/// Levels an operation's record holds.
pub(crate) const LEVELS_MAX: usize = 16;

/// Words of one operation's record.
pub(crate) const OP_WORDS: usize = 11 + 4 * LEVELS_MAX;

/// The block header's words before the reverse tables' offsets.
const HEADER_WORDS: usize = 10;

/// The apply's cell index side, in stage pixels.
const INDEX_CELL: u32 = 64;

const NONE: u32 = u32::MAX;

/// The V-cycles and the iterations around each correction, the CPU unit's.
const CYCLES: usize = 3;
const PRE: usize = 2;
const POST: usize = 2;

/// One pass, in the group's own plane and word indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairPass {
    pub kernel: &'static str,
    pub inputs: Vec<usize>,
    pub output: usize,
    pub words: usize,
    pub reads_source: bool,
}

/// A group's GPU description.
#[derive(Clone, Debug, PartialEq)]
pub struct RepairGpu {
    pub words: Vec<u32>,
    pub block: Vec<u32>,
    /// Every plane, `rgba32float`, `(width, height)` texels.
    pub planes: Vec<(u32, u32)>,
    pub passes: Vec<RepairPass>,
    /// The plane `lf_repair_apply` reads, and its words.
    pub apply_plane: usize,
    pub apply_words: usize,
    /// Tiles placed per level.
    pub tiles: Vec<u32>,
}

impl RepairGpu {
    /// The bytes the planes take.
    #[cfg_attr(not(feature = "qualification"), allow(dead_code))]
    pub fn plane_bytes(&self) -> u64 {
        self.planes
            .iter()
            .map(|&(w, h)| u64::from(w) * u64::from(h) * 16)
            .sum()
    }
}

/// One operation's grid at one level: its size, tiles across and down, and placed tiles.
struct Grid {
    width: u32,
    height: u32,
    across: u32,
    down: u32,
    placed: Vec<bool>,
}

impl Grid {
    fn new(width: u32, height: u32) -> Self {
        let across = width.div_ceil(TILE);
        let down = height.div_ceil(TILE);
        Self {
            width,
            height,
            across,
            down,
            placed: vec![false; (across * down) as usize],
        }
    }

    fn coarser(&self) -> Self {
        let mut next = Self::new(self.width.div_ceil(2), self.height.div_ceil(2));
        for ty in 0..self.down {
            for tx in 0..self.across {
                if self.placed[(ty * self.across + tx) as usize] {
                    next.placed[((ty / 2) * next.across + tx / 2) as usize] = true;
                }
            }
        }
        next
    }
}

/// The levels and grids of one operation.
fn grids(op: &RepairOp) -> Option<(u32, u32, Vec<Grid>)> {
    let stage = op.stage();
    let rect = op.footprint.solve_rect()?;
    let h = f64::from(stage.height);
    let mut finest = Grid::new(rect.width, rect.height);
    let radius = op.footprint.radius();
    let reach = radius + (std::f64::consts::SQRT_2 * f64::from(TILE) / 2.0 + 2.0) / h;
    let reach2 = reach * reach;
    let segments = op.footprint.segments_ref();
    let mut any = false;
    for ty in 0..finest.down {
        for tx in 0..finest.across {
            let cx = (f64::from(rect.x0 + tx * TILE) + f64::from(TILE) / 2.0) / h;
            let cy = (f64::from(rect.y0 + ty * TILE) + f64::from(TILE) / 2.0) / h;
            if segments.iter().any(|s| s.distance2(cx, cy) <= reach2) {
                finest.placed[(ty * finest.across + tx) as usize] = true;
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    let mut levels = vec![finest];
    if op.kind == RepairKind::Heal {
        let full = {
            let (mut w, mut hh, mut n) = (rect.width, rect.height, 1usize);
            while w > 1 || hh > 1 {
                w = w.div_ceil(2);
                hh = hh.div_ceil(2);
                n += 1;
            }
            n
        };
        // An unbroken ring: the rectangle is inside the stage on every side and so is its source.
        let shifted = |start: u32, delta: i64| i64::from(start) + delta;
        let unbroken = rect.x0 > 0
            && rect.y0 > 0
            && rect.x1() < stage.width
            && rect.y1() < stage.height
            && shifted(rect.x0, op.delta[0]) >= 0
            && shifted(rect.y0, op.delta[1]) >= 0
            && shifted(rect.x1(), op.delta[0]) <= i64::from(stage.width)
            && shifted(rect.y1(), op.delta[1]) <= i64::from(stage.height);
        let bounded = ((2.0 * radius * h + 1.0).log2().floor() as usize) + 2;
        let count = if unbroken { bounded.min(full) } else { full }.min(LEVELS_MAX);
        while levels.len() < count {
            let next = levels.last().expect("a level").coarser();
            levels.push(next);
        }
    }
    Some((rect.x0, rect.y0, levels))
}

/// Tiles each level of `ops` places: a capacity to hold the planes of several ticks fixed.
#[cfg_attr(not(feature = "qualification"), allow(dead_code))]
pub fn tile_counts(ops: &[RepairOp]) -> Vec<u32> {
    let mut counts: Vec<u32> = Vec::new();
    for op in ops {
        if let Some((_, _, levels)) = grids(op) {
            for (k, grid) in levels.iter().enumerate() {
                if counts.len() <= k {
                    counts.push(0);
                }
                counts[k] += grid.placed.iter().filter(|p| **p).count() as u32;
            }
        }
    }
    counts
}

/// The GPU description of the group `ops` at `stage`. `capacity`, when given, fixes each level's
/// tile capacity (at least the tiles placed) so plans of successive ticks hold the same planes.
pub fn describe(ops: &[RepairOp], stage: Stage, capacity: Option<&[u32]>) -> RepairGpu {
    let built: Vec<Option<(u32, u32, Vec<Grid>)>> = ops.iter().map(grids).collect();
    let levels = built
        .iter()
        .flatten()
        .map(|(_, _, grids)| grids.len())
        .max()
        .unwrap_or(1);
    // Atlas tiles per level.
    let mut tiles = vec![0u32; levels];
    for (_, _, grids) in built.iter().flatten() {
        for (k, grid) in grids.iter().enumerate() {
            tiles[k] += grid.placed.iter().filter(|p| **p).count() as u32;
        }
    }
    let capacity: Vec<u32> = (0..levels)
        .map(|k| {
            capacity
                .and_then(|c| c.get(k).copied())
                .unwrap_or(0)
                .max(tiles[k])
                .max(1)
        })
        .collect();
    let rows: Vec<u32> = capacity
        .iter()
        .map(|c| c.div_ceil(ATLAS_TILES_ACROSS))
        .collect();
    // The block: header, records, segments, tile maps, reverse tables, the apply's index.
    let mut block = vec![0u32; HEADER_WORDS + LEVELS_MAX];
    let records_at = block.len();
    block.resize(records_at + OP_WORDS * ops.len(), 0);
    let segments_at = block.len();
    let mut first_segment = Vec::with_capacity(ops.len());
    for op in ops {
        first_segment.push(((block.len() - segments_at) / 5) as u32);
        for s in op.footprint.segments_ref() {
            block.extend([s.ax, s.ay, s.ex, s.ey, s.len2].map(|v| (v as f32).to_bits()));
        }
    }
    let mut next_tile = vec![0u32; levels];
    let mut reverse: Vec<Vec<u32>> = (0..levels)
        .map(|k| vec![NONE; (rows[k] * ATLAS_TILES_ACROSS) as usize])
        .collect();
    for (n, op) in ops.iter().enumerate() {
        let record = records_at + n * OP_WORDS;
        block[record] = u32::from(op.kind == RepairKind::Heal);
        block[record + 1] = (op.delta[0] as i32).cast_unsigned();
        block[record + 2] = (op.delta[1] as i32).cast_unsigned();
        block[record + 3] = (op.footprint.radius() as f32).to_bits();
        block[record + 4] = (op.footprint.band() as f32).to_bits();
        block[record + 5] = (op.footprint.amount() as f32).to_bits();
        block[record + 6] = first_segment[n];
        block[record + 7] = op.footprint.segments_ref().len() as u32;
        let Some((x0, y0, grids)) = &built[n] else {
            continue;
        };
        block[record + 8] = *x0;
        block[record + 9] = *y0;
        block[record + 10] = grids.len() as u32;
        for (k, grid) in grids.iter().enumerate() {
            let map_at = block.len() as u32;
            let at = record + 11 + 4 * k;
            block[at] = grid.width;
            block[at + 1] = grid.height;
            block[at + 2] = grid.across;
            block[at + 3] = map_at;
            for ty in 0..grid.down {
                for tx in 0..grid.across {
                    if grid.placed[(ty * grid.across + tx) as usize] {
                        let tile = next_tile[k];
                        next_tile[k] += 1;
                        reverse[k][tile as usize] = ((n as u32) << 24) | (ty << 12) | tx;
                        block.push(tile);
                    } else {
                        block.push(NONE);
                    }
                }
            }
        }
    }
    for (k, table) in reverse.iter().enumerate() {
        block[HEADER_WORDS + k] = block.len() as u32;
        block.extend_from_slice(table);
    }
    // The apply's index: per cell of the stage, the operations whose solve rectangle meets it.
    let columns = stage.width.div_ceil(INDEX_CELL);
    let index_rows = stage.height.div_ceil(INDEX_CELL);
    let cells = (columns * index_rows) as usize;
    let mut lists: Vec<Vec<u32>> = vec![Vec::new(); cells];
    for (n, built) in built.iter().enumerate() {
        let Some((x0, y0, grids)) = built else {
            continue;
        };
        let (x1, y1) = (x0 + grids[0].width, y0 + grids[0].height);
        for row in y0 / INDEX_CELL..=(y1 - 1) / INDEX_CELL {
            for column in x0 / INDEX_CELL..=(x1 - 1) / INDEX_CELL {
                lists[(row * columns + column) as usize].push(n as u32);
            }
        }
    }
    let index_at = block.len();
    let mut offset = (cells + 1) as u32;
    let mut table = Vec::with_capacity(cells + 1);
    for list in &lists {
        table.push(offset);
        offset += list.len() as u32;
    }
    table.push(offset);
    block.extend_from_slice(&table);
    for list in &lists {
        block.extend_from_slice(list);
    }
    block[0] = ops.len() as u32;
    block[1] = records_at as u32;
    block[2] = segments_at as u32;
    block[3] = ATLAS_TILES_ACROSS;
    block[4] = index_at as u32;
    block[5] = INDEX_CELL;
    block[6] = columns;
    block[7] = index_rows;
    block[8] = stage.width;
    block[9] = (stage.height as f32).to_bits();

    // Planes and passes.
    let size = |k: usize| (ATLAS_TILES_ACROSS * TILE, rows[k] * TILE);
    let mut planes: Vec<(u32, u32)> = Vec::new();
    let plane = |planes: &mut Vec<(u32, u32)>, k: usize| {
        planes.push(size(k));
        planes.len() - 1
    };
    let gather = plane(&mut planes, 0);
    let means: Vec<usize> = (0..levels)
        .map(|k| {
            if k == 0 {
                gather
            } else {
                plane(&mut planes, k)
            }
        })
        .collect();
    let rhs: Vec<usize> = (0..levels)
        .map(|k| {
            if k == 0 {
                usize::MAX
            } else {
                plane(&mut planes, k)
            }
        })
        .collect();
    let pairs: Vec<[usize; 2]> = (0..levels)
        .map(|k| [plane(&mut planes, k), plane(&mut planes, k)])
        .collect();
    let composite = plane(&mut planes, 0);
    let mut words = vec![0u32];
    let record = |words: &mut Vec<u32>, values: [u32; 3]| {
        // The fourth word is the group's offset in the step's block, which the render
        // integration writes when it chains groups; zero for one group alone.
        words.extend_from_slice(&values);
        words.push(0);
        words.len() - 4
    };
    let mut passes = Vec::new();
    let pass = |passes: &mut Vec<RepairPass>,
                kernel: &'static str,
                inputs: Vec<usize>,
                output: usize,
                words: usize,
                reads_source: bool| {
        passes.push(RepairPass {
            kernel,
            inputs,
            output,
            words,
            reads_source,
        });
    };
    let w = record(&mut words, [0, 0, 0]);
    pass(&mut passes, "lf_repair_gather", vec![], gather, w, true);
    for k in 1..levels {
        let w = record(&mut words, [k as u32, 0, 0]);
        pass(
            &mut passes,
            "lf_repair_coarse",
            vec![means[k - 1]],
            means[k],
            w,
            false,
        );
    }
    // The start: level L - 1's means are its start; each level below takes its parents'.
    let mut current: Vec<usize> = means.clone();
    for k in (0..levels.saturating_sub(1)).rev() {
        let w = record(&mut words, [k as u32, 0, 0]);
        let out = pairs[k][0];
        pass(
            &mut passes,
            "lf_repair_start",
            vec![means[k], current[k + 1]],
            out,
            w,
            false,
        );
        current[k] = out;
    }
    // The V-cycles over the levels that can hold unknowns: every one but the last, unless the last
    // is the only one.
    let solved = levels.saturating_sub(1).max(1);
    struct Cycle<'a> {
        words: &'a mut Vec<u32>,
        passes: &'a mut Vec<RepairPass>,
        current: &'a mut Vec<usize>,
        pairs: &'a [[usize; 2]],
        means: &'a [usize],
        rhs: &'a [usize],
        solved: usize,
    }
    impl Cycle<'_> {
        fn word(&mut self, values: [u32; 3]) -> usize {
            self.words.extend_from_slice(&values);
            self.words.push(0);
            self.words.len() - 4
        }
        fn other(&self, k: usize) -> usize {
            if self.current[k] == self.pairs[k][0] {
                self.pairs[k][1]
            } else {
                self.pairs[k][0]
            }
        }
        fn relax(&mut self, k: usize, parity: u32, zero: bool) {
            let has_rhs = k > 0;
            let flags = u32::from(zero) | if has_rhs { 2 } else { 0 };
            let w = self.word([k as u32, parity, flags]);
            let out = self.other(k);
            let inputs = if zero {
                vec![self.rhs[k], self.rhs[k]]
            } else if has_rhs {
                vec![self.current[k], self.rhs[k]]
            } else {
                vec![self.current[k]]
            };
            self.passes.push(RepairPass {
                kernel: "lf_repair_relax",
                inputs,
                output: out,
                words: w,
                reads_source: false,
            });
            self.current[k] = out;
        }
        fn v(&mut self, k: usize) {
            for iteration in 0..PRE {
                self.relax(k, 0, k > 0 && iteration == 0);
                self.relax(k, 1, false);
            }
            if k + 1 < self.solved {
                let has_rhs = k > 0;
                let w = self.word([k as u32, 0, if has_rhs { 2 } else { 0 }]);
                let second = if has_rhs {
                    self.rhs[k]
                } else {
                    self.means[k + 1]
                };
                self.passes.push(RepairPass {
                    kernel: "lf_repair_restrict",
                    inputs: vec![self.current[k], second, self.means[k + 1]],
                    output: self.rhs[k + 1],
                    words: w,
                    reads_source: false,
                });
                self.v(k + 1);
                let w = self.word([k as u32, 0, 0]);
                let out = self.other(k);
                self.passes.push(RepairPass {
                    kernel: "lf_repair_correct",
                    inputs: vec![self.current[k], self.current[k + 1]],
                    output: out,
                    words: w,
                    reads_source: false,
                });
                self.current[k] = out;
            }
            for _ in 0..POST {
                self.relax(k, 0, false);
                self.relax(k, 1, false);
            }
        }
    }
    let heals = ops.iter().any(|op| op.kind == RepairKind::Heal);
    if heals {
        // Level 0 starts in its means plane when it has no level above to start from.
        let mut cycle = Cycle {
            words: &mut words,
            passes: &mut passes,
            current: &mut current,
            pairs: &pairs,
            means: &means,
            rhs: &rhs,
            solved,
        };
        for _ in 0..CYCLES {
            cycle.v(0);
        }
    }
    let w = record(&mut words, [0, 0, 0]);
    pass(
        &mut passes,
        "lf_repair_composite",
        vec![current[0]],
        composite,
        w,
        true,
    );
    let apply_words = record(&mut words, [0, 0, 0]);
    RepairGpu {
        words,
        block,
        planes,
        passes,
        apply_plane: composite,
        apply_words,
        tiles,
    }
}
