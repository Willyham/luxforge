//! The host-planned reference read a task reads its asset's pixels through
//! (`docs/design/module-capabilities.md#the-host-planned-read`).
//!
//! # Shape
//!
//! A task that declares `reads` ([`ReadBoundary`]) is given a [`StagePlan`] when it is requested:
//! the catalog owner binds the asset's current entry as one immutable [`Evaluation`] with its
//! verified source and artifacts, chooses the stage the boundary names — for
//! [`ReadBoundary::RepairInput`], the stage the repair layer receives, `ReadStage::Before {layer,
//! mode}` at the repair layer's index or the index a new one would take — and records that stage's
//! input identity, the four components a new top correction's candidates are checked by
//! (`source`, `dimensions`, `upstream` development and `earlier` corrections). The owner reads no
//! pixel: planning is `O(layers)`.
//!
//! On the module lane the task opens a [`StageReader`] over the plan and reads bounded rectangles
//! of linear values. Every read comes from the reference renderer ([`ReferenceReads`]) on the
//! lane's own thread, whatever tile provider the host gave the owner (every AI input is a
//! reference read), so an answer is byte-identical to the reference renderer's own read
//! of the same rectangle on any host. A reader holds one reference session for its life, so a
//! spatial prefix's frames are evaluated once however many rectangles are read, and released when
//! the reader is dropped. Every read checks the job's cancellation first and the reference checks it
//! before every row.
//!
//! # Bounds
//!
//! Module reads never enter the owner's tile queue: its [`crate::tiles::TILE_QUEUE_CAPACITY`] of
//! nine waiting calls stays the eight loopback clients' and the desktop's. They have their own
//! admission instead: at most [`MODULE_READS_IN_FLIGHT`] reader per owner at once — the module lane
//! runs one job at a time, so this is the lane's own bound made explicit — and a second is refused
//! `resource-limit` at once. One read answers at most [`MAX_MODULE_READ_PIXELS`] pixels (48 MiB of
//! linear values); a task reads a larger region in tiles and reduces it on its lane.
// No shipped module declares `reads` yet; the generative fill is the first consumer.
#![cfg_attr(not(test), allow(dead_code))]
use super::descriptor::ReadBoundary;
use crate::{
    Cancel, Error, Evaluation, Region, RepairInput, Stage,
    tiles::{ReadPixels, ReadStage, ReadValues, ReferenceReads, TileReads, TileSession, clipped},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// The most pixels one module read answers: 4 Mi pixels, 48 MiB of linear `f32` RGB.
pub const MAX_MODULE_READ_PIXELS: u64 = 1 << 22;

/// How many module readers may be open at once per catalog owner.
pub const MODULE_READS_IN_FLIGHT: usize = 1;

/// One task's planned stage: what its reads read, bound by the owner when the task was requested.
pub(crate) struct StagePlan {
    pub(crate) boundary: ReadBoundary,
    pub(crate) evaluation: Evaluation,
    pub(crate) stage: ReadStage,
    pub(crate) size: Stage,
    pub(crate) input: RepairInput,
}

impl std::fmt::Debug for StagePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagePlan")
            .field("boundary", &self.boundary)
            .field("stage", &self.stage)
            .field("size", &self.size)
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

/// One rectangle of a planned stage, as linear sRGB values.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StageRead {
    pub(crate) boundary: ReadBoundary,
    /// The whole stage read, which the rectangle lies in.
    pub(crate) stage: Stage,
    /// The rectangle answered: the one asked for, clipped to the stage.
    pub(crate) rect: Region,
    /// One linear RGB value per pixel of `rect`, row by row from its top-left.
    pub(crate) linear: Vec<[f32; 3]>,
    /// The stage's input identity: what a candidate made from it is generated against.
    pub(crate) input: RepairInput,
}

/// The owner's admission of module readers, shared with every task's context.
#[derive(Debug, Default)]
pub(crate) struct ModuleReads {
    open: AtomicUsize,
}

impl ModuleReads {
    /// One reader's place, or `resource-limit` at once when [`MODULE_READS_IN_FLIGHT`] are open.
    fn admit(self: &Arc<Self>) -> Result<ReadPermit, Error> {
        self.open
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < MODULE_READS_IN_FLIGHT).then_some(open + 1)
            })
            .map_err(|_| {
                Error::resource_limit(format!(
                    "{MODULE_READS_IN_FLIGHT} module read is already open; retry after it is \
                     released"
                ))
            })?;
        Ok(ReadPermit(self.clone()))
    }

    #[cfg(test)]
    pub(crate) fn open(&self) -> usize {
        self.open.load(Ordering::Acquire)
    }
}

/// A reader's place in [`ModuleReads`], returned when it is dropped.
struct ReadPermit(Arc<ModuleReads>);

impl Drop for ReadPermit {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What a task's context holds to read its planned stage.
pub(crate) struct GrantedRead {
    pub(crate) plan: StagePlan,
    pub(crate) reads: Arc<ModuleReads>,
}

/// An open reader of one task's planned stage. See the [module documentation](self).
pub(crate) struct StageReader<'a> {
    plan: &'a StagePlan,
    session: Box<dyn TileSession + 'a>,
    cancel: &'a Cancel,
    _permit: ReadPermit,
}

impl<'a> StageReader<'a> {
    /// Admit and open a reader of `granted`'s stage under `cancel`.
    pub(crate) fn open(granted: &'a GrantedRead, cancel: &'a Cancel) -> Result<Self, Error> {
        cancel.check()?;
        let permit = granted.reads.admit()?;
        Ok(Self {
            plan: &granted.plan,
            session: ReferenceReads.session(&granted.plan.evaluation, cancel),
            cancel,
            _permit: permit,
        })
    }

    /// Which boundary the stage is.
    pub(crate) fn boundary(&self) -> ReadBoundary {
        self.plan.boundary
    }

    /// The whole stage's size.
    pub(crate) fn stage(&self) -> Stage {
        self.plan.size
    }

    /// The stage's input identity.
    pub(crate) fn input(&self) -> &RepairInput {
        &self.plan.input
    }

    /// The linear values of `rect` of the stage, clipped to it: refused `resource-limit` past
    /// [`MAX_MODULE_READ_PIXELS`] before anything is read, and `cancelled` once the job is.
    pub(crate) fn read(&mut self, rect: Region) -> Result<StageRead, Error> {
        self.cancel.check()?;
        let answered = clipped(rect, self.plan.size);
        if answered.pixels() > MAX_MODULE_READ_PIXELS {
            return Err(Error::resource_limit(format!(
                "a module read answers at most {MAX_MODULE_READ_PIXELS} pixels, not {} x {}; \
                 read it in tiles",
                answered.width, answered.height
            )));
        }
        let answer = self
            .session
            .read(self.plan.stage, rect, ReadValues::Linear)?;
        let ReadPixels::Linear(linear) = answer.pixels else {
            return Err(Error::internal("a linear read answered codes"));
        };
        Ok(StageRead {
            boundary: self.plan.boundary,
            stage: answer.stage,
            rect: answer.rect,
            linear,
            input: self.plan.input.clone(),
        })
    }
}
