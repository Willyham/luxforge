//! The state a render reads besides its source and its recipe: the colour scratch budget, the
//! spatial budget, the store of prepared global estimates and the store of reduced planes
//! ([`super::reduced`]), with their high-water marks and counters.
//!
//! None of it is process-global. One [`RenderContext`] is created by whoever owns the evaluations
//! that should share it — the editor service, and through it the catalog owner, the preview jobs
//! and the analysis jobs it plans — and every render receives it by reference. Two renders that
//! share a context share its budgets, which is what paces a render that overlaps another; two that
//! do not share nothing, which is what lets a test measure one render alone.

use super::reduced::ReducedStore;
use crate::modules::{ESTIMATE_STORE_ENTRIES, Global, REDUCED_STORE_BYTES, SPATIAL_BUDGET_BYTES};
use std::{
    borrow::Cow,
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// The default aggregate target for transient float scratch: 64 MiB across every active render.
pub(crate) const DEFAULT_SCRATCH_BYTES: u64 = 64 * 1024 * 1024;

/// The budgets and the stores every render in one context shares. Cloning it clones one
/// `Arc`: the clone is the same context, not a copy of it.
#[derive(Clone)]
pub struct RenderContext(Arc<Shared>);

struct Shared {
    scratch: ScratchBudget,
    spatial: SpatialBudget,
    estimates: EstimateStore,
    reduced: ReducedStore,
    /// How many stacks [`super::render`] compiled in this context, for the tests that prove a
    /// preview job compiles once per stage it renders at.
    #[cfg(test)]
    compiles: AtomicU64,
    /// Largest byte entry frame or RAW f64 tap-block allocation in this context.
    /// Tests use a fresh context to guard against silently computing a whole virtual crop stage.
    #[cfg(test)]
    resample_peak_bytes: AtomicU64,
    /// How many spatial operations' whole-stage outputs were materialized in this context.
    #[cfg(test)]
    spatial_frames: AtomicU64,
}

impl RenderContext {
    /// A context with empty budgets at their default targets and an empty estimate store.
    pub fn new() -> Self {
        Self::targeted(
            DEFAULT_SCRATCH_BYTES,
            SPATIAL_BUDGET_BYTES,
            REDUCED_STORE_BYTES,
        )
    }

    /// A context whose scratch budget has this target: how a test watches a colour pass meet a
    /// target smaller than the default, in a context nothing else renders through.
    #[cfg(test)]
    pub(crate) fn with_scratch_target(bytes: u64) -> Self {
        Self::targeted(bytes, SPATIAL_BUDGET_BYTES, REDUCED_STORE_BYTES)
    }

    /// A context whose spatial budget has this target, for a test that narrows the tile windows of
    /// the renders it makes through it.
    #[cfg(test)]
    pub(crate) fn with_spatial_target(bytes: u64) -> Self {
        Self::targeted(DEFAULT_SCRATCH_BYTES, bytes, REDUCED_STORE_BYTES)
    }

    /// A context whose store of reduced planes holds at most `bytes`, for a test that watches it
    /// evict or refuse at a small stage.
    #[cfg(test)]
    pub(crate) fn with_reduced_limit(bytes: u64) -> Self {
        Self::targeted(DEFAULT_SCRATCH_BYTES, SPATIAL_BUDGET_BYTES, bytes)
    }

    fn targeted(scratch: u64, spatial: u64, reduced: u64) -> Self {
        Self(Arc::new(Shared {
            scratch: ScratchBudget::new(scratch),
            spatial: SpatialBudget::new(spatial),
            estimates: EstimateStore::default(),
            reduced: ReducedStore::new(reduced),
            #[cfg(test)]
            compiles: AtomicU64::new(0),
            #[cfg(test)]
            resample_peak_bytes: AtomicU64::new(0),
            #[cfg(test)]
            spatial_frames: AtomicU64::new(0),
        }))
    }

    /// The colour scratch budget: the transient float buffers a colour run streams through.
    pub fn scratch(&self) -> &ScratchBudget {
        &self.0.scratch
    }

    /// The spatial budget: the working sets of spatial tiles.
    pub(crate) fn spatial(&self) -> &SpatialBudget {
        &self.0.spatial
    }

    /// The prepared global estimates of spatial units.
    pub(crate) fn estimates(&self) -> &EstimateStore {
        &self.0.estimates
    }

    /// The reduced planes of spatial units that run first in their operations.
    pub(crate) fn reduced(&self) -> &ReducedStore {
        &self.0.reduced
    }

    #[cfg(test)]
    pub(crate) fn note_compile(&self) {
        self.0.compiles.fetch_add(1, Ordering::Relaxed);
    }

    /// How many stacks the entry point has compiled in this context.
    #[cfg(test)]
    pub(crate) fn compiles(&self) -> u64 {
        self.0.compiles.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn note_resample_bytes(&self, bytes: usize) {
        self.0
            .resample_peak_bytes
            .fetch_max(bytes as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn resample_peak_bytes(&self) -> u64 {
        self.0.resample_peak_bytes.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn note_spatial_frame(&self) {
        self.0.spatial_frames.fetch_add(1, Ordering::Relaxed);
    }

    /// How many spatial frames renders in this context have materialized.
    #[cfg(test)]
    pub(crate) fn spatial_frames(&self) -> u64 {
        self.0.spatial_frames.load(Ordering::Relaxed)
    }
}

impl Default for RenderContext {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RenderContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RenderContext")
            .field("scratch_in_use", &self.scratch().in_use())
            .field("spatial_in_use", &self.spatial().in_use())
            .finish_non_exhaustive()
    }
}

/// An account of the transient float buffers a render streams through. Frames have their own
/// 512 MiB limit; this covers everything that is neither a frame nor the source, so a colour pass
/// never trades a bounded frame for scratch nobody counts. Reservations are taken before the
/// allocation they pay for and released when it is dropped.
///
/// It is a target, not a limit. What keeps scratch inside it is the row chunk, sized so one chunk
/// per pool worker stays well below the target; a chunk that finds the target taken — because more
/// renders overlap than the sizing assumed, or the target is small — still runs, and
/// [`Self::peak`] shows the overshoot. Nothing here refuses work.
pub struct ScratchBudget {
    target: u64,
    used: AtomicU64,
    /// The largest `used` any reservation ever reached, so a context that is idle when it is asked
    /// can still report what the budget actually had to carry. It is only ever raised.
    peak: AtomicU64,
}

impl ScratchBudget {
    fn new(target: u64) -> Self {
        Self {
            target,
            used: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        }
    }

    /// The figure the high-water mark is read against. It changes nothing a reservation does.
    pub fn target(&self) -> u64 {
        self.target
    }

    pub fn in_use(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    /// The high-water mark of [`Self::in_use`] since the context was created. A render's scratch
    /// is released as soon as its chunk is done, so `in_use` observed from outside a pass is almost
    /// always zero; this is what makes the budget observable after the fact, including a peak
    /// above the target.
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Reserve `bytes`. It never fails. The reservation is released when the returned guard is
    /// dropped, including on an early return from the work it covers.
    pub(crate) fn reserve(&self, bytes: usize) -> ScratchReservation<'_> {
        let bytes = bytes as u64;
        let total = self
            .used
            .fetch_add(bytes, Ordering::SeqCst)
            .saturating_add(bytes);
        // One relaxed maximum beside the reservation that already happened: the counter is only
        // read by diagnostics, so no other value depends on the order it becomes visible in.
        self.peak.fetch_max(total, Ordering::Relaxed);
        ScratchReservation {
            budget: self,
            bytes,
        }
    }
}

pub(crate) struct ScratchReservation<'a> {
    budget: &'a ScratchBudget,
    bytes: u64,
}

impl Drop for ScratchReservation<'_> {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// A target for the working sets of spatial tiles, separate from the colour run's
/// [`ScratchBudget`] because one tile is orders of magnitude larger than one row chunk: a 512 × 512
/// tile of a 60 MP stage with all three frozen presence units reads a 1408 × 1408 input region and
/// needs about 101 MiB, which the 64 MiB scratch target could not hold at all.
///
/// It is a target, not a limit. It decides how many tiles run at once: a render's window of tiles
/// starts with as many working sets as fit beside what other evaluations hold, and never fewer
/// than one, then gives one back when the target is passed and takes one more when one fits
/// (`render::spatial::run_tiles`). So a render that meets the target already taken — the
/// histogram's analysis rendering the same stack as the preview, say — slows to one tile at a time
/// instead of failing, and a tile larger than the whole target still runs, alone. The overshoot is
/// at most one working set per spatial evaluation in flight, and [`Self::peak`] shows it. Nothing
/// here refuses work.
///
/// Each share is reserved before the tiles it runs allocate, and given back the moment its worker
/// stops, so a render's window sees whatever other evaluations released in the meantime.
pub(crate) struct SpatialBudget {
    target: u64,
    used: AtomicU64,
    peak: AtomicU64,
    /// How many times a tile slot's buffer was allocated by the renders under this budget.
    #[cfg(test)]
    slot_allocations: AtomicU64,
}

impl SpatialBudget {
    fn new(target: u64) -> Self {
        Self {
            target,
            used: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            #[cfg(test)]
            slot_allocations: AtomicU64::new(0),
        }
    }

    /// Count `allocations` of tile slot buffers made by a render under this budget, for a test that
    /// proves a render allocates its planes once per slot, on whichever threads its tiles ran.
    #[cfg(test)]
    pub(crate) fn note_slot_allocations(&self, allocations: u64) {
        self.slot_allocations
            .fetch_add(allocations, Ordering::Relaxed);
    }

    /// Every tile slot buffer allocated by the renders under this budget.
    #[cfg(test)]
    pub(crate) fn slot_allocations(&self) -> u64 {
        self.slot_allocations.load(Ordering::Relaxed)
    }

    pub(crate) fn target(&self) -> u64 {
        self.target
    }

    pub(crate) fn in_use(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    /// The high-water mark of [`Self::in_use`]. A render releases each share as soon as its worker
    /// stops, so `in_use` observed from outside a render is almost always zero; this is
    /// what makes the budget observable after the fact, including a peak above the target when
    /// evaluations overlapped or one tile needed more than all of it.
    pub(crate) fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Start the high-water mark again from what is reserved right now, so a release measurement
    /// can report the peak of its measured renders rather than of the warm-up that filled the
    /// context's estimate store.
    #[cfg(test)]
    pub(crate) fn reset_peak(&self) {
        self.peak.store(self.in_use(), Ordering::Relaxed);
    }

    /// How many tiles of `working_set` bytes the target and the pool allow in flight together when
    /// nothing else holds any of the target: the widest a render's window asks to be, and at least
    /// one.
    pub(crate) fn concurrency(&self, working_set: u64) -> usize {
        usize::try_from(self.target() / working_set.max(1))
            .unwrap_or(usize::MAX)
            .clamp(1, rayon::current_num_threads().max(1))
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

    /// Reserve one more working set only if it fits in what the target has left: how a render's
    /// window widens. Unlike [`Self::reserve`] it never passes the target, and the high-water mark
    /// follows the same rule.
    pub(crate) fn try_reserve(&self, working_set: u64) -> Option<SpatialReservation<'_>> {
        let target = self.target();
        let used = self
            .used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                let after = used.saturating_add(working_set);
                (after <= target).then_some(after)
            })
            .ok()?;
        self.peak
            .fetch_max(used.saturating_add(working_set), Ordering::Relaxed);
        Some(SpatialReservation {
            budget: self,
            bytes: working_set,
            tiles: 1,
        })
    }
}

pub(crate) struct SpatialReservation<'a> {
    budget: &'a SpatialBudget,
    bytes: u64,
    tiles: usize,
}

impl<'a> SpatialReservation<'a> {
    /// How many tiles this reservation covers: at least one, at most what was asked for.
    #[cfg(test)]
    pub(crate) fn tiles(&self) -> usize {
        self.tiles
    }

    /// This reservation as one guard per tile it covers, each releasing its own share when it is
    /// dropped, so a render's window can give back one share at a time. Together they hold and
    /// release exactly what this one did.
    pub(crate) fn into_shares(mut self) -> Vec<SpatialReservation<'a>> {
        let bytes = std::mem::take(&mut self.bytes);
        let tiles = self.tiles.max(1);
        let share = bytes / tiles as u64;
        (0..tiles)
            .map(|index| SpatialReservation {
                budget: self.budget,
                // A reservation's bytes are its tiles times one working set, so the remainder is
                // zero unless that product saturated; the first share keeps it either way.
                bytes: if index == 0 {
                    share + bytes % tiles as u64
                } else {
                    share
                },
                tiles: 1,
            })
            .collect()
    }
}

impl Drop for SpatialReservation<'_> {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

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
/// the same stack in the same context costs no reduction at all.
#[derive(Default)]
pub(crate) struct EstimateStore {
    entries: Mutex<VecDeque<(EstimateKey, Option<Global>)>>,
}

impl EstimateStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<(EstimateKey, Option<Global>)>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `Some(estimate)` on a hit, where the estimate itself may be `None` for a preparation that
    /// yielded none.
    pub(crate) fn cached(&self, key: &EstimateKey) -> Option<Option<Global>> {
        self.lock()
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.clone())
    }

    pub(crate) fn remember(&self, key: EstimateKey, value: Option<Global>) {
        let mut store = self.lock();
        if store.iter().any(|(stored, _)| *stored == key) {
            return;
        }
        store.push_back((key, value));
        while store.len() > ESTIMATE_STORE_ENTRIES {
            store.pop_front();
        }
    }

    /// How many estimates are held right now.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }

    /// The keys held right now, oldest first.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<EstimateKey> {
        self.lock().iter().map(|(key, _)| key.clone()).collect()
    }
}
