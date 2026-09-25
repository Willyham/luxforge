//! Executing the spatial primitive: the budget, the tiling, the unit chain, the global-estimate
//! store and the cancellation token.
//!
//! The contract a module writes against is in [`crate::modules::SpatialUnit`]. This module owns the
//! other half: how much one tile costs, how many tiles may be in flight, where the intermediate
//! planes come from and how a point query evaluates only the tiles it needs, each once, so that a
//! sampled byte is the byte a render of that tile produces.

pub(crate) use super::Cancel;
use crate::{
    Error, ErrorKind,
    mask_field::{MaskField, MaskSampling},
    modules::{
        ESTIMATE_REDUCTION, ESTIMATE_STORE_ENTRIES, Global, MAX_REDUCTION_PIXELS, MAX_SPATIAL_HALO,
        Parallelism, Planes, PlanesMut, Reduction, Region, SPATIAL_BUDGET_BYTES, SPATIAL_TILE,
        SpatialOperation, Stage,
    },
};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(test)]
const MIB: f64 = (1024 * 1024) as f64;

/// A process-wide target for the working sets of spatial tiles, separate from the colour run's
/// [`ScratchBudget`](super::ScratchBudget) because one tile is orders of magnitude larger than one
/// row chunk: a 512 × 512 tile of a 60 MP stage with all three frozen presence units reads a
/// 1408 × 1408 input region and needs about 101 MiB, which the 64 MiB scratch target could not hold
/// at all.
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
        // **Where the mask is read.** A masked colour operation lives inside a segment, so more
        // exact geometry may follow it in that same segment and the host has to compose that suffix
        // and `unmap` the frame coordinate back to the stage the mask was compiled against. A
        // spatial operation needs the equivalent reasoning and reaches the opposite conclusion: it
        // *is* a stage boundary, so it opens a new segment at the stage its layer received and the
        // segment before it is closed at exactly that stage. The frame this operation reads and the
        // stage this mask was compiled against are therefore the same frame, and the suffix is the
        // identity — a tile's own stage coordinates are the mask's coordinates, with no mapping at
        // all. This is not an assumption: the host compiles the mask against the same `stage` it
        // builds this plan from, and disagreement is a host bug rather than a stack to refuse, so
        // it is `internal` and it fires before a pixel is read.
        if let Some(mask) = operation.mask()
            && mask.stage() != stage
        {
            return Err(Error::new(
                ErrorKind::Internal,
                format!(
                    "a spatial mask compiled against {}x{} reached a {}x{} stage",
                    mask.stage().width,
                    mask.stage().height,
                    stage.width,
                    stage.height
                ),
            ));
        }
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
///
/// A **masked** operation adds exactly one tile-sized plane buffer on top of that: the snapshot of
/// the tile's own input the blend is against. The blend itself is in place in the last unit's
/// planes, which the chain already counted. One tile does not scale with the frame, and an unmasked
/// operation adds nothing at all, so its working set, its concurrency and therefore its batching are
/// byte for byte what they were before masks existed.
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
    if operation.mask().is_some() {
        let tile = Region {
            x0: 0,
            y0: 0,
            width: tile_width,
            height: tile_height,
        };
        planes = planes.saturating_add(tile.plane_bytes());
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

/// Run one tile's unit chain. `fill` writes the input region's three planes; the result is the
/// rectangle the values cover and those planar values, which always contains `tile`. `parallelism`
/// is handed to every unit and decides whether this function's own finiteness check runs on the
/// pool; it never changes a value.
///
/// Every path uses this one function: the byte render, the RAW float frame and the point sample.
/// That is what makes a sample equal to the rendered byte by construction rather than by
/// agreement between two implementations — **including the mask**, because the blend below is
/// inside this function and not in any caller.
///
/// An unmasked operation takes exactly the path it always did and returns the last unit's
/// rectangle. A masked operation takes one of two paths, and neither of them touches the halo, the
/// tile alignment, what a unit reads, the scratch or the cached global estimates:
///
/// - **A tile the mask cannot reach is a copy.** Coverage is exactly zero outside
///   [`MaskField::bounds`], so the blend there is the identity and no unit is evaluated at all.
///   The tile is filled directly, which reads the tile and not the tile grown by the summed halo,
///   and that is what keeps a small masked Presence layer affordable on a 60 MP frame.
/// - **Every other tile runs the whole chain unchanged** and the *write* is blended in place:
///   `out = (1 − M)·in + M·u` per channel, in linear float, against the same input the tile already
///   holds. **The blend covers `tile` and nothing else**, which is exactly what every caller reads
///   out of the result — away from the stage edges the last unit's rectangle *is* the tile, and
///   against an edge the shrink rule leaves it wider and the extra rows hold unblended filter
///   output that nobody takes. Blending in place rather than copying the tile out is what keeps a
///   masked tile to one extra allocation instead of two.
pub(crate) fn run_tile(
    plan: &SpatialPlan,
    operation: &SpatialOperation,
    globals: &[Option<Global>],
    tile: Region,
    parallelism: Parallelism,
    fill: impl FnOnce(Region, &mut [f32]) -> Result<(), Error>,
) -> Result<(Region, Vec<f32>), Error> {
    let stage = plan.stage;
    let mask = operation.mask();
    if let Some(mask) = mask
        && !reaches(mask.bounds(), tile)
    {
        #[cfg(test)]
        MASKED_TILES_COPIED.fetch_add(1, Ordering::Relaxed);
        let mut values = vec![0.0_f32; (tile.pixels() * 3) as usize];
        fill(tile, &mut values)?;
        return Ok((tile, values));
    }
    let regions = plan.regions(tile);
    let mut values = vec![0.0_f32; (regions[0].pixels() * 3) as usize];
    fill(regions[0], &mut values)?;
    // The snapshot of the tile's own input, taken before the chain runs because the chain consumes
    // the buffer it was read into. It is one tile and it is charged to the budget through
    // `worst_case_working_set`; nothing here scales with the frame.
    let input = if mask.is_some() {
        #[cfg(test)]
        MASKED_TILES_EVALUATED.fetch_add(1, Ordering::Relaxed);
        Some(cut_out(regions[0], &values, tile))
    } else {
        None
    };
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
            parallelism,
        )?;
        let finite = match parallelism {
            Parallelism::Pool => next.par_iter().all(|value| value.is_finite()),
            Parallelism::Serial => next.iter().all(|value| value.is_finite()),
        };
        if !finite {
            return Err(Error::new(ErrorKind::ResourceLimit, NON_FINITE_SPATIAL));
        }
        values = next;
    }
    let region = *regions.last().expect("a chain always has an input region");
    if let (Some(mask), Some(input)) = (mask, &input) {
        blend(mask, region, tile, input, &mut values);
    }
    Ok((region, values))
}

/// Whether a mask with these bounds can reach any pixel of this tile. An empty rectangle reaches
/// nothing, which is how an `amount` of exactly zero makes every tile of the frame a copy.
fn reaches(bounds: Region, tile: Region) -> bool {
    !bounds.is_empty()
        && !tile.is_empty()
        && bounds.x0 < tile.x1()
        && tile.x0 < bounds.x1()
        && bounds.y0 < tile.y1()
        && tile.y0 < bounds.y1()
}

/// Copy one tile's three planes out of a larger rectangle's planes, row by row. `tile` must lie
/// inside `region`, which the chain's own shrink rule guarantees.
///
/// It is built with `with_capacity` and `extend_from_slice` rather than a zeroed `vec!`, because
/// every value is written before any is read and zeroing one tile plane per tile is a page fault
/// per 4 KiB for nothing.
fn cut_out(region: Region, values: &[f32], tile: Region) -> Vec<f32> {
    let source = region.pixels() as usize;
    let width = tile.width as usize;
    let mut out = Vec::with_capacity(tile.pixels() as usize * 3);
    for channel in 0..3 {
        for y in tile.y0..tile.y1() {
            let from = channel * source
                + (y - region.y0) as usize * region.width as usize
                + (tile.x0 - region.x0) as usize;
            out.extend_from_slice(&values[from..from + width]);
        }
    }
    out
}

/// The masked write: `out = (1 − M)·in + M·u` per channel, over the tile, in linear float, in place
/// in the last unit's planes.
///
/// It is the colour primitive's spelling on purpose, and for the same reason: `M = 0` leaves
/// `1·in + 0·u`, which is `in`, and `M = 1` leaves `0·in + 1·u`, which is `u` — both bit for bit,
/// which the algebraically equal `in + M·(u − in)` is not. The coverage comes from
/// [`MaskField::evaluate`] at the tile's own stage coordinates, which are the mask's own stage
/// coordinates because a spatial operation opens its segment at the stage its layer received.
///
/// `input` is the tile-shaped snapshot [`cut_out`] took before the chain ran; `output` is the whole
/// of the last unit's rectangle, and only the `tile` part of it is touched.
fn blend(mask: &MaskField, region: Region, tile: Region, input: &[f32], output: &mut [f32]) {
    let source = region.pixels() as usize;
    let target = tile.pixels() as usize;
    let width = tile.width as usize;
    for (row, y) in (tile.y0..tile.y1()).enumerate() {
        let from = row * width;
        let to = (y - region.y0) as usize * region.width as usize + (tile.x0 - region.x0) as usize;
        for (column, x) in (tile.x0..tile.x1()).enumerate() {
            // One coverage evaluation per pixel, not per channel: the mask is a scalar field and
            // the three channels of a pixel share it. The pixel it is evaluated for is this
            // operation's own input, which is exactly what `input` holds — the snapshot taken
            // before the chain ran — so a value-based component reads the same value here that a
            // point sample of the same pixel reads.
            let pixel = [
                input[from + column],
                input[target + from + column],
                input[2 * target + from + column],
            ];
            let coverage = mask.evaluate(x, y, pixel);
            for channel in 0..3 {
                let value = &mut output[channel * source + to + column];
                *value = (1.0 - coverage) * pixel[channel] + coverage * *value;
            }
        }
    }
}

/// How many tiles of a masked operation were copied because the mask could not reach them, and how
/// many ran the unit chain. A masked layer is affordable exactly when the first number dominates
/// for a small mask, so the release measurements below print it beside their timings. An unmasked
/// operation touches neither.
///
/// Test builds only: process-wide counters bumped once per tile are shared state on the render's
/// hot path, and nothing in production reads them. A test that asserts what a mask saves counts its
/// own unit's evaluations instead (`a_tile_the_mask_cannot_reach_evaluates_no_unit`), because any
/// other test rendering a masked spatial layer at the same time would move these.
#[cfg(test)]
static MASKED_TILES_COPIED: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static MASKED_TILES_EVALUATED: AtomicU64 = AtomicU64::new(0);

/// `(copied, evaluated)` since the last [`reset_masked_tile_counts`].
#[cfg(test)]
pub(crate) fn masked_tile_counts() -> (u64, u64) {
    (
        MASKED_TILES_COPIED.load(Ordering::Relaxed),
        MASKED_TILES_EVALUATED.load(Ordering::Relaxed),
    )
}

/// Start counting masked tiles again from zero.
#[cfg(test)]
pub(crate) fn reset_masked_tile_counts() {
    MASKED_TILES_COPIED.store(0, Ordering::Relaxed);
    MASKED_TILES_EVALUATED.store(0, Ordering::Relaxed);
}

/// Run every tile of a stage in batches, on the shared Rayon pool above the same one-megapixel
/// threshold the other passes use and serially below it, checking the cancellation token between
/// batches.
///
/// Each batch reserves its working sets from the budget before any of its tiles allocates, asking
/// for the plan's concurrency and running as many tiles as the reservation covers, so a render that
/// overlaps another slows down rather than failing and speeds up again once the other releases.
/// When that leaves the batch narrower than the pool, each tile's own passes run on the pool as
/// well (see [`tile_parallelism`]), so an operation whose working set holds the batch to two tiles
/// still uses every worker without taking more memory.
///
/// `work` computes one tile's result under the parallelism it is given and `write` places it, so
/// the tiles themselves never share a mutable frame: a batch's results are bounded by its
/// concurrency times one tile.
pub(crate) fn run_batches<T: Send>(
    plan: &SpatialPlan,
    cancel: &Cancel,
    work: impl Fn(Region, Parallelism) -> Result<T, Error> + Sync,
    mut write: impl FnMut(Region, T) -> Result<(), Error>,
) -> Result<(), Error> {
    let tiles = plan.tiles();
    let large = plan.stage.width as u64 * plan.stage.height as u64 >= super::PARALLEL_RENDER_PIXELS;
    let workers = rayon::current_num_threads();
    let mut start = 0;
    while start < tiles.len() {
        // Before the reservation, so a cancelled render never takes working sets it will not use.
        cancel.check()?;
        let reservation = SpatialBudget::default()
            .reserve(plan.working_set, plan.concurrency.min(tiles.len() - start));
        let batch = &tiles[start..start + reservation.tiles()];
        start += batch.len();
        let parallelism = tile_parallelism(large, batch.len(), workers);
        let results: Vec<T> = if large && batch.len() > 1 {
            batch
                .par_iter()
                .map(|tile| work(*tile, parallelism))
                .collect::<Result<Vec<T>, Error>>()?
        } else {
            batch
                .iter()
                .map(|tile| work(*tile, parallelism))
                .collect::<Result<Vec<T>, Error>>()?
        };
        for (tile, result) in batch.iter().zip(results) {
            write(*tile, result)?;
        }
    }
    Ok(())
}

/// How a batch's tiles schedule their own passes: on the pool only for a stage at or above the
/// parallel threshold whose batch holds fewer tiles than the pool has workers, which is when the
/// budget rather than the pool limits the batch. A batch as wide as the pool already occupies every
/// worker, and splitting its tiles' passes as well measured 20 to 40% slower (Texture or Dehaze
/// alone at 24 and 60 MP). A point sample never comes through here; it runs serially on its calling
/// thread, where a pass on the pool would queue behind a render holding it.
pub(crate) fn tile_parallelism(large: bool, tiles: usize, workers: usize) -> Parallelism {
    if large && tiles < workers {
        Parallelism::Pool
    } else {
        Parallelism::Serial
    }
}

// ---------------------------------------------------------------------------------------------
// Point queries.
// ---------------------------------------------------------------------------------------------

/// The fewest tiles a point query holds whatever the target is. One row of a tile's input region,
/// grown by at most one tile of halo, crosses at most four tiles of the stage below it, and a nested
/// evaluation holds its own beside them; below this a lowered target could make every row of a fill
/// miss the tiles the row before it read.
const POINT_TILES_FLOOR: usize = 16;

/// The spatial tiles one point query has evaluated, of every spatial segment it reads through: the
/// one cache the byte and the linear point paths share.
///
/// A point through a spatial segment evaluates the stage-aligned tile that contains it with
/// [`run_tile`], the plan, the global estimates and the input pulls the render uses, so the value is
/// the rendered value by construction. That tile's input region is the tile grown by the summed halo,
/// at most one tile on each side, so when the stage it reads comes through an earlier spatial
/// segment the region covers at most 3 × 3 of that segment's tiles (in the host's stage order, where
/// no geometry separates two spatial layers), and the fill reads them from here. Each tile is
/// evaluated on its first read and held for the rest of the query, so the four neighbours a
/// resample blends, the points of a grid and a reduction of a stage behind a spatial segment share
/// them. Nothing is materialized.
///
/// **The bound.** The cache holds at most as many tiles as the spatial target has bytes for — 85
/// production tiles of 3 MiB — and never fewer than [`POINT_TILES_FLOOR`]; each is charged to the
/// budget while held, so a render running beside the query paces itself around it, and all of them
/// are released with the query. Past that the least recently read tile is released, and a later
/// read evaluates it again: slower, never refused. While what a query reads fits, each (segment,
/// tile) is evaluated at most once. One point through `k` spatial segments reads at most
/// `Σ (2d + 1)²` tiles over `d < k` — 10 for two, 35 for three, 84 for four — and `Σ (2d + 2)²`
/// behind a resample — 4 for one, 20 for two, 56 for three.
///
/// A reduction of a stage behind a spatial segment ([`Self::reduce`]) reads it one tile at a time,
/// each once and as a whole, so each of that segment's tiles is evaluated once. Two things can
/// still be evaluated again once the stage has more tiles than the cap: the at most 3 × 3 tiles the
/// point's own halo reads after the reduction, which it may have released by then, and, behind two
/// spatial segments, the tiles of the earlier one, which a walk keeps reading across three of its
/// tile rows and so holds only while those rows and the walked row fit the cap: on stages up to
/// about 10,700 px wide, 60 MP included.
///
/// Evaluating a tile reserves one working set while it runs, released before the tile is held; a
/// tile whose fill reads an earlier segment's missing tile holds its own reservation while that one
/// is evaluated, so a point through `k` spatial segments holds at most `k`.
///
/// Every evaluation here runs serially on the calling thread: on the pool it would queue behind a
/// render holding it. The lock is never held across an evaluation, because an evaluation reads
/// through this cache itself and its global estimate may reduce a stage on the pool.
pub(crate) struct PointTiles {
    tile: u32,
    capacity: usize,
    state: Mutex<PointState>,
    /// Every (segment, tile) this query evaluated, in order.
    #[cfg(test)]
    evaluated: Mutex<Vec<(usize, Region)>>,
}

#[derive(Default)]
struct PointState {
    prepared: Vec<Arc<Prepared>>,
    /// The held tiles, most recently read first.
    held: Vec<HeldTile>,
}

/// One spatial segment's plan and global estimates, resolved for its first tile.
struct Prepared {
    segment: usize,
    plan: SpatialPlan,
    globals: Vec<Option<Global>>,
}

/// One evaluated tile of one spatial segment: exactly the tile's three planes, cut from the last
/// unit's rectangle, and the budget it is charged to.
struct HeldTile {
    segment: usize,
    tile: Region,
    values: Vec<f32>,
    _reservation: SpatialReservation<'static>,
}

impl PointState {
    fn read(&mut self, segment: usize, x: u32, y: u32) -> Option<[f32; 3]> {
        let index = self
            .held
            .iter()
            .position(|held| held.segment == segment && held.tile.contains(x, y))?;
        self.held[..=index].rotate_right(1);
        let held = &self.held[0];
        Some(plane_pixel(held.tile, &held.values, x, y))
    }
}

impl PointTiles {
    /// An empty cache for one query evaluated in tiles of `tile` pixels.
    pub(crate) fn new(tile: u32) -> Self {
        let tile_bytes = Region {
            x0: 0,
            y0: 0,
            width: tile.max(1),
            height: tile.max(1),
        }
        .plane_bytes();
        let capacity = usize::try_from(SpatialBudget::default().target() / tile_bytes)
            .unwrap_or(usize::MAX)
            .max(POINT_TILES_FLOOR);
        Self {
            tile,
            capacity,
            state: Mutex::default(),
            #[cfg(test)]
            evaluated: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PointState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// One pixel of the output of spatial segment `segment`, whose operation reads and writes
    /// `stage`: from the held tile that contains it, or else from that tile evaluated now. `globals`
    /// resolves the operation's estimates once for the segment's first tile, and `read` pulls one
    /// pixel of the stage the operation reads.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pixel(
        &self,
        segment: usize,
        operation: &SpatialOperation,
        stage: Stage,
        x: u32,
        y: u32,
        globals: impl FnOnce() -> Result<Vec<Option<Global>>, Error>,
        read: impl Fn(u32, u32) -> Result<[f32; 3], Error> + Sync,
    ) -> Result<[f32; 3], Error> {
        let prepared = {
            let mut state = self.lock();
            if let Some(value) = state.read(segment, x, y) {
                return Ok(value);
            }
            state
                .prepared
                .iter()
                .find(|prepared| prepared.segment == segment)
                .cloned()
        };
        let prepared = match prepared {
            Some(prepared) => prepared,
            None => {
                let prepared = Arc::new(Prepared {
                    segment,
                    plan: SpatialPlan::new(operation, stage, self.tile)?,
                    globals: globals()?,
                });
                self.lock().prepared.push(prepared.clone());
                prepared
            }
        };
        let Prepared { plan, globals, .. } = &*prepared;
        let tile = plan.tile_containing(x, y);
        let (region, values) = {
            let _reservation = SpatialBudget::default().reserve(plan.working_set, 1);
            run_tile(
                plan,
                operation,
                globals,
                tile,
                Parallelism::Serial,
                |region, planes| fill_planes(region, planes, Parallelism::Serial, &read),
            )?
        };
        let values = if region == tile {
            values
        } else {
            cut_out(region, &values, tile)
        };
        let value = plane_pixel(tile, &values, x, y);
        #[cfg(test)]
        self.evaluated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((segment, tile));
        let mut state = self.lock();
        if !state
            .held
            .iter()
            .any(|held| held.segment == segment && held.tile == tile)
        {
            state.held.truncate(self.capacity - 1);
            let bytes = tile.plane_bytes();
            state.held.insert(
                0,
                HeldTile {
                    segment,
                    tile,
                    values,
                    _reservation: SpatialBudget::default().reserve(bytes, 1),
                },
            );
        }
        Ok(value)
    }

    /// The reduction of `stage` a global estimate of this query is prepared from, on a store miss.
    /// When the stage comes through an earlier spatial segment (`through_tiles`) it is read tile by
    /// tile on this thread through this cache, so each of that segment's tiles is evaluated once;
    /// otherwise it is read as a render reads it, on the pool above the parallel threshold.
    pub(crate) fn reduce(
        &self,
        stage: Stage,
        through_tiles: bool,
        fetch: impl Fn(u32, u32) -> Result<[f32; 3], Error> + Sync,
    ) -> Result<Reduction, Error> {
        if through_tiles {
            build_reduction_by_tiles(stage, self.tile, fetch)
        } else {
            build_reduction(stage, fetch)
        }
    }

    /// Every (segment, tile) this query evaluated, in order.
    #[cfg(test)]
    pub(crate) fn evaluated(&self) -> Vec<(usize, Region)> {
        self.evaluated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many tiles the query holds right now.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.lock().held.len()
    }
}

// ---------------------------------------------------------------------------------------------
// Global estimates.
// ---------------------------------------------------------------------------------------------

/// What one cached global estimate belongs to: the source, the layers before the operation (which
/// decide what its input stage holds), the stage the reduction was built from, and the unit's own
/// [`SpatialUnit::estimate_key`](crate::modules::SpatialUnit::estimate_key), which names everything
/// its preparation reads besides that reduction.
///
/// Neither the unit's position nor its description is part of it. A module compiles whichever units
/// its payload needs — the Presence module omits a unit whose amount is zero, which moves the others
/// up — so a position alone could hand one unit the estimate another prepared; the key is the unit's
/// own and does not move with it. The description names coefficients only `apply` reads, such as an
/// amount, and keying by it would reduce the whole stage again for every new amount although the
/// estimate is the same; two units that declare one key over one stage prepare one estimate by the
/// trait's own rule, so they share it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EstimateKey {
    pub(crate) fingerprint: String,
    pub(crate) prefix_hash: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) estimate: Cow<'static, str>,
}

/// The bounded store of prepared estimates: [`ESTIMATE_STORE_ENTRIES`] entries, oldest first, each
/// at most [`crate::modules::MAX_GLOBAL_BYTES`]. Only a unit that declares an estimate key has an
/// entry, and an entry may hold `None` when its preparation yielded none, so a second evaluation of
/// the same stack costs no reduction at all.
static ESTIMATES: Mutex<VecDeque<(EstimateKey, Option<Global>)>> = Mutex::new(VecDeque::new());

fn estimates() -> std::sync::MutexGuard<'static, VecDeque<(EstimateKey, Option<Global>)>> {
    ESTIMATES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `Some(estimate)` on a hit, where the estimate itself may be `None` for a preparation that
/// yielded none.
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
#[cfg(test)]
pub(crate) fn clear_estimates() {
    estimates().clear();
}

/// How many estimates are held right now.
#[cfg(test)]
pub(crate) fn cached_estimates() -> usize {
    estimates().len()
}

/// The global estimate of every unit of an operation, in unit order: `None` for a unit that
/// declares no estimate key, which is never prepared and never reduces anything, and for every
/// other unit the store's entry under its key, or else one preparation from one reduction of the
/// operation's input stage. The reduction is built at most once, and only when a unit that declares
/// a key is missing from the store: a stack evaluated twice reduces nothing the second time, and
/// neither does one whose units changed only in coefficients their keys do not name.
pub(crate) fn resolve_globals(
    operation: &SpatialOperation,
    stage: Stage,
    fingerprint: &str,
    prefix_hash: &str,
    reduce: impl FnOnce() -> Result<Reduction, Error>,
) -> Result<Vec<Option<Global>>, Error> {
    let units = operation.units();
    let keys: Vec<Option<EstimateKey>> = units
        .iter()
        .map(|unit| {
            unit.estimate_key().map(|estimate| EstimateKey {
                fingerprint: fingerprint.to_owned(),
                prefix_hash: prefix_hash.to_owned(),
                width: stage.width,
                height: stage.height,
                estimate,
            })
        })
        .collect();
    let mut globals: Vec<Option<Global>> = vec![None; units.len()];
    let mut missing: Vec<(usize, &EstimateKey)> = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        if let Some(key) = key {
            match cached(key) {
                Some(global) => globals[index] = global,
                None => missing.push((index, key)),
            }
        }
    }
    if missing.is_empty() {
        return Ok(globals);
    }
    let reduction = reduce()?;
    // Two units of one operation that declare the same key share one preparation, as they would
    // share one stored entry.
    let mut prepared: Vec<(&EstimateKey, Option<Global>)> = Vec::with_capacity(missing.len());
    for (index, key) in missing {
        let global = match prepared.iter().find(|(done, _)| *done == key) {
            Some((_, global)) => global.clone(),
            None => {
                let global = units[index].prepare(&reduction);
                remember(key.clone(), global.clone());
                prepared.push((key, global.clone()));
                global
            }
        };
        globals[index] = global;
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
    let (width, mut blocks) = reduction_blocks(stage)?;
    let row = |j: usize, row: &mut [[f32; 3]]| -> Result<(), Error> {
        for (i, block) in row.iter_mut().enumerate() {
            *block = block_mean(stage, i as u32, j as u32, &fetch)?;
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
    reduction_from(stage, &blocks)
}

/// [`build_reduction`] read one stage-aligned `tile` × `tile` square at a time, row-major, on the
/// calling thread: what a point query does when the stage comes through an earlier spatial
/// segment's tiles in its [`PointTiles`]. Each of those tiles is then read once, as a whole, so the
/// walk holds the tile it reads and what that tile's own halo reads, rather than a whole row of
/// tiles that a block-row walk reads again for every block row. Every block is summed in the same
/// order either way, so the reduction is the same.
fn build_reduction_by_tiles(
    stage: Stage,
    tile: u32,
    fetch: impl Fn(u32, u32) -> Result<[f32; 3], Error>,
) -> Result<Reduction, Error> {
    let (width, mut blocks) = reduction_blocks(stage)?;
    let height = blocks.len() as u32 / width;
    let side = (tile / ESTIMATE_REDUCTION).max(1) as usize;
    for j0 in (0..height).step_by(side) {
        for i0 in (0..width).step_by(side) {
            for j in j0..(j0 + side as u32).min(height) {
                for i in i0..(i0 + side as u32).min(width) {
                    blocks[(j * width + i) as usize] = block_mean(stage, i, j, &fetch)?;
                }
            }
        }
    }
    reduction_from(stage, &blocks)
}

/// The reduced frame's width and its zeroed blocks, or the bound it would exceed.
fn reduction_blocks(stage: Stage) -> Result<(u32, Vec<[f32; 3]>), Error> {
    let (width, height) = Reduction::dimensions(stage, ESTIMATE_REDUCTION);
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_REDUCTION_PIXELS {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "a spatial reduction of {pixels} pixels exceeds the {MAX_REDUCTION_PIXELS} pixel bound"
            ),
        ));
    }
    Ok((width, vec![[0.0_f32; 3]; pixels as usize]))
}

/// The mean of reduced block `(i, j)`: its pixels summed in `f64`, row by row, over the part of the
/// block inside the stage.
fn block_mean(
    stage: Stage,
    i: u32,
    j: u32,
    fetch: &impl Fn(u32, u32) -> Result<[f32; 3], Error>,
) -> Result<[f32; 3], Error> {
    let factor = ESTIMATE_REDUCTION;
    let (top, left) = (j * factor, i * factor);
    let bottom = (top + factor).min(stage.height);
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
    Ok(std::array::from_fn(|channel| (sum[channel] / count) as f32))
}

fn reduction_from(stage: Stage, blocks: &[[f32; 3]]) -> Result<Reduction, Error> {
    let mut planes = Vec::with_capacity(blocks.len() * 3);
    for channel in 0..3 {
        planes.extend(blocks.iter().map(|block| block[channel]));
    }
    Reduction::new(stage, ESTIMATE_REDUCTION, planes)
}

/// The SHA-256 of the canonical JSON of a layer prefix and the masks it reads. A mask ID in a
/// layer is not its value: editing that mask changes the operation's input without changing the
/// layer. Mask sampling also belongs to the identity, because thin-feature proxy coverage may
/// differ from point coverage over the same source and stage. The spatial operation's own mask
/// and unrelated masks do not change its input and are excluded.
pub(crate) fn prefix_hash(
    layers: &[crate::Layer],
    masks: &[crate::Mask],
    sampling: MaskSampling,
) -> Result<String, Error> {
    let upstream_masks: Vec<_> = masks
        .iter()
        .filter(|mask| {
            layers
                .iter()
                .any(|layer| layer.mask.as_ref() == Some(&mask.id))
        })
        .collect();
    // Without an upstream mask, both sampling modes read identical input pixels.
    let thin = !upstream_masks.is_empty() && sampling == MaskSampling::ThinFeature;
    let canonical = serde_json::to_vec(&(layers, upstream_masks, thin)).map_err(|error| {
        Error::new(
            ErrorKind::Internal,
            format!("a layer prefix could not be serialized for hashing: {error}"),
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(&canonical)))
}

/// Read one rectangle of a stage into three planar `f32` planes, in the layout
/// [`crate::modules::Planes`] expects: row by row, on the pool under [`Parallelism::Pool`]. Each
/// value is one `read` wherever it runs.
pub(crate) fn fill_planes(
    region: Region,
    planes: &mut [f32],
    parallelism: Parallelism,
    read: impl Fn(u32, u32) -> Result<[f32; 3], Error> + Sync,
) -> Result<(), Error> {
    if region.is_empty() {
        return Ok(());
    }
    let len = region.pixels() as usize;
    let width = region.width as usize;
    let (red, rest) = planes[..3 * len].split_at_mut(len);
    let (green, blue) = rest.split_at_mut(len);
    let row = |row: usize, red: &mut [f32], green: &mut [f32], blue: &mut [f32]| {
        let y = region.y0 + row as u32;
        for (column, x) in (region.x0..region.x1()).enumerate() {
            let pixel = read(x, y)?;
            red[column] = pixel[0];
            green[column] = pixel[1];
            blue[column] = pixel[2];
        }
        Ok(())
    };
    match parallelism {
        Parallelism::Pool => red
            .par_chunks_mut(width)
            .zip(green.par_chunks_mut(width))
            .zip(blue.par_chunks_mut(width))
            .enumerate()
            .try_for_each(|(index, ((red, green), blue))| row(index, red, green, blue)),
        Parallelism::Serial => red
            .chunks_mut(width)
            .zip(green.chunks_mut(width))
            .zip(blue.chunks_mut(width))
            .enumerate()
            .try_for_each(|(index, ((red, green), blue))| row(index, red, green, blue)),
    }
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
            linear::{render_linear_tiled, sample_linear_tiled},
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
    /// holds this lock instead of racing. That is every test that renders a spatial layer: each one
    /// takes working sets from the budget, and a unit that declares an estimate key reads and writes
    /// the store.
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

        fn apply(
            &self,
            input: &Planes<'_>,
            output: &mut PlanesMut<'_>,
            _: Option<&Global>,
            scratch: &mut [f32],
            _: Parallelism,
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

        fn estimate_key(&self) -> Option<Cow<'static, str>> {
            Some(Cow::Borrowed("test mean of the reduction"))
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
            _: Parallelism,
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

    /// How many times any [`Counted`] unit has been applied, which is once per tile its operation
    /// evaluated. Only `a_tile_the_mask_cannot_reach_evaluates_no_unit` compiles one, so no other
    /// test moves it, whatever else renders a masked spatial layer at the same time.
    static APPLIED: AtomicUsize = AtomicUsize::new(0);

    /// A unit with no neighbourhood that lifts every value by a quarter and counts its own
    /// evaluations, so what a mask saves is counted by the unit that would have done the work.
    #[derive(Debug)]
    struct Counted;

    impl SpatialUnit for Counted {
        fn halo(&self, _: Stage) -> u32 {
            0
        }

        fn scratch_bytes(&self, _: Stage) -> u64 {
            0
        }

        fn apply(
            &self,
            input: &Planes<'_>,
            output: &mut PlanesMut<'_>,
            _: Option<&Global>,
            _: &mut [f32],
            _: Parallelism,
        ) -> Result<(), Error> {
            APPLIED.fetch_add(1, AtomicOrdering::SeqCst);
            let out = output.region();
            for y in out.y0..out.y1() {
                for x in out.x0..out.x1() {
                    let pixel = input.sample(i64::from(x), i64::from(y));
                    output.set(x, y, pixel.map(|value| value + 0.25));
                }
            }
            Ok(())
        }

        fn is_finite(&self) -> bool {
            true
        }

        fn describe(&self) -> String {
            "counted lift".into()
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
        fn apply(
            &self,
            _: &Planes<'_>,
            _: &mut PlanesMut<'_>,
            _: Option<&Global>,
            _: &mut [f32],
            _: Parallelism,
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
                    maskable: true,
                    artifacts: false,
                    single: false,
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
                ..ModuleDescriptor::default()
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
                    None if unit == "count" => Arc::new(Counted),
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
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        }
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
    fn strength_independent_dehaze_estimates_keep_white_balance_inputs_distinct() {
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let source = linear_source(40, 30);
        let seed = recipe(vec![Layer {
            id: LayerId::new(),
            effect_id: crate::PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"dehaze": 60.0}),
            mask: None,
            artifacts: Vec::new(),
        }]);
        let mut changed = seed.clone();
        changed.layers[0].payload = json!({"dehaze": 61.0});
        let exact = LinearSettings::default();
        let approximate = |red, blue| LinearSettings {
            exposure_ev: 0.0,
            white_balance: Some(
                crate::WhiteBalanceApproximation::from_matrix([
                    [red, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, blue],
                ])
                .unwrap(),
            ),
        };
        let settings = [exact, approximate(1.4, 0.6), approximate(0.7, 1.3)];
        let render = |recipe: &Recipe, settings| {
            crate::render_linear(&registry, &source, SnapshotId::new(), recipe, settings)
                .unwrap()
                .rgba
        };
        let expected = settings.map(|settings| {
            clear_estimates();
            render(&changed, settings)
        });
        clear_estimates();
        for settings in settings {
            render(&seed, settings);
        }
        assert_eq!(
            cached_estimates(),
            3,
            "each white balance owns its estimate"
        );
        for (settings, expected) in settings.into_iter().zip(expected) {
            assert_eq!(render(&changed, settings), expected);
            assert_eq!(
                cached_estimates(),
                3,
                "only amount changed: reuse the matching input"
            );
        }
    }

    fn masked_colour_before_dehaze() -> Recipe {
        let mut mask = crate::Mask::new("Upstream mask");
        mask.components.push(crate::Component::new(
            "Radial 1",
            crate::ComponentMode::Add,
            "radial",
            json!({"x":0.47,"y":0.53,"radius_x":0.31,"radius_y":0.27,"angle":0.0,"feather":0.0}),
        ));
        let mut stack = recipe(vec![
            Layer {
                id: LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": 1.0}),
                mask: Some(mask.id.clone()),
                artifacts: Vec::new(),
            },
            Layer {
                id: LayerId::new(),
                effect_id: crate::PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"dehaze": 60.0}),
                mask: None,
                artifacts: Vec::new(),
            },
        ]);
        stack.masks.push(mask);
        stack
    }

    #[test]
    fn estimate_identity_tracks_upstream_mask_values_on_both_paths() {
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let byte = gradient(40, 30);
        let linear = linear_source(40, 30);
        let seed = masked_colour_before_dehaze();
        let mut variants = vec![seed.clone(); 3];
        variants[0].masks[0].amount = 25.0;
        variants[1].masks[0].invert = true;
        variants[2].masks[0].components[0].payload["radius_x"] = json!(0.12);
        for linear_path in [false, true] {
            let render = |stack: &Recipe| {
                if linear_path {
                    crate::render_linear(
                        &registry,
                        &linear,
                        SnapshotId::new(),
                        stack,
                        LinearSettings::default(),
                    )
                    .unwrap()
                } else {
                    crate::render(&registry, &byte, SnapshotId::new(), stack).unwrap()
                }
            };
            for changed in &variants {
                clear_estimates();
                let seed_frame = render(&seed);
                let cached = render(changed);
                assert_eq!(
                    cached_estimates(),
                    2,
                    "same mask ID with different pixels must miss"
                );
                clear_estimates();
                let fresh = render(changed);
                assert_eq!(
                    cached.rgba, fresh.rgba,
                    "no old atmosphere after a mask edit"
                );
                assert_ne!(
                    seed_frame.rgba, fresh.rgba,
                    "the fixture makes this mask edit visible"
                );
                let sample = if linear_path {
                    crate::sample_linear(
                        &registry,
                        &linear,
                        changed,
                        LinearSettings::default(),
                        17,
                        13,
                    )
                    .unwrap()
                } else {
                    crate::sample(&registry, &byte, changed, 17, 13).unwrap()
                };
                assert_eq!(sample.rgba, cached.pixel(17, 13));
            }
        }
        let prefix = &seed.layers[..1];
        let original = prefix_hash(prefix, &seed.masks, MaskSampling::Point).unwrap();
        let mut unrelated = seed.masks.clone();
        unrelated.push(crate::Mask::new("Unrelated mask"));
        assert_eq!(
            prefix_hash(prefix, &unrelated, MaskSampling::Point).unwrap(),
            original
        );
        // The operation's own mask also cannot affect the pixels its estimate reads.
        let mut own_mask = seed.clone();
        own_mask.layers[1].mask = Some(unrelated[1].id.clone());
        assert_eq!(
            prefix_hash(&own_mask.layers[..1], &unrelated, MaskSampling::Point).unwrap(),
            original
        );
    }

    #[test]
    fn estimate_identity_separates_point_and_thin_feature_mask_sampling() {
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let byte = gradient(40, 30);
        let linear = linear_source(40, 30);
        let stack = masked_colour_before_dehaze();
        assert!(
            registry
                .compile_sampled(40, 30, &stack, MaskSampling::ThinFeature)
                .unwrap()
                .supersampled_masks()
        );
        for linear_path in [false, true] {
            let render = |proxy| {
                let id = SnapshotId::new();
                let cancel = Cancel::new();
                match (linear_path, proxy) {
                    (false, false) => crate::render(&registry, &byte, id, &stack),
                    (false, true) => crate::render::render_proxy_cancellable(
                        &registry, &byte, id, &stack, &cancel,
                    ),
                    (true, false) => crate::render_linear(
                        &registry,
                        &linear,
                        id,
                        &stack,
                        LinearSettings::default(),
                    ),
                    (true, true) => crate::render::linear::render_linear_proxy_cancellable(
                        &registry,
                        &linear,
                        id,
                        &stack,
                        LinearSettings::default(),
                        &cancel,
                    ),
                }
                .unwrap()
                .rgba
            };
            let expected = [false, true].map(|proxy| {
                clear_estimates();
                render(proxy)
            });
            assert_ne!(
                expected[0], expected[1],
                "thin-feature sampling changes this fixture"
            );
            for order in [[false, true], [true, false]] {
                clear_estimates();
                for proxy in order {
                    assert_eq!(render(proxy), expected[usize::from(proxy)]);
                }
                assert_eq!(
                    cached_estimates(),
                    2,
                    "sampling modes own distinct atmospheres"
                );
            }
        }
    }

    #[test]
    fn linear_estimate_identity_tracks_development_view_and_direct_exposure() {
        let _guard = spatial_guard();
        let registry = spatial_registry();
        let stack = recipe(vec![spatial_layer(&["shift"])]);
        let planes: Vec<f32> = (0..3)
            .flat_map(|channel| {
                (0..32).flat_map(move |y| {
                    (0..48).map(move |x| ((x * x + y * 17 + channel * 31) % 251) as f32 / 300.0)
                })
            })
            .collect();
        // Public new() legitimately supplies no fingerprint; development identity must suffice.
        let first = LinearImage::new(48, 32, planes.clone()).unwrap();
        let second = LinearImage::new(
            48,
            32,
            planes.iter().map(|value| value * 0.6).collect::<Vec<_>>(),
        )
        .unwrap();
        let default = LinearSettings::default();
        let cases = [
            (first.clone(), default),
            (second, default),
            (first.with_view([0, 0, 24, 24], 1).unwrap(), default),
            (first.with_view([16, 0, 24, 24], 1).unwrap(), default),
            (first.with_view([0, 0, 24, 24], 2).unwrap(), default),
            (
                first.clone(),
                LinearSettings {
                    exposure_ev: 0.7,
                    ..default
                },
            ),
        ];
        let render = |source: &LinearImage, settings| {
            crate::render_linear(&registry, source, SnapshotId::new(), &stack, settings).unwrap()
        };
        let expected: Vec<_> = cases
            .iter()
            .map(|(source, settings)| {
                clear_estimates();
                render(source, *settings).rgba
            })
            .collect();
        clear_estimates();
        PREPARED.store(0, AtomicOrdering::SeqCst);
        for (index, ((source, settings), expected)) in cases.iter().zip(expected).enumerate() {
            let cached = render(source, *settings);
            assert_eq!(
                cached.rgba, expected,
                "input case {index} must not reuse another input"
            );
            assert_eq!(PREPARED.load(AtomicOrdering::SeqCst), index + 1);
            assert_eq!(cached_estimates(), index + 1);
            assert_eq!(render(&source.clone(), *settings).rgba, expected);
            let sampled =
                crate::sample_linear(&registry, source, &stack, *settings, 7, 11).unwrap();
            assert_eq!(sampled.rgba, cached.pixel(7, 11));
            assert_eq!(
                PREPARED.load(AtomicOrdering::SeqCst),
                index + 1,
                "clones and samples reuse the matching estimate"
            );
        }
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

    /// A linear point sample evaluates only the tile its pixel falls in and still equals the
    /// rendered byte everywhere: across tile boundaries, in partial edge tiles, through a rotated
    /// crop whose four bilinear neighbours straddle tiles, and after an earlier spatial layer
    /// whose output the last one reads whole neighbourhoods of and whose mean the second shift
    /// reduces.
    #[test]
    fn a_linear_sample_evaluates_its_tile_and_equals_the_tiled_render() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = linear_source(70, 52);
        let crop = fitted_crop(70, 52, 6.0, [0.15, 0.2, 0.6, 0.55]);
        for (case, stack) in [
            ("spatial", recipe(vec![spatial_layer(&["blur:3", "shift"])])),
            (
                "spatial then a rotated crop",
                recipe(vec![spatial_layer(&["blur:2", "shift"]), crop_layer(crop)]),
            ),
            (
                "two spatial layers then a rotated crop",
                recipe(vec![
                    spatial_layer(&["blur:2"]),
                    spatial_layer(&["blur:1", "shift"]),
                    crop_layer(crop),
                ]),
            ),
        ] {
            let rendered = render_linear_tiled(
                &registry,
                &source,
                SnapshotId::new(),
                &stack,
                LinearSettings::default(),
                &Cancel::new(),
                16,
            )
            .unwrap();
            for y in 0..rendered.height {
                for x in 0..rendered.width {
                    let sampled = sample_linear_tiled(
                        &registry,
                        &source,
                        &stack,
                        LinearSettings::default(),
                        x,
                        y,
                        16,
                    )
                    .unwrap();
                    assert_eq!(
                        sampled.rgba,
                        rendered.pixel(x, y),
                        "{case}, tile 16: sample at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn a_linear_sample_through_a_spatial_layer_reserves_one_working_set() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = linear_source(600, 400);
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
        assert_eq!(budget.in_use(), 0, "nothing is held between evaluations");
        budget.reset_peak();
        let sampled = crate::sample_linear(
            &registry,
            &source,
            &stack,
            LinearSettings::default(),
            300,
            200,
        )
        .unwrap();
        assert!(sampled.rgba.is_some());
        assert_eq!(
            budget.peak(),
            plan.working_set(),
            "a linear sample reserves exactly one tile working set"
        );
        assert_eq!(budget.in_use(), 0, "and releases it");
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

    /// A stored estimate belongs to the key of the unit that prepared it, not to a position. A module
    /// compiles whichever units its payload needs — the Presence module leaves out a unit whose
    /// amount is zero, which moves the others up — so the same position of the same source, prefix
    /// and stage can hold a different unit from one render to the next, and what one unit at that
    /// position was given must not be handed to another that wants an estimate.
    #[test]
    fn an_estimate_belongs_to_its_key_and_not_to_its_position() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        let source = gradient(64, 48);
        // A unit that declares no estimate at position 0, given none.
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

    /// An operation whose units declare no estimate key never reduces its stage and never touches
    /// the store, on its first evaluation or any other, and hands every unit no global.
    #[test]
    fn an_operation_whose_units_declare_no_key_never_reduces() {
        let _guard = spatial_guard();
        let stage = Stage {
            width: 64,
            height: 48,
        };
        let operation = SpatialOperation::new(vec![
            Arc::new(BoxBlur { radius: 2 }),
            Arc::new(Counted),
            Arc::new(BoxBlur { radius: 5 }),
        ])
        .unwrap();
        for _ in 0..2 {
            let globals = resolve_globals(
                &operation,
                stage,
                "sha256:no-estimate-key",
                "prefix",
                || -> Result<Reduction, Error> {
                    panic!("an operation that needs no estimate reduced its stage")
                },
            )
            .unwrap();
            assert_eq!(globals, vec![None, None, None]);
        }
        assert!(
            estimates()
                .iter()
                .all(|(key, _)| key.fingerprint != "sha256:no-estimate-key"),
            "and it stored nothing"
        );

        // Through a render as well: a blur-only layer prepares nothing and renders exactly.
        PREPARED.store(0, AtomicOrdering::SeqCst);
        let registry = spatial_registry();
        let source = gradient(64, 48);
        let raster = crate::render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![spatial_layer(&["blur:2"])]),
        )
        .unwrap();
        let expected = reference_chain(
            64,
            48,
            decode_frame(source.rgba.as_ref()),
            &[RefUnit::Blur(2)],
        );
        assert_frame(&raster, &expected, "a blur that needs no estimate");
        assert_eq!(PREPARED.load(AtomicOrdering::SeqCst), 0);
    }

    /// Two units of one operation that declare the same key over the same stage share one
    /// preparation, whatever their positions.
    #[test]
    fn units_that_declare_one_key_share_one_preparation() {
        let _guard = spatial_guard();
        PREPARED.store(0, AtomicOrdering::SeqCst);
        let stage = Stage {
            width: 64,
            height: 48,
        };
        let source = gradient(64, 48);
        let read = |x: u32, y: u32| -> Result<[f32; 3], Error> {
            let offset = ((y * 64 + x) * 4) as usize;
            Ok(crate::render::decode_pixel([
                source.rgba[offset],
                source.rgba[offset + 1],
                source.rgba[offset + 2],
            ]))
        };
        let operation = SpatialOperation::new(vec![
            Arc::new(MeanShift),
            Arc::new(BoxBlur { radius: 1 }),
            Arc::new(MeanShift),
        ])
        .unwrap();
        // A fingerprint no other test uses, so the first resolve is a miss.
        let fingerprint = format!("sha256:one-key-{}", SnapshotId::new());
        let mut reductions = 0;
        let globals = resolve_globals(&operation, stage, &fingerprint, "prefix", || {
            reductions += 1;
            build_reduction(stage, read)
        })
        .unwrap();
        assert_eq!(reductions, 1);
        assert_eq!(PREPARED.load(AtomicOrdering::SeqCst), 1, "one preparation");
        assert!(globals[0].is_some());
        assert_eq!(globals[0], globals[2], "both units hold the one estimate");
        assert_eq!(globals[1], None);
    }

    /// A Presence amount is a coefficient of its unit, not of its estimate: Dehaze's atmospheric
    /// light reads the reduction and nothing else, so every other Dehaze amount, alone or beside
    /// Texture and Clarity, prepares from the stored estimate without reducing the stage again, and
    /// Texture and Clarity, which declare no estimate, never reduce at all.
    #[test]
    fn changing_only_a_presence_amount_prepares_nothing_new() {
        let _guard = spatial_guard();
        let (width, height) = (96_u32, 64_u32);
        let stage = Stage { width, height };
        let source = gradient(width, height);
        let read = |x: u32, y: u32| -> Result<[f32; 3], Error> {
            let offset = ((y * width + x) * 4) as usize;
            Ok(crate::render::decode_pixel([
                source.rgba[offset],
                source.rgba[offset + 1],
                source.rgba[offset + 2],
            ]))
        };
        let module = crate::PresenceModule::new();
        let compile = |payload: &Value| -> SpatialOperation {
            match module
                .compile(crate::PRESENCE_EFFECT, EFFECT_FORMAT, payload, stage)
                .unwrap()
            {
                Processing::Spatial(operation) => operation,
                other => panic!("a spatial operation, not {other:?}"),
            }
        };
        // A fingerprint no other test uses, so the first Dehaze resolve is a miss.
        let fingerprint = format!("sha256:presence-amounts-{}", SnapshotId::new());
        let reductions = AtomicUsize::new(0);
        let resolve = |payload: &Value| {
            resolve_globals(&compile(payload), stage, &fingerprint, "prefix", || {
                reductions.fetch_add(1, AtomicOrdering::SeqCst);
                build_reduction(stage, read)
            })
            .unwrap()
        };

        for payload in [
            json!({"texture": 40.0}),
            json!({"clarity": -20.0}),
            json!({"texture": 40.0, "clarity": -20.0}),
        ] {
            assert!(resolve(&payload).iter().all(Option::is_none), "{payload}");
        }
        assert_eq!(
            reductions.load(AtomicOrdering::SeqCst),
            0,
            "texture and clarity never reduce"
        );

        let first = resolve(&json!({"dehaze": 30.0}));
        assert_eq!(reductions.load(AtomicOrdering::SeqCst), 1);
        let atmosphere = first[0].clone().expect("dehaze's atmospheric light");
        for payload in [
            json!({"dehaze": 60.0}),
            json!({"dehaze": 30.25}),
            json!({"dehaze": -45.0, "texture": 10.0}),
            json!({"dehaze": 100.0, "texture": 5.0, "clarity": -5.0}),
        ] {
            let globals = resolve(&payload);
            assert_eq!(globals[0].as_ref(), Some(&atmosphere), "{payload}");
            assert!(globals[1..].iter().all(Option::is_none), "{payload}");
        }
        assert_eq!(
            reductions.load(AtomicOrdering::SeqCst),
            1,
            "a new amount prepares from the stored estimate"
        );
    }

    /// The stored atmospheric light is the one a fresh preparation gives, so a frame rendered from
    /// it after another amount's render is byte for byte the frame rendered from a cold store.
    #[test]
    fn a_stored_atmospheric_light_renders_every_dehaze_amount_as_a_cold_store_does() {
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let source = gradient(96, 64);
        let render = |dehaze: f64| {
            let stack = recipe(vec![Layer {
                id: LayerId::new(),
                effect_id: crate::PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"dehaze": dehaze, "clarity": 25.0}),
                mask: None,
                artifacts: Vec::new(),
            }]);
            crate::render(&registry, &source, SnapshotId::new(), &stack)
                .unwrap()
                .rgba
        };
        clear_estimates();
        let cold = render(35.0);
        clear_estimates();
        let other = render(-60.0);
        let warm = render(35.0);
        assert_ne!(cold, other, "the amount changes the frame");
        assert_eq!(cold, warm, "the stored estimate renders the cold frame");
    }

    // -----------------------------------------------------------------------------------------
    // Point queries through several spatial segments.
    // -----------------------------------------------------------------------------------------

    /// A stage of 6 × 5 tiles of [`POINT_TILE`] pixels. Presence's summed halo at this stage is
    /// inside one tile, so one tile's input region covers at most 3 × 3 tiles of the stage below.
    const POINT_STAGE: (u32, u32) = (384, 320);
    const POINT_TILE: u32 = 64;

    /// A mask of one horizontal linear gradient from `x0` to `x1`, as fractions of the width.
    fn point_mask(name: &str, x0: f64, x1: f64) -> crate::Mask {
        let mut mask = crate::Mask::new(name);
        let component = mask.next_component_name("linear");
        mask.components.push(crate::Component::new(
            component,
            crate::ComponentMode::Add,
            "linear",
            json!({"x0": x0, "y0": 0.5, "x1": x1, "y1": 0.5}),
        ));
        mask
    }

    fn presence(payload: Value, mask: Option<&crate::Mask>) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: crate::PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: mask.map(|mask| mask.id.clone()),
            artifacts: Vec::new(),
        }
    }

    /// A global Presence layer followed by one masked Presence layer (two spatial segments), by
    /// two (three), or by a straightened crop. Every layer declares Dehaze's estimate. The first
    /// mask reaches every tile; the second only the stage's left part, so a point on the right
    /// reads its tile as a copy.
    fn point_stacks() -> [(&'static str, Recipe); 3] {
        let everywhere = point_mask("Everywhere", -0.5, 1.5);
        let left = point_mask("Left", 0.45, 0.2);
        let global = || {
            presence(
                json!({"clarity": 40.0, "dehaze": 20.0, "texture": 30.0}),
                None,
            )
        };
        let first = || {
            presence(
                json!({"texture": 35.0, "clarity": -30.0, "dehaze": 25.0}),
                Some(&everywhere),
            )
        };
        let second = presence(json!({"clarity": 50.0, "dehaze": -20.0}), Some(&left));
        let crop = fitted_crop(POINT_STAGE.0, POINT_STAGE.1, 6.0, [0.15, 0.2, 0.6, 0.55]);
        let stack = |layers, masks| Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks,
            ..Recipe::default()
        };
        [
            (
                "two spatial segments",
                stack(vec![global(), first()], vec![everywhere.clone()]),
            ),
            (
                "three spatial segments",
                stack(
                    vec![global(), first(), second],
                    vec![everywhere.clone(), left.clone()],
                ),
            ),
            (
                "presence then a straightened crop",
                stack(vec![global(), Layer::crop(crop)], Vec::new()),
            ),
        ]
    }

    /// The most tiles one point may evaluate of each spatial segment, by segment index: `(2d + 1)²`
    /// for the segment `d` spatial segments below the last, `(2d + 2)²` when a resample after the
    /// last blends four neighbours, and never more than its stage holds. It also checks the
    /// premise: every summed halo is inside one tile.
    fn point_bounds(compiled: &crate::render::Compiled) -> Vec<(usize, usize)> {
        use crate::render::Entry;
        let spatial: Vec<usize> = compiled
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| matches!(segment.entry, Some(Entry::Spatial { .. })))
            .map(|(index, _)| index)
            .collect();
        let last = *spatial.last().expect("a spatial segment");
        let resampled = compiled.segments[last + 1..]
            .iter()
            .any(|segment| matches!(segment.entry, Some(Entry::Resample(_))));
        spatial
            .iter()
            .rev()
            .enumerate()
            .map(|(depth, &index)| {
                let previous = &compiled.segments[index - 1];
                let stage = Stage {
                    width: previous.width,
                    height: previous.height,
                };
                let Some(Entry::Spatial { operation, .. }) = &compiled.segments[index].entry else {
                    unreachable!()
                };
                assert!(operation.summed_halo(stage) <= POINT_TILE, "the premise");
                let side = 2 * depth + if resampled { 2 } else { 1 };
                let tiles = stage.width.div_ceil(POINT_TILE) * stage.height.div_ceil(POINT_TILE);
                (index, (side * side).min(tiles as usize))
            })
            .collect()
    }

    /// How many tiles of each spatial segment one query evaluated, asserting that none was
    /// evaluated twice.
    fn evaluations_per_segment(evaluated: &[(usize, Region)], case: &str) -> Vec<(usize, usize)> {
        let mut seen = std::collections::HashSet::new();
        for (segment, tile) in evaluated {
            assert!(
                seen.insert((*segment, tile.x0, tile.y0)),
                "{case}: tile {tile:?} of segment {segment} evaluated twice"
            );
        }
        let mut counts: Vec<(usize, usize)> = Vec::new();
        for (segment, _) in evaluated {
            match counts.iter_mut().find(|(index, _)| index == segment) {
                Some((_, count)) => *count += 1,
                None => counts.push((*segment, 1)),
            }
        }
        counts.sort_unstable();
        counts
    }

    fn assert_within(counts: &[(usize, usize)], bounds: &[(usize, usize)], case: &str) {
        for (segment, count) in counts {
            let bound = bounds
                .iter()
                .find(|(index, _)| index == segment)
                .map(|(_, bound)| *bound)
                .unwrap_or_else(|| panic!("{case}: segment {segment} is not spatial"));
            assert!(
                count <= &bound,
                "{case}: {count} tiles of segment {segment}, more than {bound}"
            );
        }
    }

    /// Tile corners, tile edges, the stage's own corners and its interior.
    fn point_query_points(width: u32, height: u32) -> Vec<(u32, u32)> {
        let t = POINT_TILE;
        [
            (0, 0),
            (t - 1, t - 1),
            (t, t),
            (t - 1, t),
            (2 * t, 2 * t - 1),
            (3 * t + 5, 2 * t + 9),
            (width / 2, height / 2),
            (width - 1, 0),
            (0, height - 1),
            (width - 1, height - 1),
        ]
        .into_iter()
        .filter(|&(x, y)| x < width && y < height)
        .collect()
    }

    /// Within one point query each spatial segment's tile is evaluated at most once, and no more of
    /// them than the halo can reach; the sampled byte is the rendered byte. On both paths, through
    /// two and three spatial segments and through Presence before a straightened crop, whose four
    /// bilinear neighbours can straddle tiles. The estimate store is warm from the render, as a
    /// preview leaves it, so each query counts the tiles its point needs; the store-miss case is
    /// `a_reduction_behind_a_spatial_segment_evaluates_each_tile_once`.
    #[test]
    fn a_point_query_evaluates_each_spatial_tile_at_most_once_on_both_paths() {
        use crate::render::linear::{LinearEvaluation, SpatialMode, terminal_pixel};
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let (width, height) = POINT_STAGE;
        let source = gradient(width, height);
        let linear = linear_source(width, height);
        let mut expected_bounds = [
            vec![(2, 1), (1, 9)],
            vec![(3, 1), (2, 9), (1, 25)],
            // The crop is a resample, so its four neighbours can straddle 2 × 2 tiles.
            vec![(1, 4)],
        ]
        .into_iter();
        for (case, stack) in point_stacks() {
            let bounds = point_bounds(&registry.compile(width, height, &stack).unwrap());
            assert_eq!(Some(&bounds), expected_bounds.next().as_ref(), "{case}");
            clear_estimates();
            let rendered = render_tiled(
                &registry,
                &source,
                SnapshotId::new(),
                &stack,
                &Cancel::new(),
                POINT_TILE,
            )
            .unwrap();
            for (x, y) in point_query_points(rendered.width, rendered.height) {
                let case = format!("{case}, byte path at ({x}, {y})");
                let evaluation = crate::render::Evaluation::new(&registry, &source, &stack)
                    .unwrap()
                    .with_tile(POINT_TILE);
                assert_eq!(
                    evaluation.pixel(x, y).unwrap(),
                    rendered.pixel(x, y),
                    "{case}"
                );
                let counts = evaluations_per_segment(&evaluation.tiles.evaluated(), &case);
                assert_within(&counts, &bounds, &case);
            }
            clear_estimates();
            let rendered = render_linear_tiled(
                &registry,
                &linear,
                SnapshotId::new(),
                &stack,
                LinearSettings::default(),
                &Cancel::new(),
                POINT_TILE,
            )
            .unwrap();
            for (x, y) in point_query_points(rendered.width, rendered.height) {
                let case = format!("{case}, linear path at ({x}, {y})");
                let evaluation = LinearEvaluation::new(
                    &registry,
                    &linear,
                    &stack,
                    LinearSettings::default(),
                    &Cancel::new(),
                    POINT_TILE,
                    SpatialMode::Point,
                )
                .unwrap();
                let sampled = evaluation.pixel(x, y).unwrap().map(terminal_pixel);
                assert_eq!(sampled.transpose().unwrap(), rendered.pixel(x, y), "{case}");
                let counts = evaluations_per_segment(&evaluation.point_tiles().evaluated(), &case);
                assert_within(&counts, &bounds, &case);
            }
        }
    }

    /// The bound is reached, not just respected: an interior point through two spatial segments
    /// reads 3 × 3 tiles of the first, and a point through three reads what the halos reach and no
    /// more — through the second mask's copy path, one tile of the segment below.
    #[test]
    fn a_point_query_evaluates_exactly_the_tiles_its_halos_reach() {
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let (width, height) = POINT_STAGE;
        let source = gradient(width, height);
        let [(_, two), (_, three), _] = point_stacks();
        for (stack, (x, y), expected) in [
            // Tile (3, 2): its region covers tile columns 2..=4 and rows 1..=3 of the first.
            (&two, (192, 160), vec![(1, 9), (2, 1)]),
            // Tile (1, 1), inside the second mask: 3 × 3 tiles of the middle segment, whose
            // regions cover columns 0..=3 and rows 0..=3 of the first.
            (&three, (64, 64), vec![(1, 16), (2, 9), (3, 1)]),
            // Tile (5, 1), which the second mask cannot reach, is a copy of the one middle tile
            // under it; that tile's region covers columns 4..=5 and rows 0..=2 of the first.
            (&three, (352, 64), vec![(1, 6), (2, 1), (3, 1)]),
        ] {
            clear_estimates();
            render_tiled(
                &registry,
                &source,
                SnapshotId::new(),
                stack,
                &Cancel::new(),
                POINT_TILE,
            )
            .unwrap();
            let evaluation = crate::render::Evaluation::new(&registry, &source, stack)
                .unwrap()
                .with_tile(POINT_TILE);
            evaluation.pixel(x, y).unwrap();
            let case = format!("({x}, {y})");
            assert_eq!(
                evaluations_per_segment(&evaluation.tiles.evaluated(), &case),
                expected,
                "{case}"
            );
        }
    }

    /// On a store miss, the reduction of a stage behind a spatial segment reads that segment's
    /// tiles from the query's cache one at a time: each of its tiles is evaluated exactly once,
    /// including the ones the point's own tile then reads, and the sample is still the rendered
    /// byte, whose render prepared its estimates from a cold store of its own.
    #[test]
    fn a_reduction_behind_a_spatial_segment_evaluates_each_tile_once() {
        use crate::render::linear::{LinearEvaluation, SpatialMode, terminal_pixel};
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let (width, height) = POINT_STAGE;
        let [(_, stack), ..] = point_stacks();
        let every_tile = (width.div_ceil(POINT_TILE) * height.div_ceil(POINT_TILE)) as usize;
        let (x, y) = (192, 160);

        let source = gradient(width, height);
        clear_estimates();
        let evaluation = crate::render::Evaluation::new(&registry, &source, &stack)
            .unwrap()
            .with_tile(POINT_TILE);
        let sampled = evaluation.pixel(x, y).unwrap();
        assert_eq!(
            evaluations_per_segment(&evaluation.tiles.evaluated(), "byte path"),
            [(1, every_tile), (2, 1)]
        );
        clear_estimates();
        let rendered = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            POINT_TILE,
        )
        .unwrap();
        assert_eq!(sampled, rendered.pixel(x, y), "byte path");

        let linear = linear_source(width, height);
        clear_estimates();
        let evaluation = LinearEvaluation::new(
            &registry,
            &linear,
            &stack,
            LinearSettings::default(),
            &Cancel::new(),
            POINT_TILE,
            SpatialMode::Point,
        )
        .unwrap();
        let sampled = evaluation.pixel(x, y).unwrap().map(terminal_pixel);
        assert_eq!(
            evaluations_per_segment(&evaluation.point_tiles().evaluated(), "linear path"),
            [(1, every_tile), (2, 1)]
        );
        clear_estimates();
        let rendered = render_linear_tiled(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
            &Cancel::new(),
            POINT_TILE,
        )
        .unwrap();
        assert_eq!(
            sampled.transpose().unwrap(),
            rendered.pixel(x, y),
            "linear path"
        );
    }

    /// A point query's tile-by-tile reduction is the render's block-row reduction, value for value,
    /// whatever the tile size — a multiple of the block, smaller than it or neither — and with
    /// partial blocks and partial tiles at both edges.
    #[test]
    fn a_reduction_read_tile_by_tile_is_the_block_row_reduction() {
        for (width, height) in [(1, 1), (37, 23), (200, 131), (384, 320)] {
            let stage = Stage { width, height };
            let fetch = |x: u32, y: u32| -> Result<[f32; 3], Error> {
                Ok([
                    (x * 7 + y * 3) as f32 / 97.0,
                    ((x ^ y) % 13) as f32 / 5.0,
                    ((x * y) % 29) as f32 - 3.5,
                ])
            };
            let rows = build_reduction(stage, fetch).unwrap();
            for tile in [1, 16, 40, 64, 512] {
                assert_eq!(
                    build_reduction_by_tiles(stage, tile, fetch).unwrap(),
                    rows,
                    "{width}x{height}, tile {tile}"
                );
            }
        }
    }

    /// Past its capacity a query releases the least recently read tile rather than growing: it
    /// never holds more than its capacity, each held tile is charged to the spatial budget until
    /// the query ends, and a tile read again after its release is evaluated again, so the sample is
    /// still the rendered byte. A target too small for any tile still leaves the floor.
    #[test]
    fn a_point_query_holds_no_more_tiles_than_its_capacity() {
        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        let (width, height) = POINT_STAGE;
        let source = gradient(width, height);
        let [_, (_, stack), _] = point_stacks();
        clear_estimates();
        let rendered = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            POINT_TILE,
        )
        .unwrap();
        let budget = SpatialBudget::default();
        assert_eq!(budget.in_use(), 0);
        let previous = budget.set_target(0);
        let evaluation = crate::render::Evaluation::new(&registry, &source, &stack)
            .unwrap()
            .with_tile(POINT_TILE);
        budget.set_target(previous);
        let (x, y) = (64, 64);
        assert_eq!(evaluation.pixel(x, y).unwrap(), rendered.pixel(x, y));
        let evaluated = evaluation.tiles.evaluated().len();
        assert!(
            evaluated > POINT_TILES_FLOOR,
            "the point reads {evaluated} tiles, more than the floor holds"
        );
        assert_eq!(evaluation.tiles.held(), POINT_TILES_FLOOR);
        let tile_bytes = Region {
            x0: 0,
            y0: 0,
            width: POINT_TILE,
            height: POINT_TILE,
        }
        .plane_bytes();
        assert_eq!(
            budget.in_use(),
            POINT_TILES_FLOOR as u64 * tile_bytes,
            "every held tile is charged while the query lasts"
        );
        drop(evaluation);
        assert_eq!(budget.in_use(), 0, "and released with it");
    }

    // -----------------------------------------------------------------------------------------
    // Masked tiles.
    // -----------------------------------------------------------------------------------------

    /// A tile entirely outside a mask's bounds is copied, and no unit of the operation is evaluated
    /// over it: the claim that makes a small masked Presence layer affordable on a 60 MP frame. The
    /// unit counts its own evaluations, so this is a counted fact on both paths and not an argument;
    /// `tests/masked_spatial.rs` shows through the public API that the copied tiles hold the
    /// operation's input.
    #[test]
    fn a_tile_the_mask_cannot_reach_evaluates_no_unit() {
        let _guard = spatial_guard();
        let registry = spatial_registry();
        // Two tile columns and two tile rows (512 + 88 by 512 + 38): the smallest frame that can
        // show a tile being copied while another is evaluated.
        let (width, height) = (600, 550);
        let source = gradient(width, height);
        let linear = linear_source(width, height);
        // A gradient confined to the right edge: its support cannot reach the two tiles whose
        // columns start at zero, so those two are copies and the two at column 512 run the chain.
        let mut mask = crate::Mask::new("Mask 1");
        let name = mask.next_component_name("linear");
        mask.components.push(crate::Component::new(
            name,
            crate::ComponentMode::Add,
            "linear",
            json!({"x0": 0.90, "y0": 0.5, "x1": 0.97, "y1": 0.5}),
        ));
        let masked = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                mask: Some(mask.id.clone()),
                ..spatial_layer(&["count"])
            }],
            masks: vec![mask],
            ..Recipe::default()
        };
        let unmasked = recipe(vec![spatial_layer(&["count"])]);
        let applied = |stack: &Recipe, linear_path: bool| {
            APPLIED.store(0, AtomicOrdering::SeqCst);
            if linear_path {
                crate::render_linear(
                    &registry,
                    &linear,
                    SnapshotId::new(),
                    stack,
                    LinearSettings::default(),
                )
                .unwrap();
            } else {
                crate::render(&registry, &source, SnapshotId::new(), stack).unwrap();
            }
            APPLIED.load(AtomicOrdering::SeqCst)
        };
        for (path, linear_path) in [("byte", false), ("linear", true)] {
            assert_eq!(
                applied(&unmasked, linear_path),
                4,
                "{path} path: an unmasked operation evaluates all four tiles"
            );
            assert_eq!(
                applied(&masked, linear_path),
                2,
                "{path} path: the two left-hand tiles cost no unit evaluation"
            );
        }
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
    fn a_tile_spreads_its_passes_over_the_pool_only_when_the_budget_narrows_its_batch() {
        assert_eq!(tile_parallelism(true, 2, 14), Parallelism::Pool);
        assert_eq!(tile_parallelism(true, 13, 14), Parallelism::Pool);
        assert_eq!(
            tile_parallelism(true, 14, 14),
            Parallelism::Serial,
            "a batch as wide as the pool already occupies every worker"
        );
        assert_eq!(
            tile_parallelism(false, 1, 14),
            Parallelism::Serial,
            "a stage below the parallel threshold stays on its thread"
        );
        assert_eq!(tile_parallelism(true, 1, 1), Parallelism::Serial);
    }

    /// A render whose batches the budget narrows to one tile fills, checks and quantizes each tile
    /// on the pool; one whose batches are as wide as the pool does all of that serially. Both paths
    /// give the same frame to the byte.
    #[test]
    fn a_render_with_pooled_tiles_equals_one_with_serial_tiles() {
        let _guard = spatial_guard();
        clear_estimates();
        let registry = spatial_registry();
        // At the one-megapixel threshold, in 64 px tiles so that a batch as wide as the pool
        // exists whatever the pool's size.
        let (width, height) = (1000, 1000);
        let tile = 64;
        let source = gradient(width, height);
        let linear = linear_source(width, height);
        let stack = recipe(vec![spatial_layer(&["blur:2", "shift"])]);
        let operation =
            SpatialOperation::new(vec![Arc::new(BoxBlur { radius: 2 }), Arc::new(MeanShift)])
                .unwrap();
        let plan = SpatialPlan::new(&operation, Stage { width, height }, tile).unwrap();
        let budget = SpatialBudget::default();
        let mut frames = Vec::new();
        // One working set: batches of one tile, pooled. Unbounded: batches as wide as the pool,
        // serial.
        for target in [plan.working_set(), u64::MAX / 2] {
            let previous = budget.set_target(target);
            let bytes = render_tiled(
                &registry,
                &source,
                SnapshotId::new(),
                &stack,
                &Cancel::new(),
                tile,
            );
            let floats = render_linear_tiled(
                &registry,
                &linear,
                SnapshotId::new(),
                &stack,
                LinearSettings::default(),
                &Cancel::new(),
                tile,
            );
            budget.set_target(previous);
            frames.push((
                bytes.unwrap().rgba.as_ref().to_vec(),
                floats.unwrap().rgba.as_ref().to_vec(),
            ));
        }
        assert_eq!(
            frames[0].0, frames[1].0,
            "byte path: pooled and serial tiles agree"
        );
        assert_eq!(
            frames[0].1, frames[1].1,
            "linear path: pooled and serial tiles agree"
        );
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

    /// Release-only measurement, run explicitly:
    ///
    /// ```sh
    /// cargo test --release --locked --package lightwell-core -- --ignored masked_spatial_timing --nocapture
    /// ```
    ///
    /// One to four **masked** Presence layers on in-memory 24 MP and 60 MP frames, against the same
    /// stacks with no mask at all, at two mask sizes: one whose bounds rectangle covers the whole
    /// frame (the worst case the cap of four exists for) and one confined to a band at the right
    /// edge (the case the tile copy exists for). Each masked spatial layer is a stage boundary and
    /// therefore a sequential full frame, so the row to read is how the cost grows with the layer
    /// count, and the copied-tile count is what says the small mask's tiles were not evaluated.
    #[test]
    #[ignore = "measurement, run explicitly in release"]
    fn masked_spatial_timing() {
        use crate::{
            Component, ComponentMode, EFFECT_FORMAT, Layer, LayerId, Mask, PRESENCE_EFFECT,
        };

        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        // One `add` linear gradient between two normalized points, as the host stores one.
        let mask = |name: &str, x0: f64, x1: f64| -> Mask {
            let mut mask = Mask::new(name);
            let component = mask.next_component_name("linear");
            mask.components.push(Component::new(
                component,
                ComponentMode::Add,
                "linear",
                json!({"x0": x0, "y0": 0.5, "x1": x1, "y1": 0.5}),
            ));
            mask
        };
        for (width, height) in [(6000_u32, 4000_u32), (10_000, 6000)] {
            let source = gradient(width, height);
            for (shape, geometry) in [
                // Beyond `p1` over the whole frame: bounds is the whole stage, every tile runs the
                // chain and every tile is blended.
                ("whole frame", (-1.0, -0.5)),
                // A band at the right edge: the bounds rectangle covers about a tenth of the
                // columns, so most tiles are copies.
                ("right-edge band", (0.90, 0.97)),
            ] {
                for count in 1..=crate::modules::MAX_MASKED_SPATIAL_LAYERS {
                    let masks: Vec<Mask> = (0..count)
                        .map(|index| mask(&format!("Mask {index}"), geometry.0, geometry.1))
                        .collect();
                    let presence = |mask: Option<&Mask>| Layer {
                        id: LayerId::new(),
                        effect_id: PRESENCE_EFFECT.into(),
                        effect_format: EFFECT_FORMAT,
                        payload: json!({"clarity": 100.0}),
                        mask: mask.map(|mask| mask.id.clone()),
                        artifacts: Vec::new(),
                    };
                    for masked in [false, true] {
                        let stack = crate::Recipe {
                            format: crate::RECIPE_FORMAT,
                            layers: masks
                                .iter()
                                .map(|mask| presence(masked.then_some(mask)))
                                .collect(),
                            masks: masks.clone(),
                            ..Recipe::default()
                        };
                        if !masked && count > 1 {
                            // Two unmasked layers of one single-layer effect share a target and are
                            // refused, which is the rule and not a defect: the unmasked baseline is
                            // the one-layer row.
                            continue;
                        }
                        // Warm the source and the estimate store, then measure.
                        crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
                        SpatialBudget::default().reset_peak();
                        reset_masked_tile_counts();
                        let mut samples = Vec::new();
                        for _ in 0..5 {
                            let started = std::time::Instant::now();
                            let raster =
                                crate::render(&registry, &source, SnapshotId::new(), &stack)
                                    .unwrap();
                            samples.push(started.elapsed().as_secs_f64() * 1000.0);
                            assert_eq!((raster.width, raster.height), (width, height));
                        }
                        samples.sort_by(f64::total_cmp);
                        let p50 = samples[samples.len() / 2];
                        let p95 = samples[samples.len() - 1];
                        let (copied, evaluated) = masked_tile_counts();
                        let runs = samples.len() as u64;
                        println!(
                            "{width}x{height} presence clarity +100 x{count} {}, {shape}: p50 \
                             {p50:.0} ms, p95 {p95:.0} ms over {runs} runs; tiles per run copied \
                             {}, evaluated {}; budget peak {:.1} MiB of {:.1} MiB",
                            if masked { "masked" } else { "unmasked" },
                            copied / runs,
                            evaluated / runs,
                            SpatialBudget::default().peak() as f64 / MIB,
                            SpatialBudget::default().target() as f64 / MIB,
                        );
                    }
                }
            }
        }
        reset_masked_tile_counts();
    }

    /// Release-only measurement, run explicitly:
    ///
    /// ```sh
    /// cargo test --release --locked --package lightwell-core -- --ignored masked_spatial_component_timing --nocapture
    /// ```
    ///
    /// What a **mask with many components** costs, which is the other half of the masked spatial
    /// cost: `masked_spatial_timing` above varies the layer count with one component per mask, and
    /// this varies the component count with one layer. Every component is a linear gradient across
    /// the whole frame, so the bounds rectangle is the whole stage and every component is evaluated
    /// at every covered pixel — the worst case, and the one the design's limit of
    /// [`crate::COMPONENTS_PER_MASK`] exists for. The modes cycle through all three, because a
    /// subtraction and an intersection are each one more `min` per pixel and nothing else.
    #[test]
    #[ignore = "measurement, run explicitly in release"]
    fn masked_spatial_component_timing() {
        use crate::{
            COMPONENTS_PER_MASK, Component, ComponentMode, EFFECT_FORMAT, Layer, LayerId, Mask,
            PRESENCE_EFFECT,
        };

        let _guard = spatial_guard();
        let registry = ModuleRegistry::builtin();
        // A mask of `count` whole-frame linear gradients, the first an add and the rest cycling
        // through the three modes, each offset so no two are the same field.
        let mask = |count: usize| -> Mask {
            let mut mask = Mask::new("Mask 1");
            for index in 0..count {
                let name = mask.next_component_name("linear");
                // Index 0 falls on `Add`, which is the rule for a mask's first component.
                let mode = match index % 3 {
                    0 => ComponentMode::Add,
                    1 => ComponentMode::Subtract,
                    _ => ComponentMode::Intersect,
                };
                // Each component's ramp ends a little further on, so no two are the same field,
                // and every one of them ends left of the frame: the whole stage is beyond `p1`, so
                // each is at coverage 1 everywhere and the bounds rectangle is the whole frame.
                let shift = index as f64 * 0.01;
                mask.components.push(Component::new(
                    name,
                    mode,
                    "linear",
                    json!({"x0": -1.0, "y0": 0.5, "x1": -0.5 + shift, "y1": 0.5}),
                ));
            }
            mask
        };
        for (width, height) in [(6000_u32, 4000_u32), (10_000, 6000)] {
            let source = gradient(width, height);
            for count in [1, 4, 16, COMPONENTS_PER_MASK] {
                let mask = mask(count);
                let stack = crate::Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers: vec![Layer {
                        id: LayerId::new(),
                        effect_id: PRESENCE_EFFECT.into(),
                        effect_format: EFFECT_FORMAT,
                        payload: json!({"clarity": 100.0}),
                        mask: Some(mask.id.clone()),
                        artifacts: Vec::new(),
                    }],
                    masks: vec![mask],
                    ..Recipe::default()
                };
                // Warm the source and the estimate store, then measure.
                crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
                SpatialBudget::default().reset_peak();
                reset_masked_tile_counts();
                let mut samples = Vec::new();
                for _ in 0..5 {
                    let started = std::time::Instant::now();
                    let raster =
                        crate::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
                    samples.push(started.elapsed().as_secs_f64() * 1000.0);
                    assert_eq!((raster.width, raster.height), (width, height));
                }
                samples.sort_by(f64::total_cmp);
                let p50 = samples[samples.len() / 2];
                let p95 = samples[samples.len() - 1];
                let (copied, evaluated) = masked_tile_counts();
                let runs = samples.len() as u64;
                println!(
                    "{width}x{height} presence clarity +100 x1, mask of {count} whole-frame \
                     components: p50 {p50:.0} ms, p95 {p95:.0} ms over {runs} runs; tiles per run \
                     copied {}, evaluated {}; budget peak {:.1} MiB of {:.1} MiB",
                    copied / runs,
                    evaluated / runs,
                    SpatialBudget::default().peak() as f64 / MIB,
                    SpatialBudget::default().target() as f64 / MIB,
                );
            }
        }
        reset_masked_tile_counts();
    }
}
