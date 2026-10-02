//! The GPU preview's hand-off on the desktop: the `gpu_preview` preference every GPU plan is gated
//! on, and the status bar's figure for a GPU frame on screen.
//!
//! The preference is per-client workspace state the owner holds (`workspace.set {gpu_preview}`, on
//! by default). The palette's toggle sends the same request an API client sends, and the desktop
//! keeps no copy of it outside the session it adopts back. [`Editor::gpu_preview_allowed`] is the
//! one question the desktop asks before it hands the photo surface a plan, and [`Editor::gpu_plan`]
//! the one place a plan is handed from: with the preference off it hands none, every frame is the
//! CPU's, and evidence names [`PREFERENCE_OFF`].
//!
//! The status bar's render slot reads "GPU preview · N ms" while the surface draws the GPU stage's
//! output for the plan the desktop handed it ([`Editor::gpu_frame_us`]). Iced's compositor creates
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
    photo_surface::{Dissolve, DrawingPath, GpuPlan},
};
use serde_json::{Value, json};

/// Why the desktop hands the surface no GPU plan while this client's `gpu_preview` preference is
/// off, as evidence names it beside the surface's own fallback reasons.
pub(crate) const PREFERENCE_OFF: &str = "preference-off";

/// The `workspace.set` body the palette's GPU preview entry sends: the preference flipped, and
/// nothing else.
pub(crate) fn toggle_params(workspace: &WorkspaceState) -> Value {
    json!({ "gpu_preview": !workspace.gpu_preview })
}

impl Editor {
    /// Whether the desktop may hand the photo surface a GPU plan at all: `Err` with
    /// [`PREFERENCE_OFF`] while this client's `gpu_preview` preference is off, so every frame is
    /// the CPU's. Every route that hands the surface a plan asks this first.
    pub(crate) fn gpu_preview_allowed(&self) -> Result<(), &'static str> {
        if self.session.workspace.gpu_preview {
            Ok(())
        } else {
            Err(PREFERENCE_OFF)
        }
    }

    /// The GPU plan the photograph `photo` is drawn from in place of its frame: none while the
    /// preference is off. An evidence run's GPU identity hook gives one; otherwise the open
    /// gesture's plan does ([`Editor::gesture_gpu_plan`]).
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
        let plan = self.surfaces().gpu?;
        let drawn = luxforge_ui::surface_diagnostics(crate::view::canvas::DEVELOP_SURFACE);
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
    /// A frame of the open draft at this revision: the shared quiet policy's settlement.
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

/// A dissolve handed to the surface.
#[derive(Clone, Debug)]
struct Running {
    dissolve: Dissolve,
    /// The open draft and its revision when it began: any newer tick, or another gesture, is an
    /// input that cancels it.
    input: Option<(DraftId, u64)>,
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
            // The shared quiet policy, or the release, settled the drafted settings on the CPU:
            // never a revision older than the GPU frame's.
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
    /// such as the exact phase after the proxy, keeps the dissolve running to it.
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

    /// After every message: follow the GPU frame on screen, and when the CPU frame of its content
    /// replaces it at Fit — the drafted settings settled on the CPU, or the entry the draft
    /// committed — dissolve from one to the other. A cancel or any older content is a plain swap.
    /// The running dissolve ends after its 150 ms, follows a newer frame of the same content, and
    /// is cancelled by any input: a newer tick of the draft, or another gesture.
    pub(crate) fn follow_gpu_settle(&mut self) {
        let photo = self.surfaces().photo.map(Frame::version);
        let input = self
            .session
            .draft
            .as_ref()
            .map(|draft| (draft.draft_id.clone(), draft.draft_revision));
        if let Some(running) = self.gpu_settle.running.clone() {
            let elapsed_ms = running.dissolve.started.elapsed().as_secs_f64() * 1000.0;
            let detail = |why: &str| {
                json!({"from": running.dissolve.from, "to": running.dissolve.to,
                    "elapsed_ms": elapsed_ms, "why": why})
            };
            if input != running.input {
                self.gpu_settle.running = None;
                self.gpu_settle.cancelled += 1;
                self.event("gpu_dissolve_cancelled", || detail("input"));
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
            let fit = matches!(self.session.preview.view.zoom, luxforge_core::Zoom::Fit)
                && self.presentation.compare_after.is_none();
            let content = self.shown_content(&shown).filter(|_| fit);
            let decision = match (&content, photo) {
                (Some(content), Some(to)) => {
                    let dissolve = Dissolve::start(shown.revision, to);
                    self.gpu_settle.running = Some(Running {
                        dissolve,
                        input: input.clone(),
                        content: content.clone(),
                        boundary: shown.boundary,
                    });
                    self.gpu_settle.started += 1;
                    json!({"dissolve": true, "case": content.case(), "from": shown.revision,
                        "to": to, "gpu_boundary": shown.boundary})
                }
                _ => json!({"dissolve": false, "from": shown.revision, "to": photo,
                    "why": if fit { "older content" } else { "not fit" }}),
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
        message::{Message, palette::PaletteMessage, view::ViewMessage},
        tasks::call,
        testing::{finish, opened, scripted_evidence},
    };
    use crate::state::palette::PaletteAction;
    use luxforge_core::ClientSession;
    use luxforge_ui::photo_surface::GpuBoundary;
    use std::sync::Arc;

    #[test]
    fn the_toggle_flips_only_the_preference() {
        let mut workspace = WorkspaceState::default();
        assert!(workspace.gpu_preview, "on by default");
        assert_eq!(toggle_params(&workspace), json!({"gpu_preview": false}));
        workspace.gpu_preview = false;
        assert_eq!(toggle_params(&workspace), json!({"gpu_preview": true}));
    }

    /// Run the palette's GPU preview entry as a person does — open, type, Enter — and then the
    /// `workspace.set` its message sends, on the desktop's own client, as the runtime's executor
    /// would, adopting the owner's answer. Returns the entry that ran.
    fn run_palette_entry(editor: &mut Editor) -> crate::state::palette::PaletteEntry {
        let _ = editor.update(Message::Palette(PaletteMessage::Open));
        let _ = editor.update(Message::Palette(PaletteMessage::Query(
            "gpu preview".into(),
        )));
        let entry = editor
            .workspace
            .palette
            .entries
            .first()
            .cloned()
            .expect("a GPU preview entry");
        let before = editor.session.clone();
        let body = toggle_params(&editor.session.workspace);
        let _ = editor.update(Message::Palette(PaletteMessage::Run));
        assert!(!editor.workspace.palette.open, "running an entry closes it");
        assert!(!editor.busy, "a preference never takes the mutation path");
        assert_eq!(
            editor.session, before,
            "the desktop holds no flag of its own: nothing changes until the owner answers"
        );
        let (answer, _) = call(&editor.owner, editor.client, "workspace.set", body).unwrap();
        let session: ClientSession = serde_json::from_value(answer).unwrap();
        let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
        entry
    }

    /// Pillar 2's parity: the palette entry and an agent's `workspace.set {gpu_preview}` produce the
    /// same session state, which `session.state`, the desktop's adopted session and its evidence all
    /// report; with the preference off the desktop names it as the reason it hands no plan.
    #[test]
    fn the_palette_entry_and_the_api_produce_the_same_preference() {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 4);
        let agent = editor.owner.register();
        let workspace = |editor: &Editor, client| {
            call(&editor.owner, client, "session.state", json!({}))
                .unwrap()
                .0["workspace"]
                .clone()
        };
        assert_eq!(editor.gpu_preview_allowed(), Ok(()), "on by default");
        assert_eq!(
            editor.snapshot()["surface"]["gpu"]["plan_fallback"],
            Value::Null
        );

        let entry = run_palette_entry(&mut editor);
        assert_eq!(entry.label, "Turn off GPU preview");
        assert_eq!(entry.detail, "workspace.set");
        assert_eq!(entry.action, PaletteAction::ToggleGpuPreview);
        let (by_api, _) = call(
            &editor.owner,
            agent,
            "workspace.set",
            json!({"gpu_preview": false}),
        )
        .unwrap();
        assert_eq!(workspace(&editor, editor.client), by_api["workspace"]);
        assert_eq!(workspace(&editor, agent), by_api["workspace"]);
        assert_eq!(editor.snapshot()["workspace"], by_api["workspace"]);
        assert_eq!(by_api["workspace"]["gpu_preview"], json!(false));
        assert_eq!(editor.gpu_preview_allowed(), Err(PREFERENCE_OFF));
        assert_eq!(
            editor.snapshot()["surface"]["gpu"]["plan_fallback"],
            json!({"reason": "preference-off"})
        );

        // And back on, the entry naming what it now does.
        let entry = run_palette_entry(&mut editor);
        assert_eq!(entry.label, "Turn on GPU preview");
        let (by_api, _) = call(
            &editor.owner,
            agent,
            "workspace.set",
            json!({"gpu_preview": true}),
        )
        .unwrap();
        assert_eq!(workspace(&editor, editor.client), by_api["workspace"]);
        assert_eq!(editor.snapshot()["workspace"], by_api["workspace"]);
        assert_eq!(editor.gpu_preview_allowed(), Ok(()));
        finish(editor, catalog);
    }

    /// With the preference off the desktop hands the surface no plan, whatever would give one; on
    /// again, the same plan is handed.
    #[test]
    fn with_the_preference_off_no_plan_reaches_the_surface() {
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
        let mut session = editor.session.clone();
        session.revision += 1;
        session.workspace.gpu_preview = false;
        let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
        assert!(editor.gpu_plan(Some(&photo)).is_none());
        assert_eq!(
            editor.gpu_frame_us(),
            None,
            "no GPU frame can be named either"
        );
        let mut session = editor.session.clone();
        session.revision += 1;
        session.workspace.gpu_preview = true;
        let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
        assert!(editor.gpu_plan(Some(&photo)).is_some());
        editor.evidence = None;
        finish(editor, catalog);
    }
}
