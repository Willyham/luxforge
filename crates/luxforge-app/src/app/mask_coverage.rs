//! Exact mask feedback on its own bounded, sleeping worker. No photograph is rasterized to change
//! a selection, an overlay mode or an unbound candidate. Only jobs hold evaluations: keeping one
//! in desktop view state would pin RAW development planes and their memory gate.
use super::{Before, Editor, message::Message, tasks};
use iced::Task;
use luxforge_core::{
    ErrorKind, MaskCoverage, MaskCoverageTarget, MaskOverlayColour, MaskOverlayMode,
    MaskOverlayOutcome, PreviewJob, Region,
    analysis::AnalysisIdentity,
    latest::{Latest, Running},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Coverage that precedes its matching photo cannot be drawn yet. Avoid waking a redraw only to
/// retain it; the photo update publishes its content and drains ready coverage in the same turn.
/// If completion follows that drain, the worker sees the published content and wakes normally.
#[derive(Default)]
struct PhotoGate {
    presented: AtomicU64,
    ready: AtomicU64,
    #[cfg(test)]
    decisions: AtomicU64,
}

/// One worker-owned painted result, including a pass still painting. The consumer releases it
/// after adoption or replacement of its waiting result. At most two RGBA grids belong to this
/// handoff, beyond the presenter's existing frames. Completions carry metadata, never another
/// coverage plane.
#[derive(Default)]
struct PaintSlot {
    busy: Mutex<bool>,
    changed: Condvar,
    #[cfg(test)]
    waiting: std::sync::atomic::AtomicUsize,
}
struct PaintLease(Arc<PaintSlot>);
impl PaintSlot {
    fn reserve(
        self: &Arc<Self>,
        cancel: &luxforge_core::Cancel,
    ) -> Result<Arc<PaintLease>, luxforge_core::Error> {
        let mut busy = self.busy.lock().unwrap();
        loop {
            cancel.check()?;
            if !*busy {
                *busy = true;
                return Ok(Arc::new(PaintLease(self.clone())));
            }
            #[cfg(test)]
            self.waiting
                .fetch_add(1, std::sync::atomic::Ordering::Release);
            busy = self.changed.wait(busy).unwrap();
            #[cfg(test)]
            self.waiting
                .fetch_sub(1, std::sync::atomic::Ordering::Release);
        }
    }
    fn wake(&self) {
        let _busy = self.busy.lock().unwrap();
        self.changed.notify_all();
    }
}
impl Drop for PaintLease {
    fn drop(&mut self) {
        *self.0.busy.lock().unwrap() = false;
        self.0.changed.notify_all();
    }
}

/// Everything a view-only coverage choice changes. An epoch fences changes even when the same
/// mask is selected again before an earlier result arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spec {
    target: MaskCoverageTarget,
    cells: (u32, u32),
    region: Option<Region>,
    mode: MaskOverlayMode,
    colour: MaskOverlayColour,
}

/// Source, recipe/draft, view and target identity travel with each answer. The source itself lives
/// only on Job, never on this stamp or a completion retained while the photo catches up.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    epoch: u64,
    spec: Spec,
    identity: AnalysisIdentity,
    content: u64,
    progressive: bool,
    feedback_key: Option<u64>,
}

impl Stamp {
    fn summary(&self) -> Value {
        let target = match &self.spec.target {
            MaskCoverageTarget::Existing { mask, component } => {
                json!({"kind":"existing","mask":mask,"component":component})
            }
            MaskCoverageTarget::DraftCreated => json!({"kind":"draft-created"}),
        };
        json!({"epoch":self.epoch,"content":self.content,"identity":self.identity,
            "progressive":self.progressive,"feedback_key":self.feedback_key,
            "target":target,"cells":[self.spec.cells.0,self.spec.cells.1],
            "region":self.spec.region.map(|r| [r.x0,r.y0,r.width,r.height]),
            "mode":self.spec.mode.as_str(),"colour":self.spec.colour.as_str()})
    }
}

struct Adopted {
    stamp: Stamp,
    generation: u64,
    quality: Option<luxforge_ui::RegionQuality>,
    mask: luxforge_core::MaskId,
    component: Option<luxforge_core::ComponentId>,
}

struct Job {
    evaluation: luxforge_core::Evaluation,
    stamp: Stamp,
}

#[derive(Clone)]
pub(crate) struct Completion {
    stamp: Stamp,
    outcome: Arc<Feedback>,
    /// The existing bounded RGBA display grid, painted on the worker rather than the UI thread.
    painted: Option<Arc<Vec<u8>>>,
    lease: Option<Arc<PaintLease>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GridStamp {
    mask: luxforge_core::MaskId,
    component: Option<luxforge_core::ComponentId>,
    cells_w: u32,
    cells_h: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Feedback {
    grid: Option<GridStamp>,
    absent: Option<String>,
}

impl Completion {
    #[cfg(test)]
    fn new(stamp: Stamp, outcome: Arc<MaskOverlayOutcome>) -> Self {
        Self::paint(stamp, outcome, &luxforge_core::Cancel::never()).unwrap()
    }

    fn paint(
        stamp: Stamp,
        outcome: Arc<MaskOverlayOutcome>,
        cancel: &luxforge_core::Cancel,
    ) -> Result<Self, luxforge_core::Error> {
        let painted = match outcome.grid.as_ref() {
            Some(grid) => super::masks::mask_overlay::paint_cancellable(
                grid,
                stamp.spec.mode,
                stamp.spec.colour,
                cancel,
            )?
            .map(Arc::new),
            None => None,
        };
        cancel.check()?;
        // The cache alone retains coverage. A delivered result needs identity and dimensions;
        // retaining its coverage as well as RGBA would increase the image-buffer bound.
        let outcome = Arc::new(Feedback {
            grid: outcome.grid.as_ref().map(|grid| GridStamp {
                mask: grid.mask.clone(),
                component: grid.component.clone(),
                cells_w: grid.cells_w,
                cells_h: grid.cells_h,
            }),
            absent: outcome.absent.clone(),
        });
        Ok(Self {
            stamp,
            outcome,
            painted,
            lease: None,
        })
    }
}

/// One active request and one replaceable pending request, with one cached bounded grid. No cache
/// entry owns an evaluation, source, recipe or photograph buffer.
pub(crate) struct CoverageQueue {
    worker: Latest<Job, Completion>,
    paint_slot: Arc<PaintSlot>,
    photo_gate: Arc<PhotoGate>,
}

impl Default for CoverageQueue {
    fn default() -> Self {
        let mut cached: Option<(u64, Arc<MaskOverlayOutcome>)> = None;
        let mut input_cache = luxforge_core::InputGridCache::default();
        let paint_slot = Arc::new(PaintSlot::default());
        let slot = paint_slot.clone();
        let photo_gate = Arc::new(PhotoGate::default());
        let gate = photo_gate.clone();
        Self {
            paint_slot,
            photo_gate,
            worker: Latest::new(
                "luxforge-mask-coverage",
                move |job: Job, running: &Running<'_, _, _>| {
                    let lease = slot.reserve(running.abandoned()).ok()?;
                    let mut done = run_coverage(job, running, &mut cached, &mut input_cache)?;
                    if done.painted.is_some() {
                        done.lease = Some(lease);
                    }
                    // An unavailable outcome needs no matching photo and must wake immediately.
                    gate.ready.store(
                        if done.painted.is_some() {
                            done.stamp.content
                        } else {
                            0
                        },
                        Ordering::SeqCst,
                    );
                    Some(done)
                },
            ),
        }
    }
}

fn run_coverage(
    job: Job,
    running: &Running<'_, Job, Completion>,
    cached: &mut Option<(u64, Arc<MaskOverlayOutcome>)>,
    input_cache: &mut luxforge_core::InputGridCache,
) -> Option<Completion> {
    let result = job.evaluation.mask_overlay_coverage_with_cache(
        &job.stamp.spec.target,
        job.stamp.spec.cells,
        job.stamp.spec.region,
        cached.as_ref().map(|(key, _)| *key),
        // Accepted mask revisions are useful feedback even while a newer set waits. Only a
        // semantic cancellation abandons the active snapshot.
        running.abandoned(),
        input_cache,
    );
    let outcome = match result {
        Ok(MaskCoverage {
            key,
            outcome: Some(outcome),
        }) => {
            let outcome = Arc::new(outcome);
            *cached = Some((key, outcome.clone()));
            outcome
        }
        Ok(MaskCoverage { outcome: None, .. }) => cached.as_ref()?.1.clone(),
        Err(error) if error.kind == ErrorKind::Cancelled => return None,
        Err(error) => Arc::new(MaskOverlayOutcome {
            grid: None,
            absent: Some(error.detail),
        }),
    };
    Completion::paint(job.stamp, outcome, running.abandoned()).ok()
}

impl CoverageQueue {
    pub(crate) fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        let gate = self.photo_gate.clone();
        self.worker.set_waker(Arc::new(move || {
            if gate.ready.load(Ordering::SeqCst) <= gate.presented.load(Ordering::SeqCst) {
                waker();
            }
            #[cfg(test)]
            gate.decisions.fetch_add(1, Ordering::Release);
        }));
    }
    fn photo_presented(&mut self, content: u64) {
        self.photo_gate.presented.store(content, Ordering::SeqCst);
    }
    fn request(&mut self, job: Job) {
        let _ = self.worker.request(job);
    }
    pub(crate) fn cancel(&mut self) {
        self.worker.cancel();
        self.paint_slot.wake();
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.worker.is_busy()
    }
    pub(crate) fn ready(&self) -> bool {
        self.worker.ready()
    }
    pub(crate) fn poll(&mut self) -> Option<Completion> {
        self.worker.poll().map(|(_, done)| done)
    }
    /// Hold the paint slot as an undelivered result does: a job taken meanwhile waits to paint
    /// until the hold drops, so its completion cannot arrive inside the message that asked for it.
    #[cfg(test)]
    pub(crate) fn hold_painting(&self) -> PaintingHeld {
        PaintingHeld(
            self.paint_slot
                .reserve(&luxforge_core::Cancel::never())
                .expect("a reservation nothing cancels"),
        )
    }
}

/// A test's hold on the coverage worker's paint slot ([`CoverageQueue::hold_painting`]).
#[cfg(test)]
pub(crate) struct PaintingHeld(#[allow(dead_code)] Arc<PaintLease>);

impl Drop for CoverageQueue {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Default)]
pub(crate) struct CoverageWorker {
    pub(crate) queue: CoverageQueue,
    epoch: u64,
    spec: Option<Spec>,
    requested: Option<Stamp>,
    /// The epoch that started the one owner plan in flight. Its answer serves the current spec.
    planning: Option<u64>,
    waiting: Option<Completion>,
    /// The latest accepted full-stack source, by identity only. A source planned before a
    /// mask-only commit cannot replace a newer grid merely because the photo pixels match.
    latest: Option<AnalysisIdentity>,
    adopted: Option<Adopted>,
    unavailable: Option<(Stamp, String)>,
}

impl CoverageWorker {
    fn accepts(&self, done: &Completion) -> bool {
        if self.spec.as_ref() != Some(&done.stamp.spec) || self.epoch != done.stamp.epoch {
            return false;
        }
        let Some(request) = self.requested.as_ref() else {
            return false;
        };
        if request == &done.stamp {
            return true;
        }
        let (Some(wanted), Some(answer)) = (&request.identity.draft, &done.stamp.identity.draft)
        else {
            return false;
        };
        request.progressive
            && done.stamp.progressive
            && request.feedback_key.is_some()
            && request.feedback_key == done.stamp.feedback_key
            && wanted.draft_id == answer.draft_id
            && answer.draft_revision <= wanted.draft_revision
            && request.identity.asset_id == done.stamp.identity.asset_id
            && request.identity.source_fingerprint == done.stamp.identity.source_fingerprint
            && request.identity.entry_id == done.stamp.identity.entry_id
            && request.identity.snapshot_id == done.stamp.identity.snapshot_id
            && request.identity.width == done.stamp.identity.width
            && request.identity.height == done.stamp.identity.height
            && request.identity.domain == done.stamp.identity.domain
            && self.adopted.as_ref().is_none_or(|shown| {
                shown.stamp.identity.draft.as_ref().is_none_or(|previous| {
                    previous.draft_id != answer.draft_id
                        || previous.draft_revision <= answer.draft_revision
                })
            })
    }
}

impl Editor {
    /// Correlation for what was asked for and what the canvas actually took, retaining identities
    /// only. A live grid's accepted revision can differ from the photo's logical settlement.
    pub(crate) fn mask_coverage_summary(&self) -> Value {
        let worker = &self.coverage_worker;
        let adopted = worker
            .adopted
            .as_ref()
            .filter(|shown| {
                shown.generation == self.presentation.presented_generation
                    && shown.stamp.content == self.presentation.presented_content
                    && worker.spec.as_ref() == Some(&shown.stamp.spec)
            })
            .map(|shown| {
                json!({"request":shown.stamp.summary(),"generation":shown.generation,
                    "quality":shown.quality.map(|q| match q {
                        luxforge_ui::RegionQuality::Interactive => "interactive",
                        luxforge_ui::RegionQuality::Exact => "exact",
                    }),
                    "mask":shown.mask,"component":shown.component})
            });
        json!({"epoch":worker.epoch,"requested":worker.requested.as_ref().map(Stamp::summary),
            "adopted":adopted,"pending":self.mask_coverage_pending(),
            "unavailable":worker.unavailable.as_ref().map(|(stamp,reason)| json!({"request":stamp.summary(),"reason":reason}))})
    }

    /// Coverage the current choice asked for has still to arrive. A job a choice change abandoned
    /// may still be winding down on the worker; its answer is dropped, so nothing waits for it.
    pub(crate) fn mask_coverage_pending(&self) -> bool {
        let worker = &self.coverage_worker;
        (worker.requested.is_some() && (worker.queue.is_busy() || worker.queue.ready()))
            || worker.planning.is_some()
            || worker.waiting.is_some()
    }

    pub(crate) fn invalidate_mask_coverage(&mut self) {
        let worker = &mut self.coverage_worker;
        worker.epoch = worker.epoch.saturating_add(1);
        worker.spec = None;
        worker.requested = None;
        worker.planning = None;
        worker.waiting = None;
        worker.latest = None;
        worker.adopted = None;
        worker.unavailable = None;
        worker.queue.cancel();
        self.presentation.presenter.clear_coverage();
    }
    fn coverage_spec(&self) -> Option<Spec> {
        // Before a first frame the source stage can plan its grid concurrently. Once a failed
        // photograph was withdrawn there is no content a deferred grid could ever catch up to.
        if self.presentation.render_error.is_some() && !self.presentation.has_picture() {
            return None;
        }
        let target = self.mask_coverage_target()?;
        let region = self
            .presentation
            .region_raster
            .as_ref()
            .filter(|region| region.content == self.presentation.presented_content);
        let cells = if region.is_some() {
            self.overlay_cells()?
        } else {
            self.whole_overlay_cells()?
        };
        Some(Spec {
            target,
            cells,
            region: region.map(|region| region.rect),
            mode: self.effective_mask_overlay(),
            colour: self.session.workspace.mask_overlay_colour,
        })
    }

    /// Invalidate both work and presented coverage at the semantic choice boundary, so A→B→A,
    /// Off, hidden eyes, cancellations and view changes cannot revive an older answer.
    fn reconcile_coverage_spec(&mut self) -> bool {
        let spec = self.coverage_spec();
        if self.coverage_worker.spec == spec {
            return false;
        }
        let worker = &mut self.coverage_worker;
        worker.epoch = worker.epoch.saturating_add(1);
        worker.spec = spec;
        worker.requested = None;
        worker.waiting = None;
        worker.adopted = None;
        worker.unavailable = None;
        worker.queue.cancel();
        self.presentation.presenter.clear_coverage();
        true
    }

    /// Photo refinement can move generation/quality without changing exact mask inputs. Carry
    /// the already adopted field with that identical content and rectangle; its texture version
    /// stays fixed. Generation changes must not abandon every in-flight bound-mask snapshot.
    fn restamp_mask_coverage(&mut self) -> bool {
        let worker = &mut self.coverage_worker;
        let Some(shown) = worker.adopted.as_mut() else {
            return false;
        };
        if worker.spec.as_ref() != Some(&shown.stamp.spec)
            || shown.stamp.content != self.presentation.presented_content
        {
            return false;
        }
        let region = self.presentation.region_raster.as_ref();
        let generation = self.presentation.presented_generation;
        let quality = region.map(|region| region.quality);
        if shown.generation == generation && shown.quality == quality {
            return false;
        }
        if self
            .presentation
            .presenter
            .restamp_coverage(generation, region)
        {
            shown.generation = generation;
            shown.quality = quality;
            return false;
        }
        worker.adopted = None;
        worker.requested = None;
        true
    }

    /// Plan only coverage for the current entry/draft. Planning returns a source-bearing job once;
    /// it goes directly to the coverage worker, and never to the photograph queue.
    pub(crate) fn refresh_mask_coverage(&mut self) -> Task<Message> {
        self.reconcile_coverage_spec();
        if self.coverage_worker.spec.is_none() {
            return Task::none();
        }
        // A live draft's next accepted set supplies its exact evaluation. A drained draft may
        // plan again for an explicit presentation choice without changing the photograph.
        let draft = match self.core_gesture() {
            Some(gesture) if gesture.draft.drained() => Some(gesture.draft.draft_id.clone()),
            Some(_) => return Task::none(),
            None if matches!(
                self.coverage_worker.spec.as_ref().map(|spec| &spec.target),
                Some(MaskCoverageTarget::DraftCreated)
            ) =>
            {
                return Task::none();
            }
            None => None,
        };
        let Some(state) = self.document.state.as_ref() else {
            return Task::none();
        };
        // One plan in flight serves whichever spec is current when it lands, so sweeping the
        // pointer across rows plans once rather than once per row.
        let epoch = self.coverage_worker.epoch;
        if self.coverage_worker.planning.is_some() {
            return Task::none();
        }
        self.coverage_worker.planning = Some(epoch);
        tasks::mask_coverage_source_task(
            self.owner.clone(),
            self.client,
            state.asset.id.clone(),
            self.displayed_entry(),
            draft,
            epoch,
        )
    }

    pub(crate) fn mask_coverage_source_planned(
        &mut self,
        epoch: u64,
        result: Result<Box<PreviewJob>, String>,
    ) {
        if self.coverage_worker.planning != Some(epoch) {
            return;
        }
        self.coverage_worker.planning = None;
        self.reconcile_coverage_spec();
        if self.coverage_worker.spec.is_none() {
            return;
        }
        match result {
            Ok(job) => {
                // A draft is compared by id and accepted revision, not by an identity from a
                // separately planned create (which mints fresh temporary mask identities).
                let matches = self.document.state.as_ref().map(|s| &s.asset.id)
                    == Some(&job.identity.asset_id)
                    && self.displayed_entry().as_ref() == Some(&job.identity.entry_id)
                    && self.presentation.matches_pixel_content(&job)
                    && job
                        .identity
                        .draft
                        .as_ref()
                        .map(|s| (&s.draft_id, s.draft_revision))
                        == self
                            .session
                            .draft
                            .as_ref()
                            .map(|d| (&d.draft_id, d.draft_revision))
                    && self.coverage_worker.latest.as_ref().is_none_or(|latest| {
                        if matches!(
                            self.coverage_worker.spec.as_ref().map(|s| &s.target),
                            Some(MaskCoverageTarget::DraftCreated)
                        ) {
                            latest.asset_id == job.identity.asset_id
                                && latest.entry_id == job.identity.entry_id
                                && latest.snapshot_id == job.identity.snapshot_id
                                && latest.draft == job.identity.draft
                        } else {
                            latest == &job.identity
                        }
                    });
                if matches {
                    self.request_mask_coverage(&job, self.presentation.content_serial);
                }
            }
            Err(reason) => {
                self.mask_overlay_unavailable(self.presentation.presented_generation, &reason);
            }
        }
    }

    /// Every accepted preview plan also supplies the candidate coverage immediately, independently
    /// of how long a bound adjustment takes to rasterize. No evaluation remains on the Editor.
    pub(crate) fn request_mask_coverage(&mut self, job: &PreviewJob, content: u64) {
        if job.layer_count.is_some() {
            return;
        }
        self.coverage_worker.latest = Some(job.identity.clone());
        self.reconcile_coverage_spec();
        let Some(spec) = self.coverage_worker.spec.clone() else {
            return;
        };
        let feedback_key = job.evaluation.mask_feedback_key(&spec.target).ok();
        let stamp = Stamp {
            epoch: self.coverage_worker.epoch,
            spec,
            identity: job.identity.clone(),
            content,
            progressive: self.mask_gesture().is_some(),
            feedback_key,
        };
        if self.coverage_worker.requested.as_ref() == Some(&stamp) {
            return;
        }
        self.coverage_worker.planning = None;
        self.coverage_worker.waiting = None;
        self.coverage_worker.requested = Some(stamp.clone());
        self.coverage_worker.unavailable = None;
        self.coverage_worker.queue.request(Job {
            evaluation: job.evaluation.clone(),
            stamp,
        });
    }

    pub(crate) fn mask_coverage_ready(&mut self, mut done: Completion) {
        // Hold the slot through this adoption, including dropping any replaced waiting result.
        // Releasing it during poll could overlap old waiting + delivered + next painted grids.
        // A retained waiting result owns no lease, so it never blocks the next coverage pass.
        let _handoff = done.lease.take();
        self.reconcile_coverage_spec();
        if !self.coverage_worker.accepts(&done) {
            return;
        }
        let current_draft = self.session.draft.as_ref();
        let same_draft = match (&done.stamp.identity.draft, current_draft) {
            (None, None) => true,
            (Some(answer), Some(current)) => {
                answer.draft_id == current.draft_id
                    && if done.stamp.progressive {
                        answer.draft_revision <= current.draft_revision
                    } else {
                        answer.draft_revision == current.draft_revision
                    }
            }
            _ => false,
        };
        if !same_draft {
            return;
        }
        if self.presentation.presented_content != done.stamp.content {
            self.coverage_worker.waiting = Some(done);
            return;
        }
        let generation = self.presentation.presented_generation;
        let Feedback { grid, absent } = done.outcome.as_ref();
        if let Some(grid) = grid {
            let mask = grid.mask.clone();
            let component = grid.component.clone();
            self.event("mask_coverage_ready", || json!({"epoch":done.stamp.epoch,
                "generation":generation,"identity":done.stamp.identity,"mask":grid.mask,
                "component":grid.component,"region":done.stamp.spec.region.map(|r| [r.x0,r.y0,r.width,r.height])}));
            if done.painted.as_ref().is_some_and(|painted| {
                self.present_painted_mask_overlay(
                    generation,
                    &grid.mask,
                    grid.component.as_ref(),
                    (grid.cells_w, grid.cells_h),
                    painted.clone(),
                )
            }) {
                self.coverage_worker.adopted = Some(Adopted {
                    stamp: done.stamp,
                    generation,
                    quality: self
                        .presentation
                        .region_raster
                        .as_ref()
                        .map(|region| region.quality),
                    mask,
                    component,
                });
            } else {
                self.coverage_worker.adopted = None;
            }
        } else if let Some(reason) = absent {
            self.coverage_worker.adopted = None;
            self.coverage_worker.unavailable = Some((done.stamp, reason.clone()));
            self.mask_overlay_unavailable(generation, reason);
        }
    }
}

pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let changed = editor.reconcile_coverage_spec();
    let missing = editor.restamp_mask_coverage();
    // Publish before polling: a worker whose wake saw the older photo already enqueued its
    // result, which this drain sees. A later completion sees the new photo and wakes normally.
    editor
        .coverage_worker
        .queue
        .photo_presented(editor.presentation.presented_content);
    while let Some(done) = editor.coverage_worker.queue.poll() {
        editor.mask_coverage_ready(done);
    }
    if let Some(done) = editor.coverage_worker.waiting.take() {
        editor.mask_coverage_ready(done);
    }
    if changed || missing {
        editor.refresh_mask_coverage()
    } else {
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::fresh_stack;
    use luxforge_core::{
        AssetId, Component, ComponentMode, EntryId, Evaluation, HistoryEntry, Mask, ModuleRegistry,
        PreviewSource, Recipe, RenderContext, Snapshot, SnapshotId, SourceImage,
    };

    fn fixture() -> (Evaluation, Mask) {
        let mut mask = Mask::new("Sky");
        mask.components.push(Component::new(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            json!({"x0":0.5,"y0":0.1,"x1":0.5,"y1":0.9}),
        ));
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let asset = AssetId::new();
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.clone(),
            sequence: 0,
            action_id: "test".into(),
            label: "Test".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: asset,
                recipe: recipe.clone(),
            },
            undo_parent: None,
            restore_target: None,
        };
        let evaluation = Evaluation::new(
            Arc::new(ModuleRegistry::builtin()),
            RenderContext::new(),
            PreviewSource::Jpeg(SourceImage {
                width: 60,
                height: 40,
                rgba: vec![128; 60 * 40 * 4].into(),
                fingerprint: "sha256:coverage-worker".into(),
                orientation: 1,
                capture: Default::default(),
            }),
            entry,
            recipe,
            None,
        );
        (evaluation, mask)
    }

    fn stamp(evaluation: &Evaluation, mask: &Mask, epoch: u64) -> Stamp {
        Stamp {
            epoch,
            spec: Spec {
                target: MaskCoverageTarget::Existing {
                    mask: mask.id.clone(),
                    component: None,
                },
                cells: (28, 19),
                region: None,
                mode: MaskOverlayMode::Tint,
                colour: MaskOverlayColour::Green,
            },
            identity: evaluation.identity().unwrap(),
            content: 1,
            progressive: false,
            feedback_key: evaluation
                .mask_feedback_key(&MaskCoverageTarget::Existing {
                    mask: mask.id.clone(),
                    component: None,
                })
                .ok(),
        }
    }

    #[test]
    fn completed_coverage_requires_the_whole_requested_stamp() {
        let (evaluation, mask) = fixture();
        let stamp = stamp(&evaluation, &mask, 3);
        let mut worker = CoverageWorker {
            epoch: 3,
            spec: Some(stamp.spec.clone()),
            requested: Some(stamp.clone()),
            ..Default::default()
        };
        let done = Completion::new(stamp.clone(), Arc::new(MaskOverlayOutcome::default()));
        assert!(worker.accepts(&done));
        let mut old = done.clone();
        old.stamp.identity.recipe_hash.push('a');
        assert!(
            !worker.accepts(&old),
            "old recipe on unchanged photo pixels is stale"
        );
        old = done.clone();
        old.stamp.content += 1;
        assert!(
            !worker.accepts(&old),
            "a different photograph content cannot adopt"
        );
        old = done.clone();
        old.stamp.spec.mode = MaskOverlayMode::Off;
        assert!(!worker.accepts(&old), "overlay choice is part of the stamp");
        old = done.clone();
        old.stamp.spec.target = MaskCoverageTarget::DraftCreated;
        assert!(
            !worker.accepts(&old),
            "creation cannot answer existing selection"
        );
        worker.epoch += 2;
        worker.requested.as_mut().unwrap().epoch += 2;
        assert!(!worker.accepts(&done), "A to B to A keeps the later epoch");
    }

    fn accepted_revision(
        evaluation: &Evaluation,
        draft_id: &luxforge_core::DraftId,
        revision: u64,
    ) -> Evaluation {
        let mut recipe = evaluation.recipe().clone();
        recipe.masks[0].amount = revision as f64 * 10.0;
        Evaluation::new(
            evaluation.registry().clone(),
            evaluation.context().clone(),
            evaluation.source().clone(),
            evaluation.entry().clone(),
            recipe,
            Some(luxforge_core::DraftStamp {
                draft_id: draft_id.clone(),
                draft_revision: revision,
            }),
        )
    }

    fn progressive_stamp(evaluation: &Evaluation, mask: &Mask, epoch: u64, content: u64) -> Stamp {
        let mut request = stamp(evaluation, mask, epoch);
        request.progressive = true;
        request.content = content;
        request
    }

    #[test]
    fn a_slow_running_snapshot_progresses_during_sustained_accepted_input_and_finishes_current() {
        use std::sync::mpsc;
        let (base, mask) = fixture();
        let draft_id = luxforge_core::DraftId::new();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut cached = None;
        let mut input_cache = luxforge_core::InputGridCache::default();
        let queue = CoverageQueue {
            paint_slot: Arc::default(),
            photo_gate: Arc::default(),
            worker: Latest::new("coverage-progress-test", move |job: Job, running| {
                started_tx
                    .send(job.stamp.identity.draft.as_ref().unwrap().draft_revision)
                    .ok()?;
                // A deterministic slow computation: several accepted inputs arrive while this job
                // runs. The same production coverage function then reads its abandonment token.
                release_rx.recv().ok()?;
                run_coverage(job, running, &mut cached, &mut input_cache)
            }),
        };
        let initial = progressive_stamp(&accepted_revision(&base, &draft_id, 1), &mask, 1, 1);
        let mut worker = CoverageWorker {
            queue,
            epoch: 1,
            spec: Some(initial.spec.clone()),
            ..Default::default()
        };
        let enqueue = |worker: &mut CoverageWorker, revision| {
            let evaluation = accepted_revision(&base, &draft_id, revision);
            let request = progressive_stamp(&evaluation, &mask, 1, 1);
            worker.requested = Some(request.clone());
            worker.queue.request(Job {
                evaluation,
                stamp: request,
            });
        };
        enqueue(&mut worker, 1);
        assert_eq!(
            started_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            1
        );
        let mut first = None;
        for (running_revision, newest_revision) in [(1, 4), (4, 7), (7, 7)] {
            for revision in running_revision + 1..=newest_revision {
                enqueue(&mut worker, revision);
            }
            release_tx.send(()).unwrap();
            let done = luxforge_testbase::wait_for("progressive accepted coverage", || {
                worker.queue.poll()
            });
            assert_eq!(
                done.stamp.identity.draft.as_ref().unwrap().draft_revision,
                running_revision
            );
            assert!(
                worker.accepts(&done),
                "a completed accepted prefix must give feedback while newer input waits"
            );
            let expected = accepted_revision(&base, &draft_id, running_revision)
                .mask_overlay_coverage(
                    &done.stamp.spec.target,
                    done.stamp.spec.cells,
                    None,
                    None,
                    &luxforge_core::Cancel::never(),
                )
                .unwrap()
                .outcome
                .unwrap();
            let expected = Completion::new(done.stamp.clone(), Arc::new(expected));
            assert_eq!(
                done.outcome, expected.outcome,
                "each progressive frame is the exact accepted snapshot"
            );
            assert_eq!(
                done.painted, expected.painted,
                "the same snapshot's exact display bytes"
            );
            worker.adopted = Some(Adopted {
                stamp: done.stamp.clone(),
                generation: 1,
                quality: None,
                mask: mask.id.clone(),
                component: None,
            });
            if let Some(earlier) = first.as_ref() {
                assert!(
                    !worker.accepts(earlier),
                    "accepted feedback never regresses"
                );
            } else {
                first = Some(done.clone());
            }
            if running_revision != newest_revision {
                assert_eq!(
                    started_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap(),
                    newest_revision
                );
            } else {
                assert_eq!(
                    worker.requested.as_ref(),
                    Some(&done.stamp),
                    "the terminal result is current"
                );
            }
        }
        let old = first.unwrap();
        let mut wrong = old.clone();
        wrong.stamp.feedback_key = Some(wrong.stamp.feedback_key.unwrap().wrapping_add(1));
        assert!(
            !worker.accepts(&wrong),
            "other recipe/source dependencies must stay exact"
        );
        worker.requested.as_mut().unwrap().identity.draft = None;
        worker.requested.as_mut().unwrap().progressive = false;
        assert!(
            !worker.accepts(&old),
            "a committed entry cannot accept old draft feedback"
        );
        enqueue(&mut worker, 8);
        assert_eq!(
            started_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            8
        );
        worker.epoch += 1;
        worker.requested = None;
        worker.queue.cancel();
        release_tx.send(()).unwrap();
        luxforge_testbase::wait_until("abandoned coverage stops", || !worker.queue.is_busy());
        assert!(worker.queue.poll().is_none());
        assert!(
            !worker.accepts(&old),
            "late selection/cancel epochs cannot revive an accepted prefix"
        );
    }

    #[test]
    fn coverage_wakes_only_when_drawable_and_photo_publication_cannot_lose_a_result() {
        let (evaluation, mask) = fixture();
        let mut queue = CoverageQueue::default();
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        queue.set_waker(Arc::new(move || wake_tx.send(()).unwrap()));
        let mut request = stamp(&evaluation, &mask, 1);
        request.content = 8;
        queue.photo_presented(7);
        queue.request(Job {
            evaluation: evaluation.clone(),
            stamp: request.clone(),
        });
        luxforge_testbase::wait_until("coverage ready before its photo", || queue.ready());
        luxforge_testbase::wait_until("the worker decided whether to wake", || {
            queue.photo_gate.decisions.load(Ordering::Acquire) == 1
        });
        assert!(
            wake_rx.try_recv().is_err(),
            "an undrawable grid causes no redraw"
        );
        // Completion before publication is adopted by the same turn's drain, without a wake.
        queue.photo_presented(8);
        let done = queue.poll().unwrap();
        assert_eq!(done.stamp.content, 8);
        assert!(done.painted.is_some());
        drop(done);

        // Completion after publication must wake, including cache hits on unchanged pixels.
        queue.request(Job {
            evaluation: evaluation.clone(),
            stamp: request.clone(),
        });
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        drop(queue.poll().unwrap());

        // An unavailable result remains immediate, even without a matching photograph.
        request.content = 9;
        request.spec.target = MaskCoverageTarget::Existing {
            mask: luxforge_core::MaskId::new(),
            component: None,
        };
        request.feedback_key = None;
        queue.request(Job {
            evaluation,
            stamp: request,
        });
        wake_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let done = queue.poll().unwrap();
        assert!(done.painted.is_none());
        assert!(done.outcome.absent.is_some());
    }

    #[test]
    fn coverage_cache_releases_sources_and_cancelled_jobs_can_be_requested_again() {
        let (evaluation, mask) = fixture();
        let mut queue = CoverageQueue::default();
        let (first, pixels) = fresh_stack(&evaluation);
        let request = stamp(&first, &mask, 1);
        queue.request(Job {
            evaluation: first,
            stamp: request.clone(),
        });
        let mut done = luxforge_testbase::wait_for("the exact coverage worker", || queue.poll());
        assert_eq!(done.stamp, request);
        assert_eq!(done.outcome.grid.as_ref().unwrap().mask, mask.id);
        done.lease.take(); // Consumer finished the handoff; retain only bytes for the comparison.
        luxforge_testbase::wait_until("the coverage source is released", || {
            pixels.strong_count() == 0 && !queue.is_busy()
        });

        queue.cancel();
        assert!(queue.poll().is_none());
        let (again, pixels) = fresh_stack(&evaluation);
        queue.request(Job {
            evaluation: again,
            stamp: request.clone(),
        });
        let cached = luxforge_testbase::wait_for("the coverage cache hit", || queue.poll());
        assert_eq!(cached.outcome, done.outcome);
        assert_eq!(
            cached.painted, done.painted,
            "cache hits paint the same exact bytes"
        );
        luxforge_testbase::wait_until("the cached job releases its source", || {
            pixels.strong_count() == 0 && !queue.is_busy()
        });
        assert!(!queue.ready());
    }

    #[test]
    fn an_unconsumed_painted_result_bounds_the_next_pass_and_cancellation_wakes_it() {
        let (evaluation, mask) = fixture();
        let mut queue = CoverageQueue::default();
        let request = stamp(&evaluation, &mask, 1);
        queue.request(Job {
            evaluation: evaluation.clone(),
            stamp: request.clone(),
        });
        luxforge_testbase::wait_until("one unconsumed painted result", || queue.ready());
        let (second, pixels) = fresh_stack(&evaluation);
        queue.request(Job {
            evaluation: second,
            stamp: request.clone(),
        });
        luxforge_testbase::wait_until("the next pass is blocked before allocating", || {
            queue
                .paint_slot
                .waiting
                .load(std::sync::atomic::Ordering::Acquire)
                == 1
        });
        assert!(*queue.paint_slot.busy.lock().unwrap());
        let delivering = queue.poll().unwrap();
        assert!(delivering.lease.is_some(), "poll retains the handoff lease");
        assert!(*queue.paint_slot.busy.lock().unwrap());
        queue.cancel();
        luxforge_testbase::wait_until("cancellation releases the blocked source", || {
            pixels.strong_count() == 0 && !queue.is_busy()
        });
        assert!(queue.poll().is_none());
        drop(delivering);
        queue.request(Job {
            evaluation,
            stamp: request,
        });
        let done = luxforge_testbase::wait_for("a new request after cancellation", || queue.poll());
        assert!(done.painted.is_some());
    }

    fn drain_photo(editor: &mut Editor) {
        luxforge_testbase::wait_until("the photograph queue settles", || {
            let _ = editor.update(Message::Preview(
                super::super::message::preview::PreviewMessage::Poll,
            ));
            !editor.presentation.queue.is_busy() && !editor.presentation.queue.ready()
        });
    }

    #[test]
    fn same_content_region_refinement_and_photo_restamp_refresh_coverage() {
        use crate::app::testing::{boot, finish};
        use luxforge_ui::RegionQuality;
        let (mut editor, catalog) = boot();
        let (evaluation, mask) = fixture();
        editor.request_preview(PreviewJob::new(evaluation.clone()).unwrap());
        drain_photo(&mut editor);
        editor.session.workspace.mode = luxforge_core::MASK_MODE.into();
        editor.session.workspace.mask_overlay = MaskOverlayMode::Tint;
        editor.mask_panel.selected_mask = Some(mask.id.clone());
        let content = editor.presentation.presented_content;
        let photo_version = editor.presentation.presenter.photo_version();
        let full_stage = luxforge_core::StageSize {
            width: 60,
            height: 40,
        };
        let full_rect = Region {
            x0: 6,
            y0: 4,
            width: 48,
            height: 32,
        };
        let initial_generation = editor.presentation.presented_generation;
        let mut coverage_version = None;
        for (offset, quality) in [(1, RegionQuality::Interactive), (2, RegionQuality::Exact)] {
            let (stage, rect) = if quality == RegionQuality::Interactive {
                (
                    luxforge_core::StageSize {
                        width: 30,
                        height: 20,
                    },
                    Region {
                        x0: 3,
                        y0: 2,
                        width: 24,
                        height: 16,
                    },
                )
            } else {
                (full_stage, full_rect)
            };
            let raster = luxforge_core::Raster {
                width: rect.width,
                height: rect.height,
                rgba: vec![128; (rect.width * rect.height * 4) as usize].into(),
                source_fingerprint: evaluation.identity().unwrap().source_fingerprint,
                snapshot_id: evaluation.entry().snapshot.id.clone(),
            };
            let frame = luxforge_core::RegionFrame {
                raster,
                rect,
                stage,
                full_rect,
                full_stage,
                approximation: Default::default(),
            };
            let delivery = super::super::preview::Delivery {
                generation: initial_generation + offset,
                stage: (60, 40),
                draft: None,
                entry_id: evaluation.entry().id.clone(),
                draft_revision: None,
                intent: luxforge_core::PreviewIntent::Interactive,
                viewport_declined: None,
                approximate_white_balance: false,
                render_ms: 1.0,
            };
            assert!(
                editor
                    .presentation
                    .show_region(&delivery, &frame, quality, content)
            );
            if offset == 1 {
                assert!(editor.reconcile_coverage_spec());
                editor
                    .request_mask_coverage(&PreviewJob::new(evaluation.clone()).unwrap(), content);
                let done = luxforge_testbase::wait_for("coverage for the region", || {
                    editor.coverage_worker.queue.poll()
                });
                editor.mask_coverage_ready(done);
            } else {
                assert!(
                    !editor.reconcile_coverage_spec(),
                    "photo refinement changes no exact coverage dependency"
                );
                assert!(
                    editor.presentation.coverage().is_none(),
                    "old photo metadata cannot draw before rebind"
                );
                assert!(!editor.restamp_mask_coverage());
                assert!(
                    !editor.mask_coverage_pending(),
                    "same-field refinement starts no coverage or photo job"
                );
            }
            let overlay = editor.presentation.presenter.region_coverage().unwrap();
            assert_eq!(overlay.generation, delivery.generation);
            assert_eq!(overlay.quality, quality);
            assert!(editor.presentation.coverage().is_some());
            assert_eq!(
                editor.mask_coverage_summary()["adopted"]["generation"],
                json!(delivery.generation)
            );
            assert!(
                !editor.presentation.queue.is_busy(),
                "coverage refinement cannot request photo work"
            );
            if let Some(version) = coverage_version {
                assert_eq!(
                    overlay.frame.version(),
                    version,
                    "the exact field is kept without another texture upload"
                );
            }
            coverage_version = Some(overlay.frame.version());
        }
        editor.presentation.region_raster = None;
        editor.presentation.presenter.clear_region();
        editor.request_mask_coverage(&PreviewJob::new(evaluation.clone()).unwrap(), content);
        let whole = luxforge_testbase::wait_for("whole mask coverage", || {
            editor.coverage_worker.queue.poll()
        });
        editor.mask_coverage_ready(whole.clone());
        assert!(editor.presentation.coverage().is_some());
        let next = editor.presentation.presented_generation + 1;
        editor.presentation.restamp(next);
        let version = editor
            .presentation
            .presenter
            .coverage(next - 1)
            .unwrap()
            .version();
        assert!(
            !editor.reconcile_coverage_spec(),
            "same pixels with a new generation keep the exact coverage inputs"
        );
        assert!(!editor.restamp_mask_coverage());
        assert!(!editor.mask_coverage_pending());
        assert_eq!(
            editor
                .presentation
                .presenter
                .coverage(next)
                .unwrap()
                .version(),
            version
        );
        assert!(editor.presentation.coverage().is_some());
        assert_eq!(
            editor.mask_coverage_summary()["adopted"]["generation"],
            json!(next)
        );
        assert_eq!(editor.presentation.presenter.photo_version(), photo_version);
        assert!(!editor.presentation.queue.is_busy());
        finish(editor, catalog);
    }

    #[test]
    fn the_preview_scheduler_refines_half_detail_and_missing_reports_but_reuses_settled_mask_only_pixels()
     {
        use crate::app::testing::{boot, finish};
        let (mut editor, catalog) = boot();
        let (initial, mask) = fixture();
        let mut recipe = initial.recipe().clone();
        recipe.layers.push(luxforge_core::Layer {
            id: luxforge_core::LayerId::new(),
            effect_id: luxforge_core::BASIC_EFFECT.into(),
            effect_format: luxforge_core::EFFECT_FORMAT,
            payload: json!({"exposure":1.0}),
            mask: Some(mask.id),
            artifacts: Vec::new(),
        });
        editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
        let build = |recipe: Recipe| {
            Evaluation::new(
                initial.registry().clone(),
                initial.context().clone(),
                initial.source().clone(),
                initial.entry().clone(),
                recipe,
                None,
            )
        };
        let mut job = PreviewJob::new(build(recipe.clone())).unwrap();
        job.analyse = true;
        editor.request_preview(job);
        drain_photo(&mut editor);
        let old_content = editor.presentation.presented_content;
        recipe.masks[0].amount = 50.0;
        let changed = build(recipe.clone());
        let mut moving = PreviewJob::new(changed.clone()).unwrap();
        moving.intent = luxforge_core::PreviewIntent::Interactive;
        moving.analyse = true;
        editor.request_preview(moving);
        drain_photo(&mut editor);
        assert_ne!(editor.presentation.presented_content, old_content);
        assert_eq!(
            editor.presentation.region_raster.as_ref().unwrap().quality,
            luxforge_ui::RegionQuality::Interactive
        );
        assert_ne!(
            editor.presentation.analysis_content,
            Some(editor.presentation.presented_content)
        );
        let moving_generation = editor.presentation.presented_generation;
        let mut settle = PreviewJob::new(changed).unwrap();
        settle.intent = luxforge_core::PreviewIntent::Settle;
        settle.analyse = true;
        let refinement_generation = editor.request_preview(settle);
        assert!(
            editor.presentation.reused.is_none(),
            "half detail must not satisfy settlement"
        );
        assert!(editor.presentation.queue.is_busy());
        assert!(refinement_generation > moving_generation);
        drain_photo(&mut editor);
        let content = editor.presentation.presented_content;
        assert_eq!(editor.presentation.exact_content(), Some(content));
        assert_eq!(editor.presentation.analysis_content, Some(content));
        assert!(
            editor
                .presentation
                .region_raster
                .as_ref()
                .is_none_or(|region| region.quality == luxforge_ui::RegionQuality::Exact)
        );
        let version = editor.presentation.presenter.photo_version();
        let mut unbound = Mask::new("Unbound");
        unbound.components.push(Component::new(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            initial.recipe().masks[0].components[0].payload.clone(),
        ));
        recipe.masks.push(unbound);
        let saved = build(recipe);
        let mut mask_only = PreviewJob::new(saved.clone()).unwrap();
        mask_only.analyse = true;
        editor.request_preview(mask_only);
        assert!(
            editor.presentation.reused.is_some(),
            "settled mask-only commits still reuse exact pixels/report at 100%"
        );
        assert!(!editor.presentation.queue.is_busy());
        editor.settle_reused_pixels();
        assert_eq!(editor.presentation.presenter.photo_version(), version);
        assert_eq!(
            editor.presentation.analysis.as_ref().unwrap().identity,
            saved.identity().unwrap()
        );
        editor.presentation.analysis = None;
        editor.presentation.analysis_content = None;
        let mut missing_report = PreviewJob::new(saved).unwrap();
        missing_report.analyse = true;
        editor.request_preview(missing_report);
        assert!(
            editor.presentation.reused.is_none(),
            "exact pixels alone cannot claim a requested report exists"
        );
        assert!(editor.presentation.queue.is_busy());
        drain_photo(&mut editor);
        assert_eq!(editor.presentation.analysis_content, Some(content));
        assert!(editor.presentation.analysis.is_some());
        finish(editor, catalog);
    }

    #[test]
    fn bound_progressive_coverage_waits_for_its_pixels_without_cancelling_on_photo_generations() {
        use crate::app::testing::{boot, finish};
        let (mut editor, catalog) = boot();
        let (initial, mask) = fixture();
        let mut recipe = initial.recipe().clone();
        recipe.layers.push(luxforge_core::Layer {
            id: luxforge_core::LayerId::new(),
            effect_id: luxforge_core::BASIC_EFFECT.into(),
            effect_format: luxforge_core::EFFECT_FORMAT,
            payload: json!({"exposure":1.0}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        });
        let base = Evaluation::new(
            initial.registry().clone(),
            initial.context().clone(),
            initial.source().clone(),
            initial.entry().clone(),
            recipe,
            None,
        );
        editor.request_preview(PreviewJob::new(base.clone()).unwrap());
        drain_photo(&mut editor);
        editor.session.workspace.mode = luxforge_core::MASK_MODE.into();
        editor.session.workspace.mask_overlay = MaskOverlayMode::Tint;
        editor.mask_panel.selected_mask = Some(mask.id.clone());
        editor.document.masks = Some(luxforge_core::mask::commands::MaskListing {
            entry_id: base.entry().id.clone(),
            masks: vec![luxforge_core::mask::commands::MaskReport {
                id: mask.id.clone(),
                index: 0,
                name: mask.name.clone(),
                amount: mask.amount,
                invert: mask.invert,
                components: Vec::new(),
                layers: Vec::new(),
            }],
        });
        editor.reconcile_coverage_spec();
        let draft_id = luxforge_core::DraftId::new();
        let epoch = editor.coverage_worker.epoch;
        let initial_content = editor.presentation.presented_content;
        let initial_generation = editor.presentation.presented_generation;
        for revision in 1..=3 {
            let evaluation = accepted_revision(&base, &draft_id, revision);
            let content = initial_content + revision;
            let mut request = progressive_stamp(&evaluation, &mask, epoch, content);
            request.spec = editor.coverage_worker.spec.clone().unwrap();
            let next = accepted_revision(&base, &draft_id, revision + 1);
            let mut current =
                luxforge_core::Draft::new("mask.update", base.entry().asset_id.clone(), 0);
            current.draft_id = draft_id.clone();
            current.draft_revision = revision + 1;
            editor.session.draft = Some(current);
            let mut wanted = progressive_stamp(&next, &mask, epoch, content + 1);
            wanted.spec = request.spec.clone();
            editor.coverage_worker.requested = Some(wanted);
            let outcome = evaluation
                .mask_overlay_coverage(
                    &request.spec.target,
                    request.spec.cells,
                    request.spec.region,
                    None,
                    &luxforge_core::Cancel::never(),
                )
                .unwrap()
                .outcome
                .unwrap();
            let done = Completion::new(request, Arc::new(outcome));
            editor.mask_coverage_ready(done.clone());
            assert!(
                editor.coverage_worker.waiting.is_some(),
                "bound coverage must wait for its matching photograph content"
            );
            editor.presentation.content_serial = content;
            editor
                .presentation
                .request(PreviewJob::new(evaluation).unwrap(), content, false);
            drain_photo(&mut editor);
            assert_eq!(
                editor.coverage_worker.epoch, epoch,
                "new bound photo generations cannot starve in-flight coverage: {:?}",
                editor.coverage_worker.spec
            );
            assert!(editor.presentation.presented_generation > initial_generation);
            assert_eq!(editor.presentation.presented_content, content);
            assert!(editor.presentation.coverage().is_some());
            let shown = editor.mask_coverage_summary();
            assert_eq!(
                shown["adopted"]["request"]["identity"]["draft"]["draft_revision"],
                json!(revision)
            );
            assert_eq!(
                shown["adopted"]["generation"],
                json!(editor.presentation.presented_generation)
            );
            if revision == 3 {
                editor.coverage_worker.requested = Some(done.stamp.clone());
                assert!(
                    editor.coverage_worker.accepts(&done),
                    "the final accepted snapshot remains current"
                );
            }
        }
        finish(editor, catalog);
    }

    #[test]
    fn an_unbound_mask_commit_and_draft_reuse_pixels_and_adopt_logical_identity() {
        use crate::app::testing::{boot, finish};
        let (mut editor, catalog) = boot();
        let (evaluation, mask) = fixture();
        let mut job = PreviewJob::new(evaluation.clone()).unwrap();
        job.analyse = true;
        editor.request_preview(job);
        drain_photo(&mut editor);
        let version = editor.presentation.presenter.photo_version();
        let generation = editor.presentation.presented_generation;
        let report = editor
            .presentation
            .analysis
            .as_ref()
            .unwrap()
            .report
            .clone();
        let mut recipe = evaluation.recipe().clone();
        recipe.masks[0].components[0].payload = json!({"x0":0.1,"y0":0.2,"x1":0.8,"y1":0.9});
        let mut entry = evaluation.entry().clone();
        entry.id = EntryId::new();
        entry.snapshot.id = SnapshotId::new();
        entry.snapshot.recipe = recipe.clone();
        let saved = Evaluation::new(
            evaluation.registry().clone(),
            evaluation.context().clone(),
            evaluation.source().clone(),
            entry.clone(),
            recipe.clone(),
            None,
        );
        let identity = saved.identity().unwrap();
        editor.request_preview(PreviewJob::new(saved).unwrap());
        assert!(!editor.presentation.queue.is_busy());
        assert!(!editor.presentation.queue.ready());
        assert!(
            editor.presentation.reused.is_some(),
            "logical settlement is still pending"
        );
        editor.settle_reused_pixels();
        assert_eq!(editor.displayed_entry(), Some(entry.id.clone()));
        assert_eq!(editor.presentation.presented_entry, Some(entry.id));
        assert_eq!(
            editor.presentation.analysis.as_ref().unwrap().identity,
            identity
        );
        assert_eq!(
            editor.presentation.analysis.as_ref().unwrap().report,
            report
        );
        assert_eq!(editor.presentation.presenter.photo_version(), version);
        assert_eq!(editor.presentation.presented_generation, generation);
        assert!(!editor.mask_coverage_pending());

        let draft = luxforge_core::DraftStamp {
            draft_id: luxforge_core::DraftId::new(),
            draft_revision: 9,
        };
        let candidate = Evaluation::new(
            evaluation.registry().clone(),
            evaluation.context().clone(),
            evaluation.source().clone(),
            evaluation.entry().clone(),
            recipe,
            Some(draft.clone()),
        );
        editor.request_preview(PreviewJob::new(candidate).unwrap());
        editor.settle_reused_pixels();
        assert_eq!(editor.presentation.displayed_draft_id, Some(draft.draft_id));
        assert_eq!(editor.presentation.displayed_draft_revision, Some(9));
        assert_eq!(editor.presentation.presenter.photo_version(), version);
        assert!(!editor.presentation.queue.is_busy());
        assert_eq!(mask.id, evaluation.recipe().masks[0].id);
        finish(editor, catalog);
    }

    #[test]
    fn bound_mask_changes_and_render_errors_use_the_photo_queue() {
        use crate::app::testing::{boot, finish};
        let (mut editor, catalog) = boot();
        let (evaluation, mask) = fixture();
        let mut recipe = evaluation.recipe().clone();
        recipe.layers.push(luxforge_core::Layer {
            id: luxforge_core::LayerId::new(),
            effect_id: luxforge_core::BASIC_EFFECT.into(),
            effect_format: luxforge_core::EFFECT_FORMAT,
            payload: json!({"exposure":1.0}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        });
        let bound = Evaluation::new(
            evaluation.registry().clone(),
            evaluation.context().clone(),
            evaluation.source().clone(),
            evaluation.entry().clone(),
            recipe.clone(),
            None,
        );
        editor.request_preview(PreviewJob::new(bound.clone()).unwrap());
        drain_photo(&mut editor);
        let first = editor.presentation.presenter.photo_version();
        recipe.masks[0].amount = 20.0;
        let changed = Evaluation::new(
            evaluation.registry().clone(),
            evaluation.context().clone(),
            evaluation.source().clone(),
            evaluation.entry().clone(),
            recipe,
            None,
        );
        editor.request_preview(PreviewJob::new(changed.clone()).unwrap());
        assert!(editor.presentation.reused.is_none());
        drain_photo(&mut editor);
        assert!(editor.presentation.presenter.photo_version() > first);

        let shown = editor.presentation.presenter.photo_version();
        editor.presentation.render_error = Some(luxforge_core::Error::validation("test refusal"));
        editor.request_preview(PreviewJob::new(changed).unwrap());
        assert!(
            editor.presentation.reused.is_none(),
            "same pixels do not hide an outstanding refusal"
        );
        drain_photo(&mut editor);
        assert!(editor.presentation.presenter.photo_version() > shown);
        finish(editor, catalog);
    }

    #[test]
    fn source_plans_cannot_replace_newer_mask_values_on_reused_pixels() {
        use crate::app::testing::{finish, opened};
        let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
        let (parts, mask) = fixture();
        let entry = editor
            .document
            .state
            .as_ref()
            .unwrap()
            .current_entry
            .clone();
        let old = Evaluation::new(
            parts.registry().clone(),
            parts.context().clone(),
            parts.source().clone(),
            entry.clone(),
            parts.recipe().clone(),
            None,
        );
        editor.request_preview(PreviewJob::new(old.clone()).unwrap());
        drain_photo(&mut editor);
        editor.session.workspace.mode = luxforge_core::MASK_MODE.into();
        editor.session.workspace.mask_overlay = MaskOverlayMode::Tint;
        editor.mask_panel.selected_mask = Some(mask.id.clone());
        editor.reconcile_coverage_spec();
        let epoch = editor.coverage_worker.epoch;
        let mut recipe = old.recipe().clone();
        recipe.masks[0].amount = 20.0;
        let newer = Evaluation::new(
            parts.registry().clone(),
            parts.context().clone(),
            parts.source().clone(),
            entry,
            recipe,
            None,
        );
        assert_eq!(
            old.pixel_content_key().unwrap(),
            newer.pixel_content_key().unwrap()
        );
        editor.coverage_worker.latest = Some(newer.identity().unwrap());
        editor.coverage_worker.planning = Some(epoch);
        editor.mask_coverage_source_planned(
            epoch,
            Ok(Box::new(PreviewJob::new(old.clone()).unwrap())),
        );
        assert!(editor.coverage_worker.requested.is_none());
        assert!(
            !editor.coverage_worker.queue.is_busy(),
            "old unbound mask cannot overwrite new values"
        );

        editor.coverage_worker.planning = Some(epoch);
        editor.mask_coverage_source_planned(epoch, Ok(Box::new(PreviewJob::new(newer).unwrap())));
        assert!(editor.coverage_worker.requested.is_some());
        let done = luxforge_testbase::wait_for("the current mask source", || {
            editor.coverage_worker.queue.poll()
        });
        assert_eq!(done.outcome.grid.as_ref().unwrap().mask, mask.id);
        editor.mask_coverage_ready(done);
        let shown = editor.mask_coverage_summary();
        assert_eq!(shown["adopted"]["mask"], json!(mask.id));
        assert_eq!(
            shown["adopted"]["request"]["identity"],
            json!(editor.coverage_worker.latest)
        );
        editor.invalidate_mask_coverage();
        assert!(editor.mask_coverage_summary()["adopted"].is_null());
        editor.reconcile_coverage_spec();
        let epoch = editor.coverage_worker.epoch;
        editor.coverage_worker.planning = Some(epoch);
        editor.mask_coverage_source_planned(epoch, Ok(Box::new(PreviewJob::new(old).unwrap())));
        assert!(
            editor.coverage_worker.requested.is_some(),
            "baseline overlay can refresh after cancellation"
        );
        editor.coverage_worker.queue.cancel();
        finish(editor, catalog);
    }

    #[test]
    fn an_unpolled_photo_completion_prevents_logical_reuse() {
        use crate::app::testing::{boot, finish};
        let (mut editor, catalog) = boot();
        let (evaluation, _) = fixture();
        editor.request_preview(PreviewJob::new(evaluation.clone()).unwrap());
        drain_photo(&mut editor);
        let content = editor.presentation.presented_content;
        editor
            .presentation
            .request(PreviewJob::new(evaluation.clone()).unwrap(), content, false);
        luxforge_testbase::wait_until("a completed unpolled photo", || {
            editor.presentation.queue.ready()
        });
        editor.request_preview(PreviewJob::new(evaluation).unwrap());
        assert!(
            editor.presentation.reused.is_none(),
            "an old completion must not restore old entry metadata after reuse"
        );
        drain_photo(&mut editor);
        finish(editor, catalog);
    }

    #[test]
    fn a_failed_photograph_ends_deferred_coverage_and_rejects_its_late_answer() {
        use crate::app::testing::{boot, finish};
        let (mut editor, catalog) = boot();
        let (evaluation, mask) = fixture();
        editor.request_preview(PreviewJob::new(evaluation.clone()).unwrap());
        drain_photo(&mut editor);
        editor.session.workspace.mode = luxforge_core::MASK_MODE.into();
        editor.session.workspace.mask_overlay = MaskOverlayMode::Tint;
        editor.mask_panel.selected_mask = Some(mask.id.clone());
        editor.reconcile_coverage_spec();
        let mut request = stamp(&evaluation, &mask, editor.coverage_worker.epoch);
        request.spec = editor.coverage_worker.spec.clone().unwrap();
        request.content = editor.presentation.presented_content + 1;
        editor.coverage_worker.requested = Some(request.clone());
        let outcome = evaluation
            .mask_overlay_coverage(
                &request.spec.target,
                request.spec.cells,
                request.spec.region,
                None,
                &luxforge_core::Cancel::never(),
            )
            .unwrap()
            .outcome
            .unwrap();
        let done = Completion::new(request, Arc::new(outcome));
        editor.mask_coverage_ready(done.clone());
        assert!(editor.coverage_worker.waiting.is_some());
        editor.preview_failed(
            editor.presentation.preview_generation + 1,
            false,
            &EntryId::new(),
            None,
            &luxforge_core::Error::resource_limit("the photograph cannot be allocated"),
        );
        assert!(!editor.presentation.has_picture());
        assert!(!editor.mask_coverage_pending());
        editor.mask_coverage_ready(done);
        assert!(editor.coverage_worker.waiting.is_none());
        assert!(editor.mask_coverage_summary()["adopted"].is_null());
        assert!(editor.presentation.coverage().is_none());
        finish(editor, catalog);
    }
}
