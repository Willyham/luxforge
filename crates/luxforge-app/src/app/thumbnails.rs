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
//! before thumbnails existed — is kept as the thumbnails' source. While Mask mode is on screen a new
//! source asks for one job; outside it nothing runs, and entering Mask mode asks for the source
//! kept meanwhile.
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
use super::Editor;
use crate::state::masks::{MaskThumbnails, Thumbnail};
use luxforge_core::{
    ErrorKind, Evaluation, MASKS_PER_RECIPE, MaskCoverage, MaskId, PreviewIntent, PreviewJob,
    analysis::AnalysisIdentity,
    latest::{Latest, Running},
};
use serde_json::json;
use std::sync::Arc;

/// One thumbnail job: the settled stack to thumbnail every mask of.
pub(crate) type ThumbnailJob = Evaluation;

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
    evaluation: &Evaluation,
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
    pub(crate) fn request(&mut self, evaluation: Evaluation) {
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

/// The settled stack the thumbnails describe, and whether it has been asked for.
#[derive(Default)]
pub(crate) struct ThumbnailSource {
    /// The newest settled full-stack preview job's stack, under its identity.
    pub(crate) latest: Option<(AnalysisIdentity, Evaluation)>,
    /// The identity last handed to the worker, so an unchanged stack starts nothing.
    pub(crate) requested: Option<AnalysisIdentity>,
}

impl Editor {
    /// Keep `job`'s stack as the thumbnails' source when it is a settled full-stack frame. A
    /// crop's truncated input and the interactive frames of a drag are not: the first is not the
    /// photograph the masks apply to, and the second changes every 16 ms tick.
    pub(super) fn note_thumbnail_source(&mut self, job: &PreviewJob) {
        if job.layer_count.is_some() || job.intent == PreviewIntent::Interactive {
            return;
        }
        if self
            .thumbnail_source
            .latest
            .as_ref()
            .is_some_and(|(identity, _)| identity == &job.identity)
        {
            return;
        }
        self.thumbnail_source.latest = Some((job.identity.clone(), job.evaluation.clone()));
    }

    /// Ask for the thumbnails of the kept source when Mask mode shows them and they have not been
    /// asked for. Outside Mask mode nothing runs; with no photograph open the source is released.
    pub(super) fn refresh_thumbnails(&mut self) {
        let open = self.state.as_ref().map(|state| &state.asset.id);
        if self
            .thumbnail_source
            .latest
            .as_ref()
            .is_some_and(|(identity, _)| Some(&identity.asset_id) != open)
        {
            // Another photograph, or none: nothing kept for the last one describes this one.
            self.thumbnail_source = ThumbnailSource::default();
            self.thumbnail_queue.cancel();
            self.adopt_thumbnails(Vec::new());
        }
        if !self.mask_mode_active() {
            return;
        }
        let Some((identity, evaluation)) = &self.thumbnail_source.latest else {
            return;
        };
        if self.thumbnail_source.requested.as_ref() == Some(identity) {
            return;
        }
        self.thumbnail_source.requested = Some(identity.clone());
        if evaluation.recipe().masks.is_empty() {
            // Nothing to describe, and no job to start for it.
            self.thumbnail_queue.cancel();
            self.adopt_thumbnails(Vec::new());
            return;
        }
        let evaluation = evaluation.clone();
        self.thumbnail_queue.request(evaluation);
    }

    /// Take up one delivered set of thumbnails.
    pub(super) fn thumbnails_ready(&mut self, done: ThumbnailResult) {
        if !done.computed.is_empty() || !done.absent.is_empty() {
            self.event(
                "mask_thumbnails",
                json!({
                    "masks": done.masks.len(),
                    "computed": done.computed.iter().map(MaskId::as_str).collect::<Vec<_>>(),
                    "absent": done.absent.iter()
                        .map(|(mask, reason)| json!({"mask": mask.as_str(), "reason": reason}))
                        .collect::<Vec<_>>(),
                }),
            );
        }
        self.adopt_thumbnails(done.masks);
    }

    /// Hold `masks` as the panel's thumbnails, moving their version only when they differ from the
    /// ones held, so an unchanged delivery derives nothing again.
    fn adopt_thumbnails(&mut self, masks: Vec<(MaskId, Option<Thumbnail>)>) {
        if self.thumbnails.masks == masks {
            return;
        }
        self.thumbnails = MaskThumbnails {
            version: self.thumbnails.version + 1,
            masks,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::{
        AssetId, BASIC_EFFECT, Component, ComponentMode, EFFECT_FORMAT, EntryId, HistoryEntry,
        Layer, LayerId, Mask, ModuleRegistry, PreviewSource, RECIPE_FORMAT, Recipe, RenderContext,
        Snapshot, SnapshotId, SourceImage, Stage,
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

    /// Through the editor: a settled preview job's stack is kept, nothing runs outside Mask mode,
    /// entering Mask mode asks for it once, the delivered thumbnails reach each mask's row, and the
    /// same stack again asks for nothing.
    #[test]
    fn mask_mode_thumbnails_every_listed_mask_from_the_settled_stack_once() {
        use crate::app::message::{Message, PreviewMessage};
        let (mut editor, catalog) =
            crate::app::testing::opened_with_modules(crate::app::testing::descriptors(), 4);
        let (sky, face) = (linear(), radial());
        let asset = editor
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
        let job = PreviewJob::new(stack.clone()).expect("a job");
        *editor.masks = Some(luxforge_core::mask::commands::MaskListing {
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
        editor.request_preview(job.clone());
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        assert!(
            !editor.thumbnail_queue.is_busy() && editor.thumbnail_source.requested.is_none(),
            "outside Mask mode no thumbnail is filled"
        );

        editor.session.workspace.mode = luxforge_core::MASK_MODE.into();
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        assert_eq!(
            editor.thumbnail_source.requested.as_ref(),
            Some(&job.identity)
        );
        luxforge_testbase::wait_until("every listed mask's thumbnail", || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            !editor.thumbnails.masks.is_empty()
        });
        let rows = &editor.workspace.masks.masks;
        for (row, mask) in rows.iter().zip([&sky, &face]) {
            let thumbnail = row.thumbnail.as_ref().expect("every listed mask has one");
            assert_eq!(&thumbnail.cells[..], &overlay_cells(mask)[..]);
        }
        let version = editor.thumbnails.version;

        // The same settled stack again starts nothing.
        editor.request_preview(job);
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        assert!(!editor.thumbnail_queue.is_busy());
        assert_eq!(editor.thumbnails.version, version);
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
}
