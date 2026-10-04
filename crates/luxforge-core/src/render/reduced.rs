//! The store of reduced planes: what a spatial unit that runs first in its operation computes from a
//! reduced grid of the operation's input before it applies any coefficient, kept so the next render
//! of the same input reads it instead of computing it again around every tile (performance rule 14,
//! `docs/design/efficiency.md`, "The reduced-grid cache").
//!
//! A unit declares its planes beside its estimate key ([`ReducedGrid`]). Only the operation's first
//! unit is ever cached: its input is the operation's input, whose reduced cells are a fixed set of
//! pixels summed in a fixed order, so a cell is the same value in every tile that computes it. A
//! later unit's input is the earlier units' output over the rectangle each tile's chain gives them,
//! whose last bits can differ from tile to tile, so no shared plane reproduces it.
//!
//! **Filled by the render's own tiles.** A frame render whose first unit finds no entry, or one that
//! does not cover a tile's reach, runs that tile as it always did and hands back, with its output,
//! the cells it computed anyway whose first pixel lies in the tile ([`ReducedGrid::owned`]), so the
//! render's tiles hand back every cell once whatever their side. The render copies them into its
//! [`PendingPlanes`] under the lock its tiles are written under, and publishes them when it
//! completes, with which tiles' cells they hold; a render that fails or is cancelled publishes
//! nothing. No pass, scratch or budget of its own is needed.
//!
//! **Bounded and disposable.** [`REDUCED_STORE_BYTES`] in total, least recently used evicted before
//! an entry is added that would pass it, an entry larger than it refused. Entries are shared as
//! `Arc`s, never part of history, an artifact or a source; losing one costs only time.

use crate::modules::{Cells, Global, GridPlanes, REDUCED_STORE_BYTES, ReducedGrid, Region, Stage};
use std::{
    borrow::Cow,
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// What one entry's planes belong to, mirroring the estimate store's key: the source, the domain's
/// identity of the layers before the operation (`estimate_prefix` over their prefix hash), the stage
/// the grid is cut from, and the unit's own [`ReducedGrid::key`].
///
/// Neither the unit's amount nor the operation's mask is part of it: the planes are computed before
/// any coefficient is applied, and a masked operation's units run on the unmasked input. The unit's
/// global estimate is checked beside the key ([`ReducedEntry`] records its bits), because a plane
/// may be computed with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReducedKey {
    pub(crate) fingerprint: String,
    pub(crate) prefix_hash: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) reduction: Cow<'static, str>,
}

impl ReducedKey {
    /// The stage the grid is cut from.
    fn stage(&self) -> Stage {
        Stage {
            width: self.width,
            height: self.height,
        }
    }
}

/// The bit patterns of the global estimate a unit's planes were computed with, `None` for a unit
/// handed none.
fn global_bits(global: Option<&Global>) -> Option<Box<[u64]>> {
    global.map(|global| {
        global
            .values()
            .iter()
            .map(|value| value.to_bits())
            .collect()
    })
}

/// Which tiles' cells a plane holds: one flag per tile of the stage cut into tiles of `side`, the
/// side of the render that handed them back. A cell is held when the tile that owns it is
/// ([`ReducedGrid::owned`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Coverage {
    stage: Stage,
    side: u32,
    columns: u32,
    tiles: Vec<bool>,
}

impl Coverage {
    fn new(stage: Stage, side: u32) -> Self {
        let side = side.max(1);
        let columns = stage.width.div_ceil(side);
        let rows = stage.height.div_ceil(side);
        Self {
            stage,
            side,
            columns,
            tiles: vec![false; columns as usize * rows as usize],
        }
    }

    /// The flag of the tile that starts at `tile`'s origin.
    fn index(&self, tile: Region) -> usize {
        debug_assert!(
            tile.x0.is_multiple_of(self.side) && tile.y0.is_multiple_of(self.side),
            "a tile on the grid of the coverage's side"
        );
        (tile.y0 / self.side) as usize * self.columns as usize + (tile.x0 / self.side) as usize
    }

    /// The tile flag `index` stands for.
    fn tile(&self, index: usize) -> Region {
        let x0 = (index % self.columns as usize) as u32 * self.side;
        let y0 = (index / self.columns as usize) as u32 * self.side;
        Region {
            x0,
            y0,
            width: self.side.min(self.stage.width - x0),
            height: self.side.min(self.stage.height - y0),
        }
    }

    /// Whether the tile that owns every cell of `reach` is held. The owners of a rectangle of cells
    /// are the tiles their first pixels fall in, read along each side once.
    fn covers(&self, factor: u32, reach: Region) -> bool {
        let owners = |start: u32, end: u32| {
            let mut owners: Vec<u32> = (start..end)
                .map(|cell| cell.saturating_mul(factor) / self.side)
                .collect();
            owners.dedup();
            owners
        };
        let columns = owners(reach.x0, reach.x1());
        owners(reach.y0, reach.y1()).into_iter().all(|row| {
            columns.iter().all(|column| {
                self.tiles
                    .get(row as usize * self.columns as usize + *column as usize)
                    .copied()
                    .unwrap_or(false)
            })
        })
    }

    fn held(&self) -> impl Iterator<Item = usize> + '_ {
        self.tiles
            .iter()
            .enumerate()
            .filter_map(|(index, held)| held.then_some(index))
    }
}

/// One stored entry: the planes of one unit's reduced grid over the rectangle of cells its covered
/// tiles own, which tiles those are, and the global estimate the planes were computed with.
#[derive(Debug)]
pub(crate) struct ReducedEntry {
    key: ReducedKey,
    global: Option<Box<[u64]>>,
    grid: ReducedGrid,
    coverage: Coverage,
    /// The rectangle of cells `values` holds: the smallest one around every covered tile's cells.
    /// A cell inside it whose tile is not covered holds nothing meaningful and is never read.
    rect: Region,
    values: Vec<f32>,
}

impl ReducedEntry {
    /// Whether every cell of `reach`, a rectangle of the grid, is held.
    pub(crate) fn covers(&self, reach: Region) -> bool {
        if reach.is_empty() {
            return true;
        }
        let rect = self.rect;
        reach.x0 >= rect.x0
            && reach.y0 >= rect.y0
            && reach.x1() <= rect.x1()
            && reach.y1() <= rect.y1()
            && self.coverage.covers(self.grid.factor, reach)
    }

    /// The planes, to read the cells [`Self::covers`] vouched for.
    pub(crate) fn planes(&self) -> GridPlanes<'_> {
        GridPlanes::new(
            self.grid.cells(self.key.stage()),
            self.rect,
            self.grid.planes,
            &self.values,
        )
    }

    /// The bytes it holds: its planes and its coverage.
    pub(crate) fn bytes(&self) -> u64 {
        (self.values.len() * std::mem::size_of::<f32>() + self.coverage.tiles.len()) as u64
    }

    fn matches(&self, key: &ReducedKey, global: &Option<Box<[u64]>>) -> bool {
        self.key == *key && self.global == *global
    }
}

/// The planes one frame render collects from its tiles before it publishes them
/// ([`ReducedStore::publish`]). Its planes span the whole grid and are allocated zeroed on the
/// first tile that hands back cells, so pages no tile writes are never touched; nothing in them
/// is read but the cells of covered tiles.
#[derive(Debug)]
pub(crate) struct PendingPlanes {
    key: ReducedKey,
    global: Option<Box<[u64]>>,
    grid: ReducedGrid,
    coverage: Coverage,
    values: Vec<f32>,
}

impl PendingPlanes {
    fn cells(&self) -> Stage {
        self.grid.cells(self.key.stage())
    }

    fn allocate(&mut self) {
        if self.values.is_empty() {
            self.values = vec![0.0; self.cells().pixels_usize() * self.grid.planes];
        }
    }

    /// Copy the cells `tile` handed back into the planes and mark the tile held.
    pub(crate) fn write(&mut self, tile: Region, cells: &Cells) {
        debug_assert_eq!(cells.rect(), self.grid.owned(tile));
        self.allocate();
        let grid = self.cells();
        copy_cells(
            &cells.planes(grid),
            cells.rect(),
            &mut self.values,
            Region::whole(grid),
        );
        let index = self.coverage.index(tile);
        self.coverage.tiles[index] = true;
    }

    /// Whether this render held any cell back from a tile.
    fn is_empty(&self) -> bool {
        !self.coverage.tiles.iter().any(|held| *held)
    }

    /// Take in the cells of every tile `entry` covers that this render did not hand back, when it
    /// holds the same planes over the same tiles: they are the same values by construction.
    fn merge(&mut self, entry: &ReducedEntry) {
        if !entry.matches(&self.key, &self.global)
            || entry.grid != self.grid
            || entry.coverage.side != self.coverage.side
            || entry.coverage.stage != self.coverage.stage
        {
            return;
        }
        self.allocate();
        let grid = self.cells();
        let planes = entry.planes();
        for index in entry.coverage.held() {
            if self.coverage.tiles[index] {
                continue;
            }
            let cells = self.grid.owned(entry.coverage.tile(index));
            copy_cells(&planes, cells, &mut self.values, Region::whole(grid));
            self.coverage.tiles[index] = true;
        }
    }

    /// The entry: the planes cut to the smallest rectangle around every held tile's cells, which
    /// is the whole grid, kept without a copy, after an unmasked render.
    fn into_entry(self) -> ReducedEntry {
        let grid = self.cells();
        let mut rect: Option<(u32, u32, u32, u32)> = None;
        for index in self.coverage.held() {
            let cells = self.grid.owned(self.coverage.tile(index));
            if cells.is_empty() {
                continue;
            }
            let (x0, y0, x1, y1) = rect.unwrap_or((u32::MAX, u32::MAX, 0, 0));
            rect = Some((
                x0.min(cells.x0),
                y0.min(cells.y0),
                x1.max(cells.x1()),
                y1.max(cells.y1()),
            ));
        }
        let rect = rect.map_or(Region::EMPTY, |(x0, y0, x1, y1)| Region {
            x0,
            y0,
            width: x1 - x0,
            height: y1 - y0,
        });
        let values = if rect == Region::whole(grid) {
            self.values
        } else {
            let mut values = vec![0.0; rect.pixels() as usize * self.grid.planes];
            let planes = GridPlanes::new(grid, Region::whole(grid), self.grid.planes, &self.values);
            copy_cells(&planes, rect, &mut values, rect);
            values
        };
        ReducedEntry {
            key: self.key,
            global: self.global,
            grid: self.grid,
            coverage: self.coverage,
            rect,
            values,
        }
    }
}

/// Copy the cells `cells` of every plane of `from` into `to`, the planes of the rectangle
/// `target` of the same grid, row by row.
fn copy_cells(from: &GridPlanes<'_>, cells: Region, to: &mut [f32], target: Region) {
    if cells.is_empty() {
        return;
    }
    let source = from.rect();
    let target_len = target.pixels() as usize;
    let width = cells.width as usize;
    for plane in 0..to.len() / target_len.max(1) {
        let values = from.plane(plane);
        let out = &mut to[plane * target_len..(plane + 1) * target_len];
        for y in cells.y0..cells.y1() {
            let from_start =
                (y - source.y0) as usize * source.width as usize + (cells.x0 - source.x0) as usize;
            let to_start =
                (y - target.y0) as usize * target.width as usize + (cells.x0 - target.x0) as usize;
            out[to_start..to_start + width]
                .copy_from_slice(&values[from_start..from_start + width]);
        }
    }
}

trait PixelsUsize {
    fn pixels_usize(self) -> usize;
}

impl PixelsUsize for Stage {
    fn pixels_usize(self) -> usize {
        self.width as usize * self.height as usize
    }
}

/// What the store has done since its context was created: every figure only ever grows, but
/// `retained_bytes` and `entries`, which are levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReducedCounts {
    pub(crate) limit_bytes: u64,
    pub(crate) retained_bytes: u64,
    pub(crate) entries: u64,
    /// Frame renders of a cacheable unit that found an entry for their key and estimate.
    pub(crate) render_hits: u64,
    /// Frame renders of a cacheable unit that found none.
    pub(crate) render_misses: u64,
    /// Tiles of those renders that read held planes.
    pub(crate) tile_hits: u64,
    /// Tiles of those renders that computed the planes, because nothing held covered their reach.
    pub(crate) tile_misses: u64,
    /// Tiles point queries evaluated that read held planes.
    pub(crate) point_hits: u64,
    /// Tiles point queries evaluated that computed the planes.
    pub(crate) point_misses: u64,
    /// Cells tiles handed back to their render's pending planes.
    pub(crate) cells_handed_back: u64,
    pub(crate) publishes: u64,
    pub(crate) evictions: u64,
    /// Entries the store would not hold because they alone pass the limit, and renders that did
    /// not collect planes because the whole grid's would.
    pub(crate) refusals: u64,
}

#[derive(Default)]
struct Counters {
    render_hits: AtomicU64,
    render_misses: AtomicU64,
    tile_hits: AtomicU64,
    tile_misses: AtomicU64,
    point_hits: AtomicU64,
    point_misses: AtomicU64,
    cells_handed_back: AtomicU64,
    publishes: AtomicU64,
    evictions: AtomicU64,
    refusals: AtomicU64,
}

impl Counters {
    fn add(counter: &AtomicU64, by: u64) {
        // Only diagnostics read them, so no value depends on the order they become visible in.
        counter.fetch_add(by, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Entries {
    /// Least recently used first.
    held: VecDeque<Arc<ReducedEntry>>,
    bytes: u64,
}

/// The render context's reduced planes: at most [`REDUCED_STORE_BYTES`] across every entry, least
/// recently used evicted first.
pub(crate) struct ReducedStore {
    limit: u64,
    entries: Mutex<Entries>,
    counters: Counters,
}

impl Default for ReducedStore {
    fn default() -> Self {
        Self::new(REDUCED_STORE_BYTES)
    }
}

impl ReducedStore {
    pub(crate) fn new(limit: u64) -> Self {
        Self {
            limit,
            entries: Mutex::default(),
            counters: Counters::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The entry of `key` computed with `global`, now the most recently used, or `None`. It counts
    /// nothing; a frame render counts its lookup with [`Self::note_render`].
    pub(crate) fn lookup(
        &self,
        key: &ReducedKey,
        global: Option<&Global>,
    ) -> Option<Arc<ReducedEntry>> {
        let global = global_bits(global);
        let mut entries = self.lock();
        let index = entries
            .held
            .iter()
            .position(|entry| entry.matches(key, &global))?;
        let entry = entries.held.remove(index)?;
        entries.held.push_back(entry.clone());
        Some(entry)
    }

    /// The planes a frame render of `grid`'s unit collects from its tiles of `side`, or `None`, a
    /// refusal, when the whole grid's planes alone would pass the limit.
    pub(crate) fn pending(
        &self,
        key: ReducedKey,
        global: Option<&Global>,
        grid: &ReducedGrid,
        side: u32,
    ) -> Option<PendingPlanes> {
        let stage = key.stage();
        let bytes = (grid.cells(stage).pixels_usize() as u64)
            .saturating_mul(grid.planes as u64)
            .saturating_mul(std::mem::size_of::<f32>() as u64);
        if bytes > self.limit {
            Counters::add(&self.counters.refusals, 1);
            return None;
        }
        Some(PendingPlanes {
            key,
            global: global_bits(global),
            grid: grid.clone(),
            coverage: Coverage::new(stage, side),
            values: Vec::new(),
        })
    }

    /// Publish what a completed render collected: merged with the cells of the entry it read,
    /// `held`, and of the store's own entry of the same key, then held in place of that entry,
    /// after evicting the least recently used entries it would otherwise pass the limit beside. An
    /// entry larger than the limit is refused. A render whose tiles handed nothing back publishes
    /// nothing.
    pub(crate) fn publish(&self, mut pending: PendingPlanes, held: Option<&Arc<ReducedEntry>>) {
        if pending.is_empty() {
            return;
        }
        let current = {
            let entries = self.lock();
            entries
                .held
                .iter()
                .find(|entry| entry.key == pending.key)
                .cloned()
        };
        if let Some(held) = held {
            pending.merge(held);
        }
        if let Some(current) = current
            && !held.is_some_and(|held| Arc::ptr_eq(held, &current))
        {
            pending.merge(&current);
        }
        let entry = pending.into_entry();
        let bytes = entry.bytes();
        if bytes > self.limit {
            Counters::add(&self.counters.refusals, 1);
            return;
        }
        let mut entries = self.lock();
        let mut replaced = 0;
        entries.held.retain(|held| {
            let same = held.key == entry.key;
            if same {
                replaced += held.bytes();
            }
            !same
        });
        entries.bytes -= replaced;
        while entries.bytes + bytes > self.limit {
            let Some(evicted) = entries.held.pop_front() else {
                break;
            };
            entries.bytes -= evicted.bytes();
            Counters::add(&self.counters.evictions, 1);
        }
        entries.bytes += bytes;
        entries.held.push_back(Arc::new(entry));
        Counters::add(&self.counters.publishes, 1);
    }

    /// Count one frame render's lookup.
    pub(crate) fn note_render(&self, hit: bool) {
        let counter = if hit {
            &self.counters.render_hits
        } else {
            &self.counters.render_misses
        };
        Counters::add(counter, 1);
    }

    /// Count one tile of a frame render, `served` from held planes or not, and the cells it
    /// handed back.
    pub(crate) fn note_tile(&self, served: bool, cells: u64) {
        let counter = if served {
            &self.counters.tile_hits
        } else {
            &self.counters.tile_misses
        };
        Counters::add(counter, 1);
        Counters::add(&self.counters.cells_handed_back, cells);
    }

    /// Count one tile a point query evaluated, `served` from held planes or not.
    pub(crate) fn note_point(&self, served: bool) {
        let counter = if served {
            &self.counters.point_hits
        } else {
            &self.counters.point_misses
        };
        Counters::add(counter, 1);
    }

    /// Every figure, read now.
    pub(crate) fn counts(&self) -> ReducedCounts {
        let (retained_bytes, entries) = {
            let entries = self.lock();
            (entries.bytes, entries.held.len() as u64)
        };
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let counters = &self.counters;
        ReducedCounts {
            limit_bytes: self.limit,
            retained_bytes,
            entries,
            render_hits: read(&counters.render_hits),
            render_misses: read(&counters.render_misses),
            tile_hits: read(&counters.tile_hits),
            tile_misses: read(&counters.tile_misses),
            point_hits: read(&counters.point_hits),
            point_misses: read(&counters.point_misses),
            cells_handed_back: read(&counters.cells_handed_back),
            publishes: read(&counters.publishes),
            evictions: read(&counters.evictions),
            refusals: read(&counters.refusals),
        }
    }

    /// The keys held, least recently used first.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<ReducedKey> {
        self.lock()
            .held
            .iter()
            .map(|entry| entry.key.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE: Stage = Stage {
        width: 37,
        height: 23,
    };

    fn grid(planes: usize) -> ReducedGrid {
        ReducedGrid {
            key: Cow::Borrowed("test grid"),
            factor: 4,
            planes,
        }
    }

    fn key() -> ReducedKey {
        ReducedKey {
            fingerprint: "sha256:source".into(),
            prefix_hash: "prefix+byte:37x23:orientation:1".into(),
            width: STAGE.width,
            height: STAGE.height,
            reduction: Cow::Borrowed("test grid"),
        }
    }

    fn light(red: f64) -> Global {
        Global::new(vec![red, 0.5, 0.25]).unwrap()
    }

    /// A cell's value in plane `plane`, distinct everywhere so a cell copied to the wrong place
    /// fails.
    fn value(plane: usize, x: u32, y: u32) -> f32 {
        (plane * 10_000) as f32 + (y * 100 + x) as f32
    }

    /// The cells `tile` owns, filled with [`value`], as a unit would hand them back.
    fn handed(grid: &ReducedGrid, tile: Region) -> Cells {
        let mut cells = Cells::for_tile(grid, tile);
        let rect = cells.rect();
        for plane in 0..grid.planes {
            let values = cells.plane_mut(plane);
            for (index, (x, y)) in (rect.y0..rect.y1())
                .flat_map(|y| (rect.x0..rect.x1()).map(move |x| (x, y)))
                .enumerate()
            {
                values[index] = value(plane, x, y);
            }
        }
        cells
    }

    fn tiles(side: u32) -> Vec<Region> {
        let mut tiles = Vec::new();
        for y0 in (0..STAGE.height).step_by(side as usize) {
            for x0 in (0..STAGE.width).step_by(side as usize) {
                tiles.push(Region {
                    x0,
                    y0,
                    width: side.min(STAGE.width - x0),
                    height: side.min(STAGE.height - y0),
                });
            }
        }
        tiles
    }

    /// A render of every tile of `side` (or those `keep` keeps) through `store`, published.
    fn publish_tiles(
        store: &ReducedStore,
        key: ReducedKey,
        global: Option<&Global>,
        grid: &ReducedGrid,
        side: u32,
        keep: impl Fn(Region) -> bool,
    ) {
        let mut pending = store.pending(key, global, grid, side).expect("it fits");
        for tile in tiles(side).into_iter().filter(|tile| keep(*tile)) {
            pending.write(tile, &handed(grid, tile));
        }
        store.publish(pending, None);
    }

    fn assert_cells(entry: &ReducedEntry, grid: &ReducedGrid, reach: Region) {
        let planes = entry.planes();
        let rect = planes.rect();
        for plane in 0..grid.planes {
            let values = planes.plane(plane);
            for y in reach.y0..reach.y1() {
                for x in reach.x0..reach.x1() {
                    let index = ((y - rect.y0) * rect.width + (x - rect.x0)) as usize;
                    assert_eq!(
                        values[index].to_bits(),
                        value(plane, x, y).to_bits(),
                        "plane {plane} cell ({x}, {y})"
                    );
                }
            }
        }
    }

    /// Each cell of the grid is owned by exactly one tile, whatever the tile side: sides that are
    /// not multiples of the factor included, and sides smaller than it, whose tiles may own none.
    #[test]
    fn every_cell_is_owned_by_exactly_one_tile_at_every_side() {
        let grid = grid(1);
        let cells = grid.cells(STAGE);
        assert_eq!(
            cells,
            Stage {
                width: 10,
                height: 6
            }
        );
        for side in [1_u32, 2, 3, 4, 5, 6, 7, 9, 13, 16, 37, 64] {
            let mut owned = vec![0_u32; cells.width as usize * cells.height as usize];
            for tile in tiles(side) {
                let rect = grid.owned(tile);
                for y in rect.y0..rect.y1() {
                    for x in rect.x0..rect.x1() {
                        assert!(tile.contains(x * 4, y * 4), "side {side}: ({x}, {y})");
                        owned[(y * cells.width + x) as usize] += 1;
                    }
                }
            }
            assert!(owned.iter().all(|count| *count == 1), "side {side}");
        }
    }

    /// A published render holds the cells its tiles handed back, bit for bit, and covers the
    /// rectangles they own; a merge of a later render's tiles holds both.
    #[test]
    fn a_published_plane_returns_the_cells_its_tiles_handed_back() {
        let store = ReducedStore::default();
        let grid = grid(2);
        let whole = Region::whole(grid.cells(STAGE));
        // A masked render: only the tiles at the left ran.
        publish_tiles(&store, key(), None, &grid, 7, |tile| tile.x0 < 14);
        let entry = store.lookup(&key(), None).expect("published");
        let left = Region {
            x0: 0,
            y0: 0,
            width: 4,
            height: 6,
        };
        assert!(entry.covers(left));
        assert!(!entry.covers(whole));
        assert_cells(&entry, &grid, left);
        // The entry holds only the cells around the covered tiles.
        assert_eq!(entry.planes().rect(), left);
        // A later render of the rest merges with it.
        publish_tiles(&store, key(), None, &grid, 7, |tile| tile.x0 >= 14);
        let entry = store.lookup(&key(), None).expect("published");
        assert!(entry.covers(whole));
        assert_cells(&entry, &grid, whole);
        assert_eq!(store.keys().len(), 1, "one entry per key");
        let counts = store.counts();
        assert_eq!(counts.publishes, 2);
        assert_eq!(counts.retained_bytes, entry.bytes());
    }

    /// A reach is covered only when every tile that owns one of its cells is held.
    #[test]
    fn a_reach_is_covered_only_by_the_tiles_that_own_its_cells() {
        let store = ReducedStore::default();
        let grid = grid(1);
        // Side 2 is narrower than the factor: tile column 1 (x 2..4) owns no cell, so a render
        // that did not run it still covers every cell around it.
        publish_tiles(&store, key(), None, &grid, 2, |tile| tile.x0 != 2);
        let entry = store.lookup(&key(), None).unwrap();
        assert!(entry.covers(Region::whole(grid.cells(STAGE))));
        // Side 8: missing the tile at (8, 8) uncovers exactly cells x 2..4, y 2..4.
        publish_tiles(&store, key(), None, &grid, 8, |tile| {
            (tile.x0, tile.y0) != (8, 8)
        });
        let entry = store.lookup(&key(), None).unwrap();
        let cell = |x0, y0| Region {
            x0,
            y0,
            width: 1,
            height: 1,
        };
        for y in 0..6 {
            for x in 0..10 {
                let owned_by_missing = (2..4).contains(&x) && (2..4).contains(&y);
                assert_eq!(entry.covers(cell(x, y)), !owned_by_missing, "({x}, {y})");
            }
        }
    }

    /// Each part of the key changed alone misses, and so does another global estimate.
    #[test]
    fn each_part_of_the_key_and_the_estimate_changed_alone_misses() {
        let store = ReducedStore::default();
        let grid = grid(1);
        let estimate = light(0.75);
        publish_tiles(&store, key(), Some(&estimate), &grid, 16, |_| true);
        assert!(store.lookup(&key(), Some(&estimate)).is_some());
        let changed: [(&str, ReducedKey); 5] = [
            (
                "fingerprint",
                ReducedKey {
                    fingerprint: "sha256:another".into(),
                    ..key()
                },
            ),
            (
                "input prefix",
                ReducedKey {
                    prefix_hash: "another+byte:37x23:orientation:1".into(),
                    ..key()
                },
            ),
            ("width", ReducedKey { width: 36, ..key() }),
            (
                "height",
                ReducedKey {
                    height: 22,
                    ..key()
                },
            ),
            (
                "reduction key",
                ReducedKey {
                    reduction: Cow::Borrowed("another grid"),
                    ..key()
                },
            ),
        ];
        for (part, key) in changed {
            assert!(store.lookup(&key, Some(&estimate)).is_none(), "{part}");
        }
        assert!(
            store.lookup(&key(), Some(&light(0.7500001))).is_none(),
            "another light"
        );
        assert!(store.lookup(&key(), None).is_none(), "no light");
        // The same key computed with another light replaces the entry rather than joining it.
        publish_tiles(&store, key(), Some(&light(0.5)), &grid, 16, |_| true);
        assert!(store.lookup(&key(), Some(&estimate)).is_none());
        assert!(store.lookup(&key(), Some(&light(0.5))).is_some());
        assert_eq!(store.keys().len(), 1);
    }

    /// The least recently used entry is evicted before one is added that would pass the limit,
    /// and an entry larger than the limit is refused without evicting anything.
    #[test]
    fn the_limit_evicts_the_least_recently_used_and_refuses_an_oversized_entry() {
        let grid = grid(1);
        let named = |name: &'static str| ReducedKey {
            reduction: Cow::Borrowed(name),
            ..key()
        };
        // 10 × 6 cells, one plane, and 6 × 4 tiles of 7 px.
        let one = 10 * 6 * 4 + 24;
        let store = ReducedStore::new(2 * one + one / 2);
        publish_tiles(&store, named("a"), None, &grid, 7, |_| true);
        publish_tiles(&store, named("b"), None, &grid, 7, |_| true);
        assert_eq!(store.counts().retained_bytes, 2 * one);
        // Reading `a` makes `b` the least recently used.
        assert!(store.lookup(&named("a"), None).is_some());
        publish_tiles(&store, named("c"), None, &grid, 7, |_| true);
        assert_eq!(store.keys(), vec![named("a"), named("c")]);
        let counts = store.counts();
        assert_eq!((counts.evictions, counts.retained_bytes), (1, 2 * one));

        // An entry larger than the whole limit: the grid alone passes it, so the render collects
        // nothing, and nothing held is evicted for it.
        let small = ReducedStore::new(10 * 6 * 4 - 1);
        assert!(small.pending(named("a"), None, &grid, 7).is_none());
        assert_eq!(small.counts().refusals, 1);
        let store_refusals = store.counts().refusals;
        // Four planes alone fit a limit of exactly their bytes, but not with their coverage
        // beside them, so the render collects them and the entry it publishes is refused.
        let four = ReducedGrid {
            planes: 4,
            ..grid.clone()
        };
        let tight = ReducedStore::new(10 * 6 * 4 * 4);
        let mut pending = tight
            .pending(named("big"), None, &four, 7)
            .expect("the planes alone fit");
        for tile in tiles(7) {
            pending.write(tile, &handed(&four, tile));
        }
        tight.publish(pending, None);
        let counts = tight.counts();
        assert_eq!(
            (counts.refusals, counts.publishes, counts.entries),
            (1, 0, 0)
        );
        assert_eq!(store.counts().refusals, store_refusals);
    }

    /// A render whose tiles handed nothing back publishes nothing, and a pending plane that is
    /// dropped, as a cancelled render drops it, leaves the store as it was.
    #[test]
    fn nothing_handed_back_or_dropped_publishes_nothing() {
        let store = ReducedStore::default();
        let grid = grid(1);
        let pending = store.pending(key(), None, &grid, 7).unwrap();
        store.publish(pending, None);
        let mut dropped = store.pending(key(), None, &grid, 7).unwrap();
        dropped.write(tiles(7)[0], &handed(&grid, tiles(7)[0]));
        drop(dropped);
        assert!(store.lookup(&key(), None).is_none());
        assert_eq!(store.counts().publishes, 0);
    }
}
