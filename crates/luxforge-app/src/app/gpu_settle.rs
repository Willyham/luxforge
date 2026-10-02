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
use luxforge_core::WorkspaceState;
use luxforge_ui::{
    Frame,
    photo_surface::{DrawingPath, GpuPlan},
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
    /// preference is off. Only an evidence run's GPU identity hook gives one today.
    pub(crate) fn gpu_plan<'a>(&'a self, photo: Option<&'a Frame>) -> Option<&'a GpuPlan> {
        self.gpu_preview_allowed().ok()?;
        let hook = self.evidence.as_ref()?.gpu_identity.as_ref()?;
        hook.plan_for(photo?)
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
        hook.adopt(GpuBoundary::from_linear(1, 1, 21, [[0.0, 0.2, 1.0, 1.0]]));
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
