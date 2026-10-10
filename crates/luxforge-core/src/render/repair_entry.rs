//! The repair boundary ([`super::Entry::Repair`]): a repair layer's operation table bound to the
//! stage it receives, the bounded regions its operations read, and the CPU reference's frame of it
//! (`docs/design/corrections.md`, "Rendering (TASK-003)").
//!
//! **Declared reads.** A spatial operation reads its output window grown by a halo. A repair
//! operation may read a source anywhere in the stage, so producing a window `W` of the repair
//! output reads a bounded **set** of rectangles ([`ReadSet`]): walking the operations from last to
//! first with `X = W`, an operation whose destination meets `X` adds its whole destination and its
//! source box to `X` — with Heal's one-pixel boundary ring around each, because a Heal solve
//! changes its whole patch. The set holds at most [`MAX_READ_RECTS`] disjoint rectangles after
//! merging overlaps; past that it is their bounding box. `MAX_SPATIAL_HALO` is not involved.
//!
//! Clone's whole destination joins the set as Heal's does, rather than only the part of it the
//! window needs (a delegated choice): every operation then evaluates over its whole destination, so
//! the pixels the set holds are exactly the whole frame's whatever window asked, and the reference
//! and the GPU evaluate one set of operations alike.
//!
//! **The reference frame.** [`repair_frame`] evaluates the operations in order over a working set
//! of `f32` planes holding the read set of every destination — the numerics study's units
//! (`crate::repair`) in place, each operation reading only what the ones before it left — and
//! writes the frame band by band, every pixel outside a destination its input. The working set is
//! reserved on the render's spatial budget before it is allocated, which paces the other spatial
//! work of the process; a failed allocation is `resource-limit`, naming the operation that needed
//! it. Nothing changes geometry or quality to fit.
use super::{PixelDomain, RenderContext};
use crate::{
    Cancel, Error,
    modules::{Parallelism, Region, RepairOperation, RepairTable, Stage},
    repair::{self, RepairOp, footprint},
};
use std::sync::{Arc, OnceLock};

/// The most rectangles a read set holds before it is replaced by their bounding box.
pub(crate) const MAX_READ_RECTS: usize = 16;

/// The rows of one band of the reference frame's write, in bytes of `f32` planes at most.
const BAND_BYTES: u64 = 16 * 1024 * 1024;

fn meets(a: Region, b: Region) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.x0 < b.x1()
        && b.x0 < a.x1()
        && a.y0 < b.y1()
        && b.y0 < a.y1()
}

fn union(a: Region, b: Region) -> Region {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let (x0, y0) = (a.x0.min(b.x0), a.y0.min(b.y0));
    let (x1, y1) = (a.x1().max(b.x1()), a.y1().max(b.y1()));
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

fn intersection(a: Region, b: Region) -> Region {
    let (x0, y0) = (a.x0.max(b.x0), a.y0.max(b.y0));
    let (x1, y1) = (a.x1().min(b.x1()), a.y1().min(b.y1()));
    if x0 >= x1 || y0 >= y1 {
        return Region::EMPTY;
    }
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// A bounded set of disjoint rectangles of one stage: what a window of a repair boundary's output
/// reads of its input ([`RepairEntry::reads`]). Adding a rectangle merges it with every rectangle
/// it overlaps into their bounding box, repeatedly, so each added rectangle lies wholly inside one
/// rectangle of the set; past [`MAX_READ_RECTS`] the set is its bounding box.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadSet {
    rects: Vec<Region>,
}

impl ReadSet {
    pub(crate) fn of(region: Region) -> Self {
        let mut set = Self::default();
        set.add(region);
        set
    }

    pub(crate) fn add(&mut self, region: Region) {
        if region.is_empty() {
            return;
        }
        let mut merged = region;
        loop {
            let before = self.rects.len();
            self.rects.retain(|held| {
                if meets(*held, merged) {
                    merged = union(merged, *held);
                    false
                } else {
                    true
                }
            });
            if self.rects.len() == before {
                break;
            }
        }
        self.rects.push(merged);
        if self.rects.len() > MAX_READ_RECTS {
            let bounds = self.bounds();
            self.rects = vec![bounds];
        }
    }

    /// Whether any rectangle of the set meets `region`.
    pub(crate) fn meets(&self, region: Region) -> bool {
        self.rects.iter().any(|held| meets(*held, region))
    }

    pub(crate) fn rects(&self) -> &[Region] {
        &self.rects
    }

    /// The bounding box of the set: the window a single texture holding it covers.
    pub(crate) fn bounds(&self) -> Region {
        self.rects
            .iter()
            .fold(Region::EMPTY, |bounds, held| union(bounds, *held))
    }

    /// The pixels the set holds.
    pub(crate) fn pixels(&self) -> u64 {
        self.rects.iter().map(|held| held.pixels()).sum()
    }
}

/// One operation's regions at the stage: what it may write and the two rectangles it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperationRegions {
    /// Every pixel whose coverage may be above zero.
    pub(crate) writes: Region,
    /// Its destination (with Heal's ring) and its source box (with Heal's ring), clipped.
    pub(crate) reads: [Region; 2],
    /// Source minus destination, in whole pixels of the stage.
    pub(crate) delta: [i64; 2],
}

/// A repair boundary: a repair layer's table bound to the `stage` it receives and writes.
#[derive(Clone)]
pub(crate) struct RepairEntry {
    pub(crate) table: RepairTable,
    pub(crate) stage: Stage,
    regions: Arc<[OperationRegions]>,
    /// The operations with their footprints' indexes, built once on the first evaluation that
    /// needs them (on a render worker, never while planning).
    bound: Arc<OnceLock<Arc<[RepairOp]>>>,
}

impl RepairEntry {
    /// The table bound to `stage`: each operation's regions, `O(points)` and no pixel.
    pub(crate) fn new(table: RepairTable, stage: Stage) -> Self {
        let regions = table
            .operations()
            .iter()
            .map(|operation| regions_of(operation, stage))
            .collect();
        Self {
            table,
            stage,
            regions,
            bound: Arc::new(OnceLock::new()),
        }
    }

    pub(crate) fn regions(&self) -> &[OperationRegions] {
        &self.regions
    }

    pub(crate) fn operations(&self) -> &[RepairOperation] {
        self.table.operations()
    }

    /// The operations bound to the stage, footprints and all.
    pub(crate) fn bound(&self) -> &[RepairOp] {
        self.bound.get_or_init(|| {
            self.table
                .operations()
                .iter()
                .map(|operation| {
                    if let Some(patch) = &operation.patch {
                        return RepairOp::patch(crate::repair::patch::BoundPatch::new(
                            patch.source.clone(),
                            operation.opacity,
                            self.stage,
                        ));
                    }
                    RepairOp::new(
                        operation.kind,
                        &operation.normalized_points(),
                        operation.radius_units(),
                        f64::from(operation.feather),
                        f64::from(operation.opacity),
                        operation.offset,
                        self.stage,
                    )
                })
                .collect()
        })
    }

    /// What producing `window` of the output reads of the input, and the operations that change
    /// any pixel of it, in application order ([module](self) docs). `O(operations · rects)`.
    pub(crate) fn reads(&self, window: Region) -> (ReadSet, Vec<usize>) {
        let mut set = ReadSet::of(intersection(window, Region::whole(self.stage)));
        let mut relevant = Vec::new();
        for (index, regions) in self.regions.iter().enumerate().rev() {
            if set.meets(regions.writes) {
                set.add(regions.reads[0]);
                set.add(regions.reads[1]);
                relevant.push(index);
            }
        }
        relevant.reverse();
        (set, relevant)
    }

    /// The single rectangle a GPU window producing `window` holds: the read set's bounding box.
    /// A GPU window is one texture, so the set's bounding box is what it allocates and charges.
    pub(crate) fn window_reads(&self, window: Region) -> Region {
        self.reads(window).0.bounds()
    }

    /// `window` with what producing it reads: a window over which a GPU step's output is the
    /// whole frame's at every pixel of `window`. `O(operations · rects)`.
    pub(crate) fn closure_once(&self, window: Region) -> Region {
        let window = intersection(window, Region::whole(self.stage));
        union(self.window_reads(window), window)
    }

    /// The read set of every destination: what a whole frame of the output reads beyond the
    /// identity, and every operation that writes a pixel.
    pub(crate) fn destination_reads(&self) -> (ReadSet, Vec<usize>) {
        let mut set = ReadSet::default();
        let mut relevant = Vec::new();
        for (index, regions) in self.regions.iter().enumerate() {
            if regions.writes.is_empty() {
                continue;
            }
            set.add(regions.reads[0]);
            set.add(regions.reads[1]);
            relevant.push(index);
        }
        (set, relevant)
    }
}

/// One operation's regions at `stage` ([`OperationRegions`]).
pub(crate) fn regions_of(operation: &RepairOperation, stage: Stage) -> OperationRegions {
    if let Some(patch) = &operation.patch {
        let writes = patch.source.writes(stage);
        return OperationRegions {
            writes,
            reads: repair::reads(operation.kind, writes, [0, 0], stage),
            delta: [0, 0],
        };
    }
    let segments = footprint::segments(&operation.normalized_points(), stage);
    let writes = footprint::support(&segments, operation.radius_units(), stage);
    let delta = repair::delta(operation.offset, stage);
    OperationRegions {
        writes,
        reads: repair::reads(operation.kind, writes, delta, stage),
        delta,
    }
}

/// `len` zeroed values, reserved first: a failed reservation is `resource-limit`, naming `what`.
fn planes(len: usize, what: impl FnOnce() -> String) -> Result<Vec<f32>, Error> {
    let mut values = Vec::new();
    values.try_reserve_exact(len).map_err(|_| {
        Error::resource_limit(format!(
            "the repair working set of {} needs {} MiB that could not be allocated",
            what(),
            (len as u64 * 4) >> 20
        ))
    })?;
    values.resize(len, 0.0);
    Ok(values)
}

/// The output of the repair boundary `entry` over its whole `stage`, in the domain's frame: every
/// destination's read set filled from the input (`fill` reads one rectangle of the stage it reads
/// into three planes), the operations applied over it in order, and the frame written band by band,
/// each band its input with the working set's values over it.
#[allow(clippy::too_many_arguments)]
pub(super) fn repair_frame<D: PixelDomain>(
    _domain: &D,
    entry: &RepairEntry,
    stage: Stage,
    cancel: &Cancel,
    context: &RenderContext,
    wide: bool,
    fill: impl Fn(Region, &mut [f32], Parallelism) -> Result<(), Error> + Sync,
) -> Result<D::SpatialFrame, Error> {
    if stage != entry.stage {
        return Err(Error::internal(format!(
            "a repair boundary bound to {}x{} received a {}x{} stage",
            entry.stage.width, entry.stage.height, stage.width, stage.height
        )));
    }
    cancel.check()?;
    let parallelism = if super::parallel::pooled(
        super::parallel::RenderPass::Repair,
        u64::from(stage.width) * u64::from(stage.height),
    ) {
        Parallelism::Pool
    } else {
        Parallelism::Serial
    };
    let mut frame = D::spatial_frame(stage, wide)?;
    let (set, relevant) = entry.destination_reads();
    let name = || {
        entry
            .operations()
            .iter()
            .map(RepairOperation::name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    // The working set, charged to the spatial budget before it is allocated.
    let bytes = set.pixels().saturating_mul(12);
    let _reservation = context.spatial().reserve(bytes, 1);
    let mut store: Vec<(Region, Vec<f32>)> = Vec::with_capacity(set.rects().len());
    for &rect in set.rects() {
        cancel.check()?;
        let mut values = planes(rect.pixels() as usize * 3, name)?;
        fill(rect, &mut values, parallelism)?;
        store.push((rect, values));
    }
    if !relevant.is_empty() {
        let bound = entry.bound();
        let ops: Vec<RepairOp> = relevant.iter().map(|&index| bound[index].clone()).collect();
        let parts = store
            .iter_mut()
            .map(|(rect, values)| (*rect, values.as_mut_slice()))
            .collect();
        let mut working = repair::RepairFrame::with_parts(stage, parts)?;
        repair::apply(&mut working, &ops, cancel).map_err(|error| {
            // Name the photograph's operation rather than its index among those that ran.
            match error.detail.strip_prefix("repair operation ") {
                Some(rest) => {
                    let (index, tail) = rest.split_once(':').unwrap_or((rest, ""));
                    let named = index
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| relevant.get(index))
                        .map(|&index| entry.operations()[index].name());
                    match named {
                        Some(name) => Error {
                            detail: format!("{name}:{tail}"),
                            ..error
                        },
                        None => error,
                    }
                }
                None => error,
            }
        })?;
    }
    // The frame, band by band: the input, with the working set's values where it holds them.
    let rows = (BAND_BYTES / (u64::from(stage.width.max(1)) * 12)).clamp(1, u64::from(u32::MAX));
    let mut y0 = 0;
    let mut values = Vec::new();
    while y0 < stage.height {
        cancel.check()?;
        let height = (rows as u32).min(stage.height - y0);
        let band = Region {
            x0: 0,
            y0,
            width: stage.width,
            height,
        };
        let len = band.pixels() as usize;
        values.resize(len * 3, 0.0);
        fill(band, &mut values, parallelism)?;
        for (rect, held) in &store {
            let shared = intersection(*rect, band);
            if shared.is_empty() {
                continue;
            }
            let held_len = rect.pixels() as usize;
            for y in shared.y0..shared.y1() {
                let from = ((y - rect.y0) * rect.width + (shared.x0 - rect.x0)) as usize;
                let to = ((y - band.y0) * band.width + shared.x0) as usize;
                let width = shared.width as usize;
                for channel in 0..3 {
                    values[channel * len + to..channel * len + to + width].copy_from_slice(
                        &held[channel * held_len + from..channel * held_len + from + width],
                    );
                }
            }
        }
        let output = D::tile_output(band, &values, band, parallelism, wide);
        D::write_tile(&mut frame, stage, band, output);
        y0 += height;
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x0: u32, y0: u32, width: u32, height: u32) -> Region {
        Region {
            x0,
            y0,
            width,
            height,
        }
    }

    #[test]
    fn a_read_set_merges_overlaps_and_bounds_its_count() {
        let mut set = ReadSet::of(region(0, 0, 10, 10));
        set.add(region(20, 0, 10, 10));
        assert_eq!(set.rects().len(), 2);
        // Overlapping both merges all three into one.
        set.add(region(5, 5, 20, 2));
        assert_eq!(set.rects(), &[region(0, 0, 30, 10)]);
        assert_eq!(set.pixels(), 300);
        let mut many = ReadSet::default();
        for index in 0..MAX_READ_RECTS as u32 {
            many.add(region(index * 10, index * 10, 5, 5));
        }
        assert_eq!(many.rects().len(), MAX_READ_RECTS);
        many.add(region(1000, 1000, 5, 5));
        assert_eq!(many.rects(), &[region(0, 0, 1005, 1005)]);
        assert!(many.meets(region(500, 500, 1, 1)));
    }
}
