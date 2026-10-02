//! A gesture drawn on the GPU (`docs/design/gpu-preview.md`, "The held input boundary" and "A
//! tick"): the desktop's half, between the owner's plan and the photo surface's GPU stage.
//!
//! - **The plan rides the tick's own answer.** Each `draft.set` is answered with its preview job,
//!   synchronously on this thread ([`super::tasks::draft_set_now`]), and the catalog owner plans
//!   the draft's GPU preview with that job (its `gpu` field): the plan in `O(layers)`, or its
//!   reason, and the boundary it starts from. It is preview state in the desktop's typed owner
//!   reply, never an API result, and a tick adds no hop for it (performance rule 12).
//! - **The boundary job.** While the draft's boundary is not held, each tick takes the CPU path as
//!   today and its preview job carries the boundary request, so the preview worker renders the
//!   boundary once after that job's Fit frame. A request in flight on the active job is not asked
//!   again; one still pending rides the job that replaces it. The boundary arrives as one more
//!   result of that job and is held here, its texels shared with the surface, which uploads them
//!   once.
//! - **A tick on the GPU.** With the boundary held and the plan converted, the surface draws the
//!   plan in the frame after the update that handled the input. Once the surface reports that it
//!   evaluated this boundary's plan with no fallback — its pipeline is ready and its slot holds the
//!   boundary — a tick makes no preview job and no upload: the converted plan's words are all it
//!   changes. Until then, and whenever the surface falls back, the tick's job goes to the worker as
//!   today, so a gesture never waits on the GPU.
//! - **Settlement.** The shared quiet policy and the release settle on the CPU as before. When the
//!   CPU frame of the drawn revision is presented, the surface holds the plan behind it: the CPU
//!   frame is the reference, and the next tick draws again with no upload.
//! - **Lifetime.** The boundary is held for the open draft only. A tick whose plan names another
//!   key releases it; so does the draft's end — commit or cancel — once the frame that replaces the
//!   drafted one is presented, so the screen never falls back to an older drafted frame.
//! - **Warming.** A committed stack's preview job carries the plans its gestures are likely to draw
//!   (its `gpu_warm` field), and the surface compiles their sequences before a drag begins.
use super::{Editor, gpu_plan};
use luxforge_core::{
    BoundaryKey, BoundaryRequest, CoordinateGrid, Draft, DraftId, GpuAnswer, GpuPreview,
};
use luxforge_ui::photo_surface::{
    self as surface, DrawingPath, GpuBoundary, GpuStep, GpuWarm, SurfaceDiagnostics,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The core's plan, beside the surface's plain data of the same name.
type CorePlan = luxforge_core::GpuPlan;
/// The surface's fallback, beside the core's reason of the same name.
type SurfaceFallback = surface::GpuFallback;

/// What the surface reports of its last frame that decides a tick's path: the boundary of the
/// plan it last evaluated, drawn or held, and why it fell back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SurfaceReport {
    pub(crate) ready_boundary: Option<u64>,
    pub(crate) fallback: Option<SurfaceFallback>,
    /// The boundary and draft revision whose GPU output the last frame drew.
    pub(crate) drawn: Option<(u64, u64)>,
}

impl SurfaceReport {
    fn of(diagnostics: &SurfaceDiagnostics) -> Self {
        Self {
            ready_boundary: diagnostics.gpu_ready_boundary,
            fallback: diagnostics.gpu_fallback,
            drawn: (diagnostics.drawn_path == Some(DrawingPath::Gpu))
                .then(|| {
                    diagnostics
                        .drawn_gpu_boundary
                        .zip(diagnostics.drawn_gpu_tag)
                })
                .flatten(),
        }
    }
}

/// The boundary held for the open draft: its key, its texels as the surface uploads them, and
/// where they lie in the plan's boundary stage.
struct Held {
    key: BoundaryKey,
    boundary: GpuBoundary,
    origin: (u32, u32),
    /// A warp tail's coordinate grid, computed with the boundary; `None` for an affine tail, or a
    /// warp whose grid could not be built, which then keeps the CPU path.
    grid: Option<Arc<CoordinateGrid>>,
}

/// The open draft's GPU preview.
struct Drag {
    draft: DraftId,
    /// The latest tick's plan and the draft revision it was planned at.
    plan: Option<(Box<CorePlan>, u64)>,
    /// The boundary that plan starts from.
    wanted: Option<BoundaryRequest>,
    held: Option<Held>,
    /// The generation of the job carrying the boundary request, while it is in flight.
    requested: Option<u64>,
    /// A boundary of this key could not be rendered; the draft keeps the CPU path for it.
    failed: Option<BoundaryKey>,
    /// The plan the surface draws, converted, and its draft revision.
    surface: Option<(surface::GpuPlan, u64)>,
    /// Why the latest tick took the CPU path.
    reason: Option<String>,
    /// The presented generation when the draft ended; the drag is released once a newer frame is
    /// presented, or nothing more is coming.
    ended: Option<u64>,
    gpu_ticks: u64,
    cpu_ticks: u64,
    boundary_requests: u64,
}

impl Drag {
    fn new(draft: DraftId) -> Self {
        Self {
            draft,
            plan: None,
            wanted: None,
            held: None,
            requested: None,
            failed: None,
            surface: None,
            reason: None,
            ended: None,
            gpu_ticks: 0,
            cpu_ticks: 0,
            boundary_requests: 0,
        }
    }
}

/// The desktop's GPU previews: the open draft's, and the warm list of the committed stack.
#[derive(Default)]
pub(crate) struct GpuPreviews {
    drag: Option<Drag>,
    /// The last boundary version handed out: each held boundary is uploaded once.
    versions: u64,
    warm: Option<GpuWarm>,
    /// What a test reports for the surface, which no test draws.
    #[cfg(test)]
    pub(crate) surface: Option<SurfaceReport>,
}

/// One tick's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    /// The surface draws the plan: no preview job and no upload.
    Gpu,
    /// The tick's preview job goes to the worker as today.
    Cpu,
}

impl GpuPreviews {
    /// The plan the surface is handed this frame, whether it holds it behind the CPU frame, and
    /// the draft revision it is tagged with.
    pub(crate) fn surface_plan(&self) -> Option<(&surface::GpuPlan, u64)> {
        self.drag
            .as_ref()
            .and_then(|drag| drag.surface.as_ref())
            .map(|(plan, revision)| (plan, *revision))
    }

    pub(crate) fn warm(&self) -> Option<&GpuWarm> {
        self.warm.as_ref()
    }

    /// The boundary version a held boundary is drawn under, for the capture's readiness.
    pub(crate) fn held_version(&self) -> Option<u64> {
        self.drag
            .as_ref()
            .and_then(|drag| drag.held.as_ref())
            .map(|held| held.boundary.version())
    }

    /// The open drag's figures, as evidence and the tests read them.
    pub(crate) fn summary(&self) -> Value {
        let Some(drag) = &self.drag else {
            return json!({"drag": null, "warm": self.warm.as_ref().map(GpuWarm::version)});
        };
        json!({
            "drag": {
                "draft_id": drag.draft.as_str(),
                "plan_revision": drag.plan.as_ref().map(|(_, revision)| *revision),
                "surface_revision": drag.surface.as_ref().map(|(_, revision)| *revision),
                "boundary": drag.held.as_ref().map(|held| json!({
                    "version": held.boundary.version(),
                    "width": held.boundary.size().0,
                    "height": held.boundary.size().1,
                    "origin": [held.origin.0, held.origin.1],
                    "layer": held.key.layer(),
                })),
                "boundary_requested": drag.requested,
                "boundary_requests": drag.boundary_requests,
                "reason": drag.reason,
                "ended": drag.ended.is_some(),
                "gpu_ticks": drag.gpu_ticks,
                "cpu_ticks": drag.cpu_ticks,
            },
            "warm": self.warm.as_ref().map(GpuWarm::version),
        })
    }

    #[cfg(test)]
    pub(crate) fn ticks(&self) -> (u64, u64, u64) {
        self.drag.as_ref().map_or((0, 0, 0), |drag| {
            (drag.gpu_ticks, drag.cpu_ticks, drag.boundary_requests)
        })
    }

    #[cfg(test)]
    pub(crate) fn holds_boundary(&self) -> bool {
        self.held_version().is_some()
    }

    #[cfg(test)]
    pub(crate) fn has_drag(&self) -> bool {
        self.drag.is_some()
    }
}

impl Editor {
    /// What the surface reports of its last frame.
    fn surface_report(&self) -> SurfaceReport {
        #[cfg(test)]
        if let Some(report) = self.gpu.surface {
            return report;
        }
        SurfaceReport::of(&luxforge_ui::surface_diagnostics(
            crate::view::canvas::DEVELOP_SURFACE,
        ))
    }

    /// One tick of the open draft's gesture, answered with the GPU `preview` its job carries:
    /// whether the surface draws it from the plan, with no preview job and no upload, or the job
    /// goes to the worker as today — carrying the returned boundary request while the boundary is
    /// not held.
    pub(crate) fn gpu_tick(
        &mut self,
        set: &Draft,
        preview: Option<Box<GpuPreview>>,
    ) -> (Tick, Option<BoundaryRequest>) {
        let mut boundary_request = None;
        // With the preference off the plan is never handed over, so nothing is asked for it.
        let allowed = self.gpu_preview_allowed();
        let report = self.surface_report();
        let mut released = None;
        let drag = match &mut self.gpu.drag {
            Some(drag) if drag.draft == set.draft_id && drag.ended.is_none() => drag,
            slot => slot.insert(Drag::new(set.draft_id.clone())),
        };
        // A tick with no plan wants no boundary: one held is let go, as on any change of key.
        let unplanned = |drag: &mut Drag, reason: &str, released: &mut Option<u64>| {
            drag.surface = None;
            drag.plan = None;
            drag.wanted = None;
            if let Some(held) = drag.held.take() {
                *released = Some(held.boundary.version());
            }
            drag.reason = Some(reason.into());
            Tick::Cpu
        };
        let tick = match preview.map(|preview| *preview) {
            _ if allowed.is_err() => unplanned(
                drag,
                allowed.err().unwrap_or(super::gpu_settle::PREFERENCE_OFF),
                &mut released,
            ),
            None => unplanned(drag, "not-fit", &mut released),
            Some(luxforge_core::GpuPreview {
                answer: GpuAnswer::Fallback(reason),
                ..
            }) => unplanned(drag, reason.code(), &mut released),
            Some(luxforge_core::GpuPreview {
                answer: GpuAnswer::Plan(plan),
                boundary,
            }) => {
                let Some(request) = boundary else {
                    drag.reason = Some("unplannable".into());
                    drag.cpu_ticks += 1;
                    return (Tick::Cpu, None);
                };
                if let Some(held) = drag.held.take_if(|held| held.key != request.key) {
                    // The plan needs another boundary: the window moved, the bounds changed, or
                    // the layers before the boundary did.
                    drag.surface = None;
                    drag.failed = None;
                    released = Some(held.boundary.version());
                }
                let revision = set.draft_revision;
                drag.wanted = Some(request.clone());
                drag.plan = Some((plan, revision));
                match &drag.held {
                    None => {
                        drag.surface = None;
                        if drag.failed.as_ref() == Some(&request.key) {
                            drag.reason = Some("boundary-failed".into());
                        } else {
                            drag.reason = Some("boundary-pending".into());
                            let pending = self.presentation.queue.pending_generation();
                            if drag.requested.is_none() || drag.requested == pending {
                                boundary_request = Some(request);
                                drag.boundary_requests += 1;
                            }
                        }
                        Tick::Cpu
                    }
                    Some(held) => {
                        let (plan, _) = drag.plan.as_ref().expect("the plan just kept");
                        match gpu_plan::surface_plan_at(
                            plan,
                            held.boundary.clone(),
                            held.origin,
                            held.grid.as_deref(),
                        ) {
                            Err(unrunnable) => {
                                drag.surface = None;
                                drag.reason = Some(unrunnable.code().into());
                                Tick::Cpu
                            }
                            Ok(converted) => {
                                let version = held.boundary.version();
                                drag.surface = Some((converted, revision));
                                if report.ready_boundary == Some(version)
                                    && report.fallback.is_none()
                                {
                                    drag.reason = None;
                                    Tick::Gpu
                                } else {
                                    drag.reason = Some(
                                        report
                                            .fallback
                                            .map_or("surface-pending", SurfaceFallback::as_str)
                                            .into(),
                                    );
                                    Tick::Cpu
                                }
                            }
                        }
                    }
                }
            }
        };
        match tick {
            Tick::Gpu => drag.gpu_ticks += 1,
            Tick::Cpu => drag.cpu_ticks += 1,
        }
        if let Some(version) = released {
            self.log_release(version, "key-changed");
        }
        (tick, boundary_request)
    }

    /// The tick's job, carrying the boundary request, was queued as `generation`.
    pub(crate) fn gpu_boundary_requested(&mut self, generation: u64) {
        if let Some(drag) = &mut self.gpu.drag {
            drag.requested = Some(generation);
        }
    }

    /// One tick drawn on the GPU: the evidence that ties it to the frame the surface draws.
    pub(crate) fn gpu_ticked(&mut self, set: &Draft) {
        let version = self.gpu.held_version();
        self.event("gpu_preview_tick", || {
            json!({
                "draft_id": set.draft_id.as_str(),
                "draft_revision": set.draft_revision,
                "path": "gpu",
                "boundary": version,
            })
        });
    }

    /// The surface is handed the plan of the open draft's newest revision, to draw on the GPU.
    pub(crate) fn gpu_draws_newest_tick(&self) -> bool {
        let surfaces = self.surfaces();
        surfaces.gpu.is_some()
            && !surfaces.gpu_hold
            && surfaces.gpu_tag.is_some()
            && surfaces.gpu_tag
                == self
                    .session
                    .draft
                    .as_ref()
                    .map(|draft| draft.draft_revision)
    }

    /// A tick drawn on the GPU puts the gesture's frame on screen as a CPU frame of it would, for
    /// whoever waits on one: its newest once nothing the gesture asked for is still to bring a
    /// frame of its own. The surface draws it at the next render.
    pub(crate) fn gpu_tick_presented(&mut self) {
        let Some(gesture) = self.core_gesture() else {
            return;
        };
        let presented = super::outcome::Presented::Draft {
            slider: gesture.slider().is_some(),
            newest: !gesture.draft.frame_pending(),
        };
        self.outcome(super::outcome::Outcome::Presented(presented));
    }

    /// A tick of the open draft that took the CPU path, and why.
    pub(crate) fn gpu_cpu_tick(&self, set: &Draft, generation: u64) {
        let Some(drag) = &self.gpu.drag else {
            return;
        };
        self.event("gpu_preview_tick", || {
            json!({
                "draft_id": set.draft_id.as_str(),
                "draft_revision": set.draft_revision,
                "path": "cpu",
                "reason": drag.reason,
                "generation": generation,
                "boundary_requested": drag.requested == Some(generation),
            })
        });
    }

    /// A job's boundary: held for the open draft when it is the one that draft's plan wants, or
    /// let go. A failed one keeps the draft on the CPU path for that key.
    pub(crate) fn gpu_boundary_ready(&mut self, result: luxforge_core::PreviewResult) {
        let generation = result.generation;
        let draft = result
            .identity
            .draft
            .as_ref()
            .map(|stamp| stamp.draft_id.clone());
        let render_ms = result.render_ms;
        let luxforge_core::PhaseOutcome::Boundary(outcome) = result.outcome else {
            return;
        };
        let Some(drag) = self
            .gpu
            .drag
            .as_mut()
            .filter(|drag| Some(&drag.draft) == draft.as_ref() && drag.ended.is_none())
        else {
            self.event(
                "gpu_boundary_dropped",
                || json!({"generation": generation, "why": "no open draft"}),
            );
            return;
        };
        if drag.requested == Some(generation) {
            drag.requested = None;
        }
        let wanted = drag
            .wanted
            .as_ref()
            .is_some_and(|wanted| wanted.key == outcome.key);
        let detail = match (outcome.result, wanted) {
            (Ok(frame), true) => {
                self.gpu.versions += 1;
                let version = self.gpu.versions;
                let size = (frame.width, frame.height);
                let origin = frame.origin;
                let format = gpu_plan::boundary_format(frame.format);
                match GpuBoundary::new(frame.texels, size.0, size.1, version, format) {
                    Some(boundary) => {
                        drag.held = Some(Held {
                            key: outcome.key,
                            boundary,
                            origin,
                            grid: outcome.grid.and_then(Result::ok),
                        });
                        drag.failed = None;
                        // The latest tick's plan is drawn now, before the next input.
                        if let Some((plan, revision)) = &drag.plan
                            && let Some(held) = &drag.held
                        {
                            drag.surface = gpu_plan::surface_plan_at(
                                plan,
                                held.boundary.clone(),
                                held.origin,
                                held.grid.as_deref(),
                            )
                            .ok()
                            .map(|converted| (converted, *revision));
                        }
                        json!({"held": true, "version": version, "width": size.0,
                            "height": size.1, "origin": [origin.0, origin.1],
                            "render_ms": render_ms})
                    }
                    None => json!({"held": false, "why": "malformed texels"}),
                }
            }
            (Ok(_), false) => json!({"held": false, "why": "another key"}),
            (Err(error), _) => {
                if wanted {
                    drag.failed = Some(outcome.key);
                }
                json!({"held": false, "why": error.to_string()})
            }
        };
        self.event("gpu_boundary", || {
            let mut detail = detail;
            detail["generation"] = json!(generation);
            detail
        });
    }

    /// The preview queue was cancelled: a boundary request in flight will not be answered.
    pub(crate) fn gpu_queue_cancelled(&mut self) {
        if let Some(drag) = &mut self.gpu.drag {
            drag.requested = None;
        }
    }

    /// A committed stack's job carries the plans its gestures are likely to draw: hand their
    /// sequences to the surface to compile before a drag begins.
    pub(crate) fn gpu_warm_from(&mut self, plans: Option<&[luxforge_core::GpuPlan]>) {
        let Some(plans) = plans else {
            return;
        };
        let sequences: Vec<Vec<GpuStep>> = plans
            .iter()
            .filter_map(|plan| gpu_plan::plan_steps(plan).ok())
            .collect();
        let same = self
            .gpu
            .warm
            .as_ref()
            .is_some_and(|warm| warm.sequences() == sequences.as_slice());
        if same || sequences.is_empty() {
            return;
        }
        let version = self.gpu.warm.as_ref().map_or(1, |warm| warm.version() + 1);
        self.event(
            "gpu_preview_warm",
            || json!({"version": version, "sequences": sequences.len()}),
        );
        self.gpu.warm = Some(GpuWarm::new(version, sequences));
    }

    /// The open gesture's converted plan and the draft revision it draws, at Fit with no
    /// comparison on screen, where the surface runs it.
    pub(crate) fn gesture_gpu_plan(&self) -> Option<(&surface::GpuPlan, u64)> {
        (self.presentation.compare_after.is_none()
            && matches!(self.session.preview.view.zoom, luxforge_core::Zoom::Fit))
        .then(|| self.gpu.surface_plan())
        .flatten()
    }

    /// Why the desktop hands the surface no plan for the open gesture's newest tick, or why that
    /// tick took the CPU path: the preference, the plan's reason, a boundary not yet held, the
    /// converter's reason or the surface's fallback.
    pub(crate) fn gpu_plan_fallback(&self) -> Option<String> {
        if let Err(reason) = self.gpu_preview_allowed() {
            return Some(reason.into());
        }
        self.gpu.drag.as_ref().and_then(|drag| drag.reason.clone())
    }

    /// Whether the surface holds the drawn plan behind the CPU frame: the CPU frame of the drawn
    /// revision, or a newer one, is presented, and it is the reference.
    pub(crate) fn gpu_held(&self) -> bool {
        let Some((_, revision)) = self.gpu.surface_plan() else {
            return false;
        };
        let draft = self.gpu.drag.as_ref().map(|drag| &drag.draft);
        self.presentation.displayed_draft_id.as_ref() == draft
            && self
                .presentation
                .displayed_draft_revision
                .is_some_and(|displayed| displayed >= revision)
    }

    /// The evidence of a boundary let go.
    fn log_release(&self, version: u64, why: &str) {
        self.event(
            "gpu_boundary_released",
            || json!({"version": version, "why": why}),
        );
    }
}

/// After every message: a draft that ended keeps its drawn plan until the frame that replaces it
/// is presented, or until nothing more is coming, and then releases its boundary.
pub(super) fn after_message(editor: &mut Editor, _: &super::Before) -> iced::Task<super::Message> {
    let open = editor
        .core_gesture()
        .map(|gesture| gesture.draft.draft_id.clone());
    let presented = editor.presentation.presented_generation;
    let idle = !editor.presentation.queue.is_busy() && !editor.presentation.queue.ready();
    let Some(drag) = &mut editor.gpu.drag else {
        return iced::Task::none();
    };
    if open.as_ref() == Some(&drag.draft) {
        return iced::Task::none();
    }
    let ended = *drag.ended.get_or_insert(presented);
    if presented > ended || idle {
        let released = drag.held.as_ref().map(|held| held.boundary.version());
        editor.gpu.drag = None;
        editor.event(
            "gpu_boundary_released",
            || json!({"version": released, "why": "draft-ended"}),
        );
    }
    iced::Task::none()
}
