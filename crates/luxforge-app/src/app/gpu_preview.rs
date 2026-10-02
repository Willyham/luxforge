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
//! - **At 100% and above.** A tick asks for the plan over the visible region of the output stage
//!   at full scale ([`GpuAsk::Region`]), or over the region its drag already asked for while that
//!   still holds the view, so a pan inside it keeps the boundary. The boundary is that region's
//!   window, and the surface draws the plan's frame alone at the region's place in the
//!   photograph. While the frame holds the view it answers it: no region job until the shared
//!   quiet policy settles the view exactly. A view the region does not hold withdraws the plan, so
//!   the CPU's frames are drawn, never a mix of the two, until a tick plans the new region and its
//!   boundary is held. A region whose boundary and frame alone would pass the GPU-preview budget
//!   asks for no boundary and keeps the CPU path, naming the budget; so does one the surface finds
//!   over it once held. While the mask overlay is shown at a percentage zoom the gesture keeps the
//!   CPU path, whose region frames carry the overlay.
use super::{Editor, gpu_plan};
use luxforge_core::{
    BoundaryKey, BoundaryRequest, CoordinateGrid, Draft, DraftId, GpuAnswer, GpuPreview, Region,
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
    /// The revision of the entry that plan's draft was planned over.
    base: Option<u64>,
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
    /// The percentage zoom the latest tick's region was asked at; `None` at Fit.
    zoom: Option<f32>,
    /// What a region's boundary or slot would take, and the bound on a boundary or the budget it
    /// passes, when the latest tick asked for no boundary because of them ([`region_charge`]).
    over_budget: Option<(u64, u64)>,
    /// At a percentage zoom, the shape the latest tick's restoration or spatial layer is planned
    /// in when its owner planned both: `gpu`, every unit, or `cpu`, the units its values need,
    /// when only that one fits the budget. `None` when there was no choice.
    shape: Option<&'static str>,
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
            base: None,
            wanted: None,
            held: None,
            requested: None,
            failed: None,
            surface: None,
            reason: None,
            zoom: None,
            over_budget: None,
            shape: None,
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
    /// The budget a test holds a region's boundary to, in place of the surface's.
    #[cfg(test)]
    pub(crate) budget: Option<u64>,
}

/// What a tick asks the owner to plan its GPU preview for, with its preview job.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum GpuAsk {
    /// No GPU preview: a zoom below 100%, where the surface draws none.
    Off,
    /// At Fit, at the job's display bounds.
    Fit,
    /// At a percentage zoom of 100% or more: this region of the output stage at full scale, drawn
    /// at this many physical pixels an output pixel.
    Region(Region, f64),
}

/// Before a region's boundary is rendered, what the boundary alone takes and the least the slot
/// drawing it takes on the GPU: the boundary over the window its request names, at its format's
/// bytes a texel; a geometry tail's intermediate of the same size, at eight at least; the frame,
/// at four bytes a pixel of the region; and a spatial step's planes over the window. The surface
/// charges the rest — the frame's size bucket, its uniform and its buffers — once it is held. At
/// Fit at the exact stage the frame is the whole output stage and the boundary the window it
/// reads, or the whole boundary stage. `None` at a Fit proxy, which the display bounds bound.
pub(crate) fn region_charge(plan: &CorePlan, request: &BoundaryRequest) -> Option<(u64, u64)> {
    let whole = |width, height| Region {
        x0: 0,
        y0: 0,
        width,
        height,
    };
    let (rect, window) = match (request.key.region(), request.key.plan()) {
        (Some(rect), _) => (rect, request.window?),
        (None, None) => {
            let (output, stage) = (plan.geometry.output(), plan.boundary.stage);
            let window = request.window.unwrap_or(whole(stage.width, stage.height));
            (whole(output.width, output.height), window)
        }
        (None, Some(_)) => return None,
    };
    let texels = u64::from(window.width) * u64::from(window.height);
    let boundary = texels
        * match request.format {
            luxforge_core::BoundaryFormat::Half => 8,
            luxforge_core::BoundaryFormat::Float => 16,
        };
    let intermediate = if gpu_plan::has_tail(plan) {
        texels * 8
    } else {
        0
    };
    let frame = u64::from(rect.width) * u64::from(rect.height) * 4;
    // The spatial steps' planes as the surface's slot holds them, chained steps sharing textures;
    // each operation's own planes summed where a step cannot be converted.
    let (origin, size) = ((window.x0, window.y0), (window.width, window.height));
    let steps: Option<Vec<surface::GpuStep>> = plan
        .spatial
        .iter()
        .map(|spatial| gpu_plan::spatial_step(spatial).ok())
        .collect();
    let planes = match steps {
        Some(steps) => surface::gpu_preview::spatial::plane_bytes(&steps, size, origin),
        None => plan
            .spatial
            .iter()
            .map(|spatial| spatial.plane_bytes(origin, size))
            .sum(),
    };
    Some((boundary, boundary + intermediate + frame + planes))
}

/// A surface region's rectangle of its stage, as the core's.
fn rect_of(region: surface::GpuRegion) -> Region {
    let [x0, y0, x1, y1] = region.rect;
    Region {
        x0,
        y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
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

    /// The draft whose plan the surface is handed, open or ended and not yet released.
    pub(crate) fn draft(&self) -> Option<&DraftId> {
        self.drag.as_ref().map(|drag| &drag.draft)
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
                // A global estimate taken on the GPU rather than read from the store.
                "approximate": drag.plan.as_ref().map(|(plan, _)| plan.approximate()),
                "surface_revision": drag.surface.as_ref().map(|(_, revision)| *revision),
                "boundary": drag.held.as_ref().map(|held| json!({
                    "version": held.boundary.version(),
                    "width": held.boundary.size().0,
                    "height": held.boundary.size().1,
                    "origin": [held.origin.0, held.origin.1],
                    "layer": held.key.layer(),
                    // Whether the desktop still holds the texels, which it lets go once the
                    // surface's slot holds them.
                    "texels_held": held.boundary.holds_texels(),
                    "region": held.key.region().map(|rect| {
                        [rect.x0, rect.y0, rect.width, rect.height]
                    }),
                })),
                "zoom": drag.zoom,
                "over_budget": drag.over_budget.map(|(requested, budget)| {
                    json!({"requested": requested, "budget": budget})
                }),
                "shape": drag.shape,
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

    /// What the latest tick's plan takes over its region ([`region_charge`]).
    #[cfg(test)]
    pub(crate) fn region_charge(&self) -> Option<(u64, u64)> {
        let drag = self.drag.as_ref()?;
        region_charge(&drag.plan.as_ref()?.0, drag.wanted.as_ref()?)
    }
}

impl Editor {
    /// The GPU-preview budget a region's boundary and frame are held to before it is rendered: the
    /// surface's own.
    fn gpu_budget(&self) -> u64 {
        #[cfg(test)]
        if let Some(budget) = self.gpu.budget {
            return budget;
        }
        surface::gpu_preview::GPU_PREVIEW_BUDGET
    }

    /// What the surface reports of its last frame.
    pub(crate) fn surface_report(&self) -> SurfaceReport {
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
        // The clipping overlay is derived from the CPU's frames, so over a GPU frame the plan marks
        // its own clipped pixels instead.
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
        // At a percentage zoom the mask overlay is drawn with the CPU's region frames, which a
        // GPU frame drawn alone does not carry: while it is shown the gesture keeps the CPU path.
        let zoom = match self.session.preview.view.zoom {
            luxforge_core::Zoom::Percent { value } => Some(value),
            luxforge_core::Zoom::Fit => None,
        };
        let overlay = zoom.is_some() && self.mask_coverage_target().is_some();
        let budget = self.gpu_budget();
        // A region's boundary, or one at the exact stage at Fit, that would pass the bound on a
        // boundary, or whose slot the GPU-preview budget, is never rendered: the figure, and the
        // bound or budget it passes.
        let over_budget = |plan: &CorePlan, request: &BoundaryRequest| {
            let (boundary, slot) = region_charge(plan, request)?;
            if boundary > luxforge_core::BOUNDARY_MAX_BYTES {
                Some((boundary, luxforge_core::BOUNDARY_MAX_BYTES))
            } else {
                (slot > budget).then_some((slot, budget))
            }
        };
        self.gpu_release_texels();
        let report = self.surface_report();
        let mut released = None;
        let mut lost = None;
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
            _ if overlay => unplanned(drag, "overlay-shown", &mut released),
            None => unplanned(drag, "not-fit", &mut released),
            Some(luxforge_core::GpuPreview {
                answer: GpuAnswer::Fallback(reason),
                ..
            }) => unplanned(drag, reason.code(), &mut released),
            Some(luxforge_core::GpuPreview {
                answer: GpuAnswer::Plan(plan),
                boundary,
                cpu_shape,
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
                // The slot let go of a boundary whose texels the desktop no longer holds: the
                // tick asks for them again.
                if report.fallback == Some(SurfaceFallback::BoundaryReleased)
                    && let Some(held) = drag.held.take_if(|held| !held.boundary.holds_texels())
                {
                    drag.surface = None;
                    lost = Some(held.boundary.version());
                }

                let revision = set.draft_revision;
                // At a percentage zoom a spatial layer is drawn in its GPU shape when that fits,
                // else in the CPU's shape when that does, with a compile where a value crosses
                // zero; when neither fits, the CPU's shape names the least the drag would take.
                let (plan, over_budget, shape) = match (over_budget(&plan, &request), cpu_shape) {
                    (Some(_), Some(smaller)) => {
                        let over = over_budget(&smaller, &request);
                        (smaller, over, Some("cpu"))
                    }
                    (over, Some(_)) => (plan, over, Some("gpu")),
                    (over, None) => (plan, over, None),
                };
                drag.shape = shape;
                drag.wanted = Some(request.clone());
                drag.plan = Some((plan, revision));
                drag.base = Some(set.base_revision);
                drag.zoom = request.key.region().and(zoom);
                drag.over_budget = over_budget;
                match &drag.held {
                    None if over_budget.is_some() => {
                        drag.surface = None;
                        // The surface's own name for a plan over the budget.
                        drag.reason = Some("budget-exceeded".into());
                        Tick::Cpu
                    }
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
                        match gpu_plan::surface_plan_over(
                            plan,
                            held.boundary.clone(),
                            held.origin,
                            held.grid.as_deref(),
                            held.key.region(),
                        )
                        .map(|converted| super::gpu_settle::marked(converted, plan, clip))
                        {
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
        if let Some(version) = lost {
            self.log_release(version, "slot-released");
        }
        (tick, boundary_request)
    }

    /// Once the surface reports that its slot holds the open drag's boundary, let its texels go:
    /// the held boundary, and the plan handed to the surface, name the resident boundary from then
    /// on ([`GpuBoundary::resident`]), so the desktop keeps no copy for the rest of the gesture.
    /// Run after every message and at each tick.
    pub(crate) fn gpu_release_texels(&mut self) {
        let report = self.surface_report();
        let Some(drag) = &mut self.gpu.drag else {
            return;
        };
        let Some(held) = drag.held.as_mut().filter(|held| {
            held.boundary.holds_texels()
                && report.ready_boundary == Some(held.boundary.version())
                && report.fallback.is_none()
        }) else {
            return;
        };
        held.boundary = held.boundary.resident();
        let resident = held.boundary.clone();
        if let Some((plan, _)) = &mut drag.surface
            && plan.boundary.version() == resident.version()
        {
            plan.boundary = resident.clone();
        }
        self.event(
            "gpu_boundary_resident",
            || json!({"version": resident.version()}),
        );
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
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
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
                            drag.surface = gpu_plan::surface_plan_over(
                                plan,
                                held.boundary.clone(),
                                held.origin,
                                held.grid.as_deref(),
                                held.key.region(),
                            )
                            .ok()
                            .map(|converted| {
                                (super::gpu_settle::marked(converted, plan, clip), *revision)
                            });
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
        // While a clipping overlay is shown the gestures' plans carry its marks.
        let clip = super::gpu_settle::clip_flags(&self.session.workspace);
        let sequences: Vec<Vec<GpuStep>> = plans
            .iter()
            .filter_map(|plan| {
                gpu_plan::plan_steps(plan)
                    .ok()
                    .map(|steps| super::gpu_settle::marked_steps(steps, plan, clip))
            })
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

    /// What the next tick asks the owner to plan its GPU preview for: at Fit, the job's bounds; at
    /// 100% or more, the visible region of the output stage — or the region the open drag asked
    /// for at this zoom while it still holds the view, so a pan inside it keeps its boundary.
    pub(crate) fn gpu_ask(&self) -> GpuAsk {
        let value = match self.session.preview.view.zoom {
            luxforge_core::Zoom::Fit => return GpuAsk::Fit,
            luxforge_core::Zoom::Percent { value } if value >= 100.0 => value,
            luxforge_core::Zoom::Percent { .. } => return GpuAsk::Off,
        };
        let Some(wanted) = self
            .presentation
            .dimensions
            .and_then(|stage| self.desired_view_for(stage))
        else {
            return GpuAsk::Off;
        };
        let asked = self
            .gpu
            .drag
            .as_ref()
            .filter(|drag| drag.ended.is_none() && drag.zoom == Some(value))
            .and_then(|drag| drag.wanted.as_ref())
            .and_then(|wanted| wanted.key.region())
            .filter(|rect| super::preview::contains_region(*rect, wanted));
        GpuAsk::Region(asked.unwrap_or(wanted), f64::from(value) / 100.0)
    }

    /// Whether the open gesture's GPU frame is the view's motion frame: the surface draws its plan
    /// of a region holding `wanted`, not held behind a CPU frame, and has evaluated it.
    pub(crate) fn gpu_draws_view(&self, wanted: Region) -> bool {
        let Some((plan, _)) = self.gesture_gpu_plan() else {
            return false;
        };
        plan.region
            .is_some_and(|region| super::preview::contains_region(rect_of(region), wanted))
            && !self.gpu_held()
            && self.surface_report().ready_boundary == Some(plan.boundary.version())
    }

    /// The open gesture's converted plan and the draft revision it draws, where the surface runs
    /// it, with no comparison on screen: a whole frame's plan at Fit, and at 100% or more a
    /// region's while its region holds the view.
    pub(crate) fn gesture_gpu_plan(&self) -> Option<(&surface::GpuPlan, u64)> {
        if self.presentation.compare_after.is_some() {
            return None;
        }
        let (plan, _) = self.gpu.surface_plan()?;
        let shown = match (&self.session.preview.view.zoom, plan.region) {
            (luxforge_core::Zoom::Fit, None) => true,
            (luxforge_core::Zoom::Percent { value }, Some(region)) if *value >= 100.0 => self
                .presentation
                .dimensions
                .filter(|stage| *stage == region.stage)
                .and_then(|stage| self.desired_view_for(stage))
                .is_some_and(|wanted| super::preview::contains_region(rect_of(region), wanted)),
            _ => false,
        };
        if !shown {
            return None;
        }
        // An open draft that another client's commit conflicted, or that was reapplied over a
        // newer entry and whose first tick there has not answered yet, draws none of its plans:
        // each was planned over the entry before, which is no longer the photograph. The frame is
        // the CPU's until a tick plans over the current entry.
        let drag = self.gpu.drag.as_ref()?;
        if let Some(gesture) = self.core_gesture()
            && gesture.draft.draft_id == drag.draft
            && (gesture.draft.conflicted || Some(gesture.draft.base_revision) != drag.base)
        {
            return None;
        }
        self.gpu.surface_plan()
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
    editor.gpu_release_texels();
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
