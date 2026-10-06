//! The GPU preview's hand-off on the desktop: the gate every GPU plan passes — the GPU stage able
//! to draw at all — and the status bar's figure for a GPU frame on screen.
//!
//! [`Editor::gpu_preview_allowed`] is the one question the desktop asks before it hands the photo
//! surface a plan, and [`Editor::gpu_plan`] the one place a plan is handed from: while the
//! surface's GPU stage cannot draw on its device, or the launch refused it (`--no-gpu-render`), it
//! hands none, every frame is the reference renderer's, and evidence names the stage's reason
//! (`no-adapter` or `device-lost`, [`super::renderer`]).
//!
//! The status bar's render slot reads "GPU preview · N ms" while the surface draws the GPU stage's
//! output for a gesture's plan, and "GPU render · N ms" while it draws the committed stack at rest,
//! its view plan or its picture at rest in tiles, N then the interface thread's time over the
//! frames that drew the tiles ([`Editor::gpu_frame_us`]). Iced's compositor creates
//! its device with no optional features, so the device the surface receives has no timestamp
//! queries on any adapter; N is therefore the interface thread's own time to prepare that frame in
//! the surface's `prepare` — writing its words, uploading a new boundary, encoding and submitting
//! its pass — and not the GPU's execution time, which nothing reads back. The queue's completion
//! callback would add the GPU's execution, but it is reported at the next submit, so on the M4 its
//! figure is mostly the wait for that submit; evidence keeps it as `gpu_preview_done_us` (the
//! design's "Labels and overlays during motion"). Only a draw knows which
//! path drew it, so the label describes the frame the surface drew last, as the photograph's
//! updating state does: a change of drawing path wakes the desktop, and the update that wake runs
//! names the new path.
use super::Editor;
use luxforge_core::{DraftId, EntryId, WorkspaceState};
use luxforge_ui::{
    Frame,
    photo_surface::{ClipMarks, Dissolve, DrawingPath, GpuPlan, GpuStep},
};
use serde_json::{Value, json};

/// The clipping overlay's classes, shadows and highlights, while this client shows one.
pub(crate) fn clip_flags(workspace: &WorkspaceState) -> Option<[bool; 2]> {
    (workspace.clip_shadows || workspace.clip_highlights)
        .then_some([workspace.clip_shadows, workspace.clip_highlights])
}

/// `steps` with the clipping overlay's marks as their last step while one is shown
/// ([`ClipMarks`]): the CPU's overlay is derived from the CPU's frames and would mark another
/// frame's pixels over a GPU one, so the plan marks its own, by the quantizer's thresholds the
/// core's `plan` names, in the overlay's own colours. Approximate, as evidence says.
pub(crate) fn marked_steps(
    mut steps: Vec<GpuStep>,
    plan: &luxforge_core::GpuPlan,
    flags: Option<[bool; 2]>,
) -> Vec<GpuStep> {
    if let Some([shadows, highlights]) = flags {
        let palette = super::overlay::palette();
        steps.push(GpuStep::Clipping(ClipMarks {
            shadows,
            highlights,
            shadow_below: plan.clipping.shadow_below,
            highlight_from: plan.clipping.highlight_from,
            palette: [palette[1], palette[2], palette[3]],
        }));
    }
    steps
}

/// [`marked_steps`] of a converted plan.
pub(crate) fn marked(
    mut converted: GpuPlan,
    plan: &luxforge_core::GpuPlan,
    flags: Option<[bool; 2]>,
) -> GpuPlan {
    converted.steps = marked_steps(std::mem::take(&mut converted.steps), plan, flags);
    converted
}

impl Editor {
    /// Whether the desktop may hand the photo surface a GPU plan at all: `Err` with the stage's
    /// reason while the surface's GPU stage cannot draw on its device or the launch refused it
    /// ([`Editor::gpu_stage_refusal`]), so every frame is the reference renderer's. Every route
    /// that hands the surface a plan asks this first.
    pub(crate) fn gpu_preview_allowed(&self) -> Result<(), &'static str> {
        match self.gpu_stage_refusal() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// The GPU plan the photograph `photo` is drawn from in place of its frame: none while the gate
    /// refuses ([`Editor::gpu_preview_allowed`]). An evidence run's GPU identity hook gives one;
    /// otherwise the open gesture's plan does ([`Editor::gesture_gpu_plan`]).
    pub(crate) fn gpu_plan<'a>(&'a self, photo: Option<&'a Frame>) -> Option<&'a GpuPlan> {
        self.gpu_preview_allowed().ok()?;
        if let Some(hook) = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.gpu_identity.as_ref())
        {
            return hook.plan_for(photo?);
        }
        self.gesture_gpu_plan().map(|(plan, _)| plan)
    }

    /// The figure for the status bar while the GPU stage's output is the photograph on screen: the
    /// interface thread's time, in microseconds, to prepare the GPU frame the surface last drew,
    /// when that draw was the GPU stage's output over the boundary of the plan handed to it now.
    /// `None` whenever the photograph is the CPU's frame, a fallback's or a dissolve's included.
    pub(crate) fn gpu_frame_us(&self) -> Option<u64> {
        let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
        // The picture at rest in tiles, once the surface draws it: the interface thread's time
        // its tiles and their quantization took over the frames that drew them.
        if let Some(rest) = self.gpu_rest_handed()
            && drawn.drawn_rest == Some(rest.version)
        {
            return drawn
                .gpu_rest
                .filter(|figures| figures.version == rest.version)
                .map(|figures| figures.prepare_us);
        }
        let plan = self.surfaces().gpu?;
        if drawn.drawn_path == Some(DrawingPath::Gpu)
            && drawn.drawn_gpu_boundary == Some(plan.boundary.version())
        {
            drawn.gpu_preview_frame_us
        } else {
            None
        }
    }
}

/// The GPU frame the surface drew last, which the CPU frame replacing it may dissolve from.
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    /// The draft whose tick it drew, that tick's revision and the boundary it was drawn over.
    draft: DraftId,
    revision: u64,
    boundary: u64,
    /// The CPU frame the surface held behind it: the frame that replaces it is a newer one.
    photo: Option<u64>,
    /// The entry on screen behind the draft: a commit makes another one current.
    entry: Option<EntryId>,
}

/// What a dissolve's CPU frame shows, which a newer frame of the same content keeps it running to.
#[derive(Clone, Debug, PartialEq)]
enum Content {
    /// A frame of the open draft at this revision: the reference's frame of a tick the GPU did
    /// not draw.
    Draft(DraftId, u64),
    /// The frame of the entry the draft committed.
    Committed(Option<EntryId>),
}

impl Content {
    fn case(&self) -> &'static str {
        match self {
            Self::Draft(..) => "held",
            Self::Committed(_) => "committed",
        }
    }
}

/// The clipping overlays shown, and how the photograph is drawn.
type SettleView = (Option<[bool; 2]>, SettleZoom);

/// How the photograph is drawn, as far as a dissolve is concerned: at Fit or at a percentage zoom,
/// where a gesture's GPU frame settles into the CPU's, or with a comparison on screen, where none
/// dissolves.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SettleZoom {
    Fit,
    /// A percentage zoom, its value: below 100% a whole frame, the displayed-size proxy, as at Fit;
    /// at 100% or more the view's whole frame or region.
    Percent(f32),
    /// A comparison.
    Other,
}

impl SettleZoom {
    /// Whether a GPU frame drawn this way settles through a dissolve.
    fn dissolves(self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// A dissolve handed to the surface.
#[derive(Clone, Debug)]
struct Running {
    dissolve: Dissolve,
    /// The open draft and its revision when it began: any newer tick, or another gesture, is an
    /// input that cancels it.
    input: Option<(DraftId, u64)>,
    /// What the surface showed over the photograph when it began: a clipping overlay turned on or
    /// off, a change of zoom or a comparison is an input that cancels it too.
    view: SettleView,
    /// At a percentage zoom, the scroll offset it began at: a pan cancels it too.
    pan: Option<(f32, f32)>,
    content: Content,
    boundary: u64,
}

/// The desktop's half of the settle hand-off: the GPU frame on screen, and the dissolve from it to
/// the CPU frame that replaces it ([`Editor::follow_gpu_settle`]).
#[derive(Debug, Default)]
pub(crate) struct GpuSettle {
    shown: Option<Shown>,
    running: Option<Running>,
    started: u64,
    cancelled: u64,
    /// The newest GPU frame's replacement, as evidence records it.
    last: Option<Value>,
    /// What the surface last showed over the photograph, whose changes evidence records
    /// (`gpu_settle_view`): when a view input took effect, beside or apart from a dissolve.
    view: Option<SettleView>,
}

impl GpuSettle {
    /// The dissolve the surface draws now.
    pub(crate) fn dissolve(&self) -> Option<Dissolve> {
        self.running.as_ref().map(|running| running.dissolve)
    }

    /// The settle hand-off as evidence records it.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "running": self.running.as_ref().map(|running| json!({
                "from": running.dissolve.from,
                "to": running.dissolve.to,
                "gpu_boundary": running.boundary,
                "case": running.content.case(),
                "elapsed_ms": running.dissolve.started.elapsed().as_secs_f64() * 1000.0,
            })),
            "dissolves": self.started,
            "cancelled": self.cancelled,
            "last": self.last,
        })
    }
}

impl Editor {
    /// What the CPU frame on screen shows, when it is the open draft's or a commit's.
    fn shown_content(&self, shown: &Shown) -> Option<Content> {
        let displayed = &self.presentation;
        if displayed.displayed_draft_id.as_ref() == Some(&shown.draft) {
            // The reference drew the drafted settings on the CPU: never a revision older than the
            // GPU frame's.
            let revision = displayed.displayed_draft_revision?;
            return (revision >= shown.revision)
                .then(|| Content::Draft(shown.draft.clone(), revision));
        }
        let open = self.session.draft.as_ref().map(|draft| &draft.draft_id);
        let entry = self.displayed_entry();
        // The draft ended, and the entry on screen is not the one it was drawn over: its commit's.
        // A cancel shows the entry it was drawn over again, which is older content.
        (displayed.displayed_draft_id.is_none()
            && open != Some(&shown.draft)
            && entry != shown.entry)
            .then_some(Content::Committed(entry))
    }

    /// Whether the CPU frame on screen still shows `content`: a newer frame of the same settings,
    /// such as the exact frame after its reduction, keeps the dissolve running to it.
    fn shows(&self, content: &Content) -> bool {
        let displayed = &self.presentation;
        match content {
            Content::Draft(draft, revision) => {
                displayed.displayed_draft_id.as_ref() == Some(draft)
                    && displayed.displayed_draft_revision == Some(*revision)
            }
            Content::Committed(entry) => {
                displayed.displayed_draft_id.is_none() && self.displayed_entry() == *entry
            }
        }
    }

    /// How the photograph is drawn now ([`SettleZoom`]).
    fn settle_zoom(&self) -> SettleZoom {
        if self.presentation.compare_after.is_some() {
            return SettleZoom::Other;
        }
        match self.session.preview.view.zoom {
            luxforge_core::Zoom::Fit => SettleZoom::Fit,
            luxforge_core::Zoom::Percent { value } => SettleZoom::Percent(value),
        }
    }

    /// The version of the CPU frame the photograph is drawn from now: at Fit and below 100% its
    /// frame, the reference's reduction to the view; at 100% or more the view's whole frame of the
    /// current content, which a GPU region frame dissolves into, as the canvas hands it to the
    /// percentage view.
    fn settle_frame(&self, zoom: SettleZoom) -> Option<u64> {
        let surfaces = self.surfaces();
        match zoom {
            SettleZoom::Percent(value) if value >= 100.0 => surfaces
                .photo
                .filter(|_| surfaces.photo_content == Some(surfaces.current_content))
                .map(Frame::version),
            SettleZoom::Fit | SettleZoom::Percent(_) | SettleZoom::Other => {
                surfaces.photo.map(Frame::version)
            }
        }
    }

    /// After every message: follow the GPU frame on screen, and when the CPU frame of its content
    /// replaces it — the drafted settings settled on the CPU, or the entry the draft committed —
    /// dissolve from one to the other, at every zoom with no comparison on screen. A cancel or any
    /// older content is a plain swap. The running dissolve ends after its 150 ms, follows a newer
    /// frame of the same content, and is cancelled by any input: a newer tick of the draft, another
    /// gesture, or a change of what is drawn over the photograph or of the view, a pan included.
    pub(crate) fn follow_gpu_settle(&mut self) {
        let zoom = self.settle_zoom();
        let photo = self.settle_frame(zoom);
        let input = self
            .session
            .draft
            .as_ref()
            .map(|draft| (draft.draft_id.clone(), draft.draft_revision));
        let dissolves = zoom.dissolves();
        let pan = matches!(zoom, SettleZoom::Percent(_)).then_some(self.view_state.local_pan);
        let view = (clip_flags(&self.session.workspace), zoom);
        if self.gpu_settle.view.replace(view) != Some(view) {
            self.event("gpu_settle_view", || {
                json!({"clipping": view.0, "fit": view.1 == SettleZoom::Fit,
                "zoom": match view.1 {
                    SettleZoom::Percent(value) => json!(value),
                    SettleZoom::Fit => json!("fit"),
                    SettleZoom::Other => Value::Null,
                }})
            });
        }
        if let Some(running) = self.gpu_settle.running.clone() {
            let elapsed_ms = running.dissolve.started.elapsed().as_secs_f64() * 1000.0;
            let detail = |why: &str| {
                json!({"from": running.dissolve.from, "to": running.dissolve.to,
                    "elapsed_ms": elapsed_ms, "why": why})
            };
            if input != running.input || view != running.view || pan != running.pan {
                self.gpu_settle.running = None;
                self.gpu_settle.cancelled += 1;
                let why = if input == running.input {
                    "view"
                } else {
                    "input"
                };
                self.event("gpu_dissolve_cancelled", || detail(why));
            } else if running.dissolve.share(std::time::Instant::now()) >= 1.0 {
                self.gpu_settle.running = None;
                self.event("gpu_dissolve_ended", || detail("ended"));
            } else if photo != Some(running.dissolve.to) {
                if let Some(to) = photo.filter(|_| self.shows(&running.content)) {
                    // The same settings at better quality: keep dissolving, to it.
                    let dissolve = Dissolve {
                        to,
                        ..running.dissolve
                    };
                    self.event("gpu_dissolve_retargeted", || {
                        let mut detail = detail("same content");
                        detail["to"] = json!(to);
                        detail
                    });
                    if let Some(running) = &mut self.gpu_settle.running {
                        running.dissolve = dissolve;
                    }
                } else {
                    self.gpu_settle.running = None;
                    self.event("gpu_dissolve_cut", || detail("other content"));
                }
            }
        }
        // The plan the surface draws in place of the CPU frame, when it is not held behind it.
        let surfaces = self.surfaces();
        let drawing = surfaces.gpu.is_some() && !surfaces.gpu_hold;
        // A newer CPU frame behind a GPU frame that stays on screen changes nothing on it.
        if drawing && let Some(shown) = &mut self.gpu_settle.shown {
            shown.photo = photo;
        }
        // A newer CPU frame replaced the GPU frame on screen.
        if let Some(shown) = self.gpu_settle.shown.clone()
            && photo != shown.photo
        {
            self.gpu_settle.shown = None;
            let content = self.shown_content(&shown).filter(|_| dissolves);
            let decision = match (&content, photo) {
                (Some(content), Some(to)) => {
                    let dissolve = Dissolve::start(shown.revision, to);
                    self.gpu_settle.running = Some(Running {
                        dissolve,
                        input: input.clone(),
                        view,
                        pan,
                        content: content.clone(),
                        boundary: shown.boundary,
                    });
                    self.gpu_settle.started += 1;
                    json!({"dissolve": true, "case": content.case(), "from": shown.revision,
                        "to": to, "gpu_boundary": shown.boundary})
                }
                _ => json!({"dissolve": false, "from": shown.revision, "to": photo,
                    "why": if dissolves { "older content" } else { "comparison" }}),
            };
            self.event(
                if content.is_some() && photo.is_some() {
                    "gpu_dissolve_started"
                } else {
                    "gpu_settle_swapped"
                },
                || decision.clone(),
            );
            self.gpu_settle.last = Some(decision);
        }
        // The GPU frame on screen: a gesture's tick the last draw showed, tagged with its revision,
        // while its plan is still drawn in place of the CPU frame. Once the plan is held or gone,
        // the last draw's report describes a frame already replaced.
        if drawing
            && let Some((boundary, revision)) = self.surface_report().drawn
            && let Some(draft) = self.gpu.draft().cloned()
        {
            let entry = self
                .gpu_settle
                .shown
                .as_ref()
                .filter(|shown| shown.draft == draft)
                .map_or_else(|| self.displayed_entry(), |shown| shown.entry.clone());
            self.gpu_settle.shown = Some(Shown {
                draft,
                revision,
                boundary,
                photo,
                entry,
            });
        }
    }
}

/// After every message: the settle hand-off follows the GPU frame on screen
/// ([`Editor::follow_gpu_settle`]).
pub(super) fn after_message(editor: &mut Editor, _: &super::Before) -> iced::Task<super::Message> {
    editor.follow_gpu_settle();
    iced::Task::none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        gpu_identity::GpuIdentity,
        testing::{finish, opened, scripted_evidence},
    };
    use luxforge_ui::photo_surface::GpuBoundary;
    use std::sync::Arc;

    /// A captured frame reports the GPU-preview budget's figures as the surface counts them, the
    /// slots' scratch pools among them: part of what is in use, each pool counted once.
    #[test]
    fn gpu_preview_a_captured_frame_reports_the_scratch_pools_bytes() {
        let (editor, _, _, _) = opened(Vec::new(), 4);
        let gpu = editor.snapshot()["surface"]["gpu"].clone();
        let figure = |name: &str| {
            gpu[name]
                .as_u64()
                .unwrap_or_else(|| panic!("{name}: {gpu}"))
        };
        assert!(figure("gpu_preview_scratch_bytes") <= figure("gpu_preview_in_use_bytes"));
        assert!(figure("gpu_preview_in_use_bytes") <= figure("gpu_preview_peak_bytes"));
    }

    /// With the GPU stage refused the desktop hands the surface no plan, whatever would give one;
    /// with it able again, the same plan is handed.
    #[test]
    fn with_the_stage_refused_no_plan_reaches_the_surface() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        let photo = Frame::new(Arc::new(vec![0, 128, 255, 255]), 1, 1, 21).unwrap();
        let mut hook = GpuIdentity::default();
        hook.adopt(GpuBoundary::from_linear(
            luxforge_ui::photo_surface::BoundaryFormat::Half,
            1,
            1,
            21,
            [[0.0, 0.2, 1.0, 1.0]],
        ));
        let mut evidence = scripted_evidence("[]");
        evidence.gpu_identity = Some(hook);
        editor.evidence = Some(evidence);
        assert_eq!(
            editor
                .gpu_plan(Some(&photo))
                .map(|plan| plan.boundary.version()),
            Some(21)
        );
        editor.renderer.stage = Some(luxforge_ui::photo_surface::GpuStageState::DeviceLost);
        assert_eq!(editor.gpu_preview_allowed(), Err("device-lost"));
        assert!(editor.gpu_plan(Some(&photo)).is_none());
        assert_eq!(
            editor.gpu_frame_us(),
            None,
            "no GPU frame can be named either"
        );
        editor.renderer.stage = Some(luxforge_ui::photo_surface::GpuStageState::Available);
        assert!(editor.gpu_plan(Some(&photo)).is_some());
        editor.evidence = None;
        finish(editor, catalog);
    }
}
