//! Executing the spatial primitive: the budget, the tiling, the unit chain, the global-estimate
//! store and the cancellation token.
//!
//! The contract a module writes against is in [`crate::modules::SpatialUnit`]. This module owns the
//! other half: how much one tile costs, how many tiles may be in flight, where the intermediate
//! planes come from and how a point sample re-runs exactly one tile so that a sampled byte is the
//! byte a render of that tile produces.

pub(crate) use super::Cancel;
use crate::{
    Error, ErrorKind,
    modules::{
        ESTIMATE_REDUCTION, ESTIMATE_STORE_ENTRIES, Global, MAX_REDUCTION_PIXELS, MAX_SPATIAL_HALO,
        Planes, PlanesMut, Reduction, Region, SPATIAL_BUDGET_BYTES, SPATIAL_TILE, SpatialOperation,
        Stage,
    },
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(test)]
const MIB: f64 = (1024 * 1024) as f64;

/// A process-wide target for the working sets of spatial tiles, separate from the colour run's
/// [`ScratchBudget`](super::ScratchBudget) because one tile is orders of magnitude larger than one
/// row chunk: a 512 × 512 tile of a 60 MP stage with the frozen presence halos reads a 1408 × 1408
/// input region and needs about 57 MiB, which would leave the 64 MiB scratch target no room for a
/// second tile.
///
/// It is a target, not a limit. It decides how many tiles run at once: a batch takes as many
/// working sets as fit beside what other evaluations hold, and never fewer than one. So a render
/// that meets the target already taken — the histogram's analysis rendering the same stack as the
/// preview, say — slows to one tile at a time instead of failing, and a tile larger than the whole
/// target still runs, alone. The overshoot is at most one working set per spatial evaluation in
/// flight, and [`Self::peak`] shows it. Nothing here refuses work.
///
/// A reservation covers one batch and is taken before any of its tiles allocates, so the next
/// batch sees whatever other evaluations released in the meantime.
pub struct SpatialBudget {
    target: AtomicU64,
    used: AtomicU64,
    peak: AtomicU64,
}

static SPATIAL_BUDGET: SpatialBudget = SpatialBudget {
    target: AtomicU64::new(SPATIAL_BUDGET_BYTES),
    used: AtomicU64::new(0),
    peak: AtomicU64::new(0),
};

impl SpatialBudget {
    /// The one process-wide budget. It is shared state, not a new instance, so this is not the
    /// `Default` trait.
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> &'static Self {
        &SPATIAL_BUDGET
    }

    pub fn target(&self) -> u64 {
        self.target.load(Ordering::Relaxed)
    }

    /// Set the target and return the previous one. Lowering it below what is already reserved does
    /// not free anything; the next batch is what runs fewer tiles.
    pub fn set_target(&self, bytes: u64) -> u64 {
        self.target.swap(bytes, Ordering::SeqCst)
    }

    pub fn in_use(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    /// The high-water mark of [`Self::in_use`]. A batch releases its reservation as soon as its
    /// tiles are written, so `in_use` observed from outside a render is almost always zero; this is
    /// what makes the budget observable after the fact, including a peak above the target when
    /// evaluations overlapped or one tile needed more than all of it.
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Start the high-water mark again from what is reserved right now, so a measurement or a test
    /// can report the peak of one render rather than of the whole process.
    pub fn reset_peak(&self) {
        self.peak.store(self.in_use(), Ordering::Relaxed);
    }

    /// Reserve working sets for up to `wanted` tiles: as many as fit in what the target has left,
    /// and one when none do. It never fails. The reservation is released when the returned guard is
    /// dropped, including on an early return from the work it covers.
    pub(crate) fn reserve(&self, working_set: u64, wanted: usize) -> SpatialReservation<'_> {
        let target = self.target();
        let wanted = wanted.max(1) as u64;
        let mut tiles = 1;
        // The closure always yields a value, so the update always succeeds; `tiles` is the count of
        // the attempt that did.
        let (Ok(used) | Err(used)) =
            self.used
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                    tiles = (target.saturating_sub(used) / working_set.max(1)).clamp(1, wanted);
                    Some(used.saturating_add(tiles.saturating_mul(working_set)))
                });
        let bytes = tiles.saturating_mul(working_set);
        self.peak
            .fetch_max(used.saturating_add(bytes), Ordering::Relaxed);
        SpatialReservation {
            budget: self,
            bytes,
            tiles: tiles as usize,
        }
    }
}

pub(crate) struct SpatialReservation<'a> {
    budget: &'a SpatialBudget,
    bytes: u64,
    tiles: usize,
}

impl SpatialReservation<'_> {
    /// How many tiles this reservation covers: at least one, at most what was asked for.
    pub(crate) fn tiles(&self) -> usize {
        self.tiles
    }
}

impl Drop for SpatialReservation<'_> {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// Everything about running one operation over one stage that does not depend on the pixels: the
/// tiling, the halos, what one tile costs and how many tiles may run at once.
///
/// It is built when the recipe is compiled, which is where an operation whose declarations the
/// host does not accept is refused, and again when a frame or a sample is actually evaluated.
/// Building it is `O(units)` and reads nothing.
#[derive(Clone, Debug)]
pub(crate) struct SpatialPlan {
    stage: Stage,
    /// Each unit's halo at this stage, in evaluation order.
    halos: Vec<u32>,
    summed_halo: u32,
    tile: u32,
    /// The bytes one tile may hold at once, computed for the largest tile of the stage.
    working_set: u64,
    /// How many tiles the budget's target and the pool allow in flight together when nothing else
    /// holds any of the target: what each batch asks for, and at least one.
    concurrency: usize,
}

impl SpatialPlan {
    /// The plan for this operation at this stage with this tile size, or the error that refuses
    /// its declarations. What one tile costs never refuses it: a tile larger than the budget's
    /// target runs alone. `tile` is [`SPATIAL_TILE`] in production; a test passes another size to
    /// prove the result does not depend on it.
    pub(crate) fn new(
        operation: &SpatialOperation,
        stage: Stage,
        tile: u32,
    ) -> Result<Self, Error> {
        operation.validate()?;
        let halos = operation.halos(stage);
        let summed_halo = operation.summed_halo(stage);
        if summed_halo > MAX_SPATIAL_HALO {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!("spatial halo {summed_halo} px exceeds the {MAX_SPATIAL_HALO} px bound"),
            ));
        }
        let tile = tile.max(1);
        let working_set = worst_case_working_set(operation, stage, &halos, summed_halo, tile);
        let concurrency = usize::try_from(SpatialBudget::default().target() / working_set.max(1))
            .unwrap_or(usize::MAX)
            .clamp(1, rayon::current_num_threads().max(1));
        Ok(Self {
            stage,
            halos,
            summed_halo,
            tile,
            working_set,
            concurrency,
        })
    }

    #[cfg(test)]
    pub(crate) fn working_set(&self) -> u64 {
        self.working_set
    }

    #[cfg(test)]
    pub(crate) fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// Every output tile of the stage, in row-major order, aligned to the stage origin with
    /// partial tiles at the right and bottom edges.
    pub(crate) fn tiles(&self) -> Vec<Region> {
        let mut tiles = Vec::new();
        let mut y0 = 0;
        while y0 < self.stage.height {
            let height = self.tile.min(self.stage.height - y0);
            let mut x0 = 0;
            while x0 < self.stage.width {
                let width = self.tile.min(self.stage.width - x0);
                tiles.push(Region {
                    x0,
                    y0,
                    width,
                    height,
                });
                x0 += width;
            }
            y0 += height;
        }
        tiles
    }

    /// The stage-aligned tile one pixel falls in. A point sample evaluates exactly this tile.
    pub(crate) fn tile_containing(&self, x: u32, y: u32) -> Region {
        let x0 = (x / self.tile) * self.tile;
        let y0 = (y / self.tile) * self.tile;
        Region {
            x0,
            y0,
            width: self.tile.min(self.stage.width.saturating_sub(x0)),
            height: self.tile.min(self.stage.height.saturating_sub(y0)),
        }
    }

    /// The rectangles of one tile's chain: index 0 is the input region the host reads, index `i`
    /// is unit `i`'s input and unit `i - 1`'s output, and the last contains the tile.
    pub(crate) fn regions(&self, tile: Region) -> Vec<Region> {
        let mut regions = Vec::with_capacity(self.halos.len() + 1);
        let mut current = tile.grown(self.summed_halo, self.stage);
        regions.push(current);
        for halo in &self.halos {
            current = current.shrunk(*halo, self.stage);
            regions.push(current);
        }
        debug_assert!(
            current.x0 <= tile.x0
                && current.y0 <= tile.y0
                && current.x1() >= tile.x1()
                && current.y1() >= tile.y1(),
            "the last unit's rectangle must contain the tile"
        );
        regions
    }
}

/// The upper bound on one tile's live bytes: the input region, every intermediate plane, the last
/// unit's output and the largest scratch any unit asks for, all sized for the largest tile of the
/// stage. It is an upper bound in two ways — the chain holds at most two plane buffers at a time,
/// and an edge tile's regions are smaller — which is what makes a batch reservation taken up front
/// enough for every tile in it.
fn worst_case_working_set(
    operation: &SpatialOperation,
    stage: Stage,
    halos: &[u32],
    summed_halo: u32,
    tile: u32,
) -> u64 {
    let tile_width = tile.min(stage.width);
    let tile_height = tile.min(stage.height);
    let mut remaining = summed_halo;
    let mut planes = 0_u64;
    let mut scratch = 0_u64;
    for step in 0..=halos.len() {
        let region = Region {
            x0: 0,
            y0: 0,
            width: (tile_width.saturating_add(2 * remaining)).min(stage.width),
            height: (tile_height.saturating_add(2 * remaining)).min(stage.height),
        };
        planes = planes.saturating_add(region.plane_bytes());
        if let (Some(unit), Some(halo)) = (operation.units().get(step), halos.get(step)) {
            scratch = scratch.max(unit.scratch_bytes(Stage {
                width: region.width,
                height: region.height,
            }));
            remaining = remaining.saturating_sub(*halo);
        }
    }
    planes.saturating_add(scratch)
}

/// How many `f32` values of scratch one tile's chain needs: the largest request of any unit for its
/// own input rectangle.
fn scratch_values(operation: &SpatialOperation, regions: &[Region]) -> usize {
    let bytes = operation
        .units()
        .iter()
        .zip(regions)
        .map(|(unit, region)| {
            unit.scratch_bytes(Stage {
                width: region.width,
                height: region.height,
            })
        })
        .max()
        .unwrap_or(0);
    usize::try_from(bytes.div_ceil(std::mem::size_of::<f32>() as u64)).unwrap_or(usize::MAX)
}

const NON_FINITE_SPATIAL: &str = "spatial processing produced a non-finite value";

/// Run one tile's unit chain. `fill` writes the input region's three planes; the result is the last
/// unit's rectangle and its planar values, which always contains `tile`.
///
/// Every path uses this one function: the byte render, the RAW float frame and the point sample.
/// That is what makes a sample equal to the rendered byte by construction rather than by
/// agreement between two implementations.
pub(crate) fn run_tile(
    plan: &SpatialPlan,
    operation: &SpatialOperation,
    globals: &[Option<Global>],
    tile: Region,
    fill: impl FnOnce(Region, &mut [f32]) -> Result<(), Error>,
) -> Result<(Region, Vec<f32>), Error> {
    let stage = plan.stage;
    let regions = plan.regions(tile);
    let mut values = vec![0.0_f32; (regions[0].pixels() * 3) as usize];
    fill(regions[0], &mut values)?;
    let mut scratch = vec![0.0_f32; scratch_values(operation, &regions)];
    for (index, unit) in operation.units().iter().enumerate() {
        let input = Planes::new(stage, regions[index], &values)?;
        let mut next = vec![0.0_f32; (regions[index + 1].pixels() * 3) as usize];
        let mut output = PlanesMut::new(stage, regions[index + 1], &mut next)?;
        unit.apply(
            &input,
            &mut output,
            globals.get(index).and_then(Option::as_ref),
            &mut scratch,
        )?;
        if !next.iter().all(|value| value.is_finite()) {
            return Err(Error::new(ErrorKind::ResourceLimit, NON_FINITE_SPATIAL));
        }
        values = next;
    }
    Ok((
        *regions.last().expect("a chain always has an input region"),
        values,
    ))
}

/// Run every tile of a stage in batches, on the shared Rayon pool above the same one-megapixel
/// threshold the other passes use and serially below it, checking the cancellation token between
/// batches.
///
/// Each batch reserves its working sets from the budget before any of its tiles allocates, asking
/// for the plan's concurrency and running as many tiles as the reservation covers, so a render that
/// overlaps another slows down rather than failing and speeds up again once the other releases.
///
/// `work` computes one tile's result and `write` places it, so the tiles themselves never share a
/// mutable frame: a batch's results are bounded by its concurrency times one tile.
pub(crate) fn run_batches<T: Send>(
    plan: &SpatialPlan,
    cancel: &Cancel,
    work: impl Fn(Region) -> Result<T, Error> + Sync,
    mut write: impl FnMut(Region, T) -> Result<(), Error>,
) -> Result<(), Error> {
    let tiles = plan.tiles();
    let large = plan.stage.width as u64 * plan.stage.height as u64 >= super::PARALLEL_RENDER_PIXELS;
    let mut start = 0;
    while start < tiles.len() {
        // Before the reservation, so a cancelled render never takes working sets it will not use.
        cancel.check()?;
        let reservation = SpatialBudget::default()
            .reserve(plan.working_set, plan.concurrency.min(tiles.len() - start));
        let batch = &tiles[start..start + reservation.tiles()];
        start += batch.len();
        let results: Vec<T> = if large && batch.len() > 1 {
            batch
                .par_iter()
                .map(|tile| work(*tile))
                .collect::<Result<Vec<T>, Error>>()?
        } else {
            batch
                .iter()
                .map(|tile| work(*tile))
                .collect::<Result<Vec<T>, Error>>()?
        };
        for (tile, result) in batch.iter().zip(results) {
            write(*tile, result)?;
        }
    }
    Ok(())
}

/// Reserve the one working set a point sample needs.
pub(crate) fn reserve_one(plan: &SpatialPlan) -> SpatialReservation<'static> {
    SpatialBudget::default().reserve(plan.working_set, 1)
}

// ---------------------------------------------------------------------------------------------
// Global estimates.
// ---------------------------------------------------------------------------------------------

/// What one cached global estimate belongs to. The stage is part of the key because a unit's
/// estimate is computed from a reduction of that stage, and the prefix hash because the layers
/// before the operation decide what the stage holds.
///
/// The unit's own description is part of it too, not just its position. A module compiles its
/// payload into whichever units that payload needs, so the same position of the same stack can hold
/// a different unit from one evaluation to the next — the Presence module omits a unit whose amount
/// is zero, which moves the others up — and a position alone would hand one unit the estimate
/// another prepared, including the answer "this unit wants none". Two units that describe themselves
/// identically process identically, which is the trait's own rule, so the description is exactly the
/// identity this store needs. The cost is that a unit whose coefficients changed prepares again; for
/// the one estimate in this design, an atmospheric light that does not depend on the amount, that is
/// one bounded reduction of the stage per changed amount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EstimateKey {
    pub(crate) fingerprint: String,
    pub(crate) prefix_hash: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) unit: usize,
    pub(crate) describe: String,
}

/// The bounded store of prepared estimates: [`ESTIMATE_STORE_ENTRIES`] entries, oldest first, each
/// at most [`crate::modules::MAX_GLOBAL_BYTES`]. A unit that wants no estimate is cached as such,
/// so a second evaluation of the same stack costs no reduction at all.
static ESTIMATES: Mutex<VecDeque<(EstimateKey, Option<Global>)>> = Mutex::new(VecDeque::new());

fn estimates() -> std::sync::MutexGuard<'static, VecDeque<(EstimateKey, Option<Global>)>> {
    ESTIMATES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `Some(estimate)` on a hit, where the estimate itself may be `None` for a unit that wants none.
fn cached(key: &EstimateKey) -> Option<Option<Global>> {
    estimates()
        .iter()
        .find(|(stored, _)| stored == key)
        .map(|(_, value)| value.clone())
}

fn remember(key: EstimateKey, value: Option<Global>) {
    let mut store = estimates();
    if store.iter().any(|(stored, _)| *stored == key) {
        return;
    }
    store.push_back((key, value));
    while store.len() > ESTIMATE_STORE_ENTRIES {
        store.pop_front();
    }
}

/// Forget every cached estimate. Tests that count `prepare` calls start from here.
pub fn clear_estimates() {
    estimates().clear();
}

/// How many estimates are held right now.
pub fn cached_estimates() -> usize {
    estimates().len()
}

/// The global estimate of every unit of an operation, from the store where it is already there and
/// from one reduction of the operation's input stage otherwise. The reduction is built at most
/// once, and only when some unit is missing: a stack evaluated twice reduces nothing the second
/// time.
pub(crate) fn resolve_globals(
    operation: &SpatialOperation,
    stage: Stage,
    fingerprint: &str,
    prefix_hash: &str,
    reduce: impl FnOnce() -> Result<Reduction, Error>,
) -> Result<Vec<Option<Global>>, Error> {
    let keys: Vec<EstimateKey> = operation
        .units()
        .iter()
        .enumerate()
        .map(|(unit, declared)| EstimateKey {
            fingerprint: fingerprint.to_owned(),
            prefix_hash: prefix_hash.to_owned(),
            width: stage.width,
            height: stage.height,
            unit,
            describe: declared.describe(),
        })
        .collect();
    let hits: Vec<Option<Option<Global>>> = keys.iter().map(cached).collect();
    if hits.iter().all(Option::is_some) {
        return Ok(hits.into_iter().map(Option::unwrap).collect());
    }
    let reduction = reduce()?;
    let mut globals = Vec::with_capacity(operation.len());
    for ((unit, key), hit) in operation.units().iter().zip(keys).zip(hits) {
        let global = match hit {
            Some(global) => global,
            None => {
                let prepared = unit.prepare(&reduction);
                remember(key, prepared.clone());
                prepared
            }
        };
        globals.push(global);
    }
    Ok(globals)
}

/// Build the bounded reduction a global estimate is prepared from: a box average over
/// [`ESTIMATE_REDUCTION`]-pixel blocks anchored at the stage origin, with a partial block averaged
/// over its actual pixels. `fetch` reads one stage pixel in linear sRGB.
///
/// The reduced frame is at most [`MAX_REDUCTION_PIXELS`] pixels, so this allocates about 3 MiB at
/// the largest stage the host accepts whatever the source is. The read itself is the whole stage,
/// which is why the store above exists.
pub(crate) fn build_reduction(
    stage: Stage,
    fetch: impl Fn(u32, u32) -> Result<[f32; 3], Error> + Sync,
) -> Result<Reduction, Error> {
    let factor = ESTIMATE_REDUCTION;
    let (width, height) = Reduction::dimensions(stage, factor);
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_REDUCTION_PIXELS {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "a spatial reduction of {pixels} pixels exceeds the {MAX_REDUCTION_PIXELS} pixel bound"
            ),
        ));
    }
    let mut blocks = vec![[0.0_f32; 3]; pixels as usize];
    let row = |j: usize, row: &mut [[f32; 3]]| -> Result<(), Error> {
        let top = j as u32 * factor;
        let bottom = (top + factor).min(stage.height);
        for (i, block) in row.iter_mut().enumerate() {
            let left = i as u32 * factor;
            let right = (left + factor).min(stage.width);
            let mut sum = [0.0_f64; 3];
            for y in top..bottom {
                for x in left..right {
                    let pixel = fetch(x, y)?;
                    for channel in 0..3 {
                        sum[channel] += f64::from(pixel[channel]);
                    }
                }
            }
            let count = f64::from(bottom - top) * f64::from(right - left);
            *block = std::array::from_fn(|channel| (sum[channel] / count) as f32);
        }
        Ok(())
    };
    if u64::from(stage.width) * u64::from(stage.height) >= super::PARALLEL_RENDER_PIXELS {
        blocks
            .par_chunks_mut(width as usize)
            .enumerate()
            .try_for_each(|(j, values)| row(j, values))?;
    } else {
        blocks
            .chunks_mut(width as usize)
            .enumerate()
            .try_for_each(|(j, values)| row(j, values))?;
    }
    let mut planes = Vec::with_capacity(blocks.len() * 3);
    for channel in 0..3 {
        planes.extend(blocks.iter().map(|block| block[channel]));
    }
    Reduction::new(stage, factor, planes)
}

/// The SHA-256 of the canonical JSON of a layer prefix, hexadecimal: what the layers before a
/// spatial operation produced, hashed exactly as an analysis identity hashes a recipe. Two stacks
/// with the same source, the same prefix and the same stage present the same pixels to the
/// operation, so they may share a global estimate; a different prefix may not.
pub(crate) fn prefix_hash(layers: &[crate::Layer]) -> Result<String, Error> {
    let canonical = serde_json::to_vec(layers).map_err(|error| {
        Error::new(
            ErrorKind::Internal,
            format!("a layer prefix could not be serialized for hashing: {error}"),
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(&canonical)))
}

/// Read one rectangle of a stage into three planar `f32` planes, in the layout
/// [`crate::modules::Planes`] expects.
pub(crate) fn fill_planes(
    region: Region,
    planes: &mut [f32],
    read: impl Fn(u32, u32) -> Result<[f32; 3], Error>,
) -> Result<(), Error> {
    let len = region.pixels() as usize;
    let mut index = 0;
    for y in region.y0..region.y1() {
        for x in region.x0..region.x1() {
            let pixel = read(x, y)?;
            planes[index] = pixel[0];
            planes[len + index] = pixel[1];
            planes[2 * len + index] = pixel[2];
            index += 1;
        }
    }
    Ok(())
}

/// One pixel of a finished tile's planes, in the rectangle the last unit filled.
pub(crate) fn plane_pixel(region: Region, values: &[f32], x: u32, y: u32) -> [f32; 3] {
    let len = region.pixels() as usize;
    let index =
        ((u64::from(y - region.y0)) * u64::from(region.width) + u64::from(x - region.x0)) as usize;
    [values[index], values[len + index], values[2 * len + index]]
}

/// The production tile size, so a caller does not have to name the constant.
pub(crate) const PRODUCTION_TILE: u32 = SPATIAL_TILE;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        EFFECT_FORMAT, Layer, LayerId, LinearImage, LinearSettings, ModuleRegistry, Raster, Recipe,
        SnapshotId, SourceImage, Transform,
        modules::{
            ActionInput, ActionPlan, Availability, EffectDescriptor, EffectStage, ModuleDescriptor,
            Processing, SpatialUnit, StageContext, ToolModule,
        },
        render::{
            linear::render_linear_tiled,
            render_tiled,
            tests::{CropReference, crop_layer, fitted_crop, geometry_registry, gradient, turn},
        },
    };
    use serde_json::{Map, Value, json};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };

    /// The spatial budget and the estimate store are process-wide, so every test that reads either
    /// holds this lock instead of racing. That is all of them: even an operation whose units want
    /// no global estimate reads and writes the store.
    static SPATIAL_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Held by every test that reads or writes either piece of process-wide state, including the
    /// Presence module's own tests in `modules::presence::oracle`.
    pub(crate) fn spatial_guard() -> std::sync::MutexGuard<'static, ()> {
        SPATIAL_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How many times any [`MeanShift`] has prepared a global estimate. The store is what should
    /// keep this small, so the tests count it.
    static PREPARED: AtomicUsize = AtomicUsize::new(0);

    // -----------------------------------------------------------------------------------------
    // Test units.
    // -----------------------------------------------------------------------------------------

    /// A separable box mean of radius `r`: the horizontal mean of the input over `2r + 1` columns,
    /// then the vertical mean of that over `2r + 1` rows, each by direct summation in a fixed
    /// order with the stage's edge clamping. The order is the presence study's, so the value at a
    /// pixel is the same whatever tile asked for it.
    #[derive(Debug)]
    struct BoxBlur {
        radius: u32,
    }

    impl SpatialUnit for BoxBlur {
        fn halo(&self, _: Stage) -> u32 {
            self.radius
        }

        /// The horizontal pass is held over the output columns and the output rows grown by the
        /// radius, which is inside the input rectangle the host hands over.
        fn scratch_bytes(&self, region: Stage) -> u64 {
            Region {
                x0: 0,
                y0: 0,
                width: region.width,
                height: region.height,
            }
            .plane_bytes()
        }

        fn prepare(&self, _: &Reduction) -> Option<Global> {
            None
        }

        fn apply(
            &self,
            input: &Planes<'_>,
            output: &mut PlanesMut<'_>,
            _: Option<&Global>,
            scratch: &mut [f32],
        ) -> Result<(), Error> {
            let stage = input.stage();
            let out = output.region();
            let radius = i64::from(self.radius);
            let count = (2 * radius + 1) as f32;
            let top = out.y0.saturating_sub(self.radius);
            let bottom = out.y1().saturating_add(self.radius).min(stage.height);
            let width = out.width as usize;
            let plane = width * (bottom - top) as usize;
            for (row, y) in (top..bottom).enumerate() {
                for (column, x) in (out.x0..out.x1()).enumerate() {
                    let mut sum = [0.0_f32; 3];
                    for dx in -radius..=radius {
                        let pixel = input.sample(i64::from(x) + dx, i64::from(y));
                        for channel in 0..3 {
                            sum[channel] += pixel[channel];
                        }
                    }
                    for (channel, total) in sum.iter().enumerate() {
                        scratch[channel * plane + row * width + column] = total / count;
                    }
                }
            }
            for y in out.y0..out.y1() {
                for (column, x) in (out.x0..out.x1()).enumerate() {
                    let mut sum = [0.0_f32; 3];
                    for dy in -radius..=radius {
                        let row =
                            (i64::from(y) + dy).clamp(0, i64::from(stage.height) - 1) as u32 - top;
                        for (channel, total) in sum.iter_mut().enumerate() {
                            *total += scratch[channel * plane + row as usize * width + column];
                        }
                    }
                    output.set(x, y, sum.map(|total| total / count));
                }
            }
            Ok(())
        }

        fn is_finite(&self) -> bool {
            true
        }

        fn describe(&self) -> String {
            format!("box blur r={}", self.radius)
        }
    }

    /// A unit with no neighbourhood at all whose value depends entirely on the global estimate:
    /// input − mean + 0.5, where the mean is the mean of the host's reduction of the operation's
    /// input stage. A wrong or missing global changes every byte, which is what makes the estimate
    /// store observable in a rendered frame.
    #[derive(Debug)]
    struct MeanShift;

    impl SpatialUnit for MeanShift {
        fn halo(&self, _: Stage) -> u32 {
            0
        }

        fn scratch_bytes(&self, _: Stage) -> u64 {
            0
        }

        fn prepare(&self, reduction: &Reduction) -> Option<Global> {
            PREPARED.fetch_add(1, AtomicOrdering::SeqCst);
            let mut sum = [0.0_f64; 3];
            let mut count = 0.0;
            for y in 0..reduction.height() {
                for x in 0..reduction.width() {
                    let pixel = reduction.pixel(x, y).expect("inside the reduced frame");
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += f64::from(pixel[channel]);
                    }
                    count += 1.0;
                }
            }
            Global::new(sum.map(|total| total / count).to_vec()).ok()
        }

        fn apply(
            &self,
            input: &Planes<'_>,
            output: &mut PlanesMut<'_>,
            global: Option<&Global>,
            _: &mut [f32],
        ) -> Result<(), Error> {
            let mean = global.map_or([0.0; 3], |global| {
                let values = global.values();
                [values[0] as f32, values[1] as f32, values[2] as f32]
            });
            let out = output.region();
            for y in out.y0..out.y1() {
                for x in out.x0..out.x1() {
                    let pixel = input.sample(i64::from(x), i64::from(y));
                    output.set(
                        x,
                        y,
                        std::array::from_fn(|channel| pixel[channel] - mean[channel] + 0.5),
                    );
                }
            }
            Ok(())
        }

        fn is_finite(&self) -> bool {
            true
        }

        fn describe(&self) -> String {
            "mean shift".into()
        }
    }

    /// A unit whose coefficients are not finite, which compilation must refuse.
    #[derive(Debug)]
    struct NonFinite;

    impl SpatialUnit for NonFinite {
        fn halo(&self, _: Stage) -> u32 {
            1
        }
        fn scratch_bytes(&self, _: Stage) -> u64 {
            0
        }
        fn prepare(&self, _: &Reduction) -> Option<Global> {
            None
        }
        fn apply(
            &self,
            _: &Planes<'_>,
            _: &mut PlanesMut<'_>,
            _: Option<&Global>,
            _: &mut [f32],
        ) -> Result<(), Error> {
            unreachable!("a non-finite unit never reaches a pixel")
        }
        fn is_finite(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "not finite".into()
        }
    }

    // -----------------------------------------------------------------------------------------
    // A test module that compiles those units.
    // -----------------------------------------------------------------------------------------

    const TEST_SPATIAL_EFFECT: &str = "test.spatial";

    struct SpatialTestModule(ModuleDescriptor);

    impl SpatialTestModule {
        fn shared() -> Arc<dyn ToolModule> {
            Arc::new(Self(ModuleDescriptor {
                id: "test.spatial".into(),
                title: "Test spatial".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: TEST_SPATIAL_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Spatial,
                    order: 0,
                }],
                actions: Vec::new(),
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
            }))
        }
    }

    impl ToolModule for SpatialTestModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Err(Error::new(ErrorKind::Internal, "no actions"))
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe_layer(&self, _: &str, _: u32, payload: &Value) -> Result<String, Error> {
            Ok(format!("test spatial {payload}"))
        }
        fn compile(&self, _: &str, _: u32, payload: &Value, _: Stage) -> Result<Processing, Error> {
            let mut units: Vec<Arc<dyn SpatialUnit>> = Vec::new();
            for unit in payload["units"].as_array().map_or(&[][..], Vec::as_slice) {
                let unit = unit.as_str().expect("a unit description");
                units.push(match unit.split_once(':') {
                    Some(("blur", radius)) => Arc::new(BoxBlur {
                        radius: radius.parse().expect("a radius"),
                    }),
                    None if unit == "shift" => Arc::new(MeanShift),
                    None if unit == "infinite" => Arc::new(NonFinite),
                    _ => panic!("unknown test unit {unit}"),
                });
            }
            Ok(Processing::Spatial(SpatialOperation::new(units)?))
        }
    }

    fn spatial_registry() -> ModuleRegistry {
        let mut registry = geometry_registry();
        registry.register(SpatialTestModule::shared()).unwrap();
        registry
    }

    fn spatial_layer(units: &[&str]) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: TEST_SPATIAL_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({ "units": units }),
        }
    }

    fn recipe(layers: Vec<Layer>) -> Recipe {
        Recipe { format: 1, layers }
    }

    // -----------------------------------------------------------------------------------------
    // An independent f64 reference for the same two units.
    // -----------------------------------------------------------------------------------------

    fn encode_reference(linear: f64) -> f64 {
        if linear <= 0.003_130_8 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    }

    fn decode_reference(encoded: f64) -> f64 {
        if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    }

    fn reference_code(linear: f64) -> u8 {
        (255.0 * encode_reference(linear.clamp(0.0, 1.0)) + 0.5).floor() as u8
    }

    /// The tolerance this task freezes for spatial float work: an exact code everywhere except
    /// within `1e-5 + 1e-5·|value|` of the threshold between two codes, where one code of
    /// difference is permitted because production filters in f32.
    fn assert_spatial_code(actual: u8, expected: f64, case: &str) {
        let clamped = expected.clamp(0.0, 1.0);
        let code = reference_code(clamped);
        if actual == code {
            return;
        }
        let difference = i32::from(actual) - i32::from(code);
        assert!(
            difference.abs() <= 1,
            "{case}: code {actual} against the reference {code} at {clamped}"
        );
        let boundary = decode_reference((f64::from(code.min(actual)) + 0.5) / 255.0);
        let tolerance = 1e-5 + 1e-5 * clamped.abs();
        assert!(
            (clamped - boundary).abs() <= tolerance,
            "{case}: code {actual} against the reference {code}, {clamped} is {} from the \
             threshold {boundary}, more than {tolerance}",
            (clamped - boundary).abs()
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum RefUnit {
        Blur(u32),
        Shift,
    }

    fn reference_blur(width: u32, height: u32, input: &[[f64; 3]], radius: u32) -> Vec<[f64; 3]> {
        let radius = i64::from(radius);
        let count = (2 * radius + 1) as f64;
        let at = |x: i64, y: i64| -> [f64; 3] {
            let x = x.clamp(0, i64::from(width) - 1) as usize;
            let y = y.clamp(0, i64::from(height) - 1) as usize;
            input[y * width as usize + x]
        };
        let mut horizontal = vec![[0.0; 3]; input.len()];
        for y in 0..i64::from(height) {
            for x in 0..i64::from(width) {
                let mut sum = [0.0; 3];
                for dx in -radius..=radius {
                    let pixel = at(x + dx, y);
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += pixel[channel];
                    }
                }
                horizontal[y as usize * width as usize + x as usize] =
                    sum.map(|total| total / count);
            }
        }
        let mut output = vec![[0.0; 3]; input.len()];
        for y in 0..i64::from(height) {
            for x in 0..i64::from(width) {
                let mut sum = [0.0; 3];
                for dy in -radius..=radius {
                    let row = (y + dy).clamp(0, i64::from(height) - 1) as usize;
                    let pixel = horizontal[row * width as usize + x as usize];
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += pixel[channel];
                    }
                }
                output[y as usize * width as usize + x as usize] = sum.map(|total| total / count);
            }
        }
        output
    }

    /// The mean of the host's reduction, computed independently: box blocks of
    /// [`ESTIMATE_REDUCTION`] anchored at the origin, partial blocks averaged over their actual
    /// pixels, then the mean of the reduced pixels.
    fn reference_mean(width: u32, height: u32, input: &[[f64; 3]]) -> [f64; 3] {
        let factor = ESTIMATE_REDUCTION;
        let (reduced_width, reduced_height) = (width.div_ceil(factor), height.div_ceil(factor));
        let mut sum = [0.0_f64; 3];
        let mut blocks = 0.0;
        for j in 0..reduced_height {
            for i in 0..reduced_width {
                let (left, top) = (i * factor, j * factor);
                let (right, bottom) = ((left + factor).min(width), (top + factor).min(height));
                let mut block = [0.0_f64; 3];
                for y in top..bottom {
                    for x in left..right {
                        let pixel = input[(y * width + x) as usize];
                        for (channel, total) in block.iter_mut().enumerate() {
                            *total += pixel[channel];
                        }
                    }
                }
                let count = f64::from(bottom - top) * f64::from(right - left);
                for (channel, total) in sum.iter_mut().enumerate() {
                    // The host stores a reduced pixel as f32 before a unit reads it.
                    *total += f64::from((block[channel] / count) as f32);
                }
                blocks += 1.0;
            }
        }
        sum.map(|total| total / blocks)
    }

    fn reference_chain(
        width: u32,
        height: u32,
        input: Vec<[f64; 3]>,
        units: &[RefUnit],
    ) -> Vec<[f64; 3]> {
        let mean = reference_mean(width, height, &input);
        let mut current = input;
        for unit in units {
            current = match unit {
                RefUnit::Blur(radius) => reference_blur(width, height, &current, *radius),
                RefUnit::Shift => current
                    .iter()
                    .map(|pixel| {
                        std::array::from_fn(|channel| pixel[channel] - mean[channel] + 0.5)
                    })
                    .collect(),
            };
        }
        current
    }

    /// One byte frame decoded into linear f64, which is what the byte path hands the operation.
    fn decode_frame(bytes: &[u8]) -> Vec<[f64; 3]> {
        bytes
            .chunks_exact(4)
            .map(|pixel| {
                std::array::from_fn(|channel| decode_reference(f64::from(pixel[channel]) / 255.0))
            })
            .collect()
    }

    fn assert_frame(raster: &Raster, expected: &[[f64; 3]], case: &str) {
        assert_eq!(
            (raster.width as usize) * (raster.height as usize),
            expected.len(),
            "{case}: dimensions"
        );
        for y in 0..raster.height {
            for x in 0..raster.width {
                let pixel = raster.pixel(x, y).expect("inside the stage");
                let reference = expected[(y * raster.width + x) as usize];
                for channel in 0..3 {
                    assert_spatial_code(
                        pixel[channel],
                        reference[channel],
                        &format!("{case}: pixel ({x}, {y}) channel {channel}"),
                    );
                }
            }
        }
    }

    fn linear_source(width: u32, height: u32) -> LinearImage {
        let mut planes = Vec::with_capacity((width * height * 3) as usize);
        for channel in 0..3 {
            for y in 0..height {
                for x in 0..width {
                    planes
                        .push(((x * 7 + y * 13 + channel * 29) % 251) as f32 / 251.0 * 0.9 + 0.02);
                }
            }
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:linear-spatial").unwrap()
    }

    fn linear_frame(source: &LinearImage) -> Vec<[f64; 3]> {
        let (width, height) = (source.width(), source.height());
        let mut frame = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                frame.push(source.pixel(x, y).expect("inside the view").map(f64::from));
            }
        }
        frame
    }

    // -----------------------------------------------------------------------------------------
    // Tiling and exactness.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_tiled_render_matches_the_whole_frame_reference_at_every_tile_size() {
        let _guard = spatial_guard();
        clear_estimates();
        // Larger than one production tile on both sides of the 512 grid, so both tile sizes
        // exercise partial edge tiles and more than one batch.
        let source = gradient(600, 400);
        let registry = spatial_registry();
        let stack = recipe(vec![spatial_layer(&["blur:4"])]);
        let expected = reference_chain(
            600,
            400,
            decode_frame(source.rgba.as_ref()),
            &[RefUnit::Blur(4)],
        );
        let mut frames = Vec::new();
        for tile in [128_u32, 512] {
            let raster = render_tiled(
                &registry,
                &source,
                SnapshotId::new(),
                &stack,
                &Cancel::new(),
                tile,
            )
            .unwrap();
            assert_frame(&raster, &expected, &format!("tile {tile}"));
            frames.push(raster.rgba.as_ref().to_vec());
        }
        assert_eq!(frames[0], frames[1], "the tile size changes no byte");
        // Deterministic across runs: the same stack rendered again is the same frame.
        let again = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            512,
        )
        .unwrap();
        assert_eq!(again.rgba.as_ref(), frames[1].as_slice(), "deterministic");
    }

    #[test]
    fn a_tiled_linear_render_matches_the_whole_frame_reference_at_every_tile_size() {
        let _guard = spatial_guard();
        clear_estimates();
        let source = linear_source(200, 150);
        let registry = spatial_registry();
        let stack = recipe(vec![spatial_layer(&["blur:3"])]);
        let expected = reference_chain(200, 150, linear_frame(&source), &[RefUnit::Blur(3)]);
        let mut frames = Vec::new();
        for tile in [64_u32, 512] {
            let raster = render_linear_tiled(
                &registry,
                &source,
                SnapshotId::new(),
                &stack,
                LinearSettings::default(),
                &Cancel::new(),
                tile,
            )
            .unwrap();
            assert_frame(&raster, &expected, &format!("linear tile {tile}"));
            frames.push(raster.rgba.as_ref().to_vec());
        }
        assert_eq!(frames[0], frames[1], "the tile size changes no byte");
    }

    #[test]
    fn a_rotate_before_a_spatial_layer_blurs_the_rotated_stage() {
        let _guard = spatial_guard();
        clear_estimates();
        let source = gradient(90, 60);
        let registry = spatial_registry();
        let stack = recipe(vec![
            turn(Transform::RotateRight),
            spatial_layer(&["blur:2"]),
        ]);
        // The stepwise reference: rotate the source bytes, then blur the rotated stage.
        let (width, height) = (60_u32, 90_u32);
        let mut rotated = vec![0_u8; (width * height * 4) as usize];
        for y in 0..source.height {
            for x in 0..source.width {
                let (to_x, to_y) = (source.height - 1 - y, x);
                let from = ((y * source.width + x) * 4) as usize;
                let to = ((to_y * width + to_x) * 4) as usize;
                rotated[to..to + 4].copy_from_slice(&source.rgba[from..from + 4]);
            }
        }
        let expected = reference_chain(width, height, decode_frame(&rotated), &[RefUnit::Blur(2)]);
        let raster = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            32,
        )
        .unwrap();
        assert_eq!((raster.width, raster.height), (width, height));
        assert_frame(&raster, &expected, "rotate then blur");
    }

    #[test]
    fn a_replacement_before_a_spatial_layer_is_blurred_and_one_after_it_is_not() {
        let _guard = spatial_guard();
        clear_estimates();
        let source = gradient(80, 60);
        let registry = spatial_registry();
        let before = [10_u8, 200, 30];
        let after = [250_u8, 5, 120];
        let stack = recipe(vec![
            Layer::pixel(20, 15, before),
            spatial_layer(&["blur:2"]),
            Layer::pixel(60, 40, after),
        ]);
        let mut input = source.rgba.as_ref().to_vec();
        let offset = ((15 * 80 + 20) * 4) as usize;
        input[offset..offset + 3].copy_from_slice(&before);
        let mut expected = reference_chain(80, 60, decode_frame(&input), &[RefUnit::Blur(2)]);
        // The replacement after the operation overwrites the blurred value exactly.
        expected[(40 * 80 + 60) as usize] =
            std::array::from_fn(|channel| decode_reference(f64::from(after[channel]) / 255.0));
        let raster = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            32,
        )
        .unwrap();
        assert_frame(&raster, &expected, "replacement around a spatial layer");
        assert_eq!(
            raster.pixel(60, 40).unwrap()[..3],
            after,
            "the replacement after the operation is written as it stands"
        );
    }

    #[test]
    fn a_crop_resample_after_a_spatial_layer_resamples_the_blurred_frame() {
        let _guard = spatial_guard();
        clear_estimates();
        let source = gradient(80, 60);
        let registry = spatial_registry();
        let crop = fitted_crop(80, 60, 8.0, [0.15, 0.15, 0.6, 0.6]);
        let stack = recipe(vec![spatial_layer(&["blur:2"]), crop_layer(crop)]);
        // The reference: blur the whole frame in f64, quantize it as the stage boundary does, then
        // resample those bytes with the independent crop sampler.
        let blurred = reference_chain(
            80,
            60,
            decode_frame(source.rgba.as_ref()),
            &[RefUnit::Blur(2)],
        );
        let mut bytes = Vec::with_capacity(blurred.len() * 4);
        for (index, pixel) in blurred.iter().enumerate() {
            bytes.extend(pixel.map(reference_code));
            bytes.push(source.rgba[index * 4 + 3]);
        }
        let blurred_source = SourceImage {
            width: 80,
            height: 60,
            rgba: bytes.into(),
            fingerprint: "sha256:blurred".into(),
            orientation: 1,
        };
        let reference = CropReference::new(&blurred_source, crop);
        let raster = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            32,
        )
        .unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                let actual = raster.pixel(x, y).unwrap();
                let expected = reference.pixel(&blurred_source, x, y);
                for channel in 0..4 {
                    assert!(
                        (i32::from(actual[channel]) - i32::from(expected[channel])).abs() <= 1,
                        "crop after blur: pixel ({x}, {y}) channel {channel}: {actual:?} against \
                         {expected:?}"
                    );
                }
            }
        }
        // And the two paths agree with each other everywhere, which is what the acceptance
        // chapter samples through.
        for y in 0..raster.height {
            for x in 0..raster.width {
                let sampled = crate::sample(&registry, &source, &stack, x, y).unwrap();
                assert_eq!(
                    sampled.rgba,
                    raster.pixel(x, y),
                    "crop after blur: sample at ({x}, {y})"
                );
            }
        }
    }

    /// A drafted white balance approximated on the developed planes names the same recipe prefix a
    /// committed render of that white balance does, so the estimate store must key it apart: in
    /// either order, the exact evaluation takes nothing estimated from approximate pixels, and the
    /// approximate one takes nothing estimated from exact ones.
    #[test]
    fn an_approximate_white_balance_never_shares_a_global_estimate_with_the_exact_evaluation() {
        let _guard = spatial_guard();
        let registry = spatial_registry();
        let stack = recipe(vec![spatial_layer(&["shift"])]);
        let source = linear_source(40, 30);
        let approximate = LinearSettings {
            exposure_ev: 0.0,
            white_balance: Some(
                crate::WhiteBalanceApproximation::from_matrix([
                    [1.4, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.6],
                ])
                .unwrap(),
            ),
        };
        let render = |settings: LinearSettings| {
            crate::render_linear(&registry, &source, SnapshotId::new(), &stack, settings)
                .unwrap()
                .rgba
        };
        clear_estimates();
        let exact_alone = render(LinearSettings::default());
        clear_estimates();
        let approximate_alone = render(approximate);
        assert_ne!(
            exact_alone, approximate_alone,
            "the mean shift moves with the approximated scene"
        );

        clear_estimates();
        let _ = render(approximate);
        assert_eq!(
            render(LinearSettings::default()),
            exact_alone,
            "an exact render after an approximate one estimated from exact pixels"
        );
        clear_estimates();
        let _ = render(LinearSettings::default());
        assert_eq!(
            render(approximate),
            approximate_alone,
            "an approximate render after an exact one estimated from its own pixels"
        );
    }

    #[test]
    fn a_sample_equals_every_rendered_byte_on_both_paths() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let stack = recipe(vec![spatial_layer(&["blur:3", "shift"])]);
        let source = gradient(48, 36);
        let raster = crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                let sampled = crate::sample(&registry, &source, &stack, x, y).unwrap();
                assert_eq!(
                    sampled.rgba,
                    raster.pixel(x, y),
                    "byte path: sample at ({x}, {y})"
                );
            }
        }
        let linear = linear_source(40, 30);
        let rendered = crate::render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap();
        for y in 0..rendered.height {
            for x in 0..rendered.width {
                let sampled = crate::sample_linear(
                    &registry,
                    &linear,
                    &stack,
                    LinearSettings::default(),
                    x,
                    y,
                )
                .unwrap();
                assert_eq!(
                    sampled.rgba,
                    rendered.pixel(x, y),
                    "linear path: sample at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn the_linear_path_agrees_with_itself_through_a_spatial_layer_and_a_crop() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = linear_source(60, 44);
        let crop = fitted_crop(60, 44, 6.0, [0.2, 0.2, 0.55, 0.55]);
        let stack = recipe(vec![spatial_layer(&["blur:2", "shift"]), crop_layer(crop)]);
        let rendered = crate::render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap();
        assert!(rendered.width > 1 && rendered.height > 1);
        for y in 0..rendered.height {
            for x in 0..rendered.width {
                let sampled = crate::sample_linear(
                    &registry,
                    &source,
                    &stack,
                    LinearSettings::default(),
                    x,
                    y,
                )
                .unwrap();
                assert_eq!(
                    sampled.rgba,
                    rendered.pixel(x, y),
                    "linear crop after blur: sample at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn a_sample_uses_the_same_tile_grid_the_render_used() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = gradient(50, 40);
        let stack = recipe(vec![spatial_layer(&["blur:2", "shift"])]);
        // A tile smaller than the frame, so the sampled pixel's tile is one of several and its
        // halo is clamped differently from the whole frame's.
        let raster = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            16,
        )
        .unwrap();
        let evaluation = crate::render::Evaluation::new(&registry, &source, &stack)
            .unwrap()
            .with_tile(16);
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    evaluation.pixel(x, y).unwrap(),
                    raster.pixel(x, y),
                    "tile 16: sample at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn a_sample_through_a_spatial_layer_reserves_one_working_set() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = gradient(600, 400);
        let stack = recipe(vec![spatial_layer(&["blur:4"])]);
        let operation = SpatialOperation::new(vec![Arc::new(BoxBlur { radius: 4 })]).unwrap();
        let plan = SpatialPlan::new(
            &operation,
            Stage {
                width: 600,
                height: 400,
            },
            SPATIAL_TILE,
        )
        .unwrap();
        let budget = SpatialBudget::default();
        assert_eq!(budget.in_use(), 0, "nothing is held between renders");
        budget.reset_peak();
        let sampled = crate::sample(&registry, &source, &stack, 300, 200).unwrap();
        assert!(sampled.rgba.is_some());
        assert_eq!(
            budget.peak(),
            plan.working_set(),
            "a sample allocates exactly one tile working set"
        );
        assert_eq!(budget.in_use(), 0, "and releases it");
    }

    #[test]
    fn two_chained_units_match_the_reference_chain_and_sum_their_halos() {
        let _guard = spatial_guard();
        clear_estimates();
        let source = gradient(120, 90);
        let registry = spatial_registry();
        let stack = recipe(vec![spatial_layer(&["blur:3", "shift"])]);
        let expected = reference_chain(
            120,
            90,
            decode_frame(source.rgba.as_ref()),
            &[RefUnit::Blur(3), RefUnit::Shift],
        );
        for tile in [32_u32, 512] {
            let raster = render_tiled(
                &registry,
                &source,
                SnapshotId::new(),
                &stack,
                &Cancel::new(),
                tile,
            )
            .unwrap();
            assert_frame(&raster, &expected, &format!("chain at tile {tile}"));
        }
        // Three units of different halos add up, and the chain's rectangles still cover the tile.
        let operation = SpatialOperation::new(vec![
            Arc::new(BoxBlur { radius: 3 }),
            Arc::new(MeanShift),
            Arc::new(BoxBlur { radius: 5 }),
        ])
        .unwrap();
        let stage = Stage {
            width: 120,
            height: 90,
        };
        assert_eq!(operation.summed_halo(stage), 8);
        assert_eq!(operation.halos(stage), vec![3, 0, 5]);
        let plan = SpatialPlan::new(&operation, stage, 32).unwrap();
        let regions = plan.regions(Region {
            x0: 32,
            y0: 32,
            width: 32,
            height: 32,
        });
        assert_eq!(regions.len(), 4);
        assert_eq!(
            regions[0],
            Region {
                x0: 24,
                y0: 24,
                width: 48,
                height: 48
            }
        );
        assert_eq!(
            regions[3],
            Region {
                x0: 32,
                y0: 32,
                width: 32,
                height: 32
            }
        );
    }

    #[test]
    fn the_tile_grid_is_anchored_at_the_stage_origin() {
        let _guard = spatial_guard();
        let operation = SpatialOperation::new(vec![Arc::new(BoxBlur { radius: 1 })]).unwrap();
        let stage = Stage {
            width: 1100,
            height: 600,
        };
        let plan = SpatialPlan::new(&operation, stage, SPATIAL_TILE).unwrap();
        let tiles = plan.tiles();
        assert_eq!(tiles.len(), 3 * 2, "three columns and two rows");
        assert_eq!(tiles[0].x0, 0);
        assert_eq!(tiles[2].width, 1100 - 1024, "a partial tile at the edge");
        assert_eq!(tiles[5].height, 600 - 512);
        assert_eq!(plan.tile_containing(700, 300).x0, 512);
        assert_eq!(plan.tile_containing(700, 300).y0, 0);
        assert_eq!(plan.tile_containing(1099, 599), tiles[5]);
    }

    // -----------------------------------------------------------------------------------------
    // Refusals.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn an_operation_the_host_cannot_run_is_refused_before_any_pixel_work() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = gradient(64, 48);
        let budget = SpatialBudget::default();
        budget.reset_peak();
        let refuse = |layers: Vec<Layer>| -> Error {
            let error = crate::render(&registry, &source, SnapshotId::new(), &recipe(layers))
                .expect_err("the host refuses this operation");
            assert_eq!(error.kind, ErrorKind::ResourceLimit, "{error}");
            error
        };
        let halo = refuse(vec![spatial_layer(&["blur:300", "blur:300"])]);
        assert_eq!(halo.detail, "spatial halo 600 px exceeds the 512 px bound");
        let units = refuse(vec![spatial_layer(&[
            "blur:1", "blur:1", "blur:1", "blur:1", "blur:1",
        ])]);
        assert!(units.detail.contains("5 units"), "{units}");
        let finite = refuse(vec![spatial_layer(&["infinite"])]);
        assert!(finite.detail.contains("not finite"), "{finite}");
        assert_eq!(budget.in_use(), 0, "no reservation was taken");
        assert_eq!(budget.peak(), 0, "and no pixel work was done");
        // A refused stack is still readable: nothing was rewritten.
        assert_eq!(
            crate::extents(
                &registry,
                &source,
                &recipe(vec![spatial_layer(&["blur:1"])])
            )
            .unwrap(),
            (64, 48)
        );
    }

    // -----------------------------------------------------------------------------------------
    // The budget is a target.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_reservation_takes_what_fits_and_never_less_than_one_tile() {
        let _guard = spatial_guard();
        let budget = SpatialBudget::default();
        assert_eq!(budget.in_use(), 0, "nothing is held between tests");
        let previous = budget.set_target(1000);
        {
            let all = budget.reserve(100, 4);
            assert_eq!(all.tiles(), 4, "everything asked for fits");
            let some = budget.reserve(100, 8);
            assert_eq!(some.tiles(), 6, "only what the target has left");
            assert_eq!(budget.in_use(), 1000);
            let over = budget.reserve(100, 8);
            assert_eq!(over.tiles(), 1, "a taken target still runs one tile");
            let huge = budget.reserve(5000, 2);
            assert_eq!(huge.tiles(), 1, "a tile larger than the target runs alone");
            assert_eq!(budget.in_use(), 6100);
        }
        assert_eq!(budget.in_use(), 0, "every reservation is released");
        budget.set_target(previous);
    }

    /// The reported failure: the histogram's analysis and the preview's exact phase rendering the
    /// same stack at once, each sizing its batch to nearly the whole target, so the second one
    /// found `254059360 of the 268435456 byte spatial budget` in use and failed. Holding the whole
    /// target stands in for the other evaluation, deterministically.
    #[test]
    fn a_render_and_a_sample_complete_when_another_evaluation_holds_the_target() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let (width, height, tile) = (200_u32, 150_u32, 32_u32);
        let stack = recipe(vec![spatial_layer(&["blur:3"])]);
        let operation = SpatialOperation::new(vec![Arc::new(BoxBlur { radius: 3 })]).unwrap();
        let plan = SpatialPlan::new(&operation, Stage { width, height }, tile).unwrap();
        assert!(
            plan.concurrency() > 1,
            "alone, the operation runs tiles together"
        );
        let budget = SpatialBudget::default();
        let held = budget.reserve(budget.target(), 1);
        budget.reset_peak();

        let source = gradient(width, height);
        let raster = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            tile,
        )
        .expect("the byte render completes past the target");
        let expected = reference_chain(
            width,
            height,
            decode_frame(source.rgba.as_ref()),
            &[RefUnit::Blur(3)],
        );
        assert_frame(&raster, &expected, "byte render beside a taken target");
        assert_eq!(
            budget.peak(),
            budget.target() + plan.working_set(),
            "one tile at a time, one working set past the target"
        );

        let linear = linear_source(width, height);
        let raster = render_linear_tiled(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
            &Cancel::new(),
            tile,
        )
        .expect("the RAW render completes past the target");
        let expected = reference_chain(width, height, linear_frame(&linear), &[RefUnit::Blur(3)]);
        assert_frame(&raster, &expected, "linear render beside a taken target");

        let rendered = crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        let sampled = crate::sample(&registry, &source, &stack, 100, 75)
            .expect("the sample completes past the target");
        assert_eq!(sampled.rgba, rendered.pixel(100, 75), "the rendered byte");

        drop(held);
        assert_eq!(budget.in_use(), 0, "every batch released its reservation");
    }

    #[test]
    fn a_tile_larger_than_the_target_renders_alone() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let (width, height, tile) = (64_u32, 48_u32, 16_u32);
        let source = gradient(width, height);
        let stack = recipe(vec![spatial_layer(&["blur:2"])]);
        let operation = SpatialOperation::new(vec![Arc::new(BoxBlur { radius: 2 })]).unwrap();
        let budget = SpatialBudget::default();
        let previous = budget.set_target(1024);
        let plan = SpatialPlan::new(&operation, Stage { width, height }, tile)
            .expect("what a tile costs never refuses a plan");
        assert!(plan.working_set() > budget.target());
        assert_eq!(plan.concurrency(), 1);
        budget.reset_peak();
        let raster = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            tile,
        );
        budget.set_target(previous);
        let raster = raster.expect("the render completes");
        let expected = reference_chain(
            width,
            height,
            decode_frame(source.rgba.as_ref()),
            &[RefUnit::Blur(2)],
        );
        assert_frame(&raster, &expected, "tiles larger than the target");
        assert_eq!(budget.peak(), plan.working_set(), "one tile at a time");
        assert_eq!(budget.in_use(), 0);
    }

    // -----------------------------------------------------------------------------------------
    // The estimate store.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_prepared_estimate_is_reused_and_the_store_evicts_the_oldest() {
        let _guard = spatial_guard();
        clear_estimates();
        PREPARED.store(0, AtomicOrdering::SeqCst);
        let registry = spatial_registry();
        let source = gradient(64, 48);
        let stack = recipe(vec![spatial_layer(&["shift"])]);
        let first = crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        assert_eq!(PREPARED.load(AtomicOrdering::SeqCst), 1, "one preparation");
        let second = crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        assert_eq!(
            PREPARED.load(AtomicOrdering::SeqCst),
            1,
            "the second render hits the store"
        );
        assert_eq!(first.rgba, second.rgba, "and produces the same frame");
        // A different prefix is a different key, so it misses.
        let prefixed = recipe(vec![
            Layer::pixel(1, 1, [3, 4, 5]),
            spatial_layer(&["shift"]),
        ]);
        crate::render(&registry, &source, SnapshotId::new(), &prefixed).unwrap();
        assert_eq!(
            PREPARED.load(AtomicOrdering::SeqCst),
            2,
            "a different prefix misses"
        );
        assert_eq!(cached_estimates(), 2);
        // Nine distinct keys evict the oldest one, which then has to be prepared again.
        for index in 0..ESTIMATE_STORE_ENTRIES {
            let stack = recipe(vec![
                Layer::pixel(2, 2, [index as u8, 0, 0]),
                spatial_layer(&["shift"]),
            ]);
            crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        }
        assert_eq!(cached_estimates(), ESTIMATE_STORE_ENTRIES);
        let before = PREPARED.load(AtomicOrdering::SeqCst);
        crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        assert_eq!(
            PREPARED.load(AtomicOrdering::SeqCst),
            before + 1,
            "the oldest entry was evicted and is prepared again"
        );
    }

    /// A stored estimate belongs to the unit that prepared it, not to its position alone. A module
    /// compiles whichever units its payload needs — the Presence module leaves out a unit whose
    /// amount is zero, which moves the others up — so the same position of the same source, prefix
    /// and stage can hold a different unit from one render to the next, and the cached answer "this
    /// unit wants none" must not be handed to a unit that wants one.
    #[test]
    fn an_estimate_belongs_to_its_unit_and_not_to_its_position_alone() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = gradient(64, 48);
        // A unit that wants no estimate at position 0, cached as such.
        crate::render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![spatial_layer(&["blur:2"])]),
        )
        .unwrap();
        // The same source, the same (empty) prefix and the same stage, with a unit that does want
        // one at that position.
        let shifted = crate::render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![spatial_layer(&["shift"])]),
        )
        .unwrap();
        let expected = reference_chain(
            64,
            48,
            decode_frame(source.rgba.as_ref()),
            &[RefUnit::Shift],
        );
        assert_frame(&shifted, &expected, "a shift after a blur at position 0");
    }

    // -----------------------------------------------------------------------------------------
    // Cancellation.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_cancelled_render_stops_between_tile_batches_and_releases_its_reservation() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        // Many tiles, one at a time: the target is lowered to exactly one working set so the
        // operation runs a batch of one tile, which is where the token is checked.
        let source = gradient(2000, 1500);
        let stack = recipe(vec![spatial_layer(&["blur:24"])]);
        let operation = SpatialOperation::new(vec![Arc::new(BoxBlur { radius: 24 })]).unwrap();
        let budget = SpatialBudget::default();
        let plan = SpatialPlan::new(
            &operation,
            Stage {
                width: 2000,
                height: 1500,
            },
            SPATIAL_TILE,
        )
        .unwrap();
        assert!(plan.tiles().len() > 4, "several batches of one tile");
        let previous = budget.set_target(plan.working_set());
        let cancel = Cancel::new();
        let handle = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                cancel.cancel();
            })
        };
        let started = std::time::Instant::now();
        let result =
            crate::render_cancellable(&registry, &source, SnapshotId::new(), &stack, &cancel);
        let elapsed = started.elapsed();
        handle.join().unwrap();
        let error = match result {
            Ok(_) => panic!("a cancelled render does not return a frame"),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "a cancelled render returns promptly, not in {elapsed:?}"
        );
        assert_eq!(budget.in_use(), 0, "the batch reservation is released");
        // An already cancelled token refuses before any tile runs.
        let error =
            match crate::render_cancellable(&registry, &source, SnapshotId::new(), &stack, &cancel)
            {
                Ok(_) => panic!("still cancelled"),
                Err(error) => error,
            };
        assert_eq!(error.kind, ErrorKind::Cancelled);
        budget.set_target(previous);
    }

    #[test]
    fn a_neutral_spatial_payload_opens_no_boundary_and_shares_the_source_buffer() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = gradient(32, 24);
        let stack = recipe(vec![spatial_layer(&[])]);
        let budget = SpatialBudget::default();
        budget.reset_peak();
        let raster = crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        assert_eq!(raster.rgba, source.rgba, "the identity byte path");
        assert!(
            Arc::ptr_eq(&raster.rgba, &source.rgba),
            "and the shared source allocation"
        );
        assert_eq!(budget.peak(), 0, "no tile ran");
    }

    // -----------------------------------------------------------------------------------------
    // Measurement.
    // -----------------------------------------------------------------------------------------

    #[test]
    #[ignore = "measurement, run explicitly in release"]
    fn spatial_timing() {
        let registry = spatial_registry();
        for (width, height, radius) in [(6000_u32, 4000_u32, 137_u32), (10_000, 6000, 224)] {
            let source = gradient(width, height);
            let stack = recipe(vec![spatial_layer(&[&format!("blur:{radius}")])]);
            let operation = SpatialOperation::new(vec![Arc::new(BoxBlur { radius })]).unwrap();
            let plan = SpatialPlan::new(&operation, Stage { width, height }, SPATIAL_TILE).unwrap();
            // Warm the source and the estimate store, then measure.
            crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
            SpatialBudget::default().reset_peak();
            let mut samples = Vec::new();
            for _ in 0..10 {
                let started = std::time::Instant::now();
                let raster = crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                assert_eq!((raster.width, raster.height), (width, height));
            }
            samples.sort_by(f64::total_cmp);
            let p50 = samples[samples.len() / 2];
            let p95 = samples[(samples.len() as f64 * 0.95).ceil() as usize - 1];
            println!(
                "{width}x{height} box blur r={radius}: p50 {p50:.0} ms, p95 {p95:.0} ms over \
                 {} runs; working set {:.1} MiB, concurrency {}, budget peak {:.1} MiB, \
                 target {:.1} MiB",
                samples.len(),
                plan.working_set() as f64 / MIB,
                plan.concurrency(),
                SpatialBudget::default().peak() as f64 / MIB,
                SpatialBudget::default().target() as f64 / MIB,
            );
        }
    }
}
