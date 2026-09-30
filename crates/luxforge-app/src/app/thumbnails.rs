//! The Masks panel's coverage thumbnails' worker.
//!
//! Each mask row shows the mask's coverage over the whole photograph at the row's 28 × 19 cells.
//! A thumbnail is the preview overlay's own grid ([`luxforge_core::Evaluation::mask_coverage`]):
//! it reads no pixel of a rendered frame, only the geometry tail of the settled frame's stack and,
//! for a mask that reads pixels, the input of its first bound layer. It is filled here, on one
//! worker, and never on the UI thread or the catalog owner thread.
//!
//! **What starts one.** The stack of every settled full-stack preview job — not a truncated crop
//! input, and not the interactive frames of a drag, whose 16 ms ticks send exactly what they sent
//! before thumbnails existed — is the thumbnails' source when it holds a mask. While the Masks
//! panel is on screen — Mask mode, or a pick taken from it — a new source is handed to the worker
//! as its frame is requested; otherwise nothing runs and only the source's identity is noted, and
//! showing the panel plans the stack on screen again on an owner task, as a preview is planned, and hands that to the worker. A stack without
//! masks clears the thumbnails.
//!
//! **What it never holds.** The desktop keeps no stack between messages, only identities. An
//! evaluation holds its source, and a RAW source's developed planes hold the source worker's memory
//! gate ([`luxforge_core`]'s `PlaneGate`), so a development kept here would keep the next one — a
//! white-balance change, a history selection at other gains, another photograph — from ever
//! starting. The worker holds a stack only for the job that reads it, exactly as the preview
//! worker does.
//!
//! **What bounds it.** The worker is the preview's own primitive, [`Latest`]: one persistent
//! thread, one running job and one replaceable pending job. A job covers at most
//! [`MASKS_PER_RECIPE`] masks at [`Thumbnail::CELLS`] cells each, and the worker keeps at most one
//! thumbnail per mask of the last stack it saw. A newer source supersedes the running job, which
//! stops at its next cell row; only the newest request's result is taken up.
//!
//! **What it does not repeat.** The worker keeps each mask's grid under the key of everything that
//! grid depends on, so an unchanged mask costs its key — `O(recipe)`, no cell — and hands back the
//! very same cells, which is also what lets a row compare by identity.
use super::{Editor, message::Message, tasks};
use crate::app::Before;
use crate::state::masks::{MaskThumbnails, Thumbnail};
use iced::Task;
use luxforge_core::{
    ErrorKind, MASKS_PER_RECIPE, MaskCoverage, MaskId, PreviewIntent, PreviewJob,
    analysis::AnalysisIdentity,
    latest::{Latest, Running},
};
use serde_json::json;
use std::sync::Arc;

/// One thumbnail job: the settled stack to thumbnail every mask of. The one place the desktop
/// names the stack's type (repository rule `desktop-keeps-no-stack`): the worker holds it for the
/// job that reads it, and nothing on the desktop keeps one.
pub(crate) type ThumbnailJob = luxforge_core::Evaluation;

/// One mask's thumbnail as the worker keeps it between jobs: the key of what its grid depends on,
/// and the grid.
#[derive(Clone, Debug)]
struct Cached {
    mask: MaskId,
    key: u64,
    thumbnail: Option<Thumbnail>,
}

/// Every mask's thumbnail over one stack, in the stack's mask order.
#[derive(Clone, Debug, Default)]
pub(crate) struct ThumbnailResult {
    pub(crate) masks: Vec<(MaskId, Option<Thumbnail>)>,
    /// The masks whose grid this job filled; every other one was handed back from the worker's own
    /// cache under an unchanged key.
    pub(crate) computed: Vec<MaskId>,
    /// The masks that have no thumbnail, each with the host's reason.
    pub(crate) absent: Vec<(MaskId, String)>,
}

/// The thumbnails of every mask `evaluation` holds, reusing each one `cache` holds under the same
/// key. `cache` becomes exactly this stack's masks, so it never holds more than
/// [`MASKS_PER_RECIPE`]. `None` when `cancel` ended the job: what it filled before then is kept.
fn thumbnails(
    evaluation: &ThumbnailJob,
    cache: &mut Vec<Cached>,
    cancel: &luxforge_core::Cancel,
) -> Option<ThumbnailResult> {
    let mut result = ThumbnailResult::default();
    let mut next = Vec::new();
    for mask in evaluation.recipe().masks.iter().take(MASKS_PER_RECIPE) {
        let held = cache.iter().find(|cached| cached.mask == mask.id);
        match evaluation.mask_coverage(
            &mask.id,
            Thumbnail::CELLS,
            held.map(|cached| cached.key),
            cancel,
        ) {
            Ok(MaskCoverage { key, outcome: None }) => {
                let thumbnail = held.and_then(|cached| cached.thumbnail.clone());
                result.masks.push((mask.id.clone(), thumbnail.clone()));
                next.push(Cached {
                    mask: mask.id.clone(),
                    key,
                    thumbnail,
                });
            }
            Ok(MaskCoverage {
                key,
                outcome: Some(outcome),
            }) => {
                let thumbnail = outcome.grid.map(|grid| Thumbnail {
                    cells: Arc::from(grid.coverage),
                    width: grid.cells_w,
                    height: grid.cells_h,
                });
                if let Some(reason) = outcome.absent {
                    result.absent.push((mask.id.clone(), reason));
                }
                result.computed.push(mask.id.clone());
                result.masks.push((mask.id.clone(), thumbnail.clone()));
                next.push(Cached {
                    mask: mask.id.clone(),
                    key,
                    thumbnail,
                });
            }
            Err(error) if error.kind == ErrorKind::Cancelled => {
                // What was filled is still right for its key; the newer job starts from it.
                for filled in next {
                    match cache.iter().position(|cached| cached.mask == filled.mask) {
                        Some(index) => cache[index] = filled,
                        None if cache.len() < MASKS_PER_RECIPE => cache.push(filled),
                        None => {}
                    }
                }
                return None;
            }
            // A mask this stack cannot compile has no thumbnail, and nothing is kept for it, so it
            // is asked again with the next stack rather than trusted under a key.
            Err(error) => {
                result.absent.push((mask.id.clone(), error.detail));
                result.masks.push((mask.id.clone(), None));
            }
        }
    }
    *cache = next;
    Some(result)
}

/// The thumbnail worker. Only the newest request's result is taken up.
pub(crate) struct ThumbnailQueue {
    worker: Latest<ThumbnailJob, ThumbnailResult>,
    newest: u64,
}

impl Default for ThumbnailQueue {
    fn default() -> Self {
        let mut cache = Vec::new();
        Self {
            worker: Latest::new(
                "luxforge-mask-thumbnails",
                move |job: ThumbnailJob, running: &Running<'_, _, _>| {
                    thumbnails(&job, &mut cache, running.superseded())
                },
            ),
            newest: 0,
        }
    }
}

impl ThumbnailQueue {
    /// Install the waker every finished job posts: the preview's own channel.
    pub(crate) fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.worker.set_waker(waker);
    }

    /// Ask for every mask's thumbnail over `evaluation`, superseding any job before it.
    pub(crate) fn request(&mut self, evaluation: ThumbnailJob) {
        self.newest = self.worker.request(evaluation).generation;
    }

    /// Forget every outstanding job.
    pub(crate) fn cancel(&mut self) {
        self.newest = self.worker.cancel();
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.worker.is_busy()
    }

    pub(crate) fn ready(&self) -> bool {
        self.worker.ready()
    }

    /// The newest request's thumbnails, or nothing. A superseded job's result is dropped: it
    /// describes a stack that is no longer the one on screen.
    pub(crate) fn poll(&mut self) -> Option<ThumbnailResult> {
        while let Some((generation, result)) = self.worker.poll() {
            if generation == self.newest {
                return Some(result);
            }
        }
        None
    }
}

/// Which settled stack the thumbnails should describe, by identity alone, and which one the worker
/// was asked for. No stack is kept here: an evaluation holds its source, a RAW source's developed
/// planes hold the source worker's memory gate, and a development the desktop kept between messages
/// would keep the next one from starting.
#[derive(Default)]
pub(crate) struct ThumbnailSource {
    /// The newest settled full-stack preview job's identity, when its stack holds a mask.
    pub(crate) latest: Option<AnalysisIdentity>,
    /// The identity last handed to the worker, so an unchanged stack starts nothing.
    pub(crate) requested: Option<AnalysisIdentity>,
    /// The identity whose stack an owner task is planning again, because the Masks panel was shown
    /// after it settled; so it is planned once.
    pub(crate) planning: Option<AnalysisIdentity>,
}

/// The thumbnail worker's side of the Masks panel: its one active and one replaceable pending job,
/// off the UI thread and the owner thread, and the settled stack they describe. What it delivers
/// is the panel's ([`crate::state::masks::MaskPanel::thumbnails`]).
#[derive(Default)]
pub(crate) struct Thumbnailer {
    pub(crate) queue: ThumbnailQueue,
    pub(crate) source: ThumbnailSource,
}

impl Editor {
    /// Take note of `job`'s stack when it is a settled full-stack frame, and hand it to the
    /// worker at once while the Masks panel shows the thumbnails. A crop's truncated input and the
    /// interactive frames of a drag are not noted: the first is not the photograph the masks apply
    /// to, and the second changes every 16 ms tick. Without the panel only the identity is noted.
    pub(super) fn note_thumbnail_source(&mut self, job: &PreviewJob) {
        if job.layer_count.is_some() || job.intent == PreviewIntent::Interactive {
            return;
        }
        // A stack without masks has no thumbnail to describe.
        if job.evaluation.recipe().masks.is_empty() {
            self.thumbnailer.source = ThumbnailSource::default();
            self.thumbnailer.queue.cancel();
            self.adopt_thumbnails(Vec::new());
            return;
        }
        if self.thumbnailer.source.latest.as_ref() == Some(&job.identity) {
            return;
        }
        self.thumbnailer.source.latest = Some(job.identity.clone());
        if self.mask_panel_shown() {
            self.thumbnailer.source.requested = Some(job.identity.clone());
            // The worker's clone lives as long as its job, as the preview worker's does.
            self.thumbnailer.queue.request(job.evaluation.clone());
        }
    }

    /// When the Masks panel shows the thumbnails and the stack on screen settled while it did not, plan
    /// that stack again on an owner task, as a preview is planned, and hand it to the worker when
    /// it arrives ([`Self::thumbnail_source_planned`]). Without the panel nothing runs; with
    /// another photograph open, or none, the last one's thumbnails are dropped.
    pub(super) fn refresh_thumbnails(&mut self) -> Task<Message> {
        let open = self.document.state.as_ref().map(|state| &state.asset.id);
        if self
            .thumbnailer
            .source
            .latest
            .as_ref()
            .is_some_and(|identity| Some(&identity.asset_id) != open)
        {
            // Another photograph, or none: nothing noted for the last one describes this one.
            self.thumbnailer.source = ThumbnailSource::default();
            self.thumbnailer.queue.cancel();
            self.adopt_thumbnails(Vec::new());
        }
        if !self.mask_panel_shown() {
            return Task::none();
        }
        let source = &self.thumbnailer.source;
        let Some(identity) = source.latest.clone() else {
            return Task::none();
        };
        if source.requested.as_ref() == Some(&identity)
            || source.planning.as_ref() == Some(&identity)
        {
            return Task::none();
        }
        self.thumbnailer.source.planning = Some(identity.clone());
        tasks::thumbnail_source_task(
            self.owner.clone(),
            self.client,
            identity.asset_id,
            identity.entry_id,
        )
    }

    /// The stack [`Self::refresh_thumbnails`] planned again: handed to the worker when the Masks
    /// panel still shows the thumbnails and it is still the stack on screen, and dropped otherwise. It is
    /// never kept.
    pub(super) fn thumbnail_source_planned(&mut self, planned: Result<Box<PreviewJob>, String>) {
        let Some(identity) = self.thumbnailer.source.planning.take() else {
            return;
        };
        if !self.mask_panel_shown() || self.thumbnailer.source.latest.as_ref() != Some(&identity) {
            return;
        }
        // Asked for either way, so a failed plan is not planned again until the stack changes.
        self.thumbnailer.source.requested = Some(identity.clone());
        match planned {
            Ok(job) if job.identity == identity => self.thumbnailer.queue.request(job.evaluation),
            Ok(job) => self.event(
                "mask_thumbnails_unavailable",
                || json!({"reason": "the stack planned again is not the settled one", "entry": job.evaluation.entry().id}),
            ),
            Err(reason) => self.event("mask_thumbnails_unavailable", || json!({ "reason": reason })),
        }
    }

    /// Take up one delivered set of thumbnails.
    pub(super) fn thumbnails_ready(&mut self, done: ThumbnailResult) {
        if !done.computed.is_empty() || !done.absent.is_empty() {
            self.event("mask_thumbnails", || {
                json!({
                    "masks": done.masks.len(),
                    "computed": done.computed.iter().map(MaskId::as_str).collect::<Vec<_>>(),
                    "absent": done.absent.iter()
                        .map(|(mask, reason)| json!({"mask": mask.as_str(), "reason": reason}))
                        .collect::<Vec<_>>(),
                })
            });
        }
        self.adopt_thumbnails(done.masks);
    }

    /// Hold `masks` as the panel's thumbnails.
    fn adopt_thumbnails(&mut self, masks: Vec<(MaskId, Option<Thumbnail>)>) {
        self.mask_panel.thumbnails = MaskThumbnails { masks };
    }
}

/// After every message: the Masks panel's thumbnails follow the settled stack
/// ([`Editor::refresh_thumbnails`]).
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    editor.refresh_thumbnails()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::fresh_stack;
    use luxforge_core::{
        AssetId, BASIC_EFFECT, Component, ComponentMode, EFFECT_FORMAT, EntryId, Evaluation,
        HistoryEntry, Layer, LayerId, Mask, ModuleRegistry, PreviewSource, RECIPE_FORMAT, Recipe,
        RenderContext, Snapshot, SnapshotId, SourceImage, Stage,
        analysis::{MaskPixels, coverage_grid},
        mask::CompiledMask,
    };
    use serde_json::{Value, json};

    fn mask(name: &str, kind: &str, payload: Value) -> Mask {
        let mut mask = Mask::new(name);
        let component = mask.next_component_name(kind);
        mask.components
            .push(Component::new(component, ComponentMode::Add, kind, payload));
        mask
    }

    fn linear() -> Mask {
        mask(
            "Sky",
            "linear",
            json!({"x0": 0.5, "y0": 0.1, "x1": 0.5, "y1": 0.6}),
        )
    }

    fn radial() -> Mask {
        mask(
            "Face",
            "radial",
            json!({"x": 0.4, "y": 0.55, "radius_x": 0.3, "radius_y": 0.2, "angle": 0.0, "feather": 0.5}),
        )
    }

    fn range() -> Mask {
        mask(
            "Shadows",
            "luminance-range",
            json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
        )
    }

    fn bound(mask: &Mask, exposure: f64) -> Layer {
        Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": exposure}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        }
    }

    /// A 60 × 40 stack holding `masks`, with `layers`.
    fn evaluation(layers: Vec<Layer>, masks: Vec<Mask>) -> Evaluation {
        let (width, height) = (60u32, 40u32);
        let asset = AssetId::new();
        let recipe = Recipe {
            format: RECIPE_FORMAT,
            layers,
            masks,
            ..Recipe::default()
        };
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            let value = (index % width as usize * 255 / width as usize) as u8;
            pixel.copy_from_slice(&[value, value, value, 255]);
        }
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.clone(),
            sequence: 1,
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
        Evaluation::new(
            Arc::new(ModuleRegistry::builtin()),
            RenderContext::new(),
            PreviewSource::Jpeg(SourceImage {
                width,
                height,
                rgba: rgba.into(),
                fingerprint: "sha256:thumbnails".into(),
                orientation: 1,
                capture: Default::default(),
            }),
            entry,
            recipe,
            None,
        )
    }

    /// The overlay grid of a position-only mask over an identity stack, quantized exactly as the
    /// overlay quantizes it.
    fn overlay_cells(mask: &Mask) -> Vec<u8> {
        let stage = Stage {
            width: 60,
            height: 40,
        };
        let compiled = CompiledMask::new(mask, stage, &Default::default()).unwrap();
        let transform = luxforge_core::StageTransform {
            content: luxforge_core::StageSize {
                width: 60,
                height: 40,
            },
            output: luxforge_core::StageSize {
                width: 60,
                height: 40,
            },
            forward: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        };
        coverage_grid(
            &compiled,
            &transform,
            Thumbnail::CELLS.0,
            Thumbnail::CELLS.1,
            MaskPixels::Unavailable("position-only"),
            &luxforge_core::Cancel::never(),
        )
        .unwrap()
        .unwrap()
    }

    fn thumbnail<'a>(result: &'a ThumbnailResult, mask: &Mask) -> Option<&'a Thumbnail> {
        result
            .masks
            .iter()
            .find(|(id, _)| id == &mask.id)
            .and_then(|(_, thumbnail)| thumbnail.as_ref())
    }

    /// Every mask gets a thumbnail — not only an open one — and its cells are the quantized
    /// coverage the overlay's grid gives at those cells, for a radial and a linear alike.
    #[test]
    fn every_mask_thumbnail_is_the_overlay_grid_reduced_to_its_cells() {
        let (sky, face) = (linear(), radial());
        let stack = evaluation(
            vec![bound(&sky, 0.5), bound(&face, 0.3)],
            vec![sky.clone(), face.clone()],
        );
        let mut cache = Vec::new();
        let result = thumbnails(&stack, &mut cache, &luxforge_core::Cancel::never()).unwrap();
        assert_eq!(result.masks.len(), 2);
        for mask in [&sky, &face] {
            let thumbnail = thumbnail(&result, mask).expect("a geometric mask has a thumbnail");
            assert_eq!((thumbnail.width, thumbnail.height), (28, 19));
            assert_eq!(&thumbnail.cells[..], &overlay_cells(mask)[..]);
        }
        assert_eq!(result.computed.len(), 2);
        assert!(cache.len() <= MASKS_PER_RECIPE);
    }

    /// An unchanged mask is not filled again, and hands back the very same cells; a changed mask
    /// is filled again; a mask that left the stack leaves the worker's cache.
    #[test]
    fn an_unchanged_mask_is_not_recomputed_and_a_changed_one_is() {
        let (sky, face) = (linear(), radial());
        let mut cache = Vec::new();
        let never = luxforge_core::Cancel::never();
        let first = thumbnails(
            &evaluation(
                vec![bound(&sky, 0.5), bound(&face, 0.3)],
                vec![sky.clone(), face.clone()],
            ),
            &mut cache,
            &never,
        )
        .unwrap();
        // A new exposure on both layers: new frames, the same masks.
        let second = thumbnails(
            &evaluation(
                vec![bound(&sky, -1.0), bound(&face, 1.0)],
                vec![sky.clone(), face.clone()],
            ),
            &mut cache,
            &never,
        )
        .unwrap();
        assert!(
            second.computed.is_empty(),
            "nothing a mask depends on moved"
        );
        for mask in [&sky, &face] {
            assert_eq!(thumbnail(&first, mask), thumbnail(&second, mask));
            assert!(Arc::ptr_eq(
                &thumbnail(&first, mask).unwrap().cells,
                &thumbnail(&second, mask).unwrap().cells
            ));
        }
        // The face moves: only it is filled again.
        let mut moved = face.clone();
        moved.components[0].payload = json!({"x": 0.6, "y": 0.45, "radius_x": 0.3, "radius_y": 0.2, "angle": 0.0, "feather": 0.5});
        let third = thumbnails(
            &evaluation(
                vec![bound(&sky, -1.0), bound(&moved, 1.0)],
                vec![sky.clone(), moved.clone()],
            ),
            &mut cache,
            &never,
        )
        .unwrap();
        assert_eq!(third.computed, vec![face.id.clone()]);
        assert_ne!(thumbnail(&third, &moved), thumbnail(&first, &face));
        assert_eq!(
            &thumbnail(&third, &moved).unwrap().cells[..],
            &overlay_cells(&moved)[..]
        );
        // The sky is deleted: the cache keeps only what the stack holds.
        thumbnails(
            &evaluation(vec![bound(&moved, 1.0)], vec![moved.clone()]),
            &mut cache,
            &never,
        )
        .unwrap();
        assert_eq!(cache.len(), 1);
    }

    /// A mask that reads pixels has a thumbnail when a layer bound to it gives it an operation's
    /// input, and none — with the host's reason — when nothing is bound to it.
    #[test]
    fn a_value_based_mask_without_an_input_has_no_thumbnail() {
        let shadows = range();
        let never = luxforge_core::Cancel::never();
        let unbound = thumbnails(
            &evaluation(Vec::new(), vec![shadows.clone()]),
            &mut Vec::new(),
            &never,
        )
        .unwrap();
        assert_eq!(thumbnail(&unbound, &shadows), None);
        assert_eq!(unbound.absent.len(), 1);
        let bound = thumbnails(
            &evaluation(vec![bound(&shadows, 0.5)], vec![shadows.clone()]),
            &mut Vec::new(),
            &never,
        )
        .unwrap();
        let cells = &thumbnail(&bound, &shadows)
            .expect("the input is read")
            .cells;
        assert_eq!(cells.len(), 28 * 19);
        assert!(bound.absent.is_empty());
    }

    /// A cancelled job delivers nothing, and what it filled first is kept for the job after it.
    #[test]
    fn a_cancelled_job_delivers_nothing() {
        let sky = linear();
        let cancel = luxforge_core::Cancel::new();
        cancel.cancel();
        let mut cache = Vec::new();
        assert!(
            thumbnails(
                &evaluation(vec![bound(&sky, 0.5)], vec![sky.clone()]),
                &mut cache,
                &cancel
            )
            .is_none()
        );
    }

    /// Through the editor: outside Mask mode a settled stack starts nothing and only its identity
    /// is noted; entering Mask mode plans that stack again once, and its delivered thumbnails reach
    /// each mask's row; in Mask mode a new settled stack is handed to the worker as its frame is
    /// requested; the same stack again asks for nothing; and a stack without masks clears the
    /// thumbnails. At no point does the desktop hold a stack's pixels once the workers are done
    /// with them: a kept RAW development would hold the source worker's memory gate.
    #[test]
    fn mask_mode_thumbnails_every_listed_mask_from_the_settled_stack_once() {
        use crate::app::message::{Message, preview::PreviewMessage};
        let (mut editor, catalog) =
            crate::app::testing::opened_with_modules(crate::app::testing::descriptors(), 4);
        let (sky, face) = (linear(), radial());
        let asset = editor
            .document
            .state
            .as_ref()
            .expect("an open asset")
            .asset
            .id
            .clone();
        let stack = evaluation(
            vec![bound(&sky, 0.5), bound(&face, 0.3)],
            vec![sky.clone(), face.clone()],
        );
        // The same stack, as an entry of the open photograph.
        let mut entry = stack.entry().clone();
        entry.asset_id = asset.clone();
        entry.snapshot.asset_id = asset;
        let stack = Evaluation::new(
            stack.registry().clone(),
            stack.context().clone(),
            stack.source().clone(),
            entry,
            stack.recipe().clone(),
            None,
        );
        editor.document.masks = Some(luxforge_core::mask::commands::MaskListing {
            entry_id: editor.displayed_entry().expect("a displayed entry"),
            masks: [&sky, &face]
                .iter()
                .enumerate()
                .map(|(index, mask)| luxforge_core::mask::commands::MaskReport {
                    id: mask.id.clone(),
                    index,
                    name: mask.name.clone(),
                    amount: 100.0,
                    invert: false,
                    components: Vec::new(),
                    layers: Vec::new(),
                })
                .collect(),
        });
        let idle = |editor: &mut Editor| {
            luxforge_testbase::wait_until("the preview and thumbnail workers", || {
                let _ = editor.update(Message::Preview(PreviewMessage::Poll));
                !editor.presentation.queue.is_busy() && !editor.thumbnailer.queue.is_busy()
            });
        };
        let identity = PreviewJob::new(stack.clone()).expect("a job").identity;

        // Outside Mask mode: nothing is filled, and nothing of the stack outlives its frame.
        let (settled, pixels) = fresh_stack(&stack);
        editor.request_preview(PreviewJob::new(settled).expect("a job"));
        assert!(
            !editor.thumbnailer.queue.is_busy(),
            "outside Mask mode no thumbnail is filled"
        );
        idle(&mut editor);
        assert_eq!(editor.thumbnailer.source.latest.as_ref(), Some(&identity));
        assert!(editor.thumbnailer.source.requested.is_none());
        assert_eq!(
            pixels.strong_count(),
            0,
            "the desktop keeps no stack once its frame is delivered"
        );

        // Entering Mask mode plans the stack on screen again, once.
        editor.session.workspace.mode = luxforge_core::MASK_MODE.into();
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        assert_eq!(editor.thumbnailer.source.planning.as_ref(), Some(&identity));
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        assert_eq!(
            editor.thumbnailer.source.planning.as_ref(),
            Some(&identity),
            "planned once"
        );
        assert!(!editor.thumbnailer.queue.is_busy());
        // The owner task's answer: the same stack, planned again.
        let (planned, pixels) = fresh_stack(&stack);
        let _ = editor.update(Message::Preview(PreviewMessage::ThumbnailSource(Ok(
            Box::new(PreviewJob::new(planned).expect("a job")),
        ))));
        assert_eq!(
            editor.thumbnailer.source.requested.as_ref(),
            Some(&identity)
        );
        assert!(editor.thumbnailer.source.planning.is_none());
        luxforge_testbase::wait_until("every listed mask's thumbnail", || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            !editor.mask_panel.thumbnails.masks.is_empty()
        });
        let rows = &editor.workspace.masks.masks;
        for (row, mask) in rows.iter().zip([&sky, &face]) {
            let thumbnail = row.thumbnail.as_ref().expect("every listed mask has one");
            assert_eq!(&thumbnail.cells[..], &overlay_cells(mask)[..]);
        }
        idle(&mut editor);
        assert_eq!(
            pixels.strong_count(),
            0,
            "the worker releases the stack with its job"
        );
        let held = editor.mask_panel.thumbnails.masks.clone();

        // The same settled stack again starts nothing.
        let (again, _) = fresh_stack(&stack);
        editor.request_preview(PreviewJob::new(again).expect("a job"));
        assert!(!editor.thumbnailer.queue.is_busy());
        idle(&mut editor);
        assert_eq!(editor.mask_panel.thumbnails.masks, held);

        // In Mask mode a new settled stack goes to the worker as its frame is requested, and is
        // released with the job that reads it.
        let brighter = Evaluation::new(
            stack.registry().clone(),
            stack.context().clone(),
            stack.source().clone(),
            stack.entry().clone(),
            Recipe {
                layers: vec![bound(&sky, 1.5), bound(&face, 0.3)],
                ..stack.recipe().clone()
            },
            None,
        );
        let (brighter, pixels) = fresh_stack(&brighter);
        let job = PreviewJob::new(brighter).expect("a job");
        let brighter = job.identity.clone();
        editor.request_preview(job);
        assert_eq!(
            editor.thumbnailer.source.requested.as_ref(),
            Some(&brighter)
        );
        assert!(
            editor.thumbnailer.source.planning.is_none(),
            "nothing planned"
        );
        idle(&mut editor);
        assert_eq!(pixels.strong_count(), 0, "nothing of it is kept");

        // A settled stack without masks clears the thumbnails of the stack before it.
        let bare = Evaluation::new(
            stack.registry().clone(),
            stack.context().clone(),
            stack.source().clone(),
            stack.entry().clone(),
            Recipe {
                format: RECIPE_FORMAT,
                ..Recipe::default()
            },
            None,
        );
        editor.request_preview(PreviewJob::new(bare).expect("a job"));
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        assert!(editor.thumbnailer.source.latest.is_none());
        assert!(!editor.thumbnailer.queue.is_busy());
        assert!(editor.mask_panel.thumbnails.masks.is_empty());
        crate::app::testing::finish(editor, catalog);
    }

    /// Only the newest request's result is taken up: a result for a stack a newer request replaced
    /// never reaches the panel.
    #[test]
    fn a_superseded_request_is_dropped() {
        let sky = linear();
        let mut queue = ThumbnailQueue::default();
        let old = evaluation(vec![bound(&sky, 0.5)], vec![sky.clone()]);
        let mut newer = sky.clone();
        newer.components[0].payload = json!({"x0": 0.5, "y0": 0.3, "x1": 0.5, "y1": 0.9});
        let new = evaluation(vec![bound(&newer, 0.5)], vec![newer.clone()]);
        queue.request(old);
        queue.request(new);
        let delivered =
            luxforge_testbase::wait_for("the newer request's thumbnails", || queue.poll());
        assert_eq!(
            &thumbnail(&delivered, &newer).unwrap().cells[..],
            &overlay_cells(&newer)[..],
            "the delivered result is the newer stack's"
        );
        // Nothing older arrives after it.
        luxforge_testbase::wait_until("the queue to go idle", || {
            assert!(queue.poll().is_none());
            !queue.is_busy()
        });
        assert!(queue.poll().is_none());
    }

    /// With the real owner and a real RAW whose recipe holds a mask, while the Masks panel shows
    /// its thumbnails: each white-balance change, each history selection at other gains and
    /// opening another photograph prepares its development within a bounded wait. Each step runs
    /// the desktop's own refresh — which plans the preview and blocks on the source job it needs —
    /// on a thread of its own, so a development that waits on planes the desktop holds fails the
    /// test instead of hanging it.
    ///
    /// `LUXFORGE_RAW_FIXTURE=/path/to/file.NEF cargo test --release -p luxforge-app --lib \
    ///   a_raw_with_a_mask_develops_again -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a private RAW fixture: set LUXFORGE_RAW_FIXTURE"]
    fn a_raw_with_a_mask_develops_again_while_its_thumbnails_are_shown() {
        use crate::app::{
            message::{Message, preview::PreviewMessage, sync::SyncMessage},
            tasks::{self, Refresh, Scope},
        };
        use std::time::{Duration, Instant};
        /// Far above a release development of any supported camera, far below a hang.
        const DEADLINE: Duration = Duration::from_secs(60);
        let raw = std::path::PathBuf::from(
            std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE"),
        );
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-thumbnail-gate-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let (mut editor, asset, _) = crate::app::testing::real_photo_at(&catalog, &raw);
        let owner = editor.owner.clone();
        let client = editor.client;
        let bounded = |what: &str, work: Box<dyn FnOnce() -> Result<Refresh, String> + Send>| {
            let started = Instant::now();
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = sender.send(work());
            });
            let refresh = receiver
                .recv_timeout(DEADLINE)
                .unwrap_or_else(|_| {
                    panic!("{what}: the development did not finish in {DEADLINE:?}")
                })
                .unwrap_or_else(|error| panic!("{what}: {error}"));
            eprintln!("{what}: {:?}", started.elapsed());
            refresh
        };
        let read = |what: &str, scope: Scope| {
            let (owner, asset) = (owner.clone(), asset.clone());
            bounded(
                what,
                Box::new(move || tasks::refresh(&owner, client, asset, scope, None)),
            )
        };
        let settle = |editor: &mut Editor, refresh: Refresh| {
            let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
            luxforge_testbase::wait_until("the frame and its thumbnails", || {
                let _ = editor.update(Message::Preview(PreviewMessage::Poll));
                !editor.presentation.queue.is_busy() && !editor.thumbnailer.queue.is_busy()
            });
        };
        let commit = |editor: &Editor, method: &str, params: Value| {
            let revision = editor
                .document
                .state
                .as_ref()
                .expect("an open photo")
                .revision;
            let mut params = params;
            params["asset_id"] = json!(asset);
            params["mutation"] = serde_json::to_value(tasks::mutation(revision)).unwrap();
            let (answer, _) = tasks::call(&owner, client, method, params).expect(method);
            Scope::Commit(answer["revision"].as_u64().expect("a revision"))
        };
        // Whether showing `entry` (the current one when `None`) needs a development first.
        let readiness = |entry: Option<&luxforge_core::EntryId>| {
            let (inspected, _) = tasks::call(
                &owner,
                client,
                "source.inspect",
                json!({"asset_id": asset, "entry_id": entry}),
            )
            .expect("source.inspect");
            inspected["readiness"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        };

        let scope = commit(
            &editor,
            "mask.create-linear",
            json!({"x0": 0.5, "y0": 0.1, "x1": 0.5, "y1": 0.6}),
        );
        let refresh = read("the mask", scope);
        settle(&mut editor, refresh);
        // Mask mode is the session's, so every refresh after this reads it back.
        let (session, _) = tasks::call(
            &owner,
            client,
            "workspace.set",
            json!({"mode": luxforge_core::MASK_MODE}),
        )
        .expect("Mask mode");
        let _ = editor.update(Message::View(
            crate::app::message::view::ViewMessage::WorkspaceUpdated(Ok(serde_json::from_value(
                session,
            )
            .unwrap())),
        ));
        assert!(editor.mask_mode_active());
        // Entering Mask mode planned the stack on screen again: the owner task's plain call.
        let planning = editor
            .thumbnailer
            .source
            .planning
            .clone()
            .expect("the stack on screen is planned again");
        let planned = tasks::thumbnail_source(&owner, client, asset.clone(), planning.entry_id);
        let _ = editor.update(Message::Preview(PreviewMessage::ThumbnailSource(
            planned.map(Box::new),
        )));
        luxforge_testbase::wait_until("the mask's thumbnail", || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            editor.mask_panel.thumbnails.masks.len() == 1
        });

        let mut entries = Vec::new();
        for temperature in [3500.0, 6500.0, 4200.0] {
            let scope = commit(&editor, "edit.set-raw", json!({"temperature": temperature}));
            assert_eq!(
                readiness(None),
                "development-required",
                "{temperature} K develops the mosaic again"
            );
            let refresh = read(&format!("{temperature} K"), scope);
            settle(&mut editor, refresh);
            assert_eq!(
                editor.mask_panel.thumbnails.masks.len(),
                1,
                "the thumbnail follows"
            );
            entries.push(
                editor
                    .document
                    .state
                    .as_ref()
                    .unwrap()
                    .current_entry
                    .id
                    .clone(),
            );
        }
        // History selections at other gains, as the history panel makes them: the Original at the
        // camera's gains, two earlier white balances neither development holds, and current again.
        // The Original holds no mask, so it has no thumbnail.
        let original = editor
            .document
            .original_entry
            .clone()
            .expect("the Original");
        for (what, entry, thumbnails) in [
            ("the Original", Some(&original), 0),
            ("3500 K again", Some(&entries[0]), 1),
            ("current", None, 1),
            ("6500 K again", Some(&entries[1]), 1),
            ("the Original again", Some(&original), 0),
            ("current again", None, 1),
        ] {
            let (method, params) = match entry {
                Some(entry) => (
                    "preview.select",
                    json!({"asset_id": asset, "entry_id": entry}),
                ),
                None => ("preview.return-current", json!({})),
            };
            eprintln!("{what}: {}", readiness(entry));
            tasks::call(&owner, client, method, params).expect("a selection");
            let refresh = read(what, Scope::Elsewhere);
            settle(&mut editor, refresh);
            assert_eq!(
                editor.mask_panel.thumbnails.masks.len(),
                thumbnails,
                "{what}"
            );
        }
        // Another photograph: its original's preparation retains no development of this one.
        let jpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg");
        let (queued, _) = tasks::call(
            &owner,
            client,
            "catalog.import",
            json!({"path": jpeg, "mutation": tasks::request()}),
        )
        .expect("an import");
        let job = queued["job_id"].as_str().expect("a source job").to_owned();
        let (import_owner, started) = (owner.clone(), Instant::now());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(tasks::wait_source_job(&import_owner, client, &job));
        });
        receiver
            .recv_timeout(DEADLINE)
            .expect("another photograph: its preparation did not finish")
            .expect("another photograph prepares");
        eprintln!("another photograph: {:?}", started.elapsed());
        crate::app::testing::finish(editor, catalog);
    }
}
